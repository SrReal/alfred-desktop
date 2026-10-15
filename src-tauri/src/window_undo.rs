//! Persistent, guarded window changes. Unknown native results remain blocked.
use crate::{geometry::Monitor, process, runtime::Core, storage, window_layouts};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Step {
    action: String,
    input: Value,
    before: Value,
    after: Option<Value>,
    state: String,
    restore_to_normal: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u32,
    tool: String,
    request: Value,
    monitors: Vec<Monitor>,
    steps: Vec<Step>,
    error: Option<String>,
}
pub(crate) struct Prepared(Record);
pub(crate) fn is_change(tool: &str) -> bool {
    matches!(
        tool,
        "window.move"
            | "window.state"
            | "window.snap"
            | "window.move_monitor"
            | "window.set_layout"
            | "window.layout_apply"
    )
}
struct Lease {
    cancel: CancellationToken,
    deadline: Instant,
}
impl Lease {
    fn new() -> Self {
        Self {
            cancel: CancellationToken::new(),
            deadline: Instant::now() + Duration::from_secs(85),
        }
    }
    fn check(&self, core: &Core) -> Result<(), String> {
        if self.cancel.is_cancelled() || Instant::now() >= self.deadline {
            return Err("WINDOW_ACTION_CANCELLED_OR_TIMEOUT".into());
        }
        if !core.enabled("window") {
            return Err("TOOL_DISABLED_OR_UNKNOWN".into());
        }
        Ok(())
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
#[cfg(windows)]
fn profile_lock(core: &Core) -> Result<File, String> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(0)
        .custom_flags(0x00200000)
        .open(core.root.join("window-actions.lock"))
        .map_err(|error| {
            if matches!(error.raw_os_error(), Some(32 | 33)) {
                "WINDOW_ACTION_BUSY".to_string()
            } else {
                format!("WINDOW_ACTION_LOCK: os={:?}", error.raw_os_error())
            }
        })?;
    let metadata = lock.metadata().map_err(|_| "WINDOW_ACTION_LOCK")?;
    if !metadata.is_file() || metadata.len() != 0 || metadata.file_attributes() & 0x400 != 0 {
        return Err("WINDOW_ACTION_LOCK_UNSAFE".into());
    }
    Ok(lock)
}
#[cfg(not(windows))]
fn profile_lock(_: &Core) -> Result<File, String> {
    Err("WINDOWS_11_REQUIRED".into())
}
fn number(value: &Value, field: &str) -> Result<i64, String> {
    let n = value[field].as_f64().ok_or("WINDOW_BOUNDS")?;
    if !n.is_finite() || n.abs() > 100000. || n.fract() != 0. {
        return Err("WINDOW_BOUNDS".into());
    }
    Ok(n as i64)
}
fn geometry(value: &Value) -> Result<(), String> {
    for field in ["x", "y", "width", "height"] {
        number(value, field)?;
    }
    if number(value, "width")? < 1 || number(value, "height")? < 1 {
        return Err("WINDOW_BOUNDS".into());
    }
    Ok(())
}
pub(crate) fn snapshot(value: &Value, require_identity: bool) -> Result<(), String> {
    if value["hwnd"].as_i64().unwrap_or(0) <= 0
        || value["process_id"].as_u64().unwrap_or(0) == 0
        || value["process_started"]
            .as_str()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0)
            == 0
        || value["monitor"].as_str().unwrap_or("").is_empty()
    {
        return Err("WINDOW_SNAPSHOT_IDENTITY".into());
    }
    if require_identity
        && !value["window_identity"].as_str().is_some_and(|s| {
            s.len() == 16
                && s != "0000000000000000"
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
    {
        return Err("WINDOW_SNAPSHOT_IDENTITY".into());
    }
    geometry(value)?;
    geometry(&value["normal_screen"])?;
    let placement = &value["placement"];
    if ![1, 2, 3].contains(&placement["show_cmd"].as_u64().unwrap_or(0)) {
        return Err("WINDOW_SNAPSHOT_STATE".into());
    }
    if let Some(latent) = placement.get("restore_maximized") {
        if latent.as_bool() != Some(true) || placement["show_cmd"] != 2 {
            return Err("WINDOW_SNAPSHOT_STATE".into());
        }
    }
    for field in [
        "min_x", "min_y", "max_x", "max_y", "left", "top", "right", "bottom",
    ] {
        number(placement, field)?;
    }
    if number(placement, "right")? <= number(placement, "left")?
        || number(placement, "bottom")? <= number(placement, "top")?
    {
        return Err("WINDOW_SNAPSHOT_STATE".into());
    }
    Ok(())
}
async fn capture(
    core: &Core,
    targets: &Value,
    initialize: bool,
    lease: &Lease,
) -> Result<Vec<Value>, String> {
    lease.check(core)?;
    let values = crate::window_snapshot::capture(targets, initialize)?;
    if values.is_empty()
        || values.len() > 32
        || values.len() != targets.as_array().ok_or("WINDOW_TARGETS")?.len()
    {
        return Err("WINDOW_SNAPSHOT".into());
    }
    for value in &values {
        snapshot(value, initialize)?;
    }
    lease.check(core)?;
    Ok(values)
}
fn planned(
    tool: &str,
    input: &Value,
    before: &Value,
    monitors: &[Monitor],
) -> Result<(String, Value, bool), String> {
    // Persist only parameters used by the native action, never caller-supplied
    // application titles/content or unrelated window metadata.
    let mut target = serde_json::Map::new();
    for field in [
        "hwnd",
        "process_id",
        "process_started",
        "window_identity",
        "x",
        "y",
        "width",
        "height",
        "state",
    ] {
        if let Some(value) = input.get(field) {
            target.insert(field.into(), value.clone());
        }
    }
    let mut target = Value::Object(target);
    target["hwnd"] = before["hwnd"].clone();
    if tool == "window.state" {
        if !["minimize", "maximize", "restore"].contains(&input["state"].as_str().unwrap_or("")) {
            return Err("WINDOW_STATE".into());
        }
        return Ok(("state".into(), target, false));
    }
    let normal = matches!(tool, "window.snap" | "window.move_monitor");
    if !normal
        && input["process_started"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
        && before["placement"]["show_cmd"] != 1
    {
        return Err("LAYOUT_WINDOW_NOT_NORMAL".into());
    }
    if normal {
        let monitor_id = if let Some(monitor) = input["monitor"].as_str() {
            monitor
        } else if tool == "window.move_monitor" {
            return Err("MONITOR_REQUIRED".into());
        } else {
            before["monitor"].as_str().ok_or("MONITOR_REQUIRED")?
        };
        let destination = monitors
            .iter()
            .find(|m| m.id == monitor_id)
            .ok_or("MONITOR_NOT_FOUND")?;
        let area = destination.work;
        let (mut x, mut y, mut width, mut height) = (
            area.x as i64,
            area.y as i64,
            area.width as i64,
            area.height as i64,
        );
        if tool == "window.snap" {
            let half_width = width / 2;
            let half_height = height / 2;
            match input["kind"].as_str().unwrap_or("") {
                "left" => width = half_width,
                "right" => {
                    x += half_width;
                    width -= half_width
                }
                "top" => height = half_height,
                "bottom" => {
                    y += half_height;
                    height -= half_height
                }
                "top_left" => {
                    width = half_width;
                    height = half_height
                }
                "top_right" => {
                    x += half_width;
                    width -= half_width;
                    height = half_height
                }
                "bottom_left" => {
                    y += half_height;
                    width = half_width;
                    height -= half_height
                }
                "bottom_right" => {
                    x += half_width;
                    y += half_height;
                    width -= half_width;
                    height -= half_height
                }
                "full" => {}
                _ => return Err("SNAP_PRESET_UNKNOWN".into()),
            }
        } else {
            let source = monitors
                .iter()
                .find(|m| Some(m.id.as_str()) == before["monitor"].as_str())
                .ok_or("MONITOR_NOT_FOUND")?
                .work;
            let bounds = &before["normal_screen"];
            width = number(bounds, "width")?.min(width);
            height = number(bounds, "height")?.min(height);
            x += ((number(bounds, "x")? as f64 - source.x) / source.width * area.width)
                .round_ties_even() as i64;
            y += ((number(bounds, "y")? as f64 - source.y) / source.height * area.height)
                .round_ties_even() as i64;
            x = x.clamp(area.x as i64, (area.x + area.width) as i64 - width);
            y = y.clamp(area.y as i64, (area.y + area.height) as i64 - height);
        }
        for (field, value) in [("x", x), ("y", y), ("width", width), ("height", height)] {
            target[field] = json!(value);
        }
    }
    geometry(&target)?;
    for field in ["x", "y", "width", "height"] {
        target[field] = json!(number(&target, field)?);
    }
    Ok(("move".into(), target, normal))
}
pub(crate) async fn prepare(
    core: &Arc<Core>,
    tool: &str,
    input: &Value,
) -> Result<Prepared, String> {
    if !is_change(tool) {
        return Err("WINDOW_ACTION".into());
    }
    let lease = Lease::new();
    lease.check(core)?;
    let monitors = core.monitors().await?;
    let targets = if tool == "window.layout_apply" {
        let (targets, stored) = window_layouts::apply_targets(core, input)?;
        window_layouts::monitors_match(&stored, &monitors)?;
        targets
    } else if tool == "window.set_layout" {
        input["body"].clone()
    } else {
        json!([input])
    };
    let targets = targets.as_array().ok_or("WINDOW_TARGETS")?;
    if targets.is_empty() || targets.len() > 32 {
        return Err("LAYOUT_COUNT".into());
    }
    let mut seen = std::collections::HashSet::new();
    for target in targets {
        if target["hwnd"].as_i64().unwrap_or(0) <= 0
            || !seen.insert(target["hwnd"].as_i64().unwrap_or(0))
        {
            return Err("LAYOUT_TARGET".into());
        }
        if ["window.set_layout", "window.layout_apply", "window.move"].contains(&tool) {
            geometry(target)?;
        }
        if ["window.set_layout", "window.layout_apply"].contains(&tool)
            && target["process_id"].as_u64().unwrap_or(0) == 0
        {
            return Err("WINDOW_TARGET_CHANGED".into());
        }
    }
    let before = capture(core, &json!(targets), false, &lease).await?;
    window_layouts::monitors_match(&monitors, &core.monitors().await?)?;
    let mut steps = Vec::new();
    for (target, before) in targets.iter().zip(before) {
        let (action, input, restore_to_normal) = planned(tool, target, &before, &monitors)?;
        steps.push(Step {
            action,
            input,
            before,
            after: None,
            state: "unstarted".into(),
            restore_to_normal,
        });
    }
    Ok(Prepared(Record {
        version: 2,
        tool: tool.into(),
        request: if tool == "window.layout_apply" {
            json!({"id":input["id"],"fingerprint":input["fingerprint"]})
        } else {
            let mut request = serde_json::Map::new();
            for field in [
                "hwnd",
                "process_id",
                "process_started",
                "window_identity",
                "x",
                "y",
                "width",
                "height",
                "state",
                "kind",
                "monitor",
            ] {
                if let Some(value) = input.get(field) {
                    request.insert(field.into(), value.clone());
                }
            }
            if tool == "window.set_layout" {
                request.insert(
                    "body".into(),
                    json!(steps.iter().map(|s| &s.input).collect::<Vec<_>>()),
                );
            }
            Value::Object(request)
        },
        monitors,
        steps,
        error: None,
    }))
}
fn unchanged_before_initialization(before: &Value, initialized: &Value) -> bool {
    let mut before = before.clone();
    if before["window_identity"].is_null() {
        before["window_identity"] = initialized["window_identity"].clone();
    }
    before == *initialized
}
fn hash(record: &Record) -> Result<String, String> {
    let immutable = json!({"version":record.version,"tool":record.tool,"request":record.request,"monitors":record.monitors,"steps":record.steps.iter().map(|s|json!({"action":s.action,"input":s.input,"before":s.before,"restore_to_normal":s.restore_to_normal})).collect::<Vec<_>>()});
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&immutable).map_err(|_| "WINDOW_JOURNAL_JSON")?)
    ))
}
fn save(core: &Core, id: &str, record: &Record, state: &str) -> Result<(), String> {
    let changed=core.store.execute("UPDATE undo_records SET metadata_json=?,state=?,updated_at=? WHERE id=? AND kind='window_change'",&[json!(serde_json::to_string(record).map_err(|_|"WINDOW_JOURNAL_JSON")?),json!(state),json!(storage::now()),json!(id)])?;
    if changed != 1 {
        return Err("WINDOW_JOURNAL_CHANGED".into());
    }
    Ok(())
}
fn load(core: &Core, id: &str) -> Result<(Record, String), String> {
    if uuid::Uuid::parse_str(id)
        .map_err(|_| "UNDO_ID")?
        .to_string()
        != id
    {
        return Err("UNDO_ID".into());
    }
    let rows = core.store.query(
        "SELECT * FROM undo_records WHERE id=? AND kind='window_change'",
        &[json!(id)],
    )?;
    let row = &rows[0];
    let record: Record = serde_json::from_str(
        row["metadata_json"]
            .as_str()
            .ok_or("WINDOW_JOURNAL_NOT_FOUND")?,
    )
    .map_err(|_| "WINDOW_JOURNAL_JSON")?;
    if ![1, 2].contains(&record.version)
        || !is_change(&record.tool)
        || record.steps.is_empty()
        || record.steps.len() > 32
        || hash(&record)? != row["expected_hash"]
    {
        return Err("WINDOW_JOURNAL_CHANGED".into());
    }
    let mut seen = std::collections::HashSet::new();
    for step in &record.steps {
        snapshot(&step.before, true)?;
        if record.version == 1
            && step.before["placement"]["show_cmd"] == 2
            && step.before["placement"].get("restore_maximized").is_none()
        {
            return Err("WINDOW_LEGACY_MINIMIZED_STATE_UNKNOWN: manual review required".into());
        }
        if !seen.insert(step.before["hwnd"].as_i64())
            || !["unstarted", "applying", "applied", "undoing", "undone"]
                .contains(&step.state.as_str())
            || !["move", "state"].contains(&step.action.as_str())
        {
            return Err("WINDOW_JOURNAL_CHANGED".into());
        }
        if let Some(after) = &step.after {
            snapshot(after, true)?;
            for field in ["hwnd", "process_id", "process_started", "window_identity"] {
                if after[field] != step.before[field] {
                    return Err("WINDOW_JOURNAL_IDENTITY".into());
                }
            }
        }
    }
    Ok((
        record,
        row["state"].as_str().ok_or("WINDOW_JOURNAL_STATE")?.into(),
    ))
}
pub(crate) fn history(core: &Core) -> Result<Value, String> {
    core.store.query("SELECT id,path AS description,state,created_at,updated_at,json_extract(metadata_json,'$.tool') AS tool,json_extract(metadata_json,'$.error') AS error FROM undo_records WHERE kind='window_change' ORDER BY created_at DESC LIMIT 100",&[])
}
pub(crate) fn recover(core: &Core) -> Result<(), String> {
    if core.store.query("SELECT COUNT(*) AS n FROM undo_records WHERE kind='window_change' AND state IN ('prepared','running')",&[])?[0]["n"] == 0 { return Ok(()); }
    let _profile = match profile_lock(core) {
        Ok(lock) => lock,
        Err(error) if error == "WINDOW_ACTION_BUSY" => return Ok(()),
        Err(error) => return Err(error),
    };
    core.store.execute("UPDATE undo_records SET state='blocked',updated_at=? WHERE kind='window_change' AND state IN ('prepared','running')",&[json!(storage::now())])?;
    Ok(())
}
pub(crate) fn review(core: &Core, input: &Value) -> Result<Value, String> {
    let (record, state) = load(core, input["id"].as_str().ok_or("UNDO_ID")?)?;
    Ok(
        json!({"id":input["id"],"state":state,"revalidate":input["revalidate"]==true,"monitors":record.monitors,"windows_in_undo_order":record.steps.iter().rev().map(|s|json!({"original":s.before,"observed":s.after,"phase":s.state})).collect::<Vec<_>>(),"on_conflict":"stop and preserve later changes"}),
    )
}
async fn guards(core: &Core, record: &Record, lease: &Lease) -> Result<(), String> {
    lease.check(core)?;
    window_layouts::monitors_match(&record.monitors, &core.monitors().await?)?;
    Ok(())
}
fn observation(outcome: &Value, expected: &Value) -> Result<(), String> {
    if outcome["before"] != *expected {
        return Err("WINDOW_NATIVE_RESULT_UNKNOWN".into());
    }
    snapshot(&outcome["after"], true)?;
    for field in ["hwnd", "process_id", "process_started", "window_identity"] {
        if outcome["after"][field] != expected[field] {
            return Err("WINDOW_NATIVE_RESULT_UNKNOWN".into());
        }
    }
    Ok(())
}
fn verify_result(step: &Step, after: &Value) -> Result<(), String> {
    if step.action == "state" {
        let show = match step.input["state"].as_str() {
            Some("minimize") => 2,
            Some("maximize") => 3,
            Some("restore")
                if step.before["placement"]["show_cmd"] == 2
                    && step.before["placement"]["restore_maximized"] == true =>
            {
                3
            }
            Some("restore") => 1,
            _ => return Err("WINDOW_STATE".into()),
        };
        if after["placement"]["show_cmd"] != show {
            return Err("WINDOW_PROVIDER_REFUSED_STATE".into());
        }
    } else {
        for field in ["x", "y", "width", "height"] {
            if after[field] != step.input[field] {
                return Err(format!(
                    "WINDOW_PROVIDER_CONSTRAINED_GEOMETRY: expected={}; observed={}",
                    json!([
                        step.input["x"],
                        step.input["y"],
                        step.input["width"],
                        step.input["height"]
                    ]),
                    json!([after["x"], after["y"], after["width"], after["height"]])
                ));
            }
        }
    }
    Ok(())
}
pub(crate) async fn execute(core: &Arc<Core>, prepared: Prepared) -> Result<Value, String> {
    let lease = Lease::new();
    let _serial = window_layouts::ACTIONS.lock().await;
    lease.check(core)?;
    let _profile = profile_lock(core)?;
    let mut record = prepared.0;
    guards(core, &record, &lease).await?;
    if record.tool == "window.layout_apply" {
        window_layouts::apply_targets(core, &record.request)?;
    }
    let targets = json!(record
        .steps
        .iter()
        .map(|s| s.before.clone())
        .collect::<Vec<_>>());
    let current = capture(core, &targets, true, &lease).await?;
    for (step, now) in record.steps.iter_mut().zip(current) {
        if !unchanged_before_initialization(&step.before, &now) {
            return Err("WINDOW_CHANGED_REVIEW_REQUIRED".into());
        }
        step.before = now;
    }
    let id = storage::id();
    core.store.execute("INSERT INTO undo_records(id,kind,path,expected_hash,state,created_at,updated_at,metadata_json) VALUES(?,'window_change',?,?,'prepared',?,?,?)",&[json!(id),json!(format!("{} · {} ventanas",record.tool,record.steps.len())),json!(hash(&record)?),json!(storage::now()),json!(storage::now()),json!(serde_json::to_string(&record).map_err(|_|"WINDOW_JOURNAL_JSON")?)])?;
    #[cfg(all(test, windows))]
    tests::recovery::crash::at(core, "prepared", 0, &id);
    let result=async {
        for index in 0..record.steps.len() {
            guards(core,&record,&lease).await?;
            if record.tool=="window.layout_apply" { window_layouts::apply_targets(core,&record.request)?; }
            let step=&record.steps[index];
            let now=capture(core,&json!([step.before]),false,&lease).await?;
            if now[0]!=step.before { return Err("WINDOW_CHANGED_REVIEW_REQUIRED".into()); }
            record.steps[index].state="applying".into();save(core,&id,&record,"prepared")?;
            #[cfg(all(test, windows))]
            tests::recovery::crash::at(core,"applying",index,&id);
            let step=&record.steps[index];
            let outcome=process::window_change(&json!({"expected":step.before,"action":step.action,"input":step.input,"restore_to_normal":step.restore_to_normal}),lease.cancel.clone()).await?;
            #[cfg(all(test, windows))]
            tests::recovery::crash::at(core,"native_result",index,&id);
            observation(&outcome,&step.before)?;
            record.steps[index].after=Some(outcome["after"].clone());record.steps[index].state="applied".into();
            save(core,&id,&record,"prepared")?;
            #[cfg(all(test, windows))]
            tests::recovery::crash::at(core,"applied",index,&id);
            guards(core,&record,&lease).await?;
            if let Some(error)=outcome["operation_error"].as_str() { return Err(format!("WINDOW_NATIVE_CHANGE: {error}")); }
            verify_result(&record.steps[index],&outcome["after"])?;
        }
        save(core,&id,&record,"available")?;
        Ok::<(),String>(())
    }.await;
    if let Err(error) = result {
        record.error = Some(error.clone());
        let durable = save(core, &id, &record, "blocked").is_ok();
        // Only observations saved before the failure can authorize rollback.
        // Cancellation/revocation, changed monitors or unknown native results stop it.
        if durable
            && record.steps.iter().all(|s| s.state != "applying")
            && guards(core, &record, &lease).await.is_ok()
        {
            if let Err(rollback_error) = Box::pin(restore(core, &id, &mut record, &lease)).await {
                record.error = Some(format!("{error}; rollback={rollback_error}"));
                let _ = save(core, &id, &record, "blocked");
            }
        }
        return Err(format!("{error}; window_recovery_id={id}"));
    }
    Ok(
        json!({"undo_token":id,"undo_available":true,"layout_id":record.request["id"],"result":{"windows":record.steps.iter().map(|s|json!({"before":s.before,"after":s.after})).collect::<Vec<_>>(),"coordinate_space":"physical"}}),
    )
}
async fn restore(
    core: &Arc<Core>,
    id: &str,
    record: &mut Record,
    lease: &Lease,
) -> Result<Value, String> {
    guards(core, record, lease).await?;
    // Preflight the complete set before touching any member.
    for step in &mut record.steps {
        if step.state == "undoing" {
            let now = capture(core, &json!([step.before]), false, lease).await?;
            if now[0] != step.before {
                return Err("WINDOW_NATIVE_RESULT_UNKNOWN".into());
            }
            step.state = "undone".into();
        }
    }
    let expected = record
        .steps
        .iter()
        .map(|s| match s.state.as_str() {
            "unstarted" | "undone" => Ok(s.before.clone()),
            "applied" => s.after.clone().ok_or("WINDOW_OBSERVATION_MISSING"),
            _ => Err("WINDOW_NATIVE_RESULT_UNKNOWN"),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let now = capture(core, &json!(expected), false, lease).await?;
    if now != expected {
        return Err("WINDOW_CHANGED_AFTER_ACTION: preserve later changes".into());
    }
    for index in (0..record.steps.len()).rev() {
        if record.steps[index].state != "applied" {
            continue;
        }
        guards(core, record, lease).await?;
        if record.steps[index].after.as_ref() == Some(&record.steps[index].before) {
            // A durable inverse observation already matches the before-image.
            // Reconcile without sending SetWindowPlacement a second time.
            record.steps[index].state = "undone".into();
            save(core, id, record, "running")?;
            continue;
        }
        record.steps[index].state = "undoing".into();
        save(core, id, record, "running")?;
        #[cfg(all(test, windows))]
        tests::recovery::crash::at(core, "undoing", index, id);
        let step = &record.steps[index];
        let outcome = process::window_change(
            &json!({"action":"restore_snapshot","expected":step.after,"restore":step.before}),
            lease.cancel.clone(),
        )
        .await?;
        #[cfg(all(test, windows))]
        tests::recovery::crash::at(core, "inverse_result", index, id);
        observation(
            &outcome,
            step.after.as_ref().ok_or("WINDOW_OBSERVATION_MISSING")?,
        )?;
        // Record any constrained restoration; another attempt must match it.
        record.steps[index].after = Some(outcome["after"].clone());
        record.steps[index].state = "applied".into();
        save(core, id, record, "running")?;
        #[cfg(all(test, windows))]
        tests::recovery::crash::at(core, "inverse_observed", index, id);
        if outcome["after"] != record.steps[index].before {
            return Err("WINDOW_RESTORE_CONSTRAINED: observed state retained".into());
        }
        guards(core, record, lease).await?;
        record.steps[index].state = "undone".into();
        save(core, id, record, "running")?;
    }
    guards(core, record, lease).await?;
    save(core, id, record, "undone")?;
    #[cfg(all(test, windows))]
    tests::recovery::crash::at(core, "undone", 0, id);
    Ok(
        json!({"undo_id":id,"state":"undone","reconciled":expected.iter().zip(&record.steps).all(|(v,s)|*v==s.before)}),
    )
}
pub(crate) async fn execute_undo(core: &Arc<Core>, input: &Value) -> Result<Value, String> {
    let lease = Lease::new();
    let _serial = window_layouts::ACTIONS.lock().await;
    lease.check(core)?;
    let _profile = profile_lock(core)?;
    let id = input["id"].as_str().ok_or("UNDO_ID")?;
    let (mut record, state) = load(core, id)?;
    if state != "available" && !(state == "blocked" && input["revalidate"] == true) {
        return Err("UNDO_NOT_AVAILABLE".into());
    }
    let claimed=core.store.execute("UPDATE undo_records SET state='running',updated_at=? WHERE id=? AND state=? AND metadata_json=?",&[json!(storage::now()),json!(id),json!(state),json!(serde_json::to_string(&record).map_err(|_|"WINDOW_JOURNAL_JSON")?)])?;
    if claimed != 1 {
        return Err("WINDOW_JOURNAL_ALREADY_CLAIMED".into());
    }
    let result = Box::pin(restore(core, id, &mut record, &lease)).await;
    if let Err(error) = &result {
        record.error = Some(error.clone());
        let _ = save(core, id, &record, "blocked");
    }
    result
}

#[cfg(all(test, windows))]
#[path = "window_undo_tests.rs"]
mod tests;

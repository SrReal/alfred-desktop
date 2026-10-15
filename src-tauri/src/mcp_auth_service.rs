//! Product lifecycle around the private actor. Jobs retain no Core/Actor in
//! their registry, and browser URLs never become tool/UI/event payloads.
use super::*;
use std::sync::Weak;
use tokio_util::sync::CancellationToken;

pub(crate) struct Job {
    ticket: String,
    session: Option<String>,
    cancel: CancellationToken,
}
struct Lease {
    core: Weak<Core>,
    connection: String,
    ticket: String,
    cancel: CancellationToken,
}
impl Lease {
    fn claim(core: &Arc<Core>, connection: &str) -> Result<Self> {
        let mut jobs = core
            .mcp_oauth_jobs
            .lock()
            .map_err(|_| "MCP_OAUTH_JOB_LOCK")?;
        if jobs.contains_key(connection) {
            return Err("MCP_OAUTH_ALREADY_RUNNING".into());
        }
        if jobs.len() >= 32 {
            return Err("MCP_OAUTH_JOB_LIMIT".into());
        }
        let ticket = storage::id();
        let cancel = CancellationToken::new();
        jobs.insert(
            connection.into(),
            Job {
                ticket: ticket.clone(),
                session: None,
                cancel: cancel.clone(),
            },
        );
        Ok(Self {
            core: Arc::downgrade(core),
            connection: connection.into(),
            ticket,
            cancel,
        })
    }
    fn session(&self, session: &str) -> Result<()> {
        let core = self.core.upgrade().ok_or("MCP_OAUTH_JOB_CANCELLED")?;
        let mut jobs = core
            .mcp_oauth_jobs
            .lock()
            .map_err(|_| "MCP_OAUTH_JOB_LOCK")?;
        let job = jobs
            .get_mut(&self.connection)
            .filter(|j| j.ticket == self.ticket)
            .ok_or("MCP_OAUTH_JOB_CANCELLED")?;
        if job.cancel.is_cancelled() {
            return Err("MCP_OAUTH_JOB_CANCELLED".into());
        }
        job.session = Some(session.into());
        Ok(())
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(core) = self.core.upgrade() {
            let session = core.mcp_oauth_jobs.lock().ok().and_then(|jobs| {
                jobs.get(&self.connection)
                    .filter(|job| job.ticket == self.ticket)
                    .and_then(|job| job.session.clone())
            });
            if let Some(session) = session {
                if let Ok(actor) = Actor::load(&core, &self.connection) {
                    let _ = actor.abandon(&session);
                }
            }
            // Empty registry means local cleanup has finished, including its
            // exclusive profile lock. A second consent must not observe an
            // empty registry while this lease is still abandoning the old one.
            if let Ok(mut jobs) = core.mcp_oauth_jobs.lock() {
                if jobs
                    .get(&self.connection)
                    .is_some_and(|job| job.ticket == self.ticket)
                {
                    jobs.remove(&self.connection);
                }
            }
        }
    }
}
pub(crate) fn public(state: &State) -> Value {
    json!({"connection":state.connection,"session_id":state.session_id,"phase":state.phase,
        "issuer":state.authorization.issuer,"resource":state.authorization.resource,
        "client_id":state.authorization.client_id,"requested_scopes":state.authorization.requested_scopes,
        "cleanup_pending":state.cleanup_pending,"automatic_replay":false})
}
pub(crate) fn snapshot(core: &Arc<Core>, id: &str) -> Result<State> {
    Actor::load(core, id)?.state()
}
pub(crate) fn optional_snapshot(core: &Arc<Core>, id: &str) -> Result<Option<State>> {
    let c = mcp_profile::config(&record(core, id)?)?;
    if !matches!(c.auth, mcp_profile::Auth::OAuth2 { .. }) {
        return Err("MCP_OAUTH_PROFILE_REQUIRED".into());
    }
    if core.store.setting(&key(id)).is_null() {
        Ok(None)
    } else {
        snapshot(core, id).map(Some)
    }
}
pub(crate) fn status(core: &Arc<Core>, input: &Value) -> Result<Value> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Input {
        connection: String,
    }
    let input: Input =
        serde_json::from_value(input.clone()).map_err(|_| "MCP_OAUTH_STATUS_INPUT")?;
    let c = mcp_profile::config(&record(core, &input.connection)?)?;
    if !matches!(c.auth, mcp_profile::Auth::OAuth2 { .. }) {
        return Err("MCP_OAUTH_PROFILE_REQUIRED".into());
    }
    if core.store.setting(&key(&input.connection)).is_null() {
        if core.store.setting("privacy") == true || !core.enabled("mcp") || !c.enabled {
            return Err("MCP_OAUTH_AUTHORITY_CHANGED".into());
        }
        return Ok(
            json!({"connection":input.connection,"phase":"not_started","automatic_replay":false}),
        );
    }
    let actor = Actor::load(core, &input.connection)?;
    let state = actor.state()?;
    let mut result = public(&state);
    if state.phase == Phase::Ready {
        // Read and drop a private native grant. The UI receives only usability.
        result["credential_status"] = json!(match actor.grant() {
            Ok(_) => "usable",
            Err(e) if e == "MCP_OAUTH_REFRESH_REQUIRED" => "expired",
            Err(_) => "unavailable",
        });
    }
    Ok(result)
}
pub(crate) async fn start(
    core: &Arc<Core>,
    id: &str,
    reviewed: enrollment::Reviewed,
    secret: Option<Text>,
) -> Result<Value> {
    reviewed.validate_start(secret.is_some())?;
    let lease = Lease::claim(core, id)?;
    let consent = tokio::select! {biased;
        _=lease.cancel.cancelled()=>return Err("MCP_OAUTH_JOB_CANCELLED".into()),
        result=reviewed.consent(core,id,secret)=>result?,
    };
    let state = consent.public_state().clone();
    lease.session(&state.session_id)?;
    if let Err(error) = open_browser(core, consent.browser_url()) {
        drop(consent);
        drop(lease);
        return Err(error);
    }
    let result = public(&state);
    let core = core.clone();
    let connection = id.to_owned();
    tokio::spawn(async move {
        let outcome = tokio::select! {biased;
            _=lease.cancel.cancelled()=>"cancelled",
            result=consent.complete()=>if result.is_ok(){"succeeded"}else{"failed"},
        };
        drop(lease);
        core.emit(
            "alfredpet:mcp_oauth",
            &json!({"connection":connection,"session_id":state.session_id,"outcome":outcome}),
        );
    });
    Ok(result)
}
pub(crate) async fn control(
    core: &Arc<Core>,
    id: &str,
    expected: &State,
    action: &str,
) -> Result<Value> {
    let actor = Actor::load(core, id)?;
    if actor.state()? != *expected {
        return Err("MCP_OAUTH_STATE_CHANGED_REVIEW_REQUIRED".into());
    }
    let state = match action {
        "oauth_status" => return status(core, &json!({"connection":id})),
        "oauth_cancel" => {
            let state = actor.abandon(&expected.session_id)?;
            if let Some(job) = core
                .mcp_oauth_jobs
                .lock()
                .map_err(|_| "MCP_OAUTH_JOB_LOCK")?
                .get(id)
            {
                if job
                    .session
                    .as_deref()
                    .is_none_or(|s| s == expected.session_id)
                {
                    job.cancel.cancel();
                }
            }
            state
        }
        "oauth_refresh" => {
            let lease = Lease::claim(core, id)?;
            let state = tokio::select! {biased;
                _=lease.cancel.cancelled()=>return Err("MCP_OAUTH_JOB_CANCELLED".into()),
                result=flow::refresh_checked(actor,Some(expected))=>result?,
            };
            drop(lease);
            state
        }
        "oauth_recover" => actor.recover_local(expected)?,
        "oauth_cleanup" => actor.cleanup_receipts(expected)?,
        "oauth_reset" => actor.reset_unknown(expected)?,
        _ => return Err("MCP_OAUTH_CONTROL_ACTION".into()),
    };
    Ok(public(&state))
}
pub(crate) fn startup(core: &Arc<Core>) -> Result<Value> {
    let records = core
        .store
        .query("SELECT id FROM connections WHERE module_id='mcp'", &[])?;
    let records = records.as_array().ok_or("MCP_OAUTH_STARTUP_RECORDS")?;
    if records.len() > 4096 {
        return Err("MCP_OAUTH_STARTUP_LIMIT".into());
    }
    let (mut reconciled, mut busy, mut skipped) = (0, 0, 0);
    for record in records {
        let Some(id) = record["id"].as_str() else {
            skipped += 1;
            continue;
        };
        if core.store.setting(&key(id)).is_null() {
            continue;
        }
        match Actor::load(core, id).and_then(|actor| actor.reconcile_startup()) {
            Ok(Some(_)) => reconciled += 1,
            Ok(None) => (),
            Err(e) if e == "MCP_OAUTH_BUSY" => busy += 1,
            Err(_) => skipped += 1,
        }
    }
    Ok(
        json!({"reconciled":reconciled,"busy_preserved":busy,"changed_or_disabled_preserved":skipped,"network_requests":0,"credentials_adopted":0}),
    )
}
pub(crate) fn stop_all(core: &Arc<Core>) {
    let jobs = core
        .mcp_oauth_jobs
        .lock()
        .map(|mut jobs| std::mem::take(&mut *jobs))
        .unwrap_or_default();
    for (id, job) in jobs {
        job.cancel.cancel();
        if let Some(session) = job.session {
            if let Ok(actor) = Actor::load(core, &id) {
                let _ = actor.abandon(&session);
            }
        }
    }
}
fn open_browser(core: &Core, url: &str) -> Result<()> {
    #[cfg(test)]
    if let Some(urls) = core
        .mcp_oauth_browser
        .lock()
        .map_err(|_| "MCP_OAUTH_BROWSER_LOCK")?
        .as_mut()
    {
        urls.push(url.to_owned());
        return Ok(());
    }
    let _ = core;
    system_browser(url)
}
#[cfg(windows)]
fn system_browser(url: &str) -> Result<()> {
    use windows::{
        core::{w, PCWSTR},
        Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL},
    };
    let wide: Vec<u16> = url.encode_utf16().chain(Some(0)).collect();
    let result = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(wide.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if result.0 as usize <= 32 {
        return Err("MCP_OAUTH_SYSTEM_BROWSER_OPEN".into());
    }
    Ok(())
}
#[cfg(not(windows))]
fn system_browser(_: &str) -> Result<()> {
    Err("WINDOWS_11_REQUIRED".into())
}

#[cfg(test)]
#[path = "mcp_auth_service_tests.rs"]
mod tests;

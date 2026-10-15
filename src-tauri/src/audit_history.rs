//! Bounded local snapshots and deterministic comparisons. No system commands here.
use crate::storage::{self, Store};
use chrono::DateTime;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

const KEY: &str = "system_audit_history";
const ORIGIN: &str = "system_audit_origin";
const LIMIT: usize = 20;
const MAX_REPORT: usize = 512 * 1024;
type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    checked_at: String,
    scope: String,
    sections: Vec<Section>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Section {
    name: String,
    status: String,
    #[serde(default)]
    data: Value,
    #[serde(default)]
    error: Option<String>,
}
#[derive(Clone, PartialEq, Serialize, Deserialize)]
struct Origin {
    id: String,
    computer: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Snapshot {
    id: String,
    saved_at: String,
    format_version: u32,
    origin: Origin,
    elevated: bool,
    untrusted: bool,
    #[serde(flatten)]
    report: Report,
}
fn validate(report: &Report) -> Result<()> {
    DateTime::parse_from_rfc3339(&report.checked_at).map_err(|_| "AUDIT_DATE_INVALID")?;
    let mut names = BTreeSet::new();
    if report.sections.len() > 16
        || serde_json::to_vec(report).map_err(|_| "AUDIT_JSON")?.len() > MAX_REPORT
    {
        return Err("AUDIT_REPORT_LIMIT".into());
    }
    for s in &report.sections {
        if s.name.is_empty()
            || s.name.len() > 256
            || !names.insert(&s.name)
            || !["observed", "unavailable"].contains(&s.status.as_str())
        {
            return Err("AUDIT_SECTION_INVALID".into());
        }
    }
    Ok(())
}
fn read(db: &Connection) -> Result<Vec<Snapshot>> {
    let raw: Option<String> = db
        .query_row("SELECT json_value FROM settings WHERE key=?", [KEY], |r| {
            r.get(0)
        })
        .optional()
        .map_err(|_| "AUDIT_HISTORY_READ")?;
    let Some(raw) = raw else { return Ok(vec![]) };
    if raw.len() > LIMIT * (MAX_REPORT + 4096) {
        return Err("AUDIT_HISTORY_LIMIT".into());
    }
    let rows: Vec<Snapshot> = serde_json::from_str(&raw).map_err(|_| "AUDIT_HISTORY_INVALID")?;
    if rows.len() > LIMIT {
        return Err("AUDIT_HISTORY_LIMIT".into());
    }
    let mut ids = BTreeSet::new();
    for row in &rows {
        validate(&row.report)?;
        if !ids.insert(&row.id) {
            return Err("AUDIT_HISTORY_INVALID".into());
        }
    }
    Ok(rows)
}
fn write(db: &Connection, rows: &[Snapshot]) -> Result<()> {
    db.execute(
        "INSERT OR REPLACE INTO settings VALUES(?,?)",
        params![KEY, serde_json::to_string(rows).map_err(|_| "AUDIT_JSON")?],
    )
    .map_err(|_| "AUDIT_HISTORY_WRITE")?;
    Ok(())
}
pub fn save(store: &Store, report: Value, elevated: bool) -> Result<Value> {
    let report: Report = serde_json::from_value(report).map_err(|_| "AUDIT_REPORT_INVALID")?;
    validate(&report)?;
    let mut db = store.0.lock().map_err(|_| "DB_LOCK")?;
    let tx = db.transaction().map_err(|_| "AUDIT_HISTORY_WRITE")?;
    let mut rows = read(&tx)?;
    let computer: String = std::env::var("COMPUTERNAME")
        .unwrap_or_else(|_| "Equipo local".into())
        .chars()
        .take(128)
        .collect();
    let prior: Option<String> = tx
        .query_row(
            "SELECT json_value FROM settings WHERE key=?",
            [ORIGIN],
            |r| r.get(0),
        )
        .optional()
        .map_err(|_| "AUDIT_HISTORY_READ")?;
    let origin = prior
        .and_then(|raw| serde_json::from_str::<Origin>(&raw).ok())
        .filter(|o| o.computer == computer)
        .unwrap_or_else(|| Origin {
            id: storage::id(),
            computer,
        });
    let snapshot = Snapshot {
        id: storage::id(),
        saved_at: storage::now(),
        format_version: 1,
        origin,
        elevated,
        untrusted: true,
        report,
    };
    tx.execute(
        "INSERT OR REPLACE INTO settings VALUES(?,?)",
        params![
            ORIGIN,
            serde_json::to_string(&snapshot.origin).map_err(|_| "AUDIT_JSON")?
        ],
    )
    .map_err(|_| "AUDIT_HISTORY_WRITE")?;
    rows.insert(0, snapshot.clone());
    rows.truncate(LIMIT);
    write(&tx, &rows)?;
    tx.commit().map_err(|_| "AUDIT_HISTORY_WRITE")?;
    serde_json::to_value(snapshot).map_err(|_| "AUDIT_JSON".into())
}
fn summary(s: &Snapshot) -> Value {
    json!({"id":s.id,"checkedAt":s.report.checked_at,"savedAt":s.saved_at,"origin":s.origin,"elevated":s.elevated,"unavailable":s.report.sections.iter().filter(|s| s.status!="observed").count()})
}
pub fn dispatch(store: &Store, operation: &str, input: &Value) -> Result<Value> {
    let db = store.0.lock().map_err(|_| "DB_LOCK")?;
    let mut rows = read(&db)?;
    let find = |id: &Value| {
        rows.iter()
            .find(|r| id.as_str() == Some(&r.id))
            .ok_or_else(|| {
                "La auditoría ya no está en el historial. Actualiza la lista.".to_string()
            })
    };
    match operation {
        "system_audit.list" => Ok(Value::Array(rows.iter().map(summary).collect())),
        "system_audit.get" => {
            serde_json::to_value(find(&input["id"])?).map_err(|_| "AUDIT_JSON".into())
        }
        "system_audit.compare" => compare(find(&input["beforeId"])?, find(&input["afterId"])?),
        "system_audit.delete" => {
            if input["confirmed"] != true {
                return Err("AUDIT_DELETE_CONFIRMATION".into());
            }
            let id = find(&input["id"])?.id.clone();
            rows.retain(|r| r.id != id);
            write(&db, &rows)?;
            Ok(json!({"deleted":true}))
        }
        _ => Err("AUDIT_OPERATION".into()),
    }
}

struct Spec {
    name: &'static str,
    source: &'static str,
    keys: &'static [&'static str],
    note: &'static str,
}
const SPECS: &[Spec] = &[
    Spec { name:"Protección antivirus registrada", source:"Windows SecurityCenter2 / AntiVirusProduct", keys:&["displayName"], note:"Inventario registrado por Windows. productState es el valor declarado por el proveedor; no demuestra que una protección funcione." },
    Spec { name:"Microsoft Defender", source:"Get-MpComputerStatus", keys:&[], note:"Estado comunicado por Defender. Los cambios de fechas o antigüedad de firmas no son por sí solos un diagnóstico de seguridad." },
    Spec { name:"Amenazas detectadas por Defender", source:"Get-MpThreat", keys:&["ThreatID"], note:"Hasta 50 registros. Que una detección deje de aparecer no acredita eliminación ni ausencia de malware." },
    Spec { name:"Errores de Windows en las últimas 24 horas", source:"Get-WinEvent / System y Application", keys:&["log","provider","id","level"], note:"Se comparan cantidades por tipo dentro de ventanas móviles de 24 horas, con un máximo de 30 eventos por registro. Un error puede salir de la ventana o de la muestra sin haberse corregido." },
    Spec { name:"Servicios automáticos detenidos", source:"Win32_Service", keys:&["Name"], note:"Hasta 60 servicios automáticos detenidos. Dejar de aparecer puede indicar que arrancó, cambió su configuración o quedó fuera de la muestra; no prueba una reparación." },
    Spec { name:"Programas de inicio (inventario)", source:"Win32_StartupCommand", keys:&["Name","Location"], note:"Hasta 100 entradas. La comparación usa nombre y ubicación, no inspecciona binarios ni demuestra que un programa sea malicioso." },
    Spec { name:"Espacio en discos locales", source:"Win32_LogicalDisk / DriveType=3", keys:&["drive"], note:"Espacio observado en cada consulta. Los valores de tamaño y espacio libre se expresan en bytes y porcentaje." },
    Spec { name:"Reinicio pendiente de Windows", source:"Registro de Windows / Component Based Servicing y WindowsUpdate", keys:&[], note:"Estas señales indican un reinicio pendiente comunicado por Windows; no comprueban todos sus posibles motivos." },
];
fn items(value: &Value) -> Option<Vec<&Value>> {
    match value {
        Value::Null => Some(vec![]),
        Value::Array(a) => Some(a.iter().collect()),
        Value::Object(_) => Some(vec![value]),
        _ => None,
    }
}
fn inventory(value: &Value, spec: &Spec) -> Option<BTreeMap<String, Value>> {
    let mut map = BTreeMap::new();
    for row in items(value)? {
        let mut key = vec![];
        for field in spec.keys {
            let v = row.get(*field)?;
            if !v.is_string() && !v.is_number() {
                return None;
            }
            key.push(v.clone());
        }
        let key = serde_json::to_string(&key).ok()?;
        if spec.source.starts_with("Get-WinEvent") {
            let count = map.entry(key).or_insert(json!(0));
            *count = json!(count.as_u64()? + 1);
        } else if map.insert(key, row.clone()).is_some() {
            return None; // Ambiguous identities must not silently lose rows.
        }
    }
    Some(map)
}
fn field_label(key: &str) -> &str {
    match key {
        "RealTimeProtectionEnabled" => "Protección en tiempo real",
        "AntivirusEnabled" => "Antivirus activado",
        "AntispywareEnabled" => "Protección antispyware",
        "AMServiceEnabled" => "Servicio antimalware activado",
        "BehaviorMonitorEnabled" => "Supervisión de comportamiento",
        "IsTamperProtected" => "Protección contra alteraciones",
        "AMRunningMode" => "Modo de Defender",
        "AntivirusSignatureLastUpdated" => "Última actualización de firmas",
        "AntivirusSignatureAge" => "Antigüedad de firmas (días)",
        "QuickScanStartTime" => "Inicio del análisis rápido",
        "QuickScanEndTime" => "Fin del análisis rápido",
        "FullScanStartTime" => "Inicio del análisis completo",
        "FullScanEndTime" => "Fin del análisis completo",
        "pending" => "Señal de reinicio pendiente",
        "freeBytes" => "espacio libre (bytes)",
        "sizeBytes" => "capacidad (bytes)",
        "freePercent" => "espacio libre (%)",
        "State" => "estado del servicio",
        "StartMode" => "modo de inicio",
        "productState" => "estado declarado por el antivirus",
        "IsActive" => "detección activa",
        "DidThreatExecute" => "ejecución de la amenaza comunicada",
        "SeverityID" => "nivel comunicado por Defender",
        _ => key,
    }
}
fn brief(value: &Value) -> String {
    match value {
        Value::Bool(v) => if *v { "Sí" } else { "No" }.into(),
        Value::Null => "Sin dato".into(),
        Value::String(s) => s.chars().take(120).collect(),
        _ => value.to_string(),
    }
}
fn explanation(old: Option<&Value>, new: Option<&Value>) -> String {
    match (old, new) {
        (None, _) => "Se observa ahora".into(),
        (_, None) => "No aparece en la muestra actual".into(),
        (Some(a), Some(b)) if a.is_object() && b.is_object() => {
            let a = a.as_object().unwrap();
            let b = b.as_object().unwrap();
            let fields = a
                .keys()
                .chain(b.keys())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .filter(|k| a.get(*k) != b.get(*k))
                .map(|k| field_label(k))
                .collect::<Vec<_>>();
            format!("Han cambiado: {}", fields.join(", "))
        }
        (Some(a), Some(b)) => format!("{} → {}", brief(a), brief(b)),
    }
}
fn changes(before: &Value, after: &Value, spec: &Spec) -> Option<Vec<Value>> {
    let (left, right) = if spec.keys.is_empty() {
        (
            before
                .as_object()?
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect::<BTreeMap<_, _>>(),
            after
                .as_object()?
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect::<BTreeMap<_, _>>(),
        )
    } else {
        (inventory(before, spec)?, inventory(after, spec)?)
    };
    let mut output = vec![];
    for key in left.keys().chain(right.keys()).collect::<BTreeSet<_>>() {
        let old = left.get(key);
        let new = right.get(key);
        if old != new {
            let label = serde_json::from_str::<Vec<Value>>(key)
                .map(|v| {
                    v.iter()
                        .map(|v| {
                            v.as_str()
                                .map(str::to_owned)
                                .unwrap_or_else(|| v.to_string())
                        })
                        .collect::<Vec<_>>()
                        .join(" · ")
                })
                .unwrap_or_else(|_| field_label(key).to_owned());
            output.push(json!({"label":label,"explanation":explanation(old,new),"kind":if old.is_none(){"added"}else if new.is_none(){"removed"}else{"changed"},"before":old,"after":new}));
        }
    }
    Some(output)
}
fn compare(before: &Snapshot, after: &Snapshot) -> Result<Value> {
    if before.id == after.id {
        return Err("Selecciona dos auditorías distintas.".into());
    }
    if before.origin != after.origin {
        return Err(
            "Selecciona auditorías del mismo origen. Un perfil restaurado inicia un origen nuevo."
                .into(),
        );
    }
    if before.format_version != 1 || after.format_version != 1 {
        return Err("Formato de auditoría no compatible.".into());
    }
    if DateTime::parse_from_rfc3339(&before.report.checked_at).map_err(|_| "AUDIT_DATE_INVALID")?
        >= DateTime::parse_from_rfc3339(&after.report.checked_at)
            .map_err(|_| "AUDIT_DATE_INVALID")?
    {
        return Err("La auditoría anterior debe tener una fecha previa a la reciente.".into());
    }
    let mut output = vec![];
    let names: BTreeSet<_> = before
        .report
        .sections
        .iter()
        .chain(&after.report.sections)
        .map(|s| s.name.as_str())
        .chain(SPECS.iter().map(|s| s.name))
        .collect();
    for name in names {
        let left = before.report.sections.iter().find(|s| s.name == name);
        let right = after.report.sections.iter().find(|s| s.name == name);
        let spec = SPECS.iter().find(|s| s.name == name);
        let comparable = left
            .zip(right)
            .filter(|(a, b)| a.status == "observed" && b.status == "observed");
        let delta = comparable.and_then(|(a, b)| spec.and_then(|s| changes(&a.data, &b.data, s)));
        let status = if comparable.is_none() {
            "unavailable"
        } else if let Some(d) = &delta {
            if d.is_empty() {
                "unchanged"
            } else {
                "changed"
            }
        } else {
            "unsupported"
        };
        output.push(json!({"name":name,"source":spec.map(|s|s.source).unwrap_or("Fuente no reconocida"),"status":status,"beforeStatus":left.map(|s|s.status.as_str()).unwrap_or("missing"),"afterStatus":right.map(|s|s.status.as_str()).unwrap_or("missing"),"note":spec.map(|s|s.note).unwrap_or("Consulta los informes originales. No se interpreta una estructura desconocida."),"changes":delta.unwrap_or_default()}));
    }
    Ok(
        json!({"before":summary(before),"after":summary(after),"sections":output,"changedSections":output.iter().filter(|s|s["status"]=="changed").count(),"incompleteSections":output.iter().filter(|s|s["status"]=="unavailable" || s["status"]=="unsupported").count(),"differentElevation":before.elevated!=after.elevated,"untrusted":true}),
    )
}

#[cfg(test)]
mod tests;

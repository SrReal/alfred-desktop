use crate::{policy::Risk, runtime::Core, terminal};
use serde_json::{json, Value};
use std::sync::Arc;
pub const TOOLS: &[&str] = &["windows.audit", "windows.admin_action"];
pub const ACTIONS: &[&str] = &[
    "defender_update",
    "defender_quick_scan",
    "defender_full_scan",
    "sfc_verify",
    "sfc_repair",
    "dism_check",
    "dism_repair",
];
pub fn schema(tool: &str) -> Value {
    if tool == "windows.audit" {
        json!({"type":"object","properties":{"elevated":{"type":"boolean"}},"additionalProperties":false})
    } else {
        json!({"type":"object","properties":{"action":{"enum":ACTIONS}},"required":["action"],"additionalProperties":false})
    }
}
pub fn script(action: &str) -> Result<(&'static str, &'static str, u64), String> {
    Ok(match action {
        "defender_update"=>("Actualizar firmas de Microsoft Defender","Update-MpSignature -ErrorAction Stop; Write-Output 'Firmas actualizadas.'",600),
        "defender_quick_scan"=>("Análisis rápido de Microsoft Defender","Start-MpScan -ScanType QuickScan -ErrorAction Stop; Write-Output 'Solicitud de análisis procesada. Consulta el estado y las detecciones de Defender.'",1800),
        "defender_full_scan"=>("Análisis completo de Microsoft Defender","Start-MpScan -ScanType FullScan -ErrorAction Stop; Write-Output 'Solicitud de análisis procesada. Consulta el estado y las detecciones de Defender.'",3600),
        "sfc_verify"=>("Comprobar archivos de Windows (sin reparar)","& \"$env:SystemRoot/System32/sfc.exe\" /verifyonly",1800),
        "sfc_repair"=>("Comprobar y reparar archivos de Windows","& \"$env:SystemRoot/System32/sfc.exe\" /scannow",3600),
        "dism_check"=>("Consultar integridad de la imagen de Windows","& \"$env:SystemRoot/System32/Dism.exe\" /Online /Cleanup-Image /CheckHealth /NoRestart",600),
        "dism_repair"=>("Reparar imagen de Windows (puede descargar componentes)","& \"$env:SystemRoot/System32/Dism.exe\" /Online /Cleanup-Image /RestoreHealth /NoRestart",3600),
        _=>return Err("ADMIN_ACTION_UNKNOWN".into())
    })
}
pub struct Prepared {
    terminal: terminal::Prepared,
    action: String,
    title: String,
    audit: bool,
}
impl Prepared {
    pub fn prepare(tool: &str, input: &Value) -> Result<Self, String> {
        let audit = tool == "windows.audit";
        let (action, title, command, timeout, elevated) = if audit {
            (
                "audit",
                "Auditar estado del equipo",
                include_str!("../../native/system-audit.ps1"),
                90,
                input["elevated"] == true,
            )
        } else {
            let action = input["action"].as_str().ok_or("ADMIN_ACTION_REQUIRED")?;
            let (title, command, timeout) = script(action)?;
            (action, title, command, timeout, true)
        };
        Ok(Self {
            terminal: terminal::system_script(command, timeout, elevated)?,
            action: action.into(),
            title: title.into(),
            audit,
        })
    }
    pub fn review(&self) -> Value {
        json!({"action":self.action,"title":self.title,"execution":self.terminal.review(),"effects":if self.audit {"Sólo lectura. Se guardan localmente las últimas 20 auditorías para compararlas. Secciones incompletas se muestran como no disponibles."} else {"Operación administrativa autorizada. Defender aplica su política habitual de detección y respuesta. Las reparaciones pueden modificar archivos del sistema."}})
    }
    pub fn risk(&self) -> Risk {
        if self.audit && !self.terminal.is_elevated() {
            Risk::Contextual
        } else {
            Risk::Mandatory
        }
    }
    pub fn timeout(&self) -> u64 {
        self.terminal.timeout_seconds() + 15
    }
    pub async fn execute(self, c: &Arc<Core>) -> Result<Value, String> {
        if c.store.setting("privacy") == true || !c.enabled("windows") {
            return Err("ADMIN_SCOPE_REVOKED".into());
        }
        let elevated = self.terminal.is_elevated();
        let result = terminal::execute(c, self.terminal).await?;
        if self.audit {
            if c.store.setting("privacy") == true || !c.enabled("windows") {
                return Err("ADMIN_SCOPE_REVOKED".into());
            }
            let text = result["stdout"].as_str().ok_or("AUDIT_OUTPUT")?;
            let mut report: Value =
                serde_json::from_str(text.trim()).map_err(|_| "AUDIT_OUTPUT_JSON")?;
            report["untrusted"] = json!(true);
            crate::audit_history::save(&c.store, report, elevated)
        } else {
            Ok(
                json!({"action":self.action,"title":self.title,"result":result,"note":"El código de salida y la salida de la herramienta no garantizan un equipo libre de malware. Actualiza la auditoría y revisa los resultados de Windows."}),
            )
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn closed_admin_catalog_and_risk() {
        assert!(script("disable_defender").is_err());
        assert!(script("reboot").is_err());
        for action in ACTIONS {
            let (_, code, seconds) = script(action).unwrap();
            assert!(!code.contains("Set-MpPreference"));
            assert!(seconds <= 3600);
        }
        for tool in TOOLS {
            jsonschema::validator_for(&schema(tool)).unwrap();
        }
    }
}

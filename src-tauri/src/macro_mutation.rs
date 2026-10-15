//! Reviewed configuration. The blocking native worker owns serialization even
//! after cancellation; later operations cannot pass an unfinished worker.
use super::*;
use tokio_util::sync::CancellationToken;

pub(crate) struct Mutation {
    action: String,
    input: Value,
    authority: String,
    row: Option<Value>,
    review: Value,
}
impl Mutation {
    pub(crate) fn prepare(c: &Core, tool: &str, input: &Value) -> Result<Self, String> {
        let action = tool
            .strip_prefix("macros.")
            .filter(|a| ["save", "delete", "set_enabled"].contains(a))
            .ok_or("MACRO_ACTION")?;
        if !jsonschema::validator_for(&schema(tool))
            .map_err(|_| "MACRO_SCHEMA")?
            .is_valid(input)
        {
            return Err("INVALID_TOOL_INPUT".into());
        }
        let authority = initial_authority(c)?;
        let row = if action == "save" {
            None
        } else {
            let row = stored(c, input["id"].as_str().ok_or("MACRO_ID")?)?;
            if input["expected"]["fingerprint"].as_str() != Some(fingerprint(&row).as_str()) {
                return Err("MACRO_CHANGED_REVIEW_REQUIRED".into());
            }
            Some(row)
        };
        let review = if action == "save" {
            validate_save(c, input)?
        } else {
            json!({"operation":action,"id":input["id"],"name":row.as_ref().unwrap()["name"],"enabled":row.as_ref().unwrap()["enabled"],"expected":input["expected"],"proposed_enabled":input.get("enabled"),"child_execution":false})
        };
        let p = Self {
            action: action.into(),
            input: input.clone(),
            authority,
            row,
            review,
        };
        p.check(c)?;
        Ok(p)
    }
    pub(crate) fn review(&self) -> Value {
        policy::redact(&self.review)
    }
    fn scope(&self, c: &Core) -> Result<(), String> {
        if c.store.setting("privacy") == true {
            return Err("PRIVACY_DENIED".into());
        }
        if !c.enabled("macros") {
            return Err("MODULE_DISABLED: macros".into());
        }
        if authority(c)? != self.authority {
            return Err("MACRO_AUTHORITY_CHANGED_REVIEW_REQUIRED".into());
        }
        Ok(())
    }
    pub(crate) fn check(&self, c: &Core) -> Result<(), String> {
        self.scope(c)?;
        if let Some(original) = &self.row {
            let id = original["id"].as_str().ok_or("MACRO_ID")?;
            if fingerprint(&stored(c, id)?) != fingerprint(original) {
                return Err("MACRO_CHANGED_REVIEW_REQUIRED".into());
            }
        }
        Ok(())
    }
    pub(crate) async fn execute(self, c: &Arc<Core>) -> Result<Value, String> {
        self.check(c)?;
        let cancel = CancellationToken::new();
        let _cancel = CancelOnDrop(cancel.clone());
        let core = c.clone();
        tokio::task::spawn_blocking(move || {
            #[cfg(test)]
            let gate = core
                .macro_mutation_gate
                .lock()
                .map_err(|_| "MACRO_TEST_GATE_LOCK")?
                .take();
            // Declared before the lease, so its completion signal follows the
            // lease's Drop on every exit, including a cancelled/error path.
            #[cfg(test)]
            let _finished = gate.as_ref().map(|g| Finished(g.finished.clone()));
            let _lease = core
                .macro_actions
                .try_lock()
                .map_err(|_| "MACRO_CONFIGURATION_BUSY")?;
            let check = || {
                if cancel.is_cancelled() {
                    return Err("MACRO_CONFIGURATION_CANCELLED".into());
                }
                self.check(&core)
            };
            check()?;
            #[cfg(test)]
            if let Some(gate) = gate {
                gate.entered
                    .send(())
                    .map_err(|_| "MACRO_TEST_GATE_CHANNEL")?;
                gate.resume
                    .lock()
                    .map_err(|_| "MACRO_TEST_GATE_LOCK")?
                    .recv_timeout(std::time::Duration::from_secs(3))
                    .map_err(|_| "MACRO_TEST_GATE_TIMEOUT")?;
                check()?;
            }
            let result = configure_sync(&core, &self.action, &self.input, check)?;
            if cancel.is_cancelled() || self.scope(&core).is_err() {
                return Err(
                    "MACRO_CONFIGURATION_OUTCOME_UNCERTAIN: inspect before another operation"
                        .into(),
                );
            }
            Ok(result)
        })
        .await
        .map_err(|_| "MACRO_CONFIGURATION_OUTCOME_UNCERTAIN: native worker did not return")?
    }
}
struct CancelOnDrop(CancellationToken);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
#[cfg(test)]
pub(crate) struct Gate {
    pub entered: std::sync::mpsc::Sender<()>,
    pub resume: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    pub finished: std::sync::mpsc::Sender<()>,
}
#[cfg(test)]
struct Finished(std::sync::mpsc::Sender<()>);
#[cfg(test)]
impl Drop for Finished {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

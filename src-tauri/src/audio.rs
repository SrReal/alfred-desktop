use crate::runtime::Core;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

static AUDIO_LOCK: Mutex<()> = Mutex::new(());
const STATE_EPSILON: f64 = 0.01;
struct CancelOnDrop(CancellationToken);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
fn execution_allowed(
    core: &Core,
    cancel: &CancellationToken,
    deadline: Instant,
) -> Result<(), String> {
    if cancel.is_cancelled() {
        Err("AUDIO_CANCELLED".into())
    } else if Instant::now() >= deadline {
        Err("AUDIO_DEADLINE".into())
    } else if !core.enabled("windows") {
        Err("TOOL_DISABLED_OR_UNKNOWN".into())
    } else {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct State {
    volume: f64,
    muted: bool,
}
impl State {
    fn valid(self) -> bool {
        self.volume.is_finite() && (0.0..=100.0).contains(&self.volume)
    }
    fn matches(self, other: Self) -> bool {
        (self.volume - other.volume).abs() <= STATE_EPSILON && self.muted == other.muted
    }
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    device_id: String,
    expected: State,
    volume: Option<f64>,
    muted: Option<bool>,
}
impl Request {
    fn parse(input: &Value) -> Result<Self, String> {
        let value: Self = serde_json::from_value(input.clone()).map_err(|_| "AUDIO_INPUT")?;
        if value.device_id.is_empty()
            || value.device_id.len() > 4096
            || value.device_id.contains('\0')
            || !value.expected.valid()
            || value
                .volume
                .is_some_and(|n| !n.is_finite() || !(0.0..=100.0).contains(&n))
            || value.volume.is_none() && value.muted.is_none()
        {
            return Err("AUDIO_INPUT".into());
        }
        Ok(value)
    }
}

trait Endpoint {
    fn read(&mut self) -> Result<State, String>;
    fn volume(&mut self, value: f64) -> Result<(), String>;
    fn mute(&mut self, value: bool) -> Result<(), String>;
}
fn read(endpoint: &mut impl Endpoint) -> Result<State, String> {
    let value = endpoint.read()?;
    if !value.valid() {
        return Err("AUDIO_INVALID_READBACK".into());
    }
    Ok(value)
}

fn apply(
    endpoint: &mut impl Endpoint,
    request: &Request,
    guard: impl Fn() -> Result<(), String>,
) -> Result<Value, String> {
    let before = read(endpoint)?;
    if !before.matches(request.expected) {
        return Err("AUDIO_STATE_CHANGED: query the endpoint again before approval".into());
    }
    let mut owned_volume = None;
    let mut owned_mute = None;
    let result: Result<Value, String> = (|| {
        guard()?;
        if let Some(volume) = request.volume {
            endpoint.volume(volume)?;
            owned_volume = Some(volume);
            let after = read(endpoint)?;
            // A different observed scalar may belong to another client. Never claim it
            // as our write, even if it could also be hardware quantization.
            if (after.volume - volume).abs() > STATE_EPSILON {
                return Err("AUDIO_VOLUME_READBACK".into());
            }
            if after.muted != before.muted {
                return Err("AUDIO_STATE_CHANGED_DURING_WRITE".into());
            }
        }
        if let Some(muted) = request.muted {
            guard()?;
            let current = read(endpoint)?;
            let expected = State {
                volume: owned_volume.unwrap_or(before.volume),
                muted: before.muted,
            };
            if !current.matches(expected) {
                return Err("AUDIO_STATE_CHANGED_DURING_WRITE".into());
            }
            endpoint.mute(muted)?;
            owned_mute = Some(muted);
        }
        let after = read(endpoint)?;
        if owned_volume.is_some_and(|v| (after.volume - v).abs() > STATE_EPSILON) {
            return Err("AUDIO_VOLUME_READBACK".into());
        }
        if request.muted.is_some_and(|m| m != after.muted) {
            return Err("AUDIO_MUTE_READBACK".into());
        }
        Ok(
            json!({"device_id":request.device_id,"before":before,"after":after,"scope":"render_endpoint_master","undo_available":false}),
        )
    })();
    match result {
        Ok(value) => Ok(value),
        Err(error) => {
            if owned_volume.is_none() && owned_mute.is_none() {
                return Err(format!("{error}; rollback=not_needed"));
            }
            // Restore only fields still equal to our last write. Preserve an external change.
            // Core Audio has no atomic compare/exchange; the remaining race is documented.
            let rollback = (|| {
                let mut preserved = false;
                let mut restored_mute = false;
                let mut restored_volume = false;
                if let Some(muted) = owned_mute {
                    if read(endpoint)?.muted == muted {
                        endpoint.mute(before.muted)?;
                        restored_mute = true;
                    } else {
                        preserved = true;
                    }
                }
                if let Some(volume) = owned_volume {
                    if (read(endpoint)?.volume - volume).abs() <= STATE_EPSILON {
                        endpoint.volume(before.volume)?;
                        restored_volume = true;
                    } else {
                        preserved = true;
                    }
                }
                let current = read(endpoint)?;
                if restored_mute && current.muted != before.muted
                    || restored_volume && (current.volume - before.volume).abs() > STATE_EPSILON
                {
                    return Err("AUDIO_ROLLBACK_READBACK".into());
                }
                Ok::<_, String>(if preserved {
                    "preserved_external_change"
                } else {
                    "restored"
                })
            })();
            Err(format!(
                "{error}; rollback={}",
                rollback.unwrap_or("failed")
            ))
        }
    }
}

pub async fn dispatch(core: &Arc<Core>, tool: &str, input: &Value) -> Result<Value, String> {
    if tool == "windows.audio_set" {
        Request::parse(input)?;
    }
    let core = core.clone();
    let tool = tool.to_string();
    let input = input.clone();
    let cancel = CancellationToken::new();
    let _lease = CancelOnDrop(cancel.clone());
    let deadline = Instant::now() + Duration::from_secs(30);
    tokio::task::spawn_blocking(move || {
        let _lock = AUDIO_LOCK.lock().map_err(|_| "AUDIO_LOCK")?;
        let guard = || execution_allowed(&core, &cancel, deadline);
        guard()?;
        native(&tool, &input, guard)
    })
    .await
    .map_err(|_| "AUDIO_WORKER")?
}

#[cfg(not(windows))]
fn native(_: &str, _: &Value, _: impl Fn() -> Result<(), String>) -> Result<Value, String> {
    Err("WINDOWS_11_REQUIRED".into())
}

#[cfg(windows)]
fn native(
    tool: &str,
    input: &Value,
    guard: impl Fn() -> Result<(), String>,
) -> Result<Value, String> {
    use windows::{
        core::{Interface, PCWSTR},
        Win32::{
            Media::Audio::{
                eCommunications, eConsole, eMultimedia, eRender, IMMDeviceEnumerator, IMMEndpoint,
                MMDeviceEnumerator, DEVICE_STATE_ACTIVE,
            },
            System::Com::{
                CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
                COINIT_MULTITHREADED,
            },
        },
    };
    struct Apartment;
    impl Drop for Apartment {
        fn drop(&mut self) {
            unsafe { CoUninitialize() };
        }
    }
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
        .ok()
        .map_err(|e| code("AUDIO_COM", e))?;
    let _apartment = Apartment;
    let enumerator: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
            .map_err(|e| code("AUDIO_ENUMERATOR", e))?;
    let role = input["role"].as_str().unwrap_or("multimedia");
    let role_id = match role {
        "console" => eConsole,
        "multimedia" => eMultimedia,
        "communications" => eCommunications,
        _ => return Err("AUDIO_ROLE".into()),
    };
    let device = if let Some(id) = input["device_id"].as_str() {
        if id.is_empty() || id.len() > 4096 || id.contains('\0') {
            return Err("AUDIO_DEVICE_ID".into());
        }
        let wide: Vec<u16> = id.encode_utf16().chain(Some(0)).collect();
        unsafe { enumerator.GetDevice(PCWSTR(wide.as_ptr())) }
    } else {
        unsafe { enumerator.GetDefaultAudioEndpoint(eRender, role_id) }
    }
    .map_err(|e| code("AUDIO_ENDPOINT_UNAVAILABLE", e))?;
    if unsafe { device.GetState() }.map_err(|e| code("AUDIO_ENDPOINT_STATE", e))?
        != DEVICE_STATE_ACTIVE
    {
        return Err("AUDIO_ENDPOINT_INACTIVE".into());
    }
    let flow: IMMEndpoint = device.cast().map_err(|e| code("AUDIO_ENDPOINT_FLOW", e))?;
    if unsafe { flow.GetDataFlow() }.map_err(|e| code("AUDIO_ENDPOINT_FLOW", e))? != eRender {
        return Err("AUDIO_CAPTURE_ENDPOINT_DENIED".into());
    }
    let id = unsafe { device.GetId() }.map_err(|e| code("AUDIO_DEVICE_ID", e))?;
    let device_id = unsafe { id.to_string() };
    unsafe { CoTaskMemFree(Some(id.0 as _)) };
    let device_id = device_id.map_err(|_| "AUDIO_DEVICE_ID_ENCODING")?;
    let mut endpoint = NativeEndpoint(
        unsafe { device.Activate(CLSCTX_ALL, None) }
            .map_err(|e| code("AUDIO_ENDPOINT_ACTIVATE", e))?,
    );
    match tool {
        "windows.audio_get" => Ok(
            json!({"device_id":device_id,"state":read(&mut endpoint)?,"role":role,"selected_by":if input["device_id"].is_string(){"device_id"}else{"default_role"},"scope":"render_endpoint_master"}),
        ),
        "windows.audio_set" => {
            let request = Request::parse(input)?;
            if request.device_id != device_id {
                return Err("AUDIO_TARGET_CHANGED".into());
            }
            apply(&mut endpoint, &request, guard)
        }
        _ => Err("AUDIO_ACTION".into()),
    }
}
#[cfg(windows)]
fn code(operation: &str, error: windows::core::Error) -> String {
    format!("{operation}: HRESULT={:08x}", error.code().0 as u32)
}
#[cfg(windows)]
struct NativeEndpoint(windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume);
#[cfg(windows)]
impl Endpoint for NativeEndpoint {
    fn read(&mut self) -> Result<State, String> {
        Ok(State {
            volume: unsafe { self.0.GetMasterVolumeLevelScalar() }
                .map_err(|e| code("AUDIO_READ_VOLUME", e))? as f64
                * 100.0,
            muted: unsafe { self.0.GetMute() }
                .map_err(|e| code("AUDIO_READ_MUTE", e))?
                .as_bool(),
        })
    }
    fn volume(&mut self, volume: f64) -> Result<(), String> {
        unsafe {
            self.0
                .SetMasterVolumeLevelScalar((volume / 100.0) as f32, &AUDIO_EVENT_CONTEXT)
        }
        .map_err(|e| code("AUDIO_WRITE_VOLUME", e))
    }
    fn mute(&mut self, muted: bool) -> Result<(), String> {
        unsafe { self.0.SetMute(muted, &AUDIO_EVENT_CONTEXT) }
            .map_err(|e| code("AUDIO_WRITE_MUTE", e))
    }
}
#[cfg(windows)]
const AUDIO_EVENT_CONTEXT: windows::core::GUID =
    windows::core::GUID::from_u128(0x74e94487_45d5_4e36_bf00_5d3c025c87cb);

#[cfg(test)]
mod tests {
    use super::*;
    struct Fake {
        state: State,
        writes: Vec<String>,
        fail_mute: bool,
        external_volume: bool,
        external_after_volume: bool,
    }
    impl Endpoint for Fake {
        fn read(&mut self) -> Result<State, String> {
            Ok(self.state)
        }
        fn volume(&mut self, volume: f64) -> Result<(), String> {
            self.state.volume = if self.external_after_volume {
                72.
            } else {
                volume
            };
            self.writes.push(format!("volume:{volume}"));
            Ok(())
        }
        fn mute(&mut self, muted: bool) -> Result<(), String> {
            if self.fail_mute {
                if self.external_volume {
                    self.state.volume = 72.;
                }
                return Err("FIXTURE_MUTE_FAILED".into());
            }
            self.state.muted = muted;
            self.writes.push(format!("mute:{muted}"));
            Ok(())
        }
    }
    fn fixture() -> Fake {
        Fake {
            state: State {
                volume: 40.,
                muted: false,
            },
            writes: vec![],
            fail_mute: false,
            external_volume: false,
            external_after_volume: false,
        }
    }
    fn request() -> Request {
        Request {
            device_id: "fixture-render".into(),
            expected: State {
                volume: 40.,
                muted: false,
            },
            volume: Some(35.),
            muted: Some(true),
        }
    }
    #[test]
    fn schema_and_policy_reject_blind_changes_capture_and_invalid_values() {
        let get = crate::catalog::tool_schema("windows.audio_get", "");
        let set = crate::catalog::tool_schema("windows.audio_set", "device_id,expected");
        let get = jsonschema::validator_for(&get).unwrap();
        let set = jsonschema::validator_for(&set).unwrap();
        assert!(get.is_valid(&json!({"role":"communications"})));
        assert!(!get.is_valid(&json!({"role":"capture"})));
        let valid =
            json!({"device_id":"fixture","expected":{"volume":40.,"muted":false},"volume":30.});
        assert!(set.is_valid(&valid));
        assert!(Request::parse(&valid).is_ok());
        for v in [
            json!({"volume":30}),
            json!({"device_id":"fixture","expected":{"volume":40,"muted":false}}),
            json!({"device_id":"fixture","expected":{"volume":40,"muted":false},"volume":101}),
            json!({"device_id":"fixture","expected":{"volume":40,"muted":false},"muted":"false"}),
            json!({"device_id":"fixture","expected":{"volume":40,"muted":false},"volume":10,"role":"multimedia"}),
        ] {
            assert!(!set.is_valid(&v), "{v}");
            assert!(Request::parse(&v).is_err());
        }
        assert_eq!(
            crate::policy::classify("windows.audio_get", &json!({})),
            crate::policy::Risk::Free
        );
        assert_eq!(
            crate::policy::classify("windows.audio_set", &valid),
            crate::policy::Risk::Contextual
        );
    }
    #[test]
    fn stale_state_and_revocation_never_write_and_failures_restore_only_owned_fields() {
        let mut endpoint = fixture();
        let mut input = request();
        input.expected.volume = 39.;
        assert!(apply(&mut endpoint, &input, || Ok(()))
            .unwrap_err()
            .starts_with("AUDIO_STATE_CHANGED"));
        input.expected.volume = 40.;
        assert!(
            apply(&mut endpoint, &input, || Err("MODULE_REVOKED".into()))
                .unwrap_err()
                .starts_with("MODULE_REVOKED")
        );
        assert!(endpoint.writes.is_empty());
        endpoint.fail_mute = true;
        assert!(apply(&mut endpoint, &input, || Ok(()))
            .unwrap_err()
            .ends_with("rollback=restored"));
        assert!(endpoint.state.matches(input.expected));
        assert_eq!(endpoint.writes, vec!["volume:35", "volume:40"]);
        endpoint.external_volume = true;
        endpoint.writes.clear();
        assert!(apply(&mut endpoint, &input, || Ok(()))
            .unwrap_err()
            .ends_with("rollback=preserved_external_change"));
        assert_eq!(endpoint.state.volume, 72.);
        assert_eq!(endpoint.writes, vec!["volume:35"]);
        endpoint = fixture();
        endpoint.external_after_volume = true;
        let error = apply(&mut endpoint, &input, || Ok(())).unwrap_err();
        assert!(error.starts_with("AUDIO_VOLUME_READBACK"));
        assert!(error.ends_with("rollback=preserved_external_change"));
        assert_eq!(endpoint.state.volume, 72.);
        assert_eq!(endpoint.writes, vec!["volume:35"]);
    }
    #[test]
    fn partial_updates_readback_and_revocation_after_volume_roll_back() {
        let mut endpoint = fixture();
        let mut input = request();
        input.volume = None;
        let result = apply(&mut endpoint, &input, || Ok(())).unwrap();
        assert_eq!(result["after"]["volume"], 40.);
        assert_eq!(endpoint.writes, vec!["mute:true"]);
        endpoint = fixture();
        input = request();
        input.muted = None;
        let result = apply(&mut endpoint, &input, || Ok(())).unwrap();
        assert_eq!(result["after"]["muted"], false);
        assert_eq!(endpoint.writes, vec!["volume:35"]);
        endpoint = fixture();
        input = request();
        let count = std::cell::Cell::new(0);
        assert!(apply(&mut endpoint, &input, || {
            count.set(count.get() + 1);
            if count.get() > 1 {
                Err("MODULE_REVOKED".into())
            } else {
                Ok(())
            }
        })
        .unwrap_err()
        .ends_with("rollback=restored"));
        assert!(endpoint.state.matches(input.expected));
        endpoint.state.volume = f64::NAN;
        assert_eq!(
            apply(&mut endpoint, &input, || Ok(())).unwrap_err(),
            "AUDIO_INVALID_READBACK"
        );
    }
    #[tokio::test]
    async fn permission_audit_disable_deadline_and_abandoned_call_are_enforced() {
        let root = std::env::temp_dir().join(format!("ap-audio-fixture-{}", crate::storage::id()));
        let core = Arc::new(
            Core::new(
                root,
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
            )
            .unwrap(),
        );
        let input = json!({"device_id":"fixture-render","expected":{"volume":40.,"muted":false},"volume":35.});
        assert!(core
            .execute("windows.audio_set", input.clone())
            .await
            .is_err());
        let audit = core
            .store
            .query("SELECT tool_id,status FROM activity_events", &[])
            .unwrap();
        assert_eq!(audit[0]["tool_id"], "windows.audio_set");
        assert_eq!(audit[0]["status"], "denied");
        let cancel = CancellationToken::new();
        assert!(execution_allowed(&core, &cancel, Instant::now() + Duration::from_secs(1)).is_ok());
        assert_eq!(
            execution_allowed(&core, &cancel, Instant::now()).unwrap_err(),
            "AUDIO_DEADLINE"
        );
        let lease = CancelOnDrop(cancel.clone());
        drop(lease);
        assert_eq!(
            execution_allowed(&core, &cancel, Instant::now() + Duration::from_secs(1)).unwrap_err(),
            "AUDIO_CANCELLED"
        );
        core.store
            .execute(
                "UPDATE module_configs SET enabled=0 WHERE module_id='windows'",
                &[],
            )
            .unwrap();
        assert_eq!(
            core.execute("windows.audio_set", input).await.unwrap_err(),
            "TOOL_DISABLED_OR_UNKNOWN"
        );
        let cancel = CancellationToken::new();
        assert_eq!(
            execution_allowed(&core, &cancel, Instant::now() + Duration::from_secs(1)).unwrap_err(),
            "TOOL_DISABLED_OR_UNKNOWN"
        );
        assert_eq!(
            dispatch(&core, "windows.audio_get", &json!({}))
                .await
                .unwrap_err(),
            "TOOL_DISABLED_OR_UNKNOWN"
        );
    }
    #[cfg(windows)]
    #[test]
    #[ignore = "real render endpoint: read and write identical existing values only"]
    fn real_audio_read_and_identical_write() {
        let _lock = AUDIO_LOCK.lock().unwrap();
        let initial = native("windows.audio_get", &json!({}), || Ok(())).unwrap();
        let input = json!({"device_id":initial["device_id"],"expected":initial["state"],"volume":initial["state"]["volume"],"muted":initial["state"]["muted"]});
        let result = native("windows.audio_set", &input, || Ok(())).unwrap();
        let before: State = serde_json::from_value(initial["state"].clone()).unwrap();
        let after: State = serde_json::from_value(result["after"].clone()).unwrap();
        assert!(before.matches(after));
        assert!(native(
            "windows.audio_get",
            &json!({"device_id":"alfredpet-missing-endpoint"}),
            || Ok(())
        )
        .unwrap_err()
        .starts_with("AUDIO_ENDPOINT_UNAVAILABLE"));
        println!("AUDIO_REAL_CONTRACT_PASS: render read, explicit target, identical-value write/readback, missing endpoint; no sound played");
    }
}

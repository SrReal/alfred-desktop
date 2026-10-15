//! Typed WinCred adapter for the existing MCP UTF-16 envelope and UUID alias.
//! No shell, external process, serialization of private material or broad lookup.
use crate::{mcp_credentials, runtime::Core};

#[cfg(test)]
pub(crate) struct Gate {
    pub entered: std::sync::mpsc::Sender<()>,
    pub resume: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    pub finished: std::sync::mpsc::Sender<()>,
}

#[cfg(test)]
pub(crate) struct Fixture {
    pub readback: Option<String>,
    pub writes: usize,
}

pub(crate) async fn read(core: &Core, id: &str) -> Result<String, String> {
    mcp_credentials::canonical(id)?;
    #[cfg(test)]
    if let Some(fixture) = core
        .mcp_secret_fixture
        .lock()
        .map_err(|_| "MCP_NATIVE_FIXTURE_LOCK")?
        .as_ref()
    {
        return fixture
            .readback
            .clone()
            .ok_or("MCP_NATIVE_UNAVAILABLE".into());
    }
    #[cfg(not(test))]
    let _ = core;
    let id = id.to_owned();
    tokio::task::spawn_blocking(move || read_now(&id))
        .await
        .map_err(|_| "MCP_NATIVE_WORKER")?
}

pub(crate) async fn write(
    core: &Core,
    id: &str,
    value: &str,
    lease: &[std::fs::File],
) -> Result<(), String> {
    if lease.len() != 2 {
        return Err("MCP_NATIVE_LEASE_REQUIRED".into());
    }
    mcp_credentials::canonical(id)?;
    #[cfg(test)]
    if let Some(fixture) = core
        .mcp_secret_fixture
        .lock()
        .map_err(|_| "MCP_NATIVE_FIXTURE_LOCK")?
        .as_mut()
    {
        fixture.writes += 1;
        return Ok(());
    }
    #[cfg(not(test))]
    let _ = core;
    let id = id.to_owned();
    let value = value.to_owned();
    // Retain the exact caller's exclusive profile lease until the native write
    // finishes, even when the awaiting future is cancelled. A later writer must
    // not publish while an abandoned blocking worker can still overwrite it.
    let lease = lease
        .iter()
        .map(std::fs::File::try_clone)
        .collect::<std::io::Result<Vec<_>>>()
        .map_err(|_| "MCP_NATIVE_LEASE_DUPLICATE")?;
    #[cfg(test)]
    let gate = core
        .mcp_native_write_gate
        .lock()
        .map_err(|_| "MCP_NATIVE_FIXTURE_LOCK")?
        .clone();
    tokio::task::spawn_blocking(move || {
        let held = lease;
        #[cfg(test)]
        if let Some(gate) = &gate {
            gate.entered
                .send(())
                .map_err(|_| "MCP_NATIVE_FIXTURE_CHANNEL")?;
            gate.resume
                .lock()
                .map_err(|_| "MCP_NATIVE_FIXTURE_LOCK")?
                .recv_timeout(std::time::Duration::from_secs(3))
                .map_err(|_| "MCP_NATIVE_FIXTURE_TIMEOUT")?;
        }
        let result = write_now(&id, &value);
        drop(held);
        #[cfg(test)]
        if let Some(gate) = gate {
            let _ = gate.finished.send(());
        }
        result
    })
    .await
    .map_err(|_| "MCP_NATIVE_WORKER")?
}

#[cfg(windows)]
fn target(id: &str) -> Result<Vec<u16>, String> {
    Ok(format!("alfredpet.{}", mcp_credentials::alias(id)?)
        .encode_utf16()
        .chain(Some(0))
        .collect())
}
#[cfg(windows)]
fn read_now(id: &str) -> Result<String, String> {
    use windows::{
        core::PCWSTR,
        Win32::Security::Credentials::{CredFree, CredReadW, CREDENTIALW, CRED_TYPE_GENERIC},
    };
    struct Allocation(*mut CREDENTIALW);
    impl Drop for Allocation {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { CredFree(self.0.cast()) };
            }
        }
    }
    let name = target(id)?;
    let mut pointer = std::ptr::null_mut();
    unsafe { CredReadW(PCWSTR(name.as_ptr()), CRED_TYPE_GENERIC, None, &mut pointer) }
        .map_err(|_| "MCP_NATIVE_UNAVAILABLE")?;
    let _allocation = Allocation(pointer);
    if pointer.is_null() {
        return Err("MCP_NATIVE_FORMAT".into());
    }
    let credential = unsafe { &*pointer };
    let length = credential.CredentialBlobSize as usize;
    if credential.Type != CRED_TYPE_GENERIC
        || length == 0
        || length > 2560
        || !length.is_multiple_of(2)
        || credential.CredentialBlob.is_null()
        || credential.TargetName.is_null()
        || unsafe { credential.TargetName.to_string() }.map_err(|_| "MCP_NATIVE_FORMAT")?
            != format!("alfredpet.{}", mcp_credentials::alias(id)?)
    {
        return Err("MCP_NATIVE_FORMAT".into());
    }
    let bytes = unsafe { std::slice::from_raw_parts_mut(credential.CredentialBlob, length) };
    let mut wide: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect();
    let result = String::from_utf16(&wide).map_err(|_| "MCP_NATIVE_FORMAT".into());
    wide.fill(0);
    bytes.fill(0);
    result
}
#[cfg(windows)]
fn write_now(id: &str, value: &str) -> Result<(), String> {
    use windows::{
        core::PWSTR,
        Win32::Security::Credentials::{
            CredWriteW, CREDENTIALW, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC,
        },
    };
    let mut name = target(id)?;
    let mut username: Vec<u16> = "AlfredPet".encode_utf16().chain(Some(0)).collect();
    let mut bytes: Vec<u8> = value.encode_utf16().flat_map(u16::to_le_bytes).collect();
    if bytes.is_empty() || bytes.len() > 2560 || value.contains('\0') {
        bytes.fill(0);
        return Err("MCP_NATIVE_CAPACITY".into());
    }
    let credential = CREDENTIALW {
        Type: CRED_TYPE_GENERIC,
        TargetName: PWSTR(name.as_mut_ptr()),
        CredentialBlobSize: bytes.len() as u32,
        CredentialBlob: bytes.as_mut_ptr(),
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        UserName: PWSTR(username.as_mut_ptr()),
        ..Default::default()
    };
    let result = unsafe { CredWriteW(&credential, 0) }.map_err(|_| "MCP_NATIVE_WRITE".into());
    bytes.fill(0);
    result
}
#[cfg(not(windows))]
fn read_now(_: &str) -> Result<String, String> {
    Err("WINDOWS_11_REQUIRED".into())
}
#[cfg(not(windows))]
fn write_now(_: &str, _: &str) -> Result<(), String> {
    Err("WINDOWS_11_REQUIRED".into())
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{path::PathBuf, sync::Arc};
    use windows::{
        core::{PCWSTR, PWSTR},
        Win32::Security::Credentials::{
            CredDeleteW, CredFree, CredReadW, CredWriteW, CREDENTIALW, CRED_PERSIST_LOCAL_MACHINE,
            CRED_TYPE_GENERIC,
        },
    };
    struct Owned {
        id: String,
        target: Vec<u16>,
    }
    impl Owned {
        fn new() -> Self {
            let id = crate::storage::id();
            let target = target(&id).unwrap();
            let mut pointer = std::ptr::null_mut();
            let absent = unsafe {
                CredReadW(
                    PCWSTR(target.as_ptr()),
                    CRED_TYPE_GENERIC,
                    None,
                    &mut pointer,
                )
            };
            if absent.is_ok() {
                unsafe { CredFree(pointer.cast()) };
                panic!("Owned test alias already exists; do not modify it");
            }
            assert_eq!(absent.unwrap_err().code().0 as u32, 0x80070490);
            Self { id, target }
        }
    }
    impl Drop for Owned {
        fn drop(&mut self) {
            let _ = unsafe { CredDeleteW(PCWSTR(self.target.as_ptr()), CRED_TYPE_GENERIC, None) };
        }
    }
    #[tokio::test]
    async fn existing_powershell_utf16_envelope_and_direct_adapter_are_bidirectionally_compatible()
    {
        let owned = Owned::new();
        let root = std::env::temp_dir().join(format!("ap-mcp-native-{}", crate::storage::id()));
        std::fs::create_dir(&root).unwrap();
        let core = Arc::new(
            Core::new(
                root.join("profile"),
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
            )
            .unwrap(),
        );
        let alias = mcp_credentials::alias(&owned.id).unwrap();
        let legacy = json!({"kind":"owned legacy UTF16 fixture","value":"Señor 🐾 fictional-only"})
            .to_string();
        crate::process::native(
            &core.native,
            "secret.set",
            &json!({"alias":alias,"value":legacy}),
        )
        .await
        .unwrap();
        assert_eq!(read(&core, &owned.id).await.unwrap(), legacy);
        let current = json!({"kind":"owned direct UTF16 fixture","value":"é Ω 🐾 fictional-only"})
            .to_string();
        let lease = crate::integrations::save_lock(&core).unwrap();
        write(&core, &owned.id, &current, &lease).await.unwrap();
        drop(lease);
        let via_legacy =
            crate::process::native(&core.native, "secret.get", &json!({"alias":alias}))
                .await
                .unwrap();
        assert_eq!(via_legacy["value"], current);
        assert_eq!(read(&core, &owned.id).await.unwrap(), current);
        drop(core);
        let root = root.canonicalize().unwrap();
        assert_eq!(
            root.parent().unwrap(),
            std::env::temp_dir().canonicalize().unwrap()
        );
        assert!(root
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("ap-mcp-native-"));
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn capacity_invalid_encoding_and_namespace_are_denied_before_private_material_can_escape() {
        let owned = Owned::new();
        for value in ["".to_owned(), "x".repeat(1281), "owned\0invalid".to_owned()] {
            assert_eq!(
                write_now(&owned.id, &value).unwrap_err(),
                "MCP_NATIVE_CAPACITY"
            );
        }
        assert!(write_now("../../not-an-alias", "owned fictional").is_err());
        assert!(read_now("../../not-an-alias").is_err());
        assert_eq!(read_now(&owned.id).unwrap_err(), "MCP_NATIVE_UNAVAILABLE");
        let mut bytes = vec![0, 0xd8]; // Unpaired UTF16 high surrogate, own alias only.
        let mut target = owned.target.clone();
        let credential = CREDENTIALW {
            Type: CRED_TYPE_GENERIC,
            TargetName: PWSTR(target.as_mut_ptr()),
            CredentialBlobSize: 2,
            CredentialBlob: bytes.as_mut_ptr(),
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            ..Default::default()
        };
        unsafe { CredWriteW(&credential, 0) }.unwrap();
        assert_eq!(read_now(&owned.id).unwrap_err(), "MCP_NATIVE_FORMAT");
    }

    #[tokio::test]
    async fn cancelled_await_keeps_the_exact_profile_lease_until_the_owned_native_worker_finishes()
    {
        let owned = Owned::new();
        let root = std::env::temp_dir().join(format!("ap-mcp-native-{}", crate::storage::id()));
        std::fs::create_dir(&root).unwrap();
        let core = Arc::new(
            Core::new(
                root.join("profile"),
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
            )
            .unwrap(),
        );
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        let (finished_tx, finished_rx) = std::sync::mpsc::channel();
        *core.mcp_native_write_gate.lock().unwrap() = Some(Arc::new(Gate {
            entered: entered_tx,
            resume: std::sync::Mutex::new(resume_rx),
            finished: finished_tx,
        }));
        let owner = core.clone();
        let id = owned.id.clone();
        let task = tokio::spawn(async move {
            let lease = crate::integrations::save_lock(&owner).unwrap();
            write(
                &owner,
                &id,
                "owned fictional cancelled-write fixture",
                &lease,
            )
            .await
        });
        async fn signal(receiver: &std::sync::mpsc::Receiver<()>) {
            tokio::time::timeout(std::time::Duration::from_secs(3), async {
                loop {
                    match receiver.try_recv() {
                        Ok(()) => break,
                        Err(std::sync::mpsc::TryRecvError::Empty) => {
                            tokio::time::sleep(std::time::Duration::from_millis(10)).await
                        }
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                            panic!("Owned native gate disconnected")
                        }
                    }
                }
            })
            .await
            .unwrap();
        }
        signal(&entered_rx).await;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(
            crate::integrations::save_lock(&core).unwrap_err(),
            "CONNECTION_SAVE_BUSY"
        );
        resume_tx.send(()).unwrap();
        signal(&finished_rx).await;
        let lease = crate::integrations::save_lock(&core).unwrap();
        drop(lease);
        assert_eq!(
            read(&core, &owned.id).await.unwrap(),
            "owned fictional cancelled-write fixture"
        );
        drop(core);
        let root = root.canonicalize().unwrap();
        assert_eq!(
            root.parent().unwrap(),
            std::env::temp_dir().canonicalize().unwrap()
        );
        assert!(root
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("ap-mcp-native-"));
        std::fs::remove_dir_all(root).unwrap();
    }
}

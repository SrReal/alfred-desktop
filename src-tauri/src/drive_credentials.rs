//! Bounded, versioned Credential Manager storage with closed Drive/MCP domains.
//! Drive is the active actor; the MCP OAuth actor is not yet connected.
//! Not an OAuth flow: the caller must validate provider/client/grants and hold its
//! profile actor lock for the entire operation. No UI or generic secret route
//! accepts these blobs. A same-user external writer is outside that lock.
use sha2::{Digest, Sha256};
use std::{
    fmt,
    sync::atomic::{compiler_fence, Ordering},
};
use uuid::Uuid;

const LIMIT: usize = 64 * 1024;
const PART_BYTES: usize = 2048;
const BLOB_LIMIT: usize = 2560;
const MANIFEST_MAGIC: &[u8; 8] = b"APDRV1M\0";
const PART_MAGIC: &[u8; 8] = b"APDRV1P\0";
const MANIFEST_SIZE: usize = 126;
const PART_HEADER: usize = 94;
type Result<T> = std::result::Result<T, &'static str>;
#[path = "drive_receipts.rs"]
mod receipts;
pub(crate) use receipts::Recovery;
pub(crate) struct OwnedRecovery {
    pub(crate) binding: Binding,
    pub(crate) recovery: Recovery,
}

/// Application copies are cleared on drop; caller copies, OS allocations after
/// free, and provider/network buffers are not a proven full-memory erasure.
pub(crate) struct Secret(Vec<u8>);
impl Secret {
    pub(crate) fn copy(bytes: &[u8]) -> Self {
        Self(bytes.to_vec())
    }
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.0
    }
}
impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}
fn clear(bytes: &mut [u8]) {
    for byte in bytes {
        unsafe { std::ptr::write_volatile(byte, 0) };
    }
    compiler_fence(Ordering::SeqCst);
}
impl Drop for Secret {
    fn drop(&mut self) {
        clear(&mut self.0);
    }
}

/// Fixed namespaces and binary domains. Callers cannot supply an arbitrary
/// native alias. Existing Drive targets and bytes remain compatible.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Domain {
    Drive,
    McpOauth,
}
impl Domain {
    fn prefix(self) -> &'static str {
        match self {
            Self::Drive => "alfredpet.public.drive.credentials.",
            Self::McpOauth => "alfredpet.public.mcp.oauth.credentials.",
        }
    }
    fn manifest_magic(self) -> &'static [u8; 8] {
        match self {
            Self::Drive => MANIFEST_MAGIC,
            Self::McpOauth => b"APMCP1M\0",
        }
    }
    fn part_magic(self) -> &'static [u8; 8] {
        match self {
            Self::Drive => PART_MAGIC,
            Self::McpOauth => b"APMCP1P\0",
        }
    }
}

/// The public binding is hashed by the caller from its validated fixed provider
/// and client identity. It is not an access-token hash or a profile generation.
pub(crate) struct Binding {
    domain: Domain,
    connection: Uuid,
    profile: Uuid,
    client: [u8; 32],
}
fn canonical(id: &str) -> Result<Uuid> {
    let uuid = Uuid::parse_str(id).map_err(|_| "DRIVE_VAULT_UUID")?;
    if uuid.is_nil() || uuid.to_string() != id {
        return Err("DRIVE_VAULT_UUID");
    }
    Ok(uuid)
}
impl Binding {
    pub(crate) fn matches_client(&self, observation: &Observation) -> bool {
        self.domain == observation.domain
            && self.connection == observation.connection
            && self.profile == observation.profile
            && self.client == observation.client
    }
    pub(crate) fn new(connection: &str, profile: &str, client: [u8; 32]) -> Result<Self> {
        Self::for_domain(Domain::Drive, connection, profile, client)
    }
    /// The caller hashes its validated public issuer/resource/client identity
    /// into `client`, validates the private envelope/grants, and holds its
    /// cross-process profile lease for the whole publication/read/recovery.
    pub(crate) fn for_domain(
        domain: Domain,
        connection: &str,
        profile: &str,
        client: [u8; 32],
    ) -> Result<Self> {
        if client == [0; 32] {
            return Err("DRIVE_VAULT_CLIENT_BINDING");
        }
        Ok(Self {
            domain,
            connection: canonical(connection)?,
            profile: canonical(profile)?,
            client,
        })
    }
    fn main(&self) -> String {
        format!("{}{}", self.domain.prefix(), self.connection)
    }
    fn part(&self, generation: Uuid, index: usize) -> String {
        format!("{}.v.{}.p.{index:02}", self.main(), generation)
    }
}

/// Exact targets only. No credential enumeration, arbitrary aliases or fallback
/// to the legacy generic JSON/UTF-16 credential.
trait Backend {
    fn read(&mut self, target: &str) -> Result<Option<Secret>>;
    fn write(&mut self, target: &str, bytes: &[u8]) -> Result<()>;
    fn delete(&mut self, target: &str) -> Result<()>;
}
struct Manifest {
    generation: Uuid,
    length: usize,
    parts: usize,
    digest: [u8; 32],
}
/// Metadata for a confirmed reauthorization, without reading the old tokens.
/// Kept inside the native actor; not a caller-configurable credential alias.
#[derive(Clone)]
pub(crate) struct Observation {
    pub(crate) version: Uuid,
    client: [u8; 32],
    domain: Domain,
    connection: Uuid,
    profile: Uuid,
}
fn observe<B: Backend>(backend: &mut B, binding: &Binding) -> Result<Option<Observation>> {
    let Some(main) = backend.read(&binding.main())? else {
        return Ok(None);
    };
    if main.bytes().len() != MANIFEST_SIZE {
        return Err("DRIVE_VAULT_FORMAT");
    }
    let current = Binding {
        domain: binding.domain,
        connection: binding.connection,
        profile: binding.profile,
        client: main.bytes()[56..88].try_into().unwrap(),
    };
    if current.client == [0; 32] {
        return Err("DRIVE_VAULT_FORMAT");
    }
    let meta = decode(&current, main.bytes())?;
    if !matches(backend, &binding.main(), main.bytes())? {
        return Err("DRIVE_VAULT_CHANGED");
    }
    Ok(Some(Observation {
        version: meta.generation,
        client: current.client,
        domain: binding.domain,
        connection: binding.connection,
        profile: binding.profile,
    }))
}
fn decode(binding: &Binding, bytes: &[u8]) -> Result<Manifest> {
    if bytes.len() != MANIFEST_SIZE || &bytes[..8] != binding.domain.manifest_magic() {
        return Err("DRIVE_VAULT_FORMAT");
    }
    if &bytes[8..24] != binding.connection.as_bytes()
        || &bytes[24..40] != binding.profile.as_bytes()
        || bytes[56..88] != binding.client
    {
        return Err("DRIVE_VAULT_BINDING_CHANGED");
    }
    let generation = Uuid::from_slice(&bytes[40..56]).map_err(|_| "DRIVE_VAULT_FORMAT")?;
    let length = u32::from_le_bytes(bytes[88..92].try_into().unwrap()) as usize;
    let parts = u16::from_le_bytes(bytes[92..94].try_into().unwrap()) as usize;
    if generation.is_nil() || length == 0 || length > LIMIT || parts != length.div_ceil(PART_BYTES)
    {
        return Err("DRIVE_VAULT_FORMAT");
    }
    Ok(Manifest {
        generation,
        length,
        parts,
        digest: bytes[94..126].try_into().unwrap(),
    })
}
fn manifest(binding: &Binding, generation: Uuid, payload: &[u8]) -> Secret {
    let mut b = Vec::with_capacity(MANIFEST_SIZE);
    b.extend_from_slice(binding.domain.manifest_magic());
    b.extend_from_slice(binding.connection.as_bytes());
    b.extend_from_slice(binding.profile.as_bytes());
    b.extend_from_slice(generation.as_bytes());
    b.extend_from_slice(&binding.client);
    b.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    b.extend_from_slice(&(payload.len().div_ceil(PART_BYTES) as u16).to_le_bytes());
    b.extend_from_slice(&Sha256::digest(payload));
    Secret(b)
}
fn part(binding: &Binding, main: &Secret, index: usize, bytes: &[u8]) -> Secret {
    let mut b = Vec::with_capacity(PART_HEADER + bytes.len());
    b.extend_from_slice(binding.domain.part_magic());
    b.extend_from_slice(&main.bytes()[8..94]);
    b[92..94].copy_from_slice(&(index as u16).to_le_bytes());
    b.extend_from_slice(bytes);
    Secret(b)
}
struct Snapshot {
    main: Secret,
    meta: Manifest,
    parts: Vec<Secret>,
    payload: Secret,
}
fn snapshot<B: Backend>(backend: &mut B, binding: &Binding) -> Result<Option<Snapshot>> {
    let Some(main) = backend.read(&binding.main())? else {
        return Ok(None);
    };
    let meta = decode(binding, main.bytes())?;
    let mut parts = Vec::with_capacity(meta.parts);
    let mut payload = Secret(Vec::with_capacity(meta.length));
    for index in 0..meta.parts {
        let bytes = backend
            .read(&binding.part(meta.generation, index))?
            .ok_or("DRIVE_VAULT_PART_MISSING")?;
        let expected_len = (meta.length - index * PART_BYTES).min(PART_BYTES);
        if bytes.bytes().len() != PART_HEADER + expected_len
            || &bytes.bytes()[..8] != binding.domain.part_magic()
            || bytes.bytes()[8..92] != main.bytes()[8..92]
            || bytes.bytes()[92..94] != (index as u16).to_le_bytes()
        {
            return Err("DRIVE_VAULT_PART_INVALID");
        }
        payload.0.extend_from_slice(&bytes.bytes()[PART_HEADER..]);
        parts.push(bytes);
    }
    if Sha256::digest(payload.bytes())[..] != meta.digest {
        return Err("DRIVE_VAULT_INTEGRITY");
    }
    if backend
        .read(&binding.main())?
        .is_none_or(|current| current.bytes() != main.bytes())
    {
        return Err("DRIVE_VAULT_CHANGED");
    }
    Ok(Some(Snapshot {
        main,
        meta,
        parts,
        payload,
    }))
}
fn matches<B: Backend>(backend: &mut B, target: &str, expected: &[u8]) -> Result<bool> {
    Ok(backend.read(target)?.is_some_and(|v| v.bytes() == expected))
}
/// Cleanup compares exact owned bytes immediately before delete. WinCred has no
/// conditional delete; a non-cooperating writer can still race that comparison.
fn retire<B: Backend>(backend: &mut B, owned: &[(String, Secret)]) -> bool {
    let mut complete = true;
    for (target, bytes) in owned {
        match backend.read(target) {
            Ok(None) => (),
            Ok(Some(current)) if current.bytes() == bytes.bytes() => {
                if backend.delete(target).is_err() || !matches!(backend.read(target), Ok(None)) {
                    complete = false;
                }
            }
            _ => complete = false,
        }
    }
    complete
}
/// This is a private storage receipt, never a token/audit/network result.
#[derive(Debug)]
pub(crate) struct Receipt {
    pub(crate) version: Uuid,
    pub(crate) cleanup_complete: bool,
}
struct Vault<B: Backend> {
    backend: B,
}
impl<B: Backend> Vault<B> {
    #[cfg(test)]
    fn erase_owned(&mut self, binding: &Binding, expected: Uuid) -> Result<bool> {
        self.erase_context(binding, expected, None)
    }
    fn erase_context(
        &mut self,
        binding: &Binding,
        expected: Uuid,
        context: Option<[u8; 32]>,
    ) -> Result<bool> {
        let state = snapshot(&mut self.backend, binding)?.ok_or("DRIVE_VAULT_NOT_FOUND")?;
        if state.meta.generation != expected {
            return Err("DRIVE_VAULT_VERSION_CHANGED");
        }
        receipts::register(
            &mut self.backend,
            binding,
            &state.main,
            &state.parts,
            context,
        )?;
        let seen = receipts::inspect_all(&mut self.backend, binding, context)?
            .into_iter()
            .find(|r| r.version == expected)
            .ok_or("DRIVE_VAULT_RECEIPT_NOT_FOUND")?;
        if !seen.can_retire {
            return Ok(false);
        }
        receipts::retire_known(&mut self.backend, binding, context, &seen)
    }
    fn read(&mut self, binding: &Binding) -> Result<(Uuid, Secret)> {
        let state = snapshot(&mut self.backend, binding)?.ok_or("DRIVE_VAULT_NOT_FOUND")?;
        Ok((state.meta.generation, state.payload))
    }
    /// None means a new absent target. Some(version) only replaces that observed
    /// complete version with the SAME profile/client binding. Changed bindings
    /// need an explicit reauthorization/migration workflow in the future adapter.
    fn replace(
        &mut self,
        binding: &Binding,
        expected: Option<Uuid>,
        payload: &[u8],
    ) -> Result<Receipt> {
        self.replace_bound(binding, binding, expected, payload, None)
    }
    fn reauthorize(
        &mut self,
        binding: &Binding,
        observed: Option<&Observation>,
        payload: &[u8],
    ) -> Result<Receipt> {
        self.reauthorize_bound(binding, observed, payload, None)
    }
    fn reauthorize_bound(
        &mut self,
        binding: &Binding,
        observed: Option<&Observation>,
        payload: &[u8],
        context: Option<[u8; 32]>,
    ) -> Result<Receipt> {
        if observed.is_some_and(|o| {
            o.domain != binding.domain
                || o.connection != binding.connection
                || o.profile != binding.profile
        }) {
            return Err("DRIVE_VAULT_OBSERVATION_BINDING");
        }
        let old = Binding {
            domain: binding.domain,
            connection: binding.connection,
            profile: binding.profile,
            client: observed.map_or(binding.client, |o| o.client),
        };
        self.replace_bound(binding, &old, observed.map(|o| o.version), payload, context)
    }
    fn replace_bound(
        &mut self,
        binding: &Binding,
        old_binding: &Binding,
        expected: Option<Uuid>,
        payload: &[u8],
        context: Option<[u8; 32]>,
    ) -> Result<Receipt> {
        if payload.is_empty() || payload.len() > LIMIT {
            return Err("DRIVE_VAULT_PAYLOAD_LIMIT");
        }
        let old = snapshot(&mut self.backend, old_binding)?;
        if old.as_ref().map(|s| s.meta.generation) != expected {
            return Err("DRIVE_VAULT_VERSION_CHANGED");
        }
        receipts::trim_retired(&mut self.backend, binding)?;
        if let Some(old) = &old {
            receipts::register(
                &mut self.backend,
                old_binding,
                &old.main,
                &old.parts,
                context,
            )?;
        }
        let generation = Uuid::new_v4();
        let main = manifest(binding, generation, payload);
        let candidate = payload
            .chunks(PART_BYTES)
            .enumerate()
            .map(|(i, b)| part(binding, &main, i, b))
            .collect::<Vec<_>>();
        receipts::register(&mut self.backend, binding, &main, &candidate, context)?;
        let mut written = Vec::new();
        for (index, bytes) in payload.chunks(PART_BYTES).enumerate() {
            let target = binding.part(generation, index);
            let blob = part(binding, &main, index, bytes);
            let result = (|| {
                if self.backend.read(&target)?.is_some() {
                    return Err("DRIVE_VAULT_PART_COLLISION");
                }
                // Track before write: an error may be reported after the write.
                written.push((target.clone(), Secret::copy(blob.bytes())));
                self.backend.write(&target, blob.bytes())?;
                if !matches(&mut self.backend, &target, blob.bytes())? {
                    return Err("DRIVE_VAULT_PART_VERIFY");
                }
                Ok(())
            })();
            if let Err(error) = result {
                retire(&mut self.backend, &written);
                return Err(error);
            }
        }
        let current = self.backend.read(&binding.main());
        let unchanged = match (&current, &old) {
            (Ok(None), None) => true,
            (Ok(Some(current)), Some(old)) => current.bytes() == old.main.bytes(),
            _ => false,
        };
        if !unchanged {
            retire(&mut self.backend, &written);
            return Err("DRIVE_VAULT_CHANGED");
        }
        // Only now publish the manifest. If saving or verifying it fails, retain
        // all candidate parts: another reader may already see the new pointer.
        if self.backend.write(&binding.main(), main.bytes()).is_err()
            || !matches(&mut self.backend, &binding.main(), main.bytes()).unwrap_or(false)
        {
            return Err("DRIVE_VAULT_COMMIT_UNCERTAIN");
        }
        let cleanup_complete = if let Some(old) = old {
            let seen = receipts::inspect_all(&mut self.backend, old_binding, context)?
                .into_iter()
                .find(|r| r.version == old.meta.generation)
                .ok_or("DRIVE_VAULT_RECEIPT_NOT_FOUND")?;
            if seen.can_retire {
                receipts::retire_known(&mut self.backend, old_binding, context, &seen)?
            } else {
                false
            }
        } else {
            true
        };
        // An older interrupted publication is still pending even when this
        // replacement's immediate predecessor was retired successfully.
        let cleanup_complete = cleanup_complete
            && !receipts::has_pending_other(&mut self.backend, binding, generation)?;
        Ok(Receipt {
            version: generation,
            cleanup_complete,
        })
    }
    fn replace_context(
        &mut self,
        binding: &Binding,
        expected: Option<Uuid>,
        payload: &[u8],
        context: [u8; 32],
    ) -> Result<Receipt> {
        self.replace_bound(binding, binding, expected, payload, Some(context))
    }
    fn recoveries(
        &mut self,
        binding: &Binding,
        context: Option<[u8; 32]>,
    ) -> Result<Vec<Recovery>> {
        receipts::inspect_all(&mut self.backend, binding, context)
    }
    fn recover(
        &mut self,
        binding: &Binding,
        context: Option<[u8; 32]>,
        expected: &Recovery,
    ) -> Result<bool> {
        receipts::retire_known(&mut self.backend, binding, context, expected)
    }
}

#[cfg(windows)]
pub(crate) mod win {
    use super::*;
    use windows::Win32::{
        Foundation::{CloseHandle, LocalFree, HANDLE, HLOCAL, WAIT_ABANDONED, WAIT_OBJECT_0},
        Security::{
            Authorization::{
                ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
            },
            PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES,
        },
        System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject},
    };
    use windows::{
        core::{PCWSTR, PWSTR},
        Win32::Security::Credentials::{
            CredDeleteW, CredFree, CredReadW, CredWriteW, CREDENTIALW,
            CRED_MAX_CREDENTIAL_BLOB_SIZE, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC,
        },
    };
    // Coordinate this application's native API calls across owned profiles and
    // processes in the current user/session. Keep allocation clearing/freeing
    // inside the lease too. Other applications/external writers remain outside
    // this guarantee; existing exact snapshot/part/receipt guards still apply.
    struct NativeLease(HANDLE);
    impl Drop for NativeLease {
        fn drop(&mut self) {
            let _ = unsafe { ReleaseMutex(self.0) };
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
    fn native_lease() -> Result<NativeLease> {
        let sid =
            crate::asset_files::user_sid().map_err(|_| "DRIVE_VAULT_NATIVE_LEASE_IDENTITY")?;
        let name = format!(
            "Local\\AlfredPet.Drive.WinCred.{:x}",
            Sha256::digest(sid.as_bytes())
        );
        let name = name.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
        let sddl = format!("O:{sid}D:P(A;;GA;;;{sid})");
        let sddl = sddl.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(sddl.as_ptr()),
                SDDL_REVISION_1,
                &mut descriptor,
                None,
            )
        }
        .map_err(|_| "DRIVE_VAULT_NATIVE_LEASE_SECURITY")?;
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0,
            ..Default::default()
        };
        let created = unsafe { CreateMutexW(Some(&attributes), false, PCWSTR(name.as_ptr())) };
        let _ = unsafe { LocalFree(Some(HLOCAL(descriptor.0))) };
        let handle = created.map_err(|_| "DRIVE_VAULT_NATIVE_LEASE_OPEN")?;
        match unsafe { WaitForSingleObject(handle, 5000) } {
            WAIT_OBJECT_0 => Ok(NativeLease(handle)),
            WAIT_ABANDONED => {
                // Do not perform a credential operation after an abandoned
                // call. Release the lease and require a fresh explicit attempt.
                drop(NativeLease(handle));
                Err("DRIVE_VAULT_NATIVE_LEASE_ABANDONED")
            }
            _ => {
                let _ = unsafe { CloseHandle(handle) };
                Err("DRIVE_VAULT_NATIVE_LEASE_BUSY_OR_FAILED")
            }
        }
    }
    pub(super) struct Native;
    struct Allocation(*mut CREDENTIALW);
    impl Drop for Allocation {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    let c = &mut *self.0;
                    if c.CredentialBlobSize <= CRED_MAX_CREDENTIAL_BLOB_SIZE
                        && !c.CredentialBlob.is_null()
                    {
                        clear(std::slice::from_raw_parts_mut(
                            c.CredentialBlob,
                            c.CredentialBlobSize as usize,
                        ));
                    }
                    CredFree(self.0.cast());
                }
            }
        }
    }
    fn wide(target: &str) -> Result<Vec<u16>> {
        // No arbitrary WinCred target is exposed even to future internal callers.
        let rest = [Domain::Drive, Domain::McpOauth]
            .into_iter()
            .find_map(|d| target.strip_prefix(d.prefix()))
            .ok_or("DRIVE_VAULT_TARGET")?;
        let fields: Vec<_> = rest.split('.').collect();
        match fields.as_slice() {
            [connection] => {
                canonical(connection)?;
            }
            [connection, "v", version, "p", index] => {
                canonical(connection)?;
                canonical(version)?;
                let n = index.parse::<usize>().map_err(|_| "DRIVE_VAULT_TARGET")?;
                if n >= LIMIT.div_ceil(PART_BYTES) || *index != format!("{n:02}") {
                    return Err("DRIVE_VAULT_TARGET");
                }
            }
            [connection, "ledger"] => {
                canonical(connection)?;
            }
            [connection, "r", version] => {
                canonical(connection)?;
                canonical(version)?;
            }
            _ => return Err("DRIVE_VAULT_TARGET"),
        }
        Ok(target.encode_utf16().chain(Some(0)).collect())
    }
    impl Backend for Native {
        fn read(&mut self, target: &str) -> Result<Option<Secret>> {
            let name = wide(target)?;
            let _lease = native_lease()?;
            let mut ptr = std::ptr::null_mut();
            match unsafe { CredReadW(PCWSTR(name.as_ptr()), CRED_TYPE_GENERIC, None, &mut ptr) } {
                Err(e) if e.code().0 as u32 == 0x80070490 => return Ok(None),
                Err(_) => return Err("DRIVE_VAULT_NATIVE_READ"),
                Ok(()) => (),
            }
            let _allocation = Allocation(ptr);
            if ptr.is_null() {
                return Err("DRIVE_VAULT_NATIVE_FORMAT");
            }
            unsafe {
                let c = &*ptr;
                if c.Type != CRED_TYPE_GENERIC
                    || c.Persist != CRED_PERSIST_LOCAL_MACHINE
                    || c.Flags.0 != 0
                    || c.AttributeCount != 0
                    || c.CredentialBlobSize == 0
                    || c.CredentialBlobSize as usize > BLOB_LIMIT
                    || c.CredentialBlob.is_null()
                    || c.TargetName
                        .to_string()
                        .map_err(|_| "DRIVE_VAULT_NATIVE_FORMAT")?
                        .to_ascii_lowercase()
                        != target
                {
                    return Err("DRIVE_VAULT_NATIVE_FORMAT");
                }
                Ok(Some(Secret::copy(std::slice::from_raw_parts(
                    c.CredentialBlob,
                    c.CredentialBlobSize as usize,
                ))))
            }
        }
        fn write(&mut self, target: &str, bytes: &[u8]) -> Result<()> {
            #[cfg(test)]
            mcp_owned_boundary_target(target)?;
            let mut name = wide(target)?;
            if bytes.is_empty() || bytes.len() > BLOB_LIMIT {
                return Err("DRIVE_VAULT_BLOB_LIMIT");
            }
            let _lease = native_lease()?;
            let mut value = Secret::copy(bytes);
            let credential = CREDENTIALW {
                Type: CRED_TYPE_GENERIC,
                TargetName: PWSTR(name.as_mut_ptr()),
                CredentialBlobSize: value.0.len() as u32,
                CredentialBlob: value.0.as_mut_ptr(),
                Persist: CRED_PERSIST_LOCAL_MACHINE,
                ..Default::default()
            };
            unsafe { CredWriteW(&credential, 0) }.map_err(|_| "DRIVE_VAULT_NATIVE_WRITE")?;
            #[cfg(test)]
            mcp_owned_boundary(target, Some(bytes))?;
            Ok(())
        }
        fn delete(&mut self, target: &str) -> Result<()> {
            #[cfg(test)]
            mcp_owned_boundary_target(target)?;
            let name = wide(target)?;
            let _lease = native_lease()?;
            match unsafe { CredDeleteW(PCWSTR(name.as_ptr()), CRED_TYPE_GENERIC, None) } {
                Ok(()) => Ok(()),
                Err(e) if e.code().0 as u32 == 0x80070490 => Ok(()),
                Err(_) => Err("DRIVE_VAULT_NATIVE_DELETE"),
            }?;
            #[cfg(test)]
            mcp_owned_boundary(target, None)?;
            Ok(())
        }
    }
    #[cfg(test)]
    struct McpBoundary {
        prefix: String,
        stop: usize,
        count: usize,
    }
    #[cfg(test)]
    static MCP_BOUNDARY: std::sync::Mutex<Option<McpBoundary>> = std::sync::Mutex::new(None);
    // Only the spawned owned actor helper installs this. No production hook,
    // environment override, credential enumeration or arbitrary alias exists.
    #[cfg(test)]
    pub(crate) fn install_owned_mcp_boundary(binding: &Binding, stop: usize) -> Result<()> {
        if binding.domain != Domain::McpOauth || stop == 0 {
            return Err("OWNED_MCP_BOUNDARY_INPUT");
        }
        let mut hook = MCP_BOUNDARY.lock().map_err(|_| "OWNED_MCP_BOUNDARY_LOCK")?;
        if hook.is_some() {
            return Err("OWNED_MCP_BOUNDARY_ALREADY_SET");
        }
        *hook = Some(McpBoundary {
            prefix: binding.main(),
            stop,
            count: 0,
        });
        Ok(())
    }
    #[cfg(test)]
    fn mcp_owned_boundary_target(target: &str) -> Result<()> {
        let hook = MCP_BOUNDARY.lock().map_err(|_| "OWNED_MCP_BOUNDARY_LOCK")?;
        if hook
            .as_ref()
            .is_some_and(|h| target != h.prefix && !target.starts_with(&format!("{}.", h.prefix)))
        {
            return Err("OWNED_MCP_BOUNDARY_TARGET_DENIED");
        }
        Ok(())
    }
    #[cfg(test)]
    fn mcp_owned_boundary(target: &str, expected: Option<&[u8]>) -> Result<()> {
        use std::io::Write;
        let mut hook = MCP_BOUNDARY.lock().map_err(|_| "OWNED_MCP_BOUNDARY_LOCK")?;
        let Some(hook) = hook.as_mut() else {
            return Ok(());
        };
        if target != hook.prefix && !target.starts_with(&format!("{}.", hook.prefix)) {
            return Err("OWNED_MCP_BOUNDARY_TARGET_DENIED");
        }
        let current = Native.read(target)?;
        if !match (expected, current.as_ref()) {
            (None, None) => true,
            (Some(a), Some(b)) => a == b.bytes(),
            _ => false,
        } {
            return Err("OWNED_MCP_BOUNDARY_NOT_OBSERVED");
        }
        hook.count += 1;
        if hook.count == hook.stop {
            println!("OWNED_MCP_ACTOR_BOUNDARY:{}", hook.count);
            std::io::stdout()
                .flush()
                .map_err(|_| "OWNED_MCP_BOUNDARY_SIGNAL")?;
            loop {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn owned_mcp_publication_boundary_count(bytes: usize) -> usize {
        3 + bytes.div_ceil(PART_BYTES)
    }
    /// Intended for the Drive actor, which must hold its cross-process profile
    /// save/read lease. Generic secret aliases are never accepted here.
    pub(crate) fn read(binding: &Binding) -> Result<(Uuid, Secret)> {
        Vault { backend: Native }.read(binding)
    }
    #[cfg(test)]
    pub(crate) fn erase_owned(binding: &Binding, expected: Uuid) -> Result<bool> {
        Vault { backend: Native }.erase_owned(binding, expected)
    }
    pub(crate) fn erase_context(
        binding: &Binding,
        expected: Uuid,
        context: [u8; 32],
    ) -> Result<bool> {
        Vault { backend: Native }.erase_context(binding, expected, Some(context))
    }
    pub(crate) fn replace_context(
        binding: &Binding,
        expected: Option<Uuid>,
        bytes: &[u8],
        context: [u8; 32],
    ) -> Result<Receipt> {
        Vault { backend: Native }.replace_context(binding, expected, bytes, context)
    }
    pub(crate) fn recoveries(
        binding: &Binding,
        context: Option<[u8; 32]>,
    ) -> Result<Vec<Recovery>> {
        Vault { backend: Native }.recoveries(binding, context)
    }
    pub(crate) fn recover(
        binding: &Binding,
        context: Option<[u8; 32]>,
        expected: &Recovery,
    ) -> Result<bool> {
        Vault { backend: Native }.recover(binding, context, expected)
    }
    pub(crate) fn mcp_context_inventory(
        binding: &Binding,
        context: [u8; 32],
    ) -> Result<Vec<OwnedRecovery>> {
        receipts::mcp_context_inventory(&mut Native, binding, context)
    }
    pub(crate) fn compact_mcp_context(binding: &Binding, context: [u8; 32]) -> Result<()> {
        receipts::compact_mcp_context(&mut Native, binding, context)
    }
    #[cfg(test)]
    pub(crate) fn purge_retired(binding: &Binding) -> Result<()> {
        receipts::trim_retired(&mut Native, binding)
    }
    pub(crate) fn replace(
        binding: &Binding,
        expected: Option<Uuid>,
        bytes: &[u8],
    ) -> Result<Receipt> {
        Vault { backend: Native }.replace(binding, expected, bytes)
    }
    pub(crate) fn observe(binding: &Binding) -> Result<Option<Observation>> {
        super::observe(&mut Native, binding)
    }
    pub(crate) fn reauthorize(
        binding: &Binding,
        observed: Option<&Observation>,
        bytes: &[u8],
    ) -> Result<Receipt> {
        Vault { backend: Native }.reauthorize(binding, observed, bytes)
    }
    pub(crate) fn reauthorize_context(
        binding: &Binding,
        observed: Option<&Observation>,
        bytes: &[u8],
        context: [u8; 32],
    ) -> Result<Receipt> {
        Vault { backend: Native }.reauthorize_bound(binding, observed, bytes, Some(context))
    }
}
#[cfg(test)]
pub(crate) struct TestVault {
    vault: Vault<TestBackend>,
    pub(crate) fail_commit: bool,
}
#[cfg(test)]
#[derive(Default)]
struct TestBackend {
    entries: std::collections::HashMap<String, Secret>,
    fail_commit: bool,
}
#[cfg(test)]
impl Backend for TestBackend {
    fn read(&mut self, target: &str) -> Result<Option<Secret>> {
        Ok(self.entries.get(target).map(|v| Secret::copy(v.bytes())))
    }
    fn write(&mut self, target: &str, value: &[u8]) -> Result<()> {
        self.entries.insert(target.into(), Secret::copy(value));
        if self.fail_commit
            && [Domain::Drive, Domain::McpOauth]
                .iter()
                .any(|d| value.starts_with(d.manifest_magic()))
        {
            return Err("OWNED_AFTER_WRITE_FAILURE");
        }
        Ok(())
    }
    fn delete(&mut self, target: &str) -> Result<()> {
        self.entries.remove(target);
        Ok(())
    }
}
#[cfg(test)]
impl Default for TestVault {
    fn default() -> Self {
        Self {
            vault: Vault {
                backend: TestBackend::default(),
            },
            fail_commit: false,
        }
    }
}
#[cfg(test)]
impl TestVault {
    pub(crate) fn mcp_context_inventory(
        &mut self,
        binding: &Binding,
        context: [u8; 32],
    ) -> Result<Vec<OwnedRecovery>> {
        receipts::mcp_context_inventory(&mut self.vault.backend, binding, context)
    }
    pub(crate) fn compact_mcp_context(
        &mut self,
        binding: &Binding,
        context: [u8; 32],
    ) -> Result<()> {
        receipts::compact_mcp_context(&mut self.vault.backend, binding, context)
    }
    pub(crate) fn reauthorize_context(
        &mut self,
        binding: &Binding,
        observed: Option<&Observation>,
        bytes: &[u8],
        context: [u8; 32],
    ) -> Result<Receipt> {
        self.vault.backend.fail_commit = self.fail_commit;
        self.vault
            .reauthorize_bound(binding, observed, bytes, Some(context))
    }
    pub(crate) fn lose_owned_pointer(&mut self, binding: &Binding) -> Result<()> {
        snapshot(&mut self.vault.backend, binding)?.ok_or("OWNED_TEST_MANIFEST_MISSING")?;
        self.vault.backend.entries.remove(&binding.main());
        Ok(())
    }
    // A same-version metadata corruption fixture, with no native credential
    // access. Lets actor tests distinguish client binding from version checks.
    pub(crate) fn corrupt_client_metadata(&mut self, binding: &Binding) -> Result<()> {
        let entry = self
            .vault
            .backend
            .entries
            .get_mut(&binding.main())
            .ok_or("OWNED_TEST_MANIFEST_MISSING")?;
        if entry.bytes().len() != MANIFEST_SIZE {
            return Err("OWNED_TEST_MANIFEST_FORMAT");
        }
        entry.0[56..88].fill(91);
        Ok(())
    }
    pub(crate) fn read(&mut self, binding: &Binding) -> Result<(Uuid, Secret)> {
        self.vault.read(binding)
    }
    pub(crate) fn erase_context(
        &mut self,
        binding: &Binding,
        expected: Uuid,
        context: [u8; 32],
    ) -> Result<bool> {
        self.vault.erase_context(binding, expected, Some(context))
    }
    pub(crate) fn replace_context(
        &mut self,
        binding: &Binding,
        expected: Option<Uuid>,
        bytes: &[u8],
        context: [u8; 32],
    ) -> Result<Receipt> {
        self.vault.backend.fail_commit = self.fail_commit;
        self.vault
            .replace_context(binding, expected, bytes, context)
    }
    pub(crate) fn recoveries(
        &mut self,
        binding: &Binding,
        context: Option<[u8; 32]>,
    ) -> Result<Vec<Recovery>> {
        self.vault.recoveries(binding, context)
    }
    pub(crate) fn recover(
        &mut self,
        binding: &Binding,
        context: Option<[u8; 32]>,
        expected: &Recovery,
    ) -> Result<bool> {
        self.vault.recover(binding, context, expected)
    }
    pub(crate) fn observe(&mut self, binding: &Binding) -> Result<Option<Observation>> {
        observe(&mut self.vault.backend, binding)
    }
    pub(crate) fn replace(
        &mut self,
        binding: &Binding,
        expected: Option<Uuid>,
        bytes: &[u8],
    ) -> Result<Receipt> {
        self.vault.backend.fail_commit = self.fail_commit;
        self.vault.replace(binding, expected, bytes)
    }
    pub(crate) fn reauthorize(
        &mut self,
        binding: &Binding,
        expected: Option<&Observation>,
        bytes: &[u8],
    ) -> Result<Receipt> {
        self.vault.backend.fail_commit = self.fail_commit;
        self.vault.reauthorize(binding, expected, bytes)
    }
}
#[cfg(test)]
#[path = "drive_credentials_tests.rs"]
mod tests;

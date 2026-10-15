//! Protected generation inventory. Exact bounded targets, never enumeration.
use super::*;
const INDEX_MAGIC: &[u8; 8] = b"APDRV2I\0";
const RECEIPT_MAGIC: &[u8; 8] = b"APDRV2R\0";
impl Domain {
    pub(super) fn index_magic(self) -> &'static [u8; 8] {
        match self {
            Self::Drive => INDEX_MAGIC,
            Self::McpOauth => b"APMCP2I\0",
        }
    }
    pub(super) fn receipt_magic(self) -> &'static [u8; 8] {
        match self {
            Self::Drive => RECEIPT_MAGIC,
            Self::McpOauth => b"APMCP2R\0",
        }
    }
}
const INDEX_LIMIT: usize = 20;
const INDEX_HEADER: usize = 58;
const INDEX_ENTRY: usize = 115;
const RECEIPT_HEADER: usize = 8 + MANIFEST_SIZE + 33;
#[cfg(test)]
#[path = "drive_receipts_tests.rs"]
mod tests;
pub(super) type Context = Option<[u8; 32]>;
#[derive(Clone)]
struct Entry {
    version: Uuid,
    digest: [u8; 32],
    client: [u8; 32],
    context: Context,
    parts: usize,
    retired: bool,
}
struct Index {
    raw: Option<Secret>,
    revision: Uuid,
    entries: Vec<Entry>,
}
#[derive(Clone)]
pub(crate) struct Recovery {
    pub(crate) version: Uuid,
    pub(crate) revision: Uuid,
    pub(crate) state: &'static str,
    pub(crate) can_retire: bool,
    pub(crate) retirement_verified: bool,
    pub(crate) present_parts: usize,
    pub(crate) absent_parts: usize,
    pub(crate) other_pointer_preserved: bool,
    pointer: Option<[u8; 32]>,
    index: [u8; 32],
}
impl PartialEq for Recovery {
    fn eq(&self, b: &Self) -> bool {
        self.version == b.version
            && self.revision == b.revision
            && self.state == b.state
            && self.can_retire == b.can_retire
            && self.retirement_verified == b.retirement_verified
            && self.present_parts == b.present_parts
            && self.absent_parts == b.absent_parts
            && self.other_pointer_preserved == b.other_pointer_preserved
            && self.pointer == b.pointer
            && self.index == b.index
    }
}
impl Binding {
    pub(super) fn index(&self) -> String {
        format!("{}.ledger", self.main())
    }
    pub(super) fn receipt(&self, version: Uuid) -> String {
        format!("{}.r.{version}", self.main())
    }
}
fn context(bytes: &[u8]) -> Result<Context> {
    if bytes.len() != 33 {
        return Err("DRIVE_VAULT_RECEIPT_CONTEXT");
    }
    let hash: [u8; 32] = bytes[1..].try_into().unwrap();
    match bytes[0] {
        0 if hash == [0; 32] => Ok(None),
        1 if hash != [0; 32] => Ok(Some(hash)),
        _ => Err("DRIVE_VAULT_RECEIPT_CONTEXT"),
    }
}
fn put_context(bytes: &mut Vec<u8>, value: Context) {
    bytes.push(u8::from(value.is_some()));
    bytes.extend_from_slice(&value.unwrap_or([0; 32]));
}
fn uuid_bytes(bytes: &[u8]) -> Result<Uuid> {
    let v = Uuid::from_slice(bytes).map_err(|_| "DRIVE_VAULT_RECEIPT_FORMAT")?;
    if v.is_nil() {
        return Err("DRIVE_VAULT_RECEIPT_FORMAT");
    }
    Ok(v)
}
fn index<B: Backend>(backend: &mut B, binding: &Binding) -> Result<Index> {
    let Some(raw) = backend.read(&binding.index())? else {
        return Ok(Index {
            raw: None,
            revision: Uuid::new_v4(),
            entries: vec![],
        });
    };
    let b = raw.bytes();
    if b.len() < INDEX_HEADER + 32
        || b.len() > BLOB_LIMIT
        || &b[..8] != binding.domain.index_magic()
        || &b[8..24] != binding.connection.as_bytes()
        || &b[24..40] != binding.profile.as_bytes()
    {
        return Err("DRIVE_VAULT_RECEIPT_INDEX_FORMAT");
    }
    let revision = uuid_bytes(&b[40..56])?;
    let count = u16::from_le_bytes(b[56..58].try_into().unwrap()) as usize;
    if count > INDEX_LIMIT
        || b.len() != INDEX_HEADER + count * INDEX_ENTRY + 32
        || Sha256::digest(&b[..b.len() - 32])[..] != b[b.len() - 32..]
    {
        return Err("DRIVE_VAULT_RECEIPT_INDEX_INTEGRITY");
    }
    let mut entries = Vec::with_capacity(count);
    for p in b[INDEX_HEADER..b.len() - 32].chunks_exact(INDEX_ENTRY) {
        let version = uuid_bytes(&p[..16])?;
        let digest = p[16..48].try_into().unwrap();
        let client = p[48..80].try_into().unwrap();
        let context = context(&p[80..113])?;
        let parts = p[113] as usize;
        if client == [0; 32]
            || digest == [0; 32]
            || parts == 0
            || parts > LIMIT.div_ceil(PART_BYTES)
            || p[114] > 1
            || entries.iter().any(|e: &Entry| e.version == version)
        {
            return Err("DRIVE_VAULT_RECEIPT_INDEX_FORMAT");
        }
        entries.push(Entry {
            version,
            digest,
            client,
            context,
            parts,
            retired: p[114] == 1,
        });
    }
    if !matches(backend, &binding.index(), b)? {
        return Err("DRIVE_VAULT_RECEIPT_INDEX_CHANGED");
    }
    Ok(Index {
        raw: Some(raw),
        revision,
        entries,
    })
}
fn save<B: Backend>(
    backend: &mut B,
    binding: &Binding,
    old: &Index,
    entries: &[Entry],
) -> Result<()> {
    if entries.len() > INDEX_LIMIT {
        return Err("DRIVE_VAULT_RECEIPT_CAPACITY");
    }
    let current = backend.read(&binding.index())?;
    let unchanged = match (&current, &old.raw) {
        (None, None) => true,
        (Some(a), Some(b)) => a.bytes() == b.bytes(),
        _ => false,
    };
    if !unchanged {
        return Err("DRIVE_VAULT_RECEIPT_INDEX_CHANGED");
    }
    let mut b = Vec::with_capacity(INDEX_HEADER + entries.len() * INDEX_ENTRY + 32);
    b.extend_from_slice(binding.domain.index_magic());
    b.extend_from_slice(binding.connection.as_bytes());
    b.extend_from_slice(binding.profile.as_bytes());
    b.extend_from_slice(Uuid::new_v4().as_bytes());
    b.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    for e in entries {
        b.extend_from_slice(e.version.as_bytes());
        b.extend_from_slice(&e.digest);
        b.extend_from_slice(&e.client);
        put_context(&mut b, e.context);
        b.push(e.parts as u8);
        b.push(u8::from(e.retired));
    }
    let hash = Sha256::digest(&b);
    b.extend_from_slice(&hash);
    let secret = Secret(b);
    if backend.write(&binding.index(), secret.bytes()).is_err()
        || !matches(backend, &binding.index(), secret.bytes()).unwrap_or(false)
    {
        return Err("DRIVE_VAULT_RECEIPT_INDEX_WRITE_UNCERTAIN");
    }
    Ok(())
}
fn receipt(binding: &Binding, main: &Secret, parts: &[Secret], context: Context) -> Result<Secret> {
    let meta = decode(binding, main.bytes())?;
    if parts.len() != meta.parts {
        return Err("DRIVE_VAULT_RECEIPT_FORMAT");
    }
    let mut b = Vec::with_capacity(RECEIPT_HEADER + parts.len() * 32);
    b.extend_from_slice(binding.domain.receipt_magic());
    b.extend_from_slice(main.bytes());
    put_context(&mut b, context);
    for (i, p) in parts.iter().enumerate() {
        if p.bytes().len() != PART_HEADER + (meta.length - i * PART_BYTES).min(PART_BYTES)
            || &p.bytes()[..8] != binding.domain.part_magic()
            || p.bytes()[8..92] != main.bytes()[8..92]
            || p.bytes()[92..94] != (i as u16).to_le_bytes()
        {
            return Err("DRIVE_VAULT_RECEIPT_FORMAT");
        }
        b.extend_from_slice(&Sha256::digest(p.bytes()));
    }
    Ok(Secret(b))
}
pub(super) fn register<B: Backend>(
    backend: &mut B,
    binding: &Binding,
    main: &Secret,
    parts: &[Secret],
    context: Context,
) -> Result<()> {
    if context == Some([0; 32]) {
        return Err("DRIVE_VAULT_RECEIPT_CONTEXT");
    }
    let meta = decode(binding, main.bytes())?;
    let bytes = receipt(binding, main, parts, context)?;
    let entry = Entry {
        version: meta.generation,
        digest: Sha256::digest(bytes.bytes()).into(),
        client: binding.client,
        context,
        parts: meta.parts,
        retired: false,
    };
    let old = index(backend, binding)?;
    if let Some(existing) = old.entries.iter().find(|e| e.version == entry.version) {
        if existing.digest != entry.digest
            || existing.client != entry.client
            || existing.context != context
            || existing.retired
        {
            return Err("DRIVE_VAULT_RECEIPT_CHANGED");
        }
    } else {
        let mut entries = old.entries.clone();
        entries.push(entry.clone());
        save(backend, binding, &old, &entries)?;
    }
    // Index reservation precedes receipt and secret part writes. A missing
    // receipt remains attributable, but cannot bless any existing part bytes.
    if let Some(old) = backend.read(&binding.receipt(entry.version))? {
        if old.bytes() != bytes.bytes() {
            return Err("DRIVE_VAULT_RECEIPT_CHANGED");
        }
    } else if backend
        .write(&binding.receipt(entry.version), bytes.bytes())
        .is_err()
    {
        return Err("DRIVE_VAULT_RECEIPT_WRITE_UNCERTAIN");
    }
    if !matches(backend, &binding.receipt(entry.version), bytes.bytes())? {
        return Err("DRIVE_VAULT_RECEIPT_WRITE_UNCERTAIN");
    }
    Ok(())
}
fn pointer<B: Backend>(
    backend: &mut B,
    binding: &Binding,
    version: Uuid,
    expected: Option<&[u8]>,
) -> Result<(Option<Secret>, bool)> {
    let Some(main) = backend.read(&binding.main())? else {
        return Ok((None, true));
    };
    if expected.is_some_and(|v| main.bytes() == v) {
        return Ok((Some(main), true));
    }
    let b = main.bytes();
    if b.len() != MANIFEST_SIZE {
        return Ok((Some(main), false));
    }
    // Metadata only: a different valid generation can coexist with these old
    // exact targets. Its client is never adopted to read/delete token parts.
    let current = Binding {
        domain: binding.domain,
        connection: binding.connection,
        profile: binding.profile,
        client: b[56..88].try_into().unwrap(),
    };
    let independent =
        current.client != [0; 32] && decode(&current, b).is_ok_and(|m| m.generation != version);
    Ok((Some(main), independent))
}
struct Inspection {
    observation: Recovery,
    receipt: Option<Secret>,
    main: Option<Secret>,
    owned_main: bool,
    parts: Vec<(String, Secret)>,
}
fn inspect<B: Backend>(
    backend: &mut B,
    binding: &Binding,
    ledger: &Index,
    entry: &Entry,
) -> Result<Inspection> {
    let mut state = "reserved_without_receipt";
    let raw = backend.read(&binding.receipt(entry.version))?;
    let mut expected_main = None;
    let mut valid = true;
    if let Some(bytes) = &raw {
        let b = bytes.bytes();
        valid = b.len() == RECEIPT_HEADER + entry.parts * 32
            && &b[..8] == binding.domain.receipt_magic()
            && Sha256::digest(b)[..] == entry.digest;
        if valid {
            valid = decode(binding, &b[8..8 + MANIFEST_SIZE])
                .is_ok_and(|m| m.generation == entry.version && m.parts == entry.parts)
                && context(&b[8 + MANIFEST_SIZE..RECEIPT_HEADER])? == entry.context;
            if valid {
                expected_main = Some(&b[8..8 + MANIFEST_SIZE]);
            }
        }
        if !valid {
            state = "changed_receipt_preserved";
        }
    }
    let (main, independent) = pointer(backend, binding, entry.version, expected_main)?;
    let owned_main =
        expected_main.is_some_and(|expected| main.as_ref().is_some_and(|m| m.bytes() == expected));
    let mut parts = vec![];
    let mut absent = 0;
    if valid {
        // Without a receipt, prove absence throughout the entire closed part
        // keyspace. Existing bytes are preserved; filenames cannot prove them.
        let count = if raw.is_some() {
            entry.parts
        } else {
            LIMIT.div_ceil(PART_BYTES)
        };
        for i in 0..count {
            let target = binding.part(entry.version, i);
            match backend.read(&target)? {
                None => absent += 1,
                Some(p) => {
                    let matches = raw.as_ref().is_some_and(|r| {
                        let b = r.bytes();
                        let expected = &b[RECEIPT_HEADER + i * 32..RECEIPT_HEADER + (i + 1) * 32];
                        Sha256::digest(p.bytes())[..] == *expected
                    });
                    if !matches {
                        valid = false;
                        state = "changed_or_unproven_part_preserved";
                    }
                    parts.push((target, p));
                }
            }
        }
    }
    if !independent {
        valid = false;
        state = "changed_or_unproven_pointer_preserved";
    }
    if valid && raw.is_some() {
        state = if owned_main {
            "current_known_generation"
        } else if entry.retired && parts.is_empty() {
            "retired_known_generation"
        } else {
            "known_orphan_generation"
        };
    }
    let retirement_verified = valid && entry.retired && !owned_main && parts.is_empty();
    let observation = Recovery {
        version: entry.version,
        revision: ledger.revision,
        state,
        can_retire: valid,
        retirement_verified,
        present_parts: parts.len(),
        absent_parts: absent,
        other_pointer_preserved: main.is_some() && !owned_main,
        pointer: main.as_ref().map(|m| Sha256::digest(m.bytes()).into()),
        index: Sha256::digest(
            ledger
                .raw
                .as_ref()
                .ok_or("DRIVE_VAULT_RECEIPT_INDEX_CHANGED")?
                .bytes(),
        )
        .into(),
    };
    if !matches(
        backend,
        &binding.index(),
        ledger.raw.as_ref().unwrap().bytes(),
    )? {
        return Err("DRIVE_VAULT_RECEIPT_INDEX_CHANGED");
    }
    if let Some(r) = &raw {
        if !matches(backend, &binding.receipt(entry.version), r.bytes())? {
            return Err("DRIVE_VAULT_RECEIPT_CHANGED");
        }
    }
    Ok(Inspection {
        observation,
        receipt: raw,
        main,
        owned_main,
        parts,
    })
}
pub(super) fn inspect_all<B: Backend>(
    backend: &mut B,
    binding: &Binding,
    context: Context,
) -> Result<Vec<Recovery>> {
    let ledger = index(backend, binding)?;
    let mut out = vec![];
    for e in &ledger.entries {
        if e.client == binding.client && e.context == context {
            out.push(inspect(backend, binding, &ledger, e)?.observation);
        }
    }
    Ok(out)
}
// MCP lifecycle may retire predecessors with different reviewed client/scope
// hashes. Only this protected, bounded context inventory supplies their binding;
// public hashes, a credential alias or enumeration never supply ownership.
pub(super) fn mcp_context_inventory<B: Backend>(
    backend: &mut B,
    binding: &Binding,
    context: [u8; 32],
) -> Result<Vec<super::OwnedRecovery>> {
    if binding.domain != Domain::McpOauth || context == [0; 32] {
        return Err("MCP_VAULT_CONTEXT_REQUIRED");
    }
    let ledger = index(backend, binding)?;
    let mut out = vec![];
    for entry in &ledger.entries {
        if entry.context != Some(context) {
            continue;
        }
        let original = Binding {
            domain: binding.domain,
            connection: binding.connection,
            profile: binding.profile,
            client: entry.client,
        };
        let recovery = inspect(backend, &original, &ledger, entry)?.observation;
        out.push(super::OwnedRecovery {
            binding: original,
            recovery,
        });
    }
    Ok(out)
}
pub(super) fn compact_mcp_context<B: Backend>(
    backend: &mut B,
    binding: &Binding,
    context: [u8; 32],
) -> Result<()> {
    if binding.domain != Domain::McpOauth || context == [0; 32] {
        return Err("MCP_VAULT_CONTEXT_REQUIRED");
    }
    let mut ledger = index(backend, binding)?;
    for entry in ledger.entries.clone() {
        if !entry.retired || entry.context != Some(context) {
            continue;
        }
        let original = Binding {
            domain: binding.domain,
            connection: binding.connection,
            profile: binding.profile,
            client: entry.client,
        };
        let inspected = inspect(backend, &original, &ledger, &entry)?;
        if !inspected.observation.retirement_verified {
            continue;
        }
        if let Some(receipt) = inspected.receipt {
            if !retire(backend, &[(binding.receipt(entry.version), receipt)]) {
                return Err("MCP_VAULT_COMPACTION_UNCERTAIN");
            }
        }
        let entries = ledger
            .entries
            .iter()
            .filter(|e| e.version != entry.version)
            .cloned()
            .collect::<Vec<_>>();
        save(backend, binding, &ledger, &entries)?;
        ledger = index(backend, binding)?;
    }
    if ledger.entries.is_empty() {
        if let Some(raw) = ledger.raw {
            if !retire(backend, &[(binding.index(), raw)]) {
                return Err("MCP_VAULT_COMPACTION_UNCERTAIN");
            }
        }
    }
    Ok(())
}
pub(super) fn has_pending_other<B: Backend>(
    backend: &mut B,
    binding: &Binding,
    published: Uuid,
) -> Result<bool> {
    let ledger = index(backend, binding)?;
    for e in &ledger.entries {
        if e.version == published {
            continue;
        }
        if !e.retired {
            return Ok(true);
        }
        // As in compaction, positively retired protected metadata only
        // authorizes checking exact old targets. Never reconstruct a grant.
        let original = Binding {
            domain: binding.domain,
            connection: binding.connection,
            profile: binding.profile,
            client: e.client,
        };
        if !inspect(backend, &original, &ledger, e)?
            .observation
            .retirement_verified
        {
            return Ok(true);
        }
    }
    Ok(false)
}
pub(super) fn retire_known<B: Backend>(
    backend: &mut B,
    binding: &Binding,
    context: Context,
    expected: &Recovery,
) -> Result<bool> {
    let ledger = index(backend, binding)?;
    let entry = ledger
        .entries
        .iter()
        .find(|e| {
            e.version == expected.version && e.client == binding.client && e.context == context
        })
        .ok_or("DRIVE_VAULT_RECEIPT_NOT_FOUND")?;
    let fresh = inspect(backend, binding, &ledger, entry)?;
    if fresh.observation != *expected {
        return Err("DRIVE_VAULT_RECEIPT_OBSERVATION_CHANGED");
    }
    if !fresh.observation.can_retire {
        return Err("DRIVE_VAULT_RECEIPT_RECOVERY_DENIED");
    }
    if fresh.owned_main && !retire(backend, &[(binding.main(), fresh.main.unwrap())]) {
        return Ok(false);
    }
    // Preflight verified every existing part before any deletion. Recompare the
    // exact native bytes at each erase; uncertainty retains the private proof.
    if !retire(backend, &fresh.parts) {
        return Ok(false);
    }
    let mut entries = ledger.entries.clone();
    entries
        .iter_mut()
        .find(|e| e.version == expected.version)
        .unwrap()
        .retired = true;
    save(backend, binding, &ledger, &entries)?;
    // Keep the protected tombstone until a later verified compaction. A public
    // journal failure after erasure must not lose generation ownership proof.
    let _ = fresh.receipt;
    Ok(true)
}
pub(super) fn trim_retired<B: Backend>(backend: &mut B, binding: &Binding) -> Result<()> {
    let mut ledger = index(backend, binding)?;
    for entry in ledger.entries.clone() {
        if !entry.retired {
            continue;
        }
        // Only metadata of a protected positively retired generation is
        // compacted. No token/part entry is adopted through its client digest.
        let original = Binding {
            domain: binding.domain,
            connection: binding.connection,
            profile: binding.profile,
            client: entry.client,
        };
        let seen = inspect(backend, &original, &ledger, &entry)?;
        if !seen.observation.retirement_verified {
            continue;
        }
        if let Some(receipt) = seen.receipt {
            if !retire(backend, &[(binding.receipt(entry.version), receipt)]) {
                continue;
            }
        }
        let entries = ledger
            .entries
            .iter()
            .filter(|e| e.version != entry.version)
            .cloned()
            .collect::<Vec<_>>();
        save(backend, binding, &ledger, &entries)?;
        ledger = index(backend, binding)?;
    }
    if ledger.entries.is_empty() {
        if let Some(raw) = ledger.raw {
            if !retire(backend, &[(binding.index(), raw)]) {
                return Err("DRIVE_VAULT_RECEIPT_INDEX_ERASE_UNCERTAIN");
            }
        }
    }
    Ok(())
}

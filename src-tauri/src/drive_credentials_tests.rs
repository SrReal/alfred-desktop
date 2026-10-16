use super::*;
use std::collections::HashMap;

#[derive(Default)]
struct Memory {
    values: HashMap<String, Secret>,
    reads: Vec<String>,
    writes: usize,
    proof_writes: usize,
    fail_write: Option<usize>,
    write_then_fail: bool,
    fail_read_after_commit: bool,
    fail_next_read: bool,
    replace_part_at_commit: Option<String>,
    replace_main_before_commit: Option<String>,
    delete_failure: bool,
    replace_main_during_snapshot: bool,
    corrupt_next_part_write: bool,
}
impl Backend for Memory {
    fn read(&mut self, target: &str) -> Result<Option<Secret>> {
        self.reads.push(target.to_string());
        if self.replace_main_during_snapshot
            && self.reads.iter().filter(|s| *s == target).count() == 2
        {
            self.values
                .insert(target.into(), Secret::copy(b"owned changed snapshot"));
        }
        if self.fail_next_read {
            self.fail_next_read = false;
            return Err("OWNED_INJECTED_READ_FAILURE");
        }
        if let Some(main) = &self.replace_main_before_commit {
            // The old manifest was read twice during snapshot. The third main
            // read is the check after candidate parts have been verified.
            if target == main && self.reads.iter().filter(|s| *s == main).count() == 3 {
                self.values
                    .insert(target.into(), Secret::copy(b"owned outside replacement"));
            }
        }
        Ok(self.values.get(target).map(|v| Secret::copy(v.bytes())))
    }
    fn write(&mut self, target: &str, bytes: &[u8]) -> Result<()> {
        let secret_write = !target.ends_with(".ledger") && !target.contains(".r.");
        if secret_write {
            self.writes += 1;
        } else {
            self.proof_writes += 1;
        }
        assert!(!bytes.is_empty() && bytes.len() <= BLOB_LIMIT);
        let fail = secret_write && self.fail_write == Some(self.writes);
        if !fail || self.write_then_fail {
            self.values.insert(target.into(), Secret::copy(bytes));
        }
        if self.corrupt_next_part_write
            && [Domain::Drive, Domain::McpOauth]
                .iter()
                .any(|d| bytes.starts_with(d.part_magic()))
        {
            self.corrupt_next_part_write = false;
            self.values.get_mut(target).unwrap().0[PART_HEADER] ^= 1;
        }
        if [Domain::Drive, Domain::McpOauth]
            .iter()
            .any(|d| bytes.starts_with(d.manifest_magic()))
        {
            if let Some(part) = &self.replace_part_at_commit {
                self.values.insert(
                    part.clone(),
                    Secret::copy(b"owned outside part replacement"),
                );
            }
            if self.fail_read_after_commit {
                self.fail_next_read = true;
            }
        }
        if fail {
            return Err("OWNED_INJECTED_WRITE_FAILURE");
        }
        Ok(())
    }
    fn delete(&mut self, target: &str) -> Result<()> {
        if self.delete_failure {
            return Err("OWNED_INJECTED_DELETE_FAILURE");
        }
        self.values.remove(target);
        Ok(())
    }
}
fn secret_entries(memory: &Memory) -> usize {
    memory
        .values
        .keys()
        .filter(|target| !target.ends_with(".ledger") && !target.contains(".r."))
        .count()
}
fn binding() -> Binding {
    Binding::new(
        &Uuid::new_v4().to_string(),
        &Uuid::new_v4().to_string(),
        Sha256::digest(b"owned fictional client identity").into(),
    )
    .unwrap()
}
#[test]
fn drive_vault_owned_erasure_rejects_stale_version_and_preserves_changed_pointer() {
    let b = binding();
    let mut v = Vault {
        backend: Memory::default(),
    };
    let r = v.replace(&b, None, b"owned_private_session_key").unwrap();
    assert_eq!(
        v.erase_owned(&b, Uuid::new_v4()).unwrap_err(),
        "DRIVE_VAULT_VERSION_CHANGED"
    );
    assert!(v.read(&b).is_ok());
    v.backend.reads.clear();
    v.backend.replace_main_before_commit = Some(b.main());
    assert!(!v.erase_owned(&b, r.version).unwrap());
    assert_eq!(
        v.backend.values[&b.main()].bytes(),
        b"owned outside replacement"
    );
    assert!(v.backend.values.contains_key(&b.part(r.version, 0)));
    let mut v = Vault {
        backend: Memory::default(),
    };
    let r = v.replace(&b, None, b"owned_private_session_key").unwrap();
    assert!(v.erase_owned(&b, r.version).unwrap());
    assert!(secret_entries(&v.backend) == 0);
    let proof = v.recoveries(&b, None).unwrap();
    assert_eq!(proof.len(), 1);
    assert!(proof[0].retirement_verified);
    receipts::trim_retired(&mut v.backend, &b).unwrap();
    assert!(v.backend.values.is_empty());
}
fn payload(length: usize) -> Secret {
    let pattern = b"owned_fictional_token_and_private_session_";
    Secret((0..length).map(|i| pattern[i % pattern.len()]).collect())
}

#[cfg(windows)]
#[test]
fn drive_vault_native_namespace_denies_legacy_alias_and_noncanonical_targets() {
    native_namespace_denies_legacy_alias_and_noncanonical_targets(Domain::Drive);
}
#[cfg(windows)]
#[test]
fn mcp_vault_native_namespace_denies_legacy_alias_and_noncanonical_targets() {
    native_namespace_denies_legacy_alias_and_noncanonical_targets(Domain::McpOauth);
}
#[cfg(windows)]
fn native_namespace_denies_legacy_alias_and_noncanonical_targets(domain: Domain) {
    let original = binding();
    let b = Binding::for_domain(
        domain,
        &original.connection.to_string(),
        &original.profile.to_string(),
        original.client,
    )
    .unwrap();
    let mut native = win::Native;
    for bad in [
        format!("alfredpet.public.connection.{}", b.connection),
        b.main().to_ascii_uppercase(),
        format!("{}.v.{}.p.32", b.main(), Uuid::new_v4()),
        format!("{}.v.{}.p.0", b.main(), Uuid::new_v4()),
        format!("{}.v.{}.p.00.extra", b.main(), Uuid::new_v4()),
        format!("{}.v.{}.p.-1", b.main(), Uuid::new_v4()),
        "alfredpet.public.drive.credentials.00000000-0000-0000-0000-000000000000".into(),
        "alfredpet.public.drive.credentials.../another".into(),
    ] {
        assert!(native.read(&bad).is_err());
        assert!(native.write(&bad, b"owned fictional bytes").is_err());
        assert!(native.delete(&bad).is_err());
    }
}

#[test]
fn drive_vault_changed_snapshot_or_part_verification_never_publishes() {
    let b = binding();
    let mut v = Vault {
        backend: Memory::default(),
    };
    let version = v.replace(&b, None, b"owned old token").unwrap().version;
    v.backend.reads.clear();
    v.backend.replace_main_during_snapshot = true;
    assert_eq!(v.read(&b).unwrap_err(), "DRIVE_VAULT_CHANGED");

    let b = binding();
    let mut v = Vault {
        backend: Memory::default(),
    };
    let version2 = v.replace(&b, None, b"owned old token").unwrap().version;
    let old_main = Secret::copy(v.backend.values[&b.main()].bytes());
    v.backend.corrupt_next_part_write = true;
    assert_eq!(
        v.replace(&b, Some(version2), b"owned new token")
            .unwrap_err(),
        "DRIVE_VAULT_PART_VERIFY"
    );
    assert!(v.backend.values[&b.main()].bytes() == old_main.bytes());
    assert!(v.read(&b).unwrap().1.bytes() == b"owned old token");
    // Altered candidate bytes are ambiguous and deliberately retained.
    assert_eq!(secret_entries(&v.backend), 3);
    assert_ne!(version, version2);
}

#[test]
fn drive_vault_exact_capacity_binary_roundtrip_and_replacement() {
    let b = binding();
    let mut v = Vault {
        backend: Memory::default(),
    };
    for length in [1, PART_BYTES - 1, PART_BYTES, PART_BYTES + 1, LIMIT] {
        let expected = v.read(&b).ok().map(|r| r.0);
        let mut data = payload(length);
        if length > 2 {
            data.0[..3].copy_from_slice(&[0, 255, 128]);
        }
        let receipt = v.replace(&b, expected, data.bytes()).unwrap();
        assert!(receipt.cleanup_complete);
        let (version, readback) = v.read(&b).unwrap();
        assert_eq!(version, receipt.version);
        assert!(readback.bytes() == data.bytes());
        assert_eq!(secret_entries(&v.backend), 1 + length.div_ceil(PART_BYTES));
        assert!(v
            .backend
            .values
            .values()
            .all(|v| v.bytes().len() <= BLOB_LIMIT));
        assert!(!v.backend.values[&b.main()]
            .bytes()
            .windows(16)
            .any(|w| w == &data.bytes()[..data.bytes().len().min(16)]));
    }
    assert_eq!(format!("{:?}", payload(50)), "Secret(<redacted>)");
}

#[test]
fn drive_vault_uuid_limits_and_stale_version_fail_before_mutations() {
    let b = binding();
    let mut v = Vault {
        backend: Memory::default(),
    };
    assert_eq!(
        v.replace(&b, None, b"").unwrap_err(),
        "DRIVE_VAULT_PAYLOAD_LIMIT"
    );
    assert_eq!(
        v.replace(&b, None, payload(LIMIT + 1).bytes()).unwrap_err(),
        "DRIVE_VAULT_PAYLOAD_LIMIT"
    );
    assert!(v.backend.reads.is_empty() && v.backend.writes == 0);
    for bad in [
        "",
        "../other",
        "00000000-0000-0000-0000-000000000000",
        "11111111111141118111111111111111",
        "AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA",
    ] {
        assert!(Binding::new(bad, &b.profile.to_string(), b.client).is_err());
        assert!(Binding::new(&b.connection.to_string(), bad, b.client).is_err());
    }
    assert!(Binding::new(&b.connection.to_string(), &b.profile.to_string(), [0; 32]).is_err());
    assert_eq!(v.read(&b).unwrap_err(), "DRIVE_VAULT_NOT_FOUND");
    assert_eq!(
        v.replace(&b, Some(Uuid::new_v4()), b"owned").unwrap_err(),
        "DRIVE_VAULT_VERSION_CHANGED"
    );
    let version = v.replace(&b, None, b"owned").unwrap().version;
    let count = v.backend.writes;
    for expected in [None, Some(Uuid::new_v4())] {
        assert_eq!(
            v.replace(&b, expected, b"owned replacement").unwrap_err(),
            "DRIVE_VAULT_VERSION_CHANGED"
        );
        assert_eq!(v.backend.writes, count);
        assert_eq!(v.read(&b).unwrap().0, version);
    }
}

#[test]
fn drive_vault_binding_and_closed_manifest_deny_foreign_part_reads() {
    let b = binding();
    let mut v = Vault {
        backend: Memory::default(),
    };
    v.replace(&b, None, payload(PART_BYTES + 1).bytes())
        .unwrap();
    for change in 0..3 {
        let mut foreign = Binding {
            domain: b.domain,
            connection: b.connection,
            profile: b.profile,
            client: b.client,
        };
        if change == 0 {
            foreign.profile = Uuid::new_v4();
        } else if change == 1 {
            foreign.client[0] ^= 1;
        }
        let original = Secret::copy(v.backend.values[&b.main()].bytes());
        if change == 2 {
            v.backend.values.get_mut(&b.main()).unwrap().0[8] ^= 1;
        }
        v.backend.reads.clear();
        assert_eq!(v.read(&foreign).unwrap_err(), "DRIVE_VAULT_BINDING_CHANGED");
        assert_eq!(v.backend.reads, vec![b.main()]);
        v.backend.values.insert(b.main(), original);
    }
    let original = Secret::copy(v.backend.values[&b.main()].bytes());
    for bytes in [
        b"legacy arbitrary JSON".to_vec(),
        vec![0; MANIFEST_SIZE],
        {
            let mut copy = original.bytes().to_vec();
            copy[92..94].copy_from_slice(&u16::MAX.to_le_bytes());
            copy
        },
        {
            let mut copy = original.bytes().to_vec();
            copy[88..92].copy_from_slice(&((LIMIT + 1) as u32).to_le_bytes());
            copy
        },
        {
            let mut copy = original.bytes().to_vec();
            copy.push(0);
            copy
        },
    ] {
        v.backend.values.insert(b.main(), Secret(bytes));
        v.backend.reads.clear();
        let count = v.backend.writes;
        assert_eq!(v.read(&b).unwrap_err(), "DRIVE_VAULT_FORMAT");
        assert_eq!(
            v.replace(&b, None, b"owned").unwrap_err(),
            "DRIVE_VAULT_FORMAT"
        );
        assert!(v.backend.reads.iter().all(|s| s == &b.main()));
        assert_eq!(v.backend.writes, count);
    }
}

#[test]
fn drive_vault_explicit_reauthorization_observes_metadata_and_never_adopts_foreign_profile() {
    let b = binding();
    let mut v = Vault {
        backend: Memory::default(),
    };
    let old = v.replace(&b, None, b"owned old grant").unwrap().version;
    let new = Binding {
        domain: b.domain,
        connection: b.connection,
        profile: b.profile,
        client: Sha256::digest(b"owned different client").into(),
    };
    v.backend.reads.clear();
    let observation = observe(&mut v.backend, &new).unwrap().unwrap();
    assert_eq!(observation.version, old);
    assert!(v.backend.reads.iter().all(|s| s == &b.main()));
    assert_eq!(
        v.replace(&new, Some(old), b"owned new grant").unwrap_err(),
        "DRIVE_VAULT_BINDING_CHANGED"
    );
    let foreign = Binding {
        domain: b.domain,
        connection: b.connection,
        profile: Uuid::new_v4(),
        client: new.client,
    };
    assert_eq!(
        observe(&mut v.backend, &foreign).err(),
        Some("DRIVE_VAULT_BINDING_CHANGED")
    );
    let receipt = v
        .reauthorize(&new, Some(&observation), b"owned newly authorized grant")
        .unwrap();
    assert!(receipt.cleanup_complete);
    assert_eq!(v.read(&b).unwrap_err(), "DRIVE_VAULT_BINDING_CHANGED");
    assert!(v.read(&new).unwrap().1.bytes() == b"owned newly authorized grant");
    assert_eq!(
        v.reauthorize(&new, Some(&observation), b"owned stale replay")
            .unwrap_err(),
        "DRIVE_VAULT_BINDING_CHANGED"
    );
    assert_eq!(secret_entries(&v.backend), 2);
}

#[test]
fn drive_vault_missing_reordered_modified_parts_and_digest_fail_closed() {
    let b = binding();
    let mut v = Vault {
        backend: Memory::default(),
    };
    let generation = v
        .replace(&b, None, payload(PART_BYTES + 1).bytes())
        .unwrap()
        .version;
    let target = b.part(generation, 0);
    let original = v.backend.values.remove(&target).unwrap();
    assert_eq!(v.read(&b).unwrap_err(), "DRIVE_VAULT_PART_MISSING");
    for position in [0, 8, 24, 40, 56, 88, 92] {
        let mut changed = Secret::copy(original.bytes());
        changed.0[position] ^= 1;
        v.backend.values.insert(target.clone(), changed);
        assert_eq!(v.read(&b).unwrap_err(), "DRIVE_VAULT_PART_INVALID");
    }
    let mut changed = Secret::copy(original.bytes());
    *changed.0.last_mut().unwrap() ^= 1;
    v.backend.values.insert(target.clone(), changed);
    assert_eq!(v.read(&b).unwrap_err(), "DRIVE_VAULT_INTEGRITY");
    v.backend.values.insert(target, original);
    assert!(v.read(&b).is_ok());
}

#[test]
fn drive_vault_partial_write_failure_keeps_old_pointer_and_cleans_exact_candidates() {
    for write_then_fail in [false, true] {
        let b = binding();
        let mut v = Vault {
            backend: Memory::default(),
        };
        let old = payload(PART_BYTES + 3);
        let generation = v.replace(&b, None, old.bytes()).unwrap().version;
        let before = Secret::copy(v.backend.values[&b.main()].bytes());
        v.backend.fail_write = Some(v.backend.writes + 2);
        v.backend.write_then_fail = write_then_fail;
        assert_eq!(
            v.replace(&b, Some(generation), payload(PART_BYTES * 4).bytes())
                .unwrap_err(),
            "OWNED_INJECTED_WRITE_FAILURE"
        );
        assert!(v.backend.values[&b.main()].bytes() == before.bytes());
        assert!(v.read(&b).unwrap().1.bytes() == old.bytes());
        assert_eq!(secret_entries(&v.backend), 3);
    }
}

#[test]
fn drive_vault_uncertain_manifest_save_preserves_both_possible_generations() {
    for after_write in [false, true] {
        let b = binding();
        let mut v = Vault {
            backend: Memory::default(),
        };
        let old = payload(3);
        let new = payload(PART_BYTES + 1);
        let version = v.replace(&b, None, old.bytes()).unwrap().version;
        v.backend.fail_write = Some(v.backend.writes + 3);
        v.backend.write_then_fail = after_write;
        assert_eq!(
            v.replace(&b, Some(version), new.bytes()).unwrap_err(),
            "DRIVE_VAULT_COMMIT_UNCERTAIN"
        );
        assert_eq!(secret_entries(&v.backend), 4);
        assert!(v.backend.values.contains_key(&b.part(version, 0)));
        let readback = v.read(&b).unwrap();
        assert!(
            readback.1.bytes()
                == if after_write {
                    new.bytes()
                } else {
                    old.bytes()
                }
        );
    }
    let b = binding();
    let mut v = Vault {
        backend: Memory::default(),
    };
    v.backend.fail_read_after_commit = true;
    assert_eq!(
        v.replace(&b, None, b"owned").unwrap_err(),
        "DRIVE_VAULT_COMMIT_UNCERTAIN"
    );
    assert_eq!(secret_entries(&v.backend), 2);
    assert!(v.read(&b).unwrap().1.bytes() == b"owned");
}

#[test]
fn drive_vault_outside_pointer_or_part_replacements_are_preserved() {
    let b = binding();
    let mut v = Vault {
        backend: Memory::default(),
    };
    let version = v.replace(&b, None, b"owned old token").unwrap().version;
    v.backend.replace_main_before_commit = Some(b.main());
    v.backend.reads.clear();
    assert_eq!(
        v.replace(&b, Some(version), b"owned new token")
            .unwrap_err(),
        "DRIVE_VAULT_CHANGED"
    );
    assert!(v.backend.values[&b.main()].bytes() == b"owned outside replacement");
    assert_eq!(secret_entries(&v.backend), 2);
    let b = binding();
    let mut v = Vault {
        backend: Memory::default(),
    };
    let version = v.replace(&b, None, b"owned old token").unwrap().version;
    let outside = b.part(version, 0);
    v.backend.replace_part_at_commit = Some(outside.clone());
    let receipt = v.replace(&b, Some(version), b"owned new token").unwrap();
    assert!(!receipt.cleanup_complete);
    assert!(v.backend.values[&outside].bytes() == b"owned outside part replacement");
    assert!(v.read(&b).unwrap().1.bytes() == b"owned new token");
}

#[test]
fn drive_vault_cleanup_failure_does_not_replay_a_committed_generation() {
    let b = binding();
    let mut v = Vault {
        backend: Memory::default(),
    };
    let version = v.replace(&b, None, b"owned old token").unwrap().version;
    v.backend.delete_failure = true;
    let receipt = v.replace(&b, Some(version), b"owned new token").unwrap();
    assert!(!receipt.cleanup_complete);
    assert_eq!(secret_entries(&v.backend), 3);
    assert_eq!(v.read(&b).unwrap().0, receipt.version);
    let writes = v.backend.writes;
    assert_eq!(
        v.replace(&b, Some(version), b"owned replay").unwrap_err(),
        "DRIVE_VAULT_VERSION_CHANGED"
    );
    assert_eq!(v.backend.writes, writes);
}

#[cfg(windows)]
#[test]
fn drive_vault_owned_native_capacity_large_binary_rotation_and_exact_cleanup() {
    owned_native_capacity_large_binary_rotation_and_exact_cleanup(Domain::Drive);
}
#[cfg(windows)]
#[test]
fn mcp_vault_owned_native_capacity_large_binary_rotation_and_exact_cleanup() {
    owned_native_capacity_large_binary_rotation_and_exact_cleanup(Domain::McpOauth);
}
#[cfg(windows)]
fn owned_native_capacity_large_binary_rotation_and_exact_cleanup(domain: Domain) {
    use windows::{
        core::PWSTR,
        Win32::Security::Credentials::{
            CredWriteW, CREDENTIALW, CRED_MAX_CREDENTIAL_BLOB_SIZE, CRED_PERSIST_LOCAL_MACHINE,
            CRED_TYPE_GENERIC,
        },
    };
    struct Owned {
        native: win::Native,
        writes: HashMap<String, Secret>,
    }
    impl Backend for Owned {
        fn read(&mut self, target: &str) -> Result<Option<Secret>> {
            self.native.read(target)
        }
        fn write(&mut self, target: &str, bytes: &[u8]) -> Result<()> {
            if !self.writes.contains_key(target) {
                if self.native.read(target)?.is_some() {
                    return Err("OWNED_NATIVE_COLLISION_PRESERVED");
                }
            } else if !matches(&mut self.native, target, self.writes[target].bytes())? {
                return Err("OWNED_NATIVE_REPLACEMENT_PRESERVED");
            }
            self.writes.insert(target.into(), Secret::copy(bytes));
            self.native.write(target, bytes)
        }
        fn delete(&mut self, target: &str) -> Result<()> {
            self.native.delete(target)
        }
    }
    impl Owned {
        fn clean(&mut self) -> bool {
            let writes: Vec<_> = self
                .writes
                .iter()
                .map(|(t, s)| (t.clone(), Secret::copy(s.bytes())))
                .collect();
            retire(&mut self.native, &writes)
        }
    }
    impl Drop for Owned {
        fn drop(&mut self) {
            let _ = self.clean();
        }
    }
    assert_eq!(BLOB_LIMIT, CRED_MAX_CREDENTIAL_BLOB_SIZE as usize);
    let original = binding();
    let b = Binding::for_domain(
        domain,
        &original.connection.to_string(),
        &original.profile.to_string(),
        original.client,
    )
    .unwrap();
    let mut v = Vault {
        backend: Owned {
            native: win::Native,
            writes: HashMap::new(),
        },
    };
    assert!(v.backend.read(&b.main()).unwrap().is_none());
    let boundary = b.part(Uuid::new_v4(), 0);
    let capacity = payload(BLOB_LIMIT);
    v.backend.write(&boundary, capacity.bytes()).unwrap();
    assert!(v.backend.read(&boundary).unwrap().unwrap().bytes() == capacity.bytes());
    assert_eq!(
        v.backend
            .native
            .write(&boundary, payload(BLOB_LIMIT + 1).bytes())
            .unwrap_err(),
        "DRIVE_VAULT_BLOB_LIMIT"
    );
    let mut name: Vec<u16> = boundary.encode_utf16().chain(Some(0)).collect();
    let mut over = payload(BLOB_LIMIT + 1);
    let raw = CREDENTIALW {
        Type: CRED_TYPE_GENERIC,
        TargetName: PWSTR(name.as_mut_ptr()),
        CredentialBlobSize: over.0.len() as u32,
        CredentialBlob: over.0.as_mut_ptr(),
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        ..Default::default()
    };
    // Exercise the real Win32 boundary, not only the wrapper's input validator.
    assert!(unsafe { CredWriteW(&raw, 0) }.is_err());
    assert!(v.backend.read(&boundary).unwrap().unwrap().bytes() == capacity.bytes());
    let long = payload(if domain == Domain::McpOauth {
        LIMIT
    } else {
        12 * 1024 + 13
    });
    let first = v.replace(&b, None, long.bytes()).unwrap();
    assert!(v.read(&b).unwrap().1.bytes() == long.bytes());
    let second = v
        .replace(&b, Some(first.version), b"owned rotated access and refresh")
        .unwrap();
    assert!(second.cleanup_complete);
    assert!(v.read(&b).unwrap().1.bytes() == b"owned rotated access and refresh");
    for i in 0..long.bytes().len().div_ceil(PART_BYTES) {
        assert!(v
            .backend
            .native
            .read(&b.part(first.version, i))
            .unwrap()
            .is_none());
    }
    let next_client = Binding::for_domain(
        domain,
        &b.connection.to_string(),
        &b.profile.to_string(),
        [81; 32],
    )
    .unwrap();
    let observed = observe(&mut v.backend, &next_client).unwrap().unwrap();
    assert_eq!(observed.version, second.version);
    let authorized = v
        .reauthorize(
            &next_client,
            Some(&observed),
            b"owned newly authorized private envelope",
        )
        .unwrap();
    assert!(authorized.cleanup_complete);
    assert_eq!(v.read(&b).err().unwrap(), "DRIVE_VAULT_BINDING_CHANGED");
    assert!(v.read(&next_client).unwrap().1.bytes() == b"owned newly authorized private envelope");
    assert!(v.backend.clean());
    for target in v.backend.writes.keys() {
        assert!(v.backend.native.read(target).unwrap().is_none());
    }
}

#[path = "mcp_oauth_vault_tests.rs"]
mod mcp_oauth_vault_tests;

//! Private vault primitives only. These fixtures do not prove an OAuth actor,
//! issuer/client/grant envelope validation, or token exchange/refresh behavior.
use super::*;

fn mcp(b: &Binding) -> Binding {
    Binding::for_domain(
        Domain::McpOauth,
        &b.connection.to_string(),
        &b.profile.to_string(),
        b.client,
    )
    .unwrap()
}
fn ordinary() -> Vault<Memory> {
    Vault {
        backend: Memory::default(),
    }
}
#[test]
fn drive_bytes_and_targets_retain_the_existing_binary_contract() {
    let b = binding();
    let generation = Uuid::new_v4();
    let main = manifest(&b, generation, b"owned fictional bytes");
    let p = part(&b, &main, 0, b"owned fictional bytes");
    assert_eq!(
        b.main(),
        format!("alfredpet.drive.credentials.{}", b.connection)
    );
    assert_eq!(&main.bytes()[..8], b"APDRV1M\0");
    assert_eq!(&p.bytes()[..8], b"APDRV1P\0");
    assert_eq!(&main.bytes()[8..24], b.connection.as_bytes());
    assert_eq!(&main.bytes()[24..40], b.profile.as_bytes());
    assert_eq!(&main.bytes()[40..56], generation.as_bytes());
    assert_eq!(&main.bytes()[56..88], &b.client);
    assert_eq!(main.bytes().len(), 126);
    assert_eq!(p.bytes().len(), 94 + 21);
    assert_eq!(b.domain.index_magic(), b"APDRV2I\0");
    assert_eq!(b.domain.receipt_magic(), b"APDRV2R\0");
}
#[test]
fn identical_profile_connection_and_client_have_disjoint_domains_and_generations() {
    let drive = binding();
    let oauth = mcp(&drive);
    let mut v = ordinary();
    let binary = payload(LIMIT);
    let d = v.replace(&drive, None, binary.bytes()).unwrap();
    let m = v.replace(&oauth, None, binary.bytes()).unwrap();
    assert_ne!(drive.main(), oauth.main());
    assert_ne!(d.version, m.version);
    assert!(v.read(&drive).unwrap().1.bytes() == binary.bytes());
    assert!(v.read(&oauth).unwrap().1.bytes() == binary.bytes());
    assert_eq!(&v.backend.values[&oauth.main()].bytes()[..8], b"APMCP1M\0");
    assert_eq!(
        &v.backend.values[&oauth.part(m.version, 0)].bytes()[..8],
        b"APMCP1P\0"
    );
    assert_eq!(&v.backend.values[&oauth.index()].bytes()[..8], b"APMCP2I\0");
    assert_eq!(
        &v.backend.values[&oauth.receipt(m.version)].bytes()[..8],
        b"APMCP2R\0"
    );
    assert!(v.erase_owned(&oauth, m.version).unwrap());
    receipts::trim_retired(&mut v.backend, &oauth).unwrap();
    assert!(v
        .backend
        .values
        .keys()
        .all(|t| t.starts_with(Domain::Drive.prefix())));
    assert!(v.read(&drive).unwrap().1.bytes() == binary.bytes());
}
#[test]
fn copied_manifest_and_ledger_fail_before_foreign_part_reads_or_mutations() {
    for (from, to) in [
        (Domain::Drive, Domain::McpOauth),
        (Domain::McpOauth, Domain::Drive),
    ] {
        let seed = binding();
        let a = Binding {
            domain: from,
            ..seed
        };
        let b = Binding { domain: to, ..a };
        let mut v = ordinary();
        v.replace(&a, None, b"owned fictional private envelope")
            .unwrap();
        for key in [a.main(), a.index()] {
            let bytes = Secret::copy(v.backend.values[&key].bytes());
            let target = if key == a.main() { b.main() } else { b.index() };
            v.backend.values.insert(target, bytes);
        }
        v.backend.reads.clear();
        let writes = v.backend.writes + v.backend.proof_writes;
        assert_eq!(v.read(&b).err().unwrap(), "DRIVE_VAULT_FORMAT");
        assert_eq!(
            observe(&mut v.backend, &b).err().unwrap(),
            "DRIVE_VAULT_FORMAT"
        );
        assert_eq!(
            v.recoveries(&b, None).err().unwrap(),
            "DRIVE_VAULT_RECEIPT_INDEX_FORMAT"
        );
        assert!(v
            .backend
            .reads
            .iter()
            .all(|t| t == &b.main() || t == &b.index()));
        assert_eq!(v.backend.writes + v.backend.proof_writes, writes);
    }
}
#[test]
fn copied_foreign_part_is_rejected_even_with_identical_generation_and_payload() {
    let drive = binding();
    let oauth = mcp(&drive);
    let mut v = ordinary();
    let r = v.replace(&oauth, None, b"owned fictional payload").unwrap();
    let foreign = manifest(&drive, r.version, b"owned fictional payload");
    let foreign_part = part(&drive, &foreign, 0, b"owned fictional payload");
    v.backend
        .values
        .insert(oauth.part(r.version, 0), foreign_part);
    assert_eq!(v.read(&oauth).err().unwrap(), "DRIVE_VAULT_PART_INVALID");
    let seen = v.recoveries(&oauth, None).unwrap().remove(0);
    assert!(!seen.can_retire);
    assert_eq!(
        v.recover(&oauth, None, &seen).unwrap_err(),
        "DRIVE_VAULT_RECEIPT_RECOVERY_DENIED"
    );
    assert!(v.backend.values.contains_key(&oauth.main()));
}
#[test]
fn observation_cannot_cross_domain_connection_or_profile_before_writing() {
    let drive = binding();
    let oauth = mcp(&drive);
    let mut v = ordinary();
    v.replace(&drive, None, b"owned old envelope").unwrap();
    let seen = observe(&mut v.backend, &drive).unwrap().unwrap();
    let foreign_connection = Binding {
        connection: Uuid::new_v4(),
        ..drive
    };
    let foreign_profile = Binding {
        profile: Uuid::new_v4(),
        ..drive
    };
    for foreign in [&oauth, &foreign_connection, &foreign_profile] {
        let writes = v.backend.writes + v.backend.proof_writes;
        assert!(!foreign.matches_client(&seen));
        assert_eq!(
            v.reauthorize(foreign, Some(&seen), b"owned new envelope")
                .unwrap_err(),
            "DRIVE_VAULT_OBSERVATION_BINDING"
        );
        assert_eq!(v.backend.writes + v.backend.proof_writes, writes);
    }
}
#[test]
fn mcp_profile_and_client_binding_fail_before_private_parts_are_read() {
    let seed = binding();
    let b = mcp(&seed);
    let mut v = ordinary();
    v.replace(&b, None, b"owned fictional envelope").unwrap();
    for other in [
        Binding {
            profile: Uuid::new_v4(),
            ..b
        },
        Binding {
            client: [81; 32],
            ..b
        },
    ] {
        v.backend.reads.clear();
        assert_eq!(v.read(&other).err().unwrap(), "DRIVE_VAULT_BINDING_CHANGED");
        assert_eq!(v.backend.reads, vec![b.main()]);
    }
}
#[test]
fn mcp_reauthorization_is_explicit_and_metadata_observation_does_not_read_tokens() {
    let seed = binding();
    let b = mcp(&seed);
    let mut v = ordinary();
    let first = v.replace(&b, None, payload(12001).bytes()).unwrap();
    let next = Binding {
        client: [92; 32],
        ..b
    };
    v.backend.reads.clear();
    let seen = observe(&mut v.backend, &next).unwrap().unwrap();
    assert_eq!(seen.version, first.version);
    assert!(!next.matches_client(&seen));
    assert!(v.backend.reads.iter().all(|t| t == &b.main()));
    let r = v
        .reauthorize(&next, Some(&seen), b"owned new public-client envelope")
        .unwrap();
    assert!(r.cleanup_complete);
    assert_eq!(v.read(&b).err().unwrap(), "DRIVE_VAULT_BINDING_CHANGED");
    assert!(v.read(&next).unwrap().1.bytes() == b"owned new public-client envelope");
}
#[test]
fn mcp_capacity_and_stale_expected_version_fail_before_any_write() {
    let seed = binding();
    let b = mcp(&seed);
    let mut v = ordinary();
    for invalid in [Secret::copy(b""), payload(LIMIT + 1)] {
        assert_eq!(
            v.replace(&b, None, invalid.bytes()).unwrap_err(),
            "DRIVE_VAULT_PAYLOAD_LIMIT"
        );
        assert_eq!(v.backend.writes + v.backend.proof_writes, 0);
    }
    let r = v
        .replace(&b, None, b"owned current private envelope")
        .unwrap();
    let writes = v.backend.writes + v.backend.proof_writes;
    assert_eq!(
        v.replace(&b, Some(Uuid::new_v4()), b"owned stale")
            .unwrap_err(),
        "DRIVE_VAULT_VERSION_CHANGED"
    );
    assert_eq!(v.backend.writes + v.backend.proof_writes, writes);
    assert_eq!(v.read(&b).unwrap().0, r.version);
}
#[test]
fn mcp_uncertain_pointer_publication_keeps_private_proof_and_never_replays() {
    for after in [false, true] {
        let seed = binding();
        let b = mcp(&seed);
        let mut v = ordinary();
        let first = v.replace(&b, None, b"owned initial envelope").unwrap();
        v.backend.fail_write = Some(v.backend.writes + 3);
        v.backend.write_then_fail = after;
        assert_eq!(
            v.replace(&b, Some(first.version), payload(PART_BYTES + 1).bytes())
                .unwrap_err(),
            "DRIVE_VAULT_COMMIT_UNCERTAIN"
        );
        let seen = v.recoveries(&b, None).unwrap();
        assert_eq!(seen.len(), 2);
        assert!(seen.iter().all(|r| r.can_retire));
        v.backend.fail_write = None;
        let current = v.read(&b).unwrap().0;
        assert_eq!(current == first.version, !after);
        let obsolete = seen.iter().find(|r| r.version != current).unwrap();
        assert!(v.recover(&b, None, obsolete).unwrap());
        assert_eq!(v.read(&b).unwrap().0, current);
    }
}
#[test]
fn mcp_changed_pointer_readback_and_parts_preserve_uncertainty() {
    let seed = binding();
    let b = mcp(&seed);
    let mut v = ordinary();
    v.backend.fail_read_after_commit = true;
    assert_eq!(
        v.replace(&b, None, b"owned envelope").unwrap_err(),
        "DRIVE_VAULT_COMMIT_UNCERTAIN"
    );
    let seen = v.recoveries(&b, None).unwrap().remove(0);
    assert!(seen.can_retire);
    v.backend.values.insert(
        b.part(seen.version, 0),
        Secret::copy(b"owned unexpected outside bytes"),
    );
    let changed = v.recoveries(&b, None).unwrap().remove(0);
    assert!(!changed.can_retire);
    assert_eq!(
        v.recover(&b, None, &seen).unwrap_err(),
        "DRIVE_VAULT_RECEIPT_OBSERVATION_CHANGED"
    );
    assert!(v.backend.values.contains_key(&b.main()));
}

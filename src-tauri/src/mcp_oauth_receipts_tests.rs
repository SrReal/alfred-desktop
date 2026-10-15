use super::*;

#[test]
fn mcp_context_inventory_and_compaction_preserve_foreign_root_context_and_current_pointer() {
    let a = oauth_binding();
    let context = [43; 32];
    let foreign_context = [59; 32];
    let mut v = ordinary();
    let old = v
        .replace_context(&a, None, b"owned old scope packet", context)
        .unwrap();
    v.backend.values.remove(&a.main());
    let b = Binding {
        domain: a.domain,
        connection: a.connection,
        profile: a.profile,
        client: [77; 32],
    };
    let newer = v
        .replace_context(&b, None, b"owned newer scope packet", context)
        .unwrap();
    v.backend.values.remove(&b.main());
    let foreign = Binding {
        domain: a.domain,
        connection: a.connection,
        profile: a.profile,
        client: [89; 32],
    };
    let active = v
        .replace_context(
            &foreign,
            None,
            b"owned fixture for another root context",
            foreign_context,
        )
        .unwrap();
    let pointer = Secret::copy(v.backend.values[&foreign.main()].bytes());
    let inventory = mcp_context_inventory(&mut v.backend, &b, context).unwrap();
    assert_eq!(inventory.len(), 2);
    assert!(inventory.iter().any(|r| r.recovery.version == old.version));
    assert!(inventory
        .iter()
        .any(|r| r.recovery.version == newer.version));
    for item in inventory {
        let fresh = mcp_context_inventory(&mut v.backend, &b, context)
            .unwrap()
            .into_iter()
            .find(|r| r.recovery.version == item.recovery.version)
            .unwrap();
        assert!(v
            .recover(&fresh.binding, Some(context), &fresh.recovery)
            .unwrap());
    }
    compact_mcp_context(&mut v.backend, &b, context).unwrap();
    assert!(mcp_context_inventory(&mut v.backend, &b, context)
        .unwrap()
        .is_empty());
    assert_eq!(v.backend.values[&foreign.main()].bytes(), pointer.bytes());
    assert!(v
        .backend
        .values
        .contains_key(&foreign.part(active.version, 0)));
    assert!(v
        .backend
        .values
        .contains_key(&foreign.receipt(active.version)));
    assert_eq!(
        v.recoveries(&foreign, Some(foreign_context)).unwrap().len(),
        1
    );
    assert!(mcp_context_inventory(&mut v.backend, &binding(), context).is_err());
    assert!(mcp_context_inventory(&mut v.backend, &b, [0; 32]).is_err());
}
#[test]
fn mcp_context_inventory_never_converts_changed_parts_into_erase_authority() {
    let b = oauth_binding();
    let mut v = ordinary();
    let context = [71; 32];
    let packet = v
        .replace_context(&b, None, b"owned packet", context)
        .unwrap();
    v.backend.values.remove(&b.main());
    v.backend.values.insert(
        b.part(packet.version, 0),
        Secret::copy(b"owned fixture changed outside publication"),
    );
    let entry = mcp_context_inventory(&mut v.backend, &b, context)
        .unwrap()
        .remove(0);
    assert!(!entry.recovery.can_retire);
    assert_eq!(
        v.recover(&entry.binding, Some(context), &entry.recovery)
            .unwrap_err(),
        "DRIVE_VAULT_RECEIPT_RECOVERY_DENIED"
    );
    compact_mcp_context(&mut v.backend, &b, context).unwrap();
    assert_eq!(
        v.backend.values[&b.part(packet.version, 0)].bytes(),
        b"owned fixture changed outside publication"
    );
}

fn oauth_binding() -> Binding {
    let b = binding();
    Binding::for_domain(
        Domain::McpOauth,
        &b.connection.to_string(),
        &b.profile.to_string(),
        b.client,
    )
    .unwrap()
}
#[test]
fn mcp_all_six_simulated_publication_interruptions_keep_recoverable_private_proof() {
    for boundary in 1..=6 {
        let b = oauth_binding();
        let mut v = ordinary();
        v.backend.stop_write = Some(boundary);
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            v.replace_context(&b, None, &vec![37; 3 * PART_BYTES - 1], context())
        }))
        .is_err());
        v.backend.stop_write = None;
        let r = seen(&mut v, &b);
        assert!(r.can_retire);
        assert_eq!(
            r.present_parts,
            if boundary <= 2 {
                0
            } else {
                (boundary - 2).min(3)
            }
        );
        assert!(v.recover(&b, Some(context()), &r).unwrap());
        assert!(seen(&mut v, &b).retirement_verified);
        trim_retired(&mut v.backend, &b).unwrap();
        assert!(v.backend.values.is_empty());
    }
}
#[test]
fn mcp_foreign_receipt_magic_is_rejected_even_with_a_valid_matching_ledger_digest() {
    let b = oauth_binding();
    let mut v = ordinary();
    let created = v
        .replace_context(&b, None, b"owned private envelope", context())
        .unwrap();
    let mut foreign = Secret::copy(v.backend.values[&b.receipt(created.version)].bytes());
    foreign.0[..8].copy_from_slice(RECEIPT_MAGIC);
    let ledger = index(&mut v.backend, &b).unwrap();
    let mut entries = ledger.entries.clone();
    entries[0].digest = Sha256::digest(foreign.bytes()).into();
    v.backend.values.insert(b.receipt(created.version), foreign);
    save(&mut v.backend, &b, &ledger, &entries).unwrap();
    let r = seen(&mut v, &b);
    assert!(!r.can_retire);
    assert_eq!(r.state, "changed_or_unproven_pointer_preserved");
    assert_eq!(
        v.recover(&b, Some(context()), &r).unwrap_err(),
        "DRIVE_VAULT_RECEIPT_RECOVERY_DENIED"
    );
    assert!(v.backend.values.contains_key(&b.main()));
    assert!(v.backend.values.contains_key(&b.part(created.version, 0)));
}
#[test]
fn mcp_missing_pointer_can_retire_only_context_bound_known_generation() {
    let b = oauth_binding();
    let mut v = ordinary();
    let r = v
        .replace_context(&b, None, &vec![57; PART_BYTES + 7], context())
        .unwrap();
    v.backend.values.remove(&b.main());
    v.backend.values.remove(&b.part(r.version, 1));
    assert!(v.recoveries(&b, None).unwrap().is_empty());
    assert!(v.recoveries(&b, Some([91; 32])).unwrap().is_empty());
    let seen = seen(&mut v, &b);
    assert_eq!(seen.state, "known_orphan_generation");
    assert_eq!(seen.present_parts, 1);
    assert_eq!(seen.absent_parts, 1);
    assert_eq!(
        v.recover(&b, Some([91; 32]), &seen).unwrap_err(),
        "DRIVE_VAULT_RECEIPT_NOT_FOUND"
    );
    assert!(v.recover(&b, Some(context()), &seen).unwrap());
    trim_retired(&mut v.backend, &b).unwrap();
    assert!(v.backend.values.is_empty());
}
#[test]
fn mcp_retirement_preserves_changed_private_part_and_rejects_stale_observation() {
    let b = oauth_binding();
    let mut v = ordinary();
    let r = v
        .replace_context(&b, None, b"owned private envelope", context())
        .unwrap();
    let original = seen(&mut v, &b);
    v.backend.values.insert(
        b.part(r.version, 0),
        Secret::copy(b"owned unexpected outside replacement"),
    );
    assert_eq!(
        v.recover(&b, Some(context()), &original).unwrap_err(),
        "DRIVE_VAULT_RECEIPT_OBSERVATION_CHANGED"
    );
    let changed = seen(&mut v, &b);
    assert!(!changed.can_retire);
    assert_eq!(
        v.recover(&b, Some(context()), &changed).unwrap_err(),
        "DRIVE_VAULT_RECEIPT_RECOVERY_DENIED"
    );
    assert!(v.backend.values.contains_key(&b.main()));
    assert!(
        v.backend.values[&b.part(r.version, 0)].bytes() == b"owned unexpected outside replacement"
    );
}
#[test]
fn mcp_failed_private_proof_write_never_starts_secret_part_publication() {
    for step in [1, 2] {
        for after in [false, true] {
            let b = oauth_binding();
            let mut v = ordinary();
            v.backend.fail_write = Some(step);
            v.backend.write_then_fail = after;
            assert!(v
                .replace_context(&b, None, b"owned envelope", context())
                .is_err());
            assert!(v
                .backend
                .values
                .keys()
                .all(|t| t.ends_with(".ledger") || t.contains(".r.")));
            v.backend.fail_write = None;
            for r in v.recoveries(&b, Some(context())).unwrap() {
                assert!(r.can_retire);
                assert!(v.recover(&b, Some(context()), &r).unwrap());
            }
        }
    }
}

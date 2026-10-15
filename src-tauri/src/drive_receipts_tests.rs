use super::*;
use std::collections::HashMap;
#[path = "mcp_oauth_receipts_tests.rs"]
mod mcp_oauth_receipts_tests;
#[cfg(windows)]
#[path = "drive_receipts_native_tests.rs"]
mod native;
#[derive(Default)]
struct Interrupted {
    values: HashMap<String, Secret>,
    reads: Vec<String>,
    writes: usize,
    stop_write: Option<usize>,
    fail_write: Option<usize>,
    write_then_fail: bool,
    deletes: usize,
    fail_delete: Option<usize>,
    delete_then_fail: bool,
}
impl Backend for Interrupted {
    fn read(&mut self, target: &str) -> Result<Option<Secret>> {
        self.reads.push(target.into());
        Ok(self.values.get(target).map(|p| Secret::copy(p.bytes())))
    }
    fn write(&mut self, target: &str, bytes: &[u8]) -> Result<()> {
        self.writes += 1;
        assert!(bytes.len() <= BLOB_LIMIT);
        let failure = self.fail_write == Some(self.writes);
        if !failure || self.write_then_fail {
            self.values.insert(target.into(), Secret::copy(bytes));
        }
        assert_ne!(
            self.stop_write,
            Some(self.writes),
            "Owned simulated abrupt interruption"
        );
        if failure {
            Err("OWNED_PROOF_WRITE_FAILURE")
        } else {
            Ok(())
        }
    }
    fn delete(&mut self, target: &str) -> Result<()> {
        self.deletes += 1;
        let failure = self.fail_delete == Some(self.deletes);
        if !failure || self.delete_then_fail {
            self.values.remove(target);
        }
        if failure {
            Err("OWNED_PROOF_DELETE_FAILURE")
        } else {
            Ok(())
        }
    }
}
fn binding() -> Binding {
    Binding::new(
        &Uuid::new_v4().to_string(),
        &Uuid::new_v4().to_string(),
        Sha256::digest(b"owned receipt fixture purpose").into(),
    )
    .unwrap()
}
fn context() -> [u8; 32] {
    Sha256::digest(b"owned immutable upload record").into()
}
fn ordinary() -> Vault<Interrupted> {
    Vault {
        backend: Interrupted::default(),
    }
}
#[test]
fn successful_later_publication_does_not_clear_an_older_interrupted_generation() {
    let b = binding();
    let mut v = ordinary();
    v.backend.stop_write = Some(3);
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        v.replace(&b, None, b"owned interrupted OAuth rotation")
    }))
    .is_err());
    v.backend.stop_write = None;
    let first = v.replace(&b, None, b"owned later grant").unwrap();
    assert!(!first.cleanup_complete);
    let next = v
        .replace(&b, Some(first.version), b"owned subsequent refresh")
        .unwrap();
    assert!(!next.cleanup_complete);
    assert_eq!(v.read(&b).unwrap().0, next.version);
    let orphan = v
        .recoveries(&b, None)
        .unwrap()
        .into_iter()
        .find(|r| r.state == "known_orphan_generation" && !r.retirement_verified)
        .unwrap();
    assert!(v.recover(&b, None, &orphan).unwrap());
    let final_receipt = v
        .replace(&b, Some(next.version), b"owned clean refresh")
        .unwrap();
    assert!(final_receipt.cleanup_complete);
}
#[test]
fn reappeared_changed_retired_parts_are_preserved_and_keep_cleanup_pending() {
    let b = binding();
    let mut v = ordinary();
    let first = v.replace(&b, None, b"owned old token").unwrap();
    let second = v
        .replace(&b, Some(first.version), b"owned new token")
        .unwrap();
    assert!(second.cleanup_complete);
    let target = b.part(first.version, 0);
    v.backend.values.insert(
        target.clone(),
        Secret::copy(b"owned unknown replacement bytes"),
    );
    let next = v
        .replace(&b, Some(second.version), b"owned current refresh")
        .unwrap();
    assert!(!next.cleanup_complete);
    assert_eq!(
        v.backend.values[&target].bytes(),
        b"owned unknown replacement bytes"
    );
    assert_eq!(v.read(&b).unwrap().0, next.version);
    let first = v
        .recoveries(&b, None)
        .unwrap()
        .into_iter()
        .find(|r| r.version == first.version)
        .unwrap();
    assert!(!first.can_retire);
    assert!(!first.retirement_verified);
}
fn seen(v: &mut Vault<Interrupted>, b: &Binding) -> Recovery {
    v.recoveries(b, Some(context())).unwrap().remove(0)
}
#[test]
fn proof_order_and_all_six_abrupt_publication_boundaries_recover_known_parts() {
    for boundary in 1..=6 {
        let b = binding();
        let mut v = ordinary();
        v.backend.stop_write = Some(boundary);
        let stopped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            v.replace_context(&b, None, &vec![37; 3 * PART_BYTES - 1], context())
        }));
        assert!(stopped.is_err());
        v.backend.stop_write = None;
        let r = seen(&mut v, &b);
        assert!(r.can_retire, "boundary{boundary}: {}", r.state);
        assert_eq!(
            r.present_parts,
            if boundary <= 2 {
                0
            } else {
                (boundary - 2).min(3)
            }
        );
        if boundary == 1 {
            assert_eq!(r.state, "reserved_without_receipt");
        }
        if boundary == 6 {
            assert_eq!(r.state, "current_known_generation");
        }
        assert!(v.recover(&b, Some(context()), &r).unwrap());
        assert!(seen(&mut v, &b).retirement_verified);
        assert!(v
            .backend
            .values
            .keys()
            .all(|t| t.ends_with(".ledger") || t.contains(".r.")));
    }
}
#[test]
fn index_and_receipt_write_failures_precede_secret_parts_and_remain_recoverable() {
    for step in [1, 2] {
        for after in [false, true] {
            let b = binding();
            let mut v = ordinary();
            v.backend.fail_write = Some(step);
            v.backend.write_then_fail = after;
            let e = v
                .replace_context(&b, None, b"owned secret bytes", context())
                .unwrap_err();
            assert!(e.contains("RECEIPT") && e.contains("UNCERTAIN"));
            assert!(v
                .backend
                .values
                .keys()
                .all(|t| t.ends_with(".ledger") || t.contains(".r.")));
            v.backend.fail_write = None;
            let rs = v.recoveries(&b, Some(context())).unwrap();
            if step == 1 && !after {
                assert!(rs.is_empty());
            } else {
                assert!(rs[0].can_retire);
                assert!(v.recover(&b, Some(context()), &rs[0]).unwrap());
                assert!(seen(&mut v, &b).retirement_verified);
            }
        }
    }
}
#[test]
fn missing_pointer_and_partial_parts_use_exact_receipt_without_payload_reconstruction() {
    let b = binding();
    let mut v = ordinary();
    let created = v
        .replace_context(&b, None, &vec![42; 3 * PART_BYTES], context())
        .unwrap();
    v.backend.values.remove(&b.main());
    v.backend.values.remove(&b.part(created.version, 1));
    assert!(v.read(&b).is_err());
    let r = seen(&mut v, &b);
    assert_eq!(r.present_parts, 2);
    assert_eq!(r.absent_parts, 1);
    assert_eq!(r.state, "known_orphan_generation");
    assert!(v.recover(&b, Some(context()), &r).unwrap());
    assert!(seen(&mut v, &b).retirement_verified);
}
#[test]
fn changed_part_same_generation_pointer_receipt_or_index_never_blesses_unknown_bytes() {
    for mode in ["part", "pointer", "receipt", "index"] {
        let b = binding();
        let mut v = ordinary();
        let created = v
            .replace_context(&b, None, &vec![17; 2 * PART_BYTES], context())
            .unwrap();
        let target = match mode {
            "part" => b.part(created.version, 0),
            "pointer" => b.main(),
            "receipt" => b.receipt(created.version),
            _ => b.index(),
        };
        let item = v.backend.values.get_mut(&target).unwrap();
        let last = item.0.len() - 1;
        item.0[last] ^= 1;
        let original = Secret::copy(v.backend.values[&target].bytes());
        v.backend.reads.clear();
        let result = v.recoveries(&b, Some(context()));
        if mode == "index" {
            assert!(result.is_err());
        } else {
            let r = result.unwrap().remove(0);
            assert!(!r.can_retire);
            assert_eq!(
                v.recover(&b, Some(context()), &r).unwrap_err(),
                "DRIVE_VAULT_RECEIPT_RECOVERY_DENIED"
            );
        }
        assert_eq!(v.backend.deletes, 0);
        assert!(v.backend.values[&target].bytes() == original.bytes());
        if ["receipt", "index"].contains(&mode) {
            assert!(!v.backend.reads.iter().any(|t| t.contains(".v.")));
        }
    }
}
#[test]
fn other_valid_pointer_and_its_private_parts_are_preserved_during_old_orphan_retirement() {
    let b = binding();
    let mut v = ordinary();
    let old = v
        .replace_context(&b, None, b"owned original generation", context())
        .unwrap();
    let other = Uuid::new_v4();
    let foreign = Binding {
        domain: b.domain,
        connection: b.connection,
        profile: b.profile,
        client: Sha256::digest(b"owned outside replacement client").into(),
    };
    let main = manifest(&foreign, other, b"owned outside replacement payload");
    let p = part(&foreign, &main, 0, b"owned outside replacement payload");
    v.backend
        .values
        .insert(b.main(), Secret::copy(main.bytes()));
    v.backend
        .values
        .insert(b.part(other, 0), Secret::copy(p.bytes()));
    v.backend.reads.clear();
    let r = seen(&mut v, &b);
    assert!(r.can_retire && r.other_pointer_preserved);
    assert!(v.recover(&b, Some(context()), &r).unwrap());
    assert!(v.backend.values[&b.main()].bytes() == main.bytes());
    assert!(v.backend.values[&b.part(other, 0)].bytes() == p.bytes());
    assert!(!v.backend.values.contains_key(&b.part(old.version, 0)));
    assert!(!v.backend.reads.iter().any(|t| t == &b.part(other, 0)));
}
#[test]
fn receipt_context_client_profile_and_observed_revision_guards_are_independent() {
    let b = binding();
    let mut v = ordinary();
    v.replace_context(&b, None, b"owned", context()).unwrap();
    let r = seen(&mut v, &b);
    v.backend.reads.clear();
    assert!(v
        .recoveries(
            &b,
            Some(Sha256::digest(b"different immutable record").into())
        )
        .unwrap()
        .is_empty());
    let foreign = Binding {
        domain: b.domain,
        connection: b.connection,
        profile: b.profile,
        client: Sha256::digest(b"different client").into(),
    };
    assert!(v.recoveries(&foreign, Some(context())).unwrap().is_empty());
    assert_eq!(
        v.recover(&foreign, Some(context()), &r).unwrap_err(),
        "DRIVE_VAULT_RECEIPT_NOT_FOUND"
    );
    let different_profile = Binding {
        domain: b.domain,
        connection: b.connection,
        profile: Uuid::new_v4(),
        client: b.client,
    };
    assert!(v.recoveries(&different_profile, Some(context())).is_err());
    assert!(!v.backend.reads.iter().any(|t| t.contains(".v.")));
    assert!(v.recover(&b, Some(context()), &r).unwrap());
    assert_eq!(
        v.recover(&b, Some(context()), &r).unwrap_err(),
        "DRIVE_VAULT_RECEIPT_OBSERVATION_CHANGED"
    );
}
#[test]
fn interrupted_erasure_and_tombstone_update_retry_without_losing_private_proof() {
    for delete in 1..=3 {
        for after in [false, true] {
            let b = binding();
            let mut v = ordinary();
            v.replace_context(&b, None, &vec![28; 2 * PART_BYTES], context())
                .unwrap();
            let r = seen(&mut v, &b);
            v.backend.fail_delete = Some(delete);
            v.backend.delete_then_fail = after;
            assert!(!v.recover(&b, Some(context()), &r).unwrap());
            v.backend.fail_delete = None;
            let r = seen(&mut v, &b);
            assert!(r.can_retire);
            assert!(v.recover(&b, Some(context()), &r).unwrap());
            assert!(seen(&mut v, &b).retirement_verified);
        }
    }
    for after in [false, true] {
        let b = binding();
        let mut v = ordinary();
        v.replace_context(&b, None, b"owned", context()).unwrap();
        let r = seen(&mut v, &b);
        v.backend.fail_write = Some(v.backend.writes + 1);
        v.backend.write_then_fail = after;
        assert_eq!(
            v.recover(&b, Some(context()), &r).unwrap_err(),
            "DRIVE_VAULT_RECEIPT_INDEX_WRITE_UNCERTAIN"
        );
        v.backend.fail_write = None;
        let r = seen(&mut v, &b);
        assert!(r.can_retire);
        assert!(v.recover(&b, Some(context()), &r).unwrap());
        assert!(seen(&mut v, &b).retirement_verified);
    }
}
#[test]
fn missing_receipt_only_proves_absent_closed_keyspace_and_preserves_existing_parts() {
    let b = binding();
    let mut v = ordinary();
    let made = v.replace_context(&b, None, b"owned", context()).unwrap();
    v.backend.values.remove(&b.main());
    v.backend.values.remove(&b.receipt(made.version));
    let r = seen(&mut v, &b);
    assert!(!r.can_retire);
    assert!(v.backend.values.contains_key(&b.part(made.version, 0)));
    v.backend.values.remove(&b.part(made.version, 0));
    let r = seen(&mut v, &b);
    assert!(r.can_retire);
    assert_eq!(r.absent_parts, LIMIT.div_ceil(PART_BYTES));
    assert!(v.recover(&b, Some(context()), &r).unwrap());
    assert!(seen(&mut v, &b).retirement_verified);
}
#[test]
fn bounded_unresolved_inventory_blocks_new_secret_writes_and_compacts_only_verified_retirement() {
    let b = binding();
    let mut v = ordinary();
    for _ in 0..20 {
        v.backend.stop_write = Some(v.backend.writes + 3);
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| v.replace_context(
                &b,
                None,
                b"owned pending part",
                context()
            )))
            .is_err()
        );
        v.backend.stop_write = None;
    }
    let writes = v.backend.writes;
    assert_eq!(
        v.replace_context(&b, None, b"must not be written", context())
            .unwrap_err(),
        "DRIVE_VAULT_RECEIPT_CAPACITY"
    );
    assert_eq!(writes, v.backend.writes);
    for _ in 0..20 {
        let r = v
            .recoveries(&b, Some(context()))
            .unwrap()
            .into_iter()
            .find(|r| !r.retirement_verified)
            .unwrap();
        assert!(v.recover(&b, Some(context()), &r).unwrap());
    }
    receipts::trim_retired(&mut v.backend, &b).unwrap();
    assert!(v.backend.values.is_empty());
    let made = v
        .replace_context(&b, None, b"owned after compaction", context())
        .unwrap();
    assert_eq!(v.read(&b).unwrap().0, made.version);
}
#[test]
fn continuous_oauth_rotation_and_changed_client_reauthorization_keep_current_payload_and_bounded_proof(
) {
    let b = binding();
    let mut v = ordinary();
    let mut current = v.replace(&b, None, b"owned grant0").unwrap();
    for i in 0..60 {
        let p = format!("owned grant{i}");
        current = v.replace(&b, Some(current.version), p.as_bytes()).unwrap();
        assert!(current.cleanup_complete);
        assert!(v.read(&b).unwrap().1.bytes() == p.as_bytes());
        assert!(v.recoveries(&b, None).unwrap().len() <= 2);
    }
    let changed = Binding {
        domain: b.domain,
        connection: b.connection,
        profile: b.profile,
        client: Sha256::digest(b"owned new OAuth client").into(),
    };
    let observed = observe(&mut v.backend, &b).unwrap().unwrap();
    let new = v
        .reauthorize(&changed, Some(&observed), b"owned new client tokens")
        .unwrap();
    assert!(new.cleanup_complete);
    assert_eq!(v.read(&changed).unwrap().0, new.version);
    assert!(v
        .recoveries(&b, None)
        .unwrap()
        .iter()
        .all(|r| r.retirement_verified));
    assert!(v.read(&b).is_err());
}

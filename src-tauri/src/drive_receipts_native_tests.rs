//! Actual child termination with exact random UUID WinCred targets and fictional
//! bytes. No Core/HTTP/Google account, credential enumeration or GUI process.
use super::*;
use std::{
    io::{BufRead, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

struct BoundaryNative {
    prefix: String,
    event: usize,
    stop: usize,
}
impl BoundaryNative {
    fn owned(&self, target: &str) {
        assert!(target == self.prefix || target.starts_with(&format!("{}.", self.prefix)));
    }
    fn after_mutation(&mut self) {
        self.event += 1;
        if self.event == self.stop {
            println!("OWNED_RECEIPT_BOUNDARY:{}", self.event);
            std::io::stdout().flush().unwrap();
            loop {
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}
impl Backend for BoundaryNative {
    fn read(&mut self, target: &str) -> Result<Option<Secret>> {
        self.owned(target);
        win::Native.read(target)
    }
    fn write(&mut self, target: &str, bytes: &[u8]) -> Result<()> {
        self.owned(target);
        win::Native.write(target, bytes)?;
        // The barrier certifies an observed mutation, not merely a native API
        // acknowledgement interrupted before the Vault's readback check.
        if !matches(&mut win::Native, target, bytes)? {
            return Err("OWNED_NATIVE_WRITE_NOT_OBSERVED");
        }
        self.after_mutation();
        Ok(())
    }
    fn delete(&mut self, target: &str) -> Result<()> {
        self.owned(target);
        win::Native.delete(target)?;
        if win::Native.read(target)?.is_some() {
            return Err("OWNED_NATIVE_ERASURE_NOT_OBSERVED");
        }
        self.after_mutation();
        Ok(())
    }
}
fn fixture_binding(subject: &str, profile: &str) -> Binding {
    Binding::new(
        subject,
        profile,
        Sha256::digest(b"owned receipt fixture purpose").into(),
    )
    .unwrap()
}
#[test]
fn four_owned_native_writers_preserve_disjoint_targets_and_exact_readbacks() {
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
    let threads = (0..4)
        .map(|_| {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let b = binding();
                let version = Uuid::new_v4();
                let target = b.part(version, 0);
                assert!(win::Native.read(&target).unwrap().is_none());
                let values = (0..24)
                    .map(|i| {
                        Secret::copy(
                            format!("Owned fictional concurrent WinCred fixture:{version}:{i}")
                                .as_bytes(),
                        )
                    })
                    .collect::<Vec<_>>();
                let mut error = None;
                for bytes in &values {
                    barrier.wait();
                    let written = win::Native.write(&target, bytes.bytes());
                    barrier.wait();
                    let read = win::Native.read(&target);
                    if written.is_err()
                        || !read.is_ok_and(|v| v.is_some_and(|v| v.bytes() == bytes.bytes()))
                    {
                        error = Some("OWNED_NATIVE_CONCURRENT_WRITE_READBACK_UNCERTAIN");
                    }
                }
                // Exact own target and one of this fixture's known fictional bytes.
                // Never erase an unrelated/changed credential to satisfy the test.
                if let Some(current) = win::Native.read(&target).unwrap() {
                    assert!(values.iter().any(|v| v.bytes() == current.bytes()));
                    win::Native.delete(&target).unwrap();
                }
                assert!(win::Native.read(&target).unwrap().is_none());
                error
            })
        })
        .collect::<Vec<_>>();
    let errors = threads
        .into_iter()
        .filter_map(|t| t.join().unwrap())
        .collect::<Vec<_>>();
    assert!(
        errors.is_empty(),
        "{} owned concurrent writers had uncertain native readback",
        errors.len()
    );
}
fn root_guard(root: &Path) {
    let p = root.canonicalize().unwrap();
    assert_eq!(
        p.parent(),
        Some(std::env::temp_dir().canonicalize().unwrap().as_path())
    );
    let suffix = p
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .strip_prefix("ap-drive-receipts-")
        .unwrap();
    canonical(suffix).unwrap();
    crate::asset_files::directory(&p).unwrap();
}
fn kill_exact_owned_child(
    mut command: Command,
    boundary: usize,
) -> std::result::Result<(), String> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "OWNED_CHILD_START")?;
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout)
            .lines()
            .map_while(std::result::Result::ok)
        {
            if line.starts_with("OWNED_RECEIPT_BOUNDARY:") {
                let _ = tx.send(line);
            }
        }
    });
    let signal = rx.recv_timeout(Duration::from_secs(20));
    // Retain the exact handle, always terminate/reap only this spawned child.
    let killed = child.kill();
    let reaped = child.wait();
    reader.join().map_err(|_| "OWNED_CHILD_READER")?;
    if signal.ok().as_deref() != Some(&format!("OWNED_RECEIPT_BOUNDARY:{boundary}")) {
        return Err("OWNED_CHILD_BOUNDARY_MISSING".into());
    }
    killed.map_err(|_| "OWNED_CHILD_TERMINATION")?;
    let status = reaped.map_err(|_| "OWNED_CHILD_REAP")?;
    if status.success() {
        return Err("OWNED_CHILD_DID_NOT_TERMINATE".into());
    }
    Ok(())
}
#[test]
fn eleven_owned_native_process_terminations_recover_publication_and_retirement_receipts() {
    for (mode, count) in [("publish", 6), ("retire", 5)] {
        for boundary in 1..=count {
            let b = binding();
            let root = std::env::temp_dir().join(format!("ap-drive-receipts-{}", b.connection));
            std::fs::create_dir(&root).unwrap();
            root_guard(&root);
            let identity =
                crate::asset_files::identity(&crate::asset_files::directory(&root).unwrap())
                    .unwrap();
            let nonce = Uuid::new_v4().to_string();
            assert!(win::Native.read(&b.main()).unwrap().is_none());
            assert!(win::Native.read(&b.index()).unwrap().is_none());
            let mut v = Vault {
                backend: win::Native,
            };
            if mode == "retire" {
                v.replace_context(&b, None, &vec![37; 3 * PART_BYTES - 1], context())
                    .unwrap();
            }
            let marker = serde_json::json!({"subject":b.connection,"profile":b.profile,"nonce":nonce,"mode":mode,"boundary":boundary});
            std::fs::write(root.join("fixture.json"), marker.to_string()).unwrap();
            let mut command = Command::new(std::env::current_exe().unwrap());
            command.args(["--ignored","--exact","drive_credentials::receipts::tests::native::owned_native_receipt_termination_helper","--nocapture"])
                .env("AP_OWNED_RECEIPT_ROOT", &root).env("AP_OWNED_RECEIPT_NONCE", &nonce);
            let killed = kill_exact_owned_child(command, boundary);
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                killed.unwrap();
                // Fresh independent native actor, no in-process write inventory.
                let mut restarted = Vault {
                    backend: win::Native,
                };
                let observed = restarted.recoveries(&b, Some(context())).unwrap();
                assert_eq!(observed.len(), 1, "{mode}:{boundary}");
                let r = &observed[0];
                assert!(r.can_retire, "{mode}:{boundary}: {}", r.state);
                let expected_parts = if mode == "publish" {
                    boundary.saturating_sub(2).min(3)
                } else {
                    4usize.saturating_sub(boundary)
                };
                assert_eq!(r.present_parts, expected_parts, "{mode}:{boundary}");
                assert_eq!(r.retirement_verified, mode == "retire" && boundary == 5);
                assert!(restarted.recover(&b, Some(context()), r).unwrap());
                assert!(restarted.recoveries(&b, Some(context())).unwrap()[0].retirement_verified);
            }));
            // Closed private proofs only, including after a failed assertion.
            for r in v.recoveries(&b, Some(context())).unwrap() {
                if r.can_retire {
                    assert!(v.recover(&b, Some(context()), &r).unwrap());
                }
            }
            trim_retired(&mut v.backend, &b).unwrap();
            assert!(win::Native.read(&b.main()).unwrap().is_none());
            assert!(win::Native.read(&b.index()).unwrap().is_none());
            root_guard(&root);
            assert_eq!(
                identity,
                crate::asset_files::identity(&crate::asset_files::directory(&root).unwrap())
                    .unwrap()
            );
            std::fs::remove_dir_all(root.canonicalize().unwrap()).unwrap();
            if let Err(panic) = result {
                std::panic::resume_unwind(panic);
            }
        }
    }
}
#[test]
#[ignore = "Exact UUID-owned native child terminated by its parent receipt contract"]
fn owned_native_receipt_termination_helper() {
    let root = PathBuf::from(std::env::var("AP_OWNED_RECEIPT_ROOT").unwrap());
    root_guard(&root);
    let marker: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("fixture.json")).unwrap()).unwrap();
    assert_eq!(
        marker["nonce"],
        std::env::var("AP_OWNED_RECEIPT_NONCE").unwrap()
    );
    canonical(marker["nonce"].as_str().unwrap()).unwrap();
    let b = fixture_binding(
        marker["subject"].as_str().unwrap(),
        marker["profile"].as_str().unwrap(),
    );
    assert_eq!(
        root.file_name().unwrap().to_str().unwrap(),
        format!("ap-drive-receipts-{}", b.connection)
    );
    let mode = marker["mode"].as_str().unwrap();
    let stop = marker["boundary"].as_u64().unwrap() as usize;
    assert!((1..=if mode == "publish" { 6 } else { 5 }).contains(&stop));
    let mut v = Vault {
        backend: BoundaryNative {
            prefix: b.main(),
            event: 0,
            stop,
        },
    };
    if mode == "publish" {
        assert!(v.backend.read(&b.main()).unwrap().is_none());
        assert!(v.backend.read(&b.index()).unwrap().is_none());
        v.replace_context(&b, None, &vec![37; 3 * PART_BYTES - 1], context())
            .unwrap();
    } else {
        assert_eq!(mode, "retire");
        // Existing test-owned bytes must have the exact fictional payload and
        // binding; marker/UUID names alone never authorize their erasure.
        assert_eq!(v.read(&b).unwrap().1.bytes(), vec![37; 3 * PART_BYTES - 1]);
        let r = v.recoveries(&b, Some(context())).unwrap().remove(0);
        assert_eq!(r.state, "current_known_generation");
        v.recover(&b, Some(context()), &r).unwrap();
    }
    panic!("Owned child passed its required termination barrier");
}

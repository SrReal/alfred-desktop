use super::*;
fn fixture() -> (Arc<Core>, PathBuf) {
    let root = std::env::temp_dir().join(format!("ap-printer-contract-{}", crate::storage::id()));
    std::fs::create_dir(&root).unwrap();
    let mut c = Core::new(
        root.join("profile"),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
    )
    .unwrap();
    c.store
        .execute(
            "UPDATE module_configs SET enabled=1 WHERE module_id='printers'",
            &[],
        )
        .unwrap();
    c.store
        .set_setting("allowed_roots", &json!([root]))
        .unwrap();
    c.media_headless_permission
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let mark = root.join("mock-submit.json");
    let marker = serde_json::to_string(&mark.to_string_lossy())
        .unwrap()
        .replace('\'', "''");
    let mocks = format!(
        r#"
$mockPath=('{marker}' | ConvertFrom-Json)
function Get-Printer {{[pscustomobject]@{{Name='Owned printer';DriverName='Owned driver';PortName='Owned port';PrinterStatus=0}}}}
function Get-CimInstance {{param($ClassName);[pscustomobject]@{{Name='Owned printer';Default=$true}}}}
function Get-PrintJob {{param($PrinterName);[pscustomobject]@{{ID=7;DocumentName='Owned fixture';JobStatus=0;Size=10}}}}
function Remove-PrintJob {{param($PrinterName,$ID);@{{kind='cancel';name=$PrinterName;id=$ID}} | ConvertTo-Json | Set-Content -LiteralPath $mockPath}}
function Start-Process {{param($FilePath,$Verb,$ArgumentList,$WindowStyle,[switch]$Wait);@{{kind='print';name=$ArgumentList;verb=$Verb;window=$WindowStyle;wait=$Wait.IsPresent}} | ConvertTo-Json | Set-Content -LiteralPath $mockPath}}
"#
    );
    let original = include_str!("../../native/windows.ps1");
    let begin = original.find("  'printers.list' {").unwrap();
    let end = original[begin..].find("  'audio.play'").unwrap() + begin;
    let cases = &original[begin..end];
    // Exact printer cases, with no unrelated UIA compilation or OS command access.
    let script=format!("$ErrorActionPreference='Stop'\n[Console]::InputEncoding=[Text.UTF8Encoding]::new($false)\n[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false)\n$request=[Console]::In.ReadToEnd()|ConvertFrom-Json\n$p=$request.input\n{mocks}\ntry{{$result=switch($request.operation){{\n{cases}\ndefault{{throw 'OWNED_UNSUPPORTED'}}\n}};@{{ok=$true;data=$result}}|ConvertTo-Json -Depth 20 -Compress}}catch{{@{{ok=$false;error=$_.Exception.Message}}|ConvertTo-Json -Compress;exit 1}}");
    // The fake handler records invocation only. This does not certify an installed association or actual spooler.
    let script = script.replace("if('PrintTo' -notin $info.Verbs)", "if($false)");
    let script = if std::env::var_os("ALFREDPET_OWNED_PRINTER_DIAGNOSTICS").is_some() {
        let trace = "[IO.File]::AppendAllText((Join-Path $PSScriptRoot 'owned-trace.txt'),([DateTime]::UtcNow.ToString('o')+' PHASE'+[Environment]::NewLine))";
        format!("{}\n{}", trace.replace("PHASE", "started"), script)
            .replace(
                "$p=$request.input",
                &format!(
                    "{}\n$p=$request.input",
                    trace.replace("PHASE", "stdin-read")
                ),
            )
            .replace(
                ";@{ok=$true;data=$result}",
                &format!(
                    ";{};@{{ok=$true;data=$result}}",
                    trace.replace("PHASE", "completed")
                ),
            )
    } else {
        script
    };
    let path = root.join("owned-printer.ps1");
    std::fs::write(&path, script).unwrap();
    c.native = path;
    (Arc::new(c), root)
}
async fn reviewed(c: &Arc<Core>, tool: &str, input: Value, answer: &str) -> Result<Value, String> {
    let c2 = c.clone();
    let t = tool.to_owned();
    let mut job = tokio::spawn(async move { c2.execute(&t, input).await });
    // Preparation queries the native printer before publishing the permission.
    // Allow the existing 60-second bound per preflight call, plus observation
    // overhead; this never extends a product timeout or permits a submission.
    let preflight_seconds = if tool == "printers.cancel_job" {
        130
    } else {
        70
    };
    let pending = async {
        loop {
            let rows = c
                .store
                .query(
                    "SELECT id FROM permission_requests WHERE tool_id=? AND status='pending'",
                    &[json!(tool)],
                )
                .unwrap();
            if let Some(id) = rows[0]["id"].as_str() {
                if let Some(tx) = c.pending.lock().unwrap().remove(id) {
                    break tx;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    };
    let tx = tokio::select! {
        result = &mut job => panic!("Owned printer action ended before permission review: {result:?}"),
        result = tokio::time::timeout(std::time::Duration::from_secs(preflight_seconds), pending) => {
            match result {
                Ok(tx) => tx,
                Err(_) => {
                    job.abort();
                    let _ = job.await;
                    panic!("Owned printer permission was not published within its preflight bounds");
                }
            }
        }
    };
    tx.send(answer.into()).unwrap();
    job.await.unwrap()
}
#[tokio::test]
async fn printer_mock_native_default_queue_status_and_individual_cancel() {
    let (c, root) = fixture();
    let d = c.execute("printers.default", json!({})).await.unwrap();
    assert_eq!(d["Name"], "Owned printer");
    assert_eq!(
        c.execute("printers.queue", json!({"name":"Owned printer"}))
            .await
            .unwrap()[0]["ID"],
        7
    );
    assert!(reviewed(
        &c,
        "printers.cancel_job",
        json!({"name":"Owned printer","job_id":7}),
        "session"
    )
    .await
    .is_err());
    assert!(!root.join("mock-submit.json").exists());
    let r = reviewed(
        &c,
        "printers.cancel_job",
        json!({"name":"Owned printer","job_id":7}),
        "allow",
    )
    .await
    .unwrap();
    assert_eq!(r["completion"], "NOT_CONFIRMED");
    let mark: Value =
        serde_json::from_slice(&std::fs::read(root.join("mock-submit.json")).unwrap()).unwrap();
    assert_eq!(mark["id"], 7);
    assert_eq!(mark["kind"], "cancel");
    drop(c);
    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn printer_mock_file_denial_explicit_target_and_scope_restore() {
    let (c, root) = fixture();
    let path = root.join("own.txt");
    std::fs::write(&path, "Own printable fixture").unwrap();
    assert!(reviewed(&c, "printers.print", json!({"path":path}), "deny")
        .await
        .is_err());
    assert!(!root.join("mock-submit.json").exists());
    let r = reviewed(
        &c,
        "printers.print",
        json!({"path":path,"name":"Owned printer"}),
        "allow",
    )
    .await
    .unwrap();
    assert_eq!(r["submitted"], true);
    assert_eq!(r["printer"], "Owned printer");
    let mark: Value =
        serde_json::from_slice(&std::fs::read(root.join("mock-submit.json")).unwrap()).unwrap();
    assert_eq!(mark["verb"], "PrintTo");
    assert_eq!(mark["window"], "Hidden");
    assert_eq!(mark["wait"], true);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "Own printable fixture"
    );
    let p = Prepared::prepare(&c, "printers.print", &json!({"path":path}))
        .await
        .unwrap();
    crate::media::revoke(&c).unwrap();
    assert!(p.execute(&c).await.unwrap_err().contains("SCOPE_REVOKED"));
    drop(c);
    std::fs::remove_dir_all(root).unwrap();
}

use serde_json::Value;
use std::path::Path;
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt},
    process::Command,
};
pub async fn bounded_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    limit: usize,
) -> Result<Option<String>, String> {
    let mut line = Vec::new();
    loop {
        let buffer = reader.fill_buf().await.map_err(|_| "PROCESS_READ")?;
        if buffer.is_empty() {
            return if line.is_empty() {
                Ok(None)
            } else {
                String::from_utf8(line)
                    .map(Some)
                    .map_err(|_| "PROCESS_UTF8".into())
            };
        }
        let end = buffer.iter().position(|b| *b == b'\n').map(|n| n + 1);
        let count = end.unwrap_or(buffer.len());
        if line.len() + count > limit {
            return Err("PROCESS_LINE_LIMIT".into());
        }
        line.extend_from_slice(&buffer[..count]);
        reader.consume(count);
        if end.is_some() {
            return String::from_utf8(line)
                .map(|s| Some(s.trim_end_matches(['\r', '\n']).to_string()))
                .map_err(|_| "PROCESS_UTF8".into());
        }
    }
}
pub fn sanitize(command: &mut Command) {
    for (key, _) in std::env::vars() {
        if key.starts_with("ANTHROPIC_")
            || key.starts_with("CLAUDE_CODE_USE_")
            || [
                "CLAUDE_CODE_SIMPLE",
                "CLAUDE_CODE_OAUTH_TOKEN",
                "CLAUDE_CODE_SAFE_MODE",
                "HTTP_PROXY",
                "HTTPS_PROXY",
                "ALL_PROXY",
                "GOOGLE_APPLICATION_CREDENTIALS",
                "AWS_PROFILE",
                "AWS_ACCESS_KEY_ID",
                "AWS_SECRET_ACCESS_KEY",
                "AWS_SESSION_TOKEN",
            ]
            .contains(&key.as_str())
        {
            command.env_remove(key);
        }
    }
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    command.kill_on_drop(true);
}
pub async fn output(cmd: Command, input: Option<&str>, timeout: u64) -> Result<String, String> {
    output_cancellable(
        cmd,
        input,
        timeout,
        tokio_util::sync::CancellationToken::new(),
        false,
        8 * 1024 * 1024,
    )
    .await
}
async fn output_cancellable(
    mut cmd: Command,
    input: Option<&str>,
    timeout: u64,
    cancel: tokio_util::sync::CancellationToken,
    native_errors: bool,
    output_limit: usize,
) -> Result<String, String> {
    sanitize(&mut cmd);
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn().map_err(|_| "PROCESS_NOT_FOUND".to_string())?;
    #[cfg(windows)]
    let _job = crate::windows_native::ProcessJob::attach(&child)?;
    if let Some(data) = input {
        if data.len() > 8 * 1024 * 1024 {
            return Err("PROCESS_INPUT_LIMIT".into());
        }
        let mut stdin = child.stdin.take().ok_or("PROCESS_STDIN")?;
        tokio::time::timeout(
            std::time::Duration::from_secs(timeout),
            stdin.write_all(data.as_bytes()),
        )
        .await
        .map_err(|_| "PROCESS_STDIN_TIMEOUT")?
        .map_err(|_| "PROCESS_STDIN")?;
    } else {
        drop(child.stdin.take());
    }
    let stdout = child.stdout.take().ok_or("PROCESS_STDOUT")?;
    let stderr = child.stderr.take().ok_or("PROCESS_STDERR")?;
    async fn bounded<R: tokio::io::AsyncRead + Unpin>(
        mut stream: R,
        limit: usize,
    ) -> Result<Vec<u8>, String> {
        let mut bytes = Vec::new();
        let mut buffer = [0u8; 8192];
        loop {
            let n = stream.read(&mut buffer).await.map_err(|_| "PROCESS_READ")?;
            if n == 0 {
                return Ok(bytes);
            }
            if bytes.len() + n > limit {
                return Err("PROCESS_OUTPUT_LIMIT".into());
            }
            bytes.extend_from_slice(&buffer[..n]);
        }
    }
    let completed = {
        let operation = tokio::time::timeout(std::time::Duration::from_secs(timeout), async {
            tokio::try_join!(
                async { child.wait().await.map_err(|_| "PROCESS_WAIT".to_string()) },
                bounded(stdout, output_limit),
                bounded(stderr, 8 * 1024 * 1024)
            )
        });
        tokio::select! {
            _=cancel.cancelled()=>Err("PROCESS_CANCELLED".to_string()),
            result=operation=>result.map_err(|_|"PROCESS_TIMEOUT".to_string()).and_then(|value|value)
        }
    };
    let (status, stdout, _stderr) = match completed {
        Ok(result) => result,
        Err(error) => {
            // kill awaits the helper's exit before GUI button cleanup can run.
            // The Job Object also terminates descendants on scope exit.
            child
                .kill()
                .await
                .map_err(|_| "PROCESS_TERMINATION_FAILED")?;
            return Err(error);
        }
    };
    let decoded = if native_errors {
        String::from_utf8(stdout).map_err(|_| "PROCESS_UTF8")?
    } else {
        String::from_utf8_lossy(&stdout).into_owned()
    };
    let text = decoded.trim_start_matches('\u{feff}').trim().to_string();
    if !status.success() {
        if native_errors
            && serde_json::from_str::<Value>(&text)
                .ok()
                .is_some_and(|value| {
                    value["ok"] == false
                        && value["error"]
                            .as_str()
                            .is_some_and(|error| error.len() <= 2000)
                })
        {
            return Ok(text);
        }
        #[cfg(all(test, windows))]
        if native_errors {
            if let (Some(directory), Ok(request)) = (
                std::env::var_os("ALFREDPET_OWNED_WINDOW_DIAGNOSTICS"),
                serde_json::from_str::<Value>(input.unwrap_or("")),
            ) {
                // Opt-in, test-only startup diagnostics for metadata/geometry
                // operations on owned fixtures. Never instrument credentials,
                // clipboard, arbitrary scripts or application text operations.
                if request["operation"]
                    .as_str()
                    .is_some_and(|op| op.starts_with("window.recovery_"))
                {
                    let directory = std::path::PathBuf::from(directory);
                    if directory.is_absolute() && directory.is_dir() {
                        let data = serde_json::json!({"operation":request["operation"],"exit_code":status.code(),"stdout":text.chars().take(4096).collect::<String>(),"stderr":String::from_utf8_lossy(&_stderr).chars().take(16384).collect::<String>()});
                        if let Ok(mut file) = std::fs::OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .open(directory.join(format!("{}.json", crate::storage::id())))
                        {
                            let _ =
                                std::io::Write::write_all(&mut file, data.to_string().as_bytes());
                        }
                    }
                }
            }
        }
        return Err(format!("PROCESS_FAILED: {:?}", status.code()));
    }
    Ok(text)
}
pub(crate) async fn window_change(
    input: &Value,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<Value, String> {
    if cancel.is_cancelled() {
        return Err("PROCESS_CANCELLED".into());
    }
    let mut cmd = Command::new(std::env::current_exe().map_err(|_| "WINDOW_WORKER_EXE")?);
    #[cfg(test)]
    cmd.args([
        "window_worker::tests::worker_child",
        "--ignored",
        "--exact",
        "--nocapture",
        "--test-threads=1",
    ]);
    #[cfg(not(test))]
    cmd.arg("--window-worker");
    let request = input.to_string();
    if request.len() > 64 * 1024 {
        return Err("WINDOW_WORKER_INPUT_LIMIT".into());
    }
    let output = output_cancellable(cmd, Some(&request), 60, cancel, true, 64 * 1024).await?;
    let mut frames = output
        .lines()
        .filter_map(|line| line.strip_prefix(crate::window_worker::RESULT_PREFIX));
    let frame = frames.next().ok_or("WINDOW_WORKER_RESPONSE")?;
    if frames.next().is_some() {
        return Err("WINDOW_WORKER_RESPONSE".into());
    }
    let response: Value = serde_json::from_str(frame).map_err(|_| "WINDOW_WORKER_RESPONSE")?;
    if response["ok"] != true {
        return Err(response["error"]
            .as_str()
            .filter(|e| e.len() <= 200)
            .unwrap_or("WINDOW_WORKER_FAILED")
            .into());
    }
    Ok(response["data"].clone())
}

pub async fn native(script: &Path, operation: &str, input: &Value) -> Result<Value, String> {
    native_cancellable(
        script,
        operation,
        input,
        tokio_util::sync::CancellationToken::new(),
    )
    .await
}
pub async fn native_cancellable(
    script: &Path,
    operation: &str,
    input: &Value,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<Value, String> {
    native_with_limit(script, operation, input, cancel, 8 * 1024 * 1024).await
}
pub async fn native_capture(
    script: &Path,
    operation: &str,
    input: &Value,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<Value, String> {
    if ![
        "vision.capture_window",
        "vision.capture_monitor",
        "vision.capture_region",
        "gui.screenshot_target",
    ]
    .contains(&operation)
    {
        return Err("CAPTURE_OPERATION".into());
    }
    native_with_limit(script, operation, input, cancel, 48 * 1024 * 1024).await
}
async fn native_with_limit(
    script: &Path,
    operation: &str,
    input: &Value,
    cancel: tokio_util::sync::CancellationToken,
    output_limit: usize,
) -> Result<Value, String> {
    let system_root = std::env::var("SystemRoot").map_err(|_| "WINDOWS_SYSTEM_ROOT")?;
    let shell_root = Path::new(&system_root).join("System32/WindowsPowerShell/v1.0");
    // Pin the .NET Framework host for UIA/WinForms/WinRT. A PATH shim may use
    // a different runtime with incompatible accessibility proxy assemblies.
    let mut cmd = Command::new(shell_root.join("powershell.exe"));
    if operation == "database.odbc" {
        // Database drivers/libpq also accept ambient credentials, routing and
        // tracing options. Supply only the native OS/runtime environment.
        cmd.env_clear();
        for key in [
            "SystemRoot",
            "WINDIR",
            "TEMP",
            "TMP",
            "USERPROFILE",
            "APPDATA",
            "LOCALAPPDATA",
        ] {
            if let Some(value) = std::env::var_os(key) {
                cmd.env(key, value);
            }
        }
    }
    cmd.env("PSModulePath", shell_root.join("Modules"));
    cmd.args([
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-STA",
        "-ExecutionPolicy",
        "Bypass",
        "-File",
    ])
    // Tauri resources use canonical Windows paths. PowerShell 5 cannot resolve
    // $PSScriptRoot for an otherwise valid \\?\ path. Preserve the same helper
    // identity while using the equivalent legacy-compatible path when possible.
    .arg(dunce::simplified(script));
    let data = serde_json::json!({"operation":operation,"input":input}).to_string();
    #[cfg(test)]
    let started = std::time::Instant::now();
    let result = Box::pin(output_cancellable(
        cmd,
        Some(&data),
        60,
        cancel,
        true,
        output_limit,
    ))
    .await;
    #[cfg(test)]
    if operation.starts_with("window.recovery_") || operation.starts_with("printers.") {
        // Only operation/status/timing, never window titles, request or output.
        eprintln!(
            "NATIVE_TIMING operation={operation} elapsed_ms={} status={}",
            started.elapsed().as_millis(),
            if result.is_ok() {
                "completed"
            } else {
                "failed"
            }
        );
    }
    let result = result?;
    let value: Value = serde_json::from_str(&result).map_err(|_| "NATIVE_INVALID_JSON")?;
    if value["ok"] != true {
        return Err(value["error"].as_str().unwrap_or("NATIVE_FAILED").into());
    }
    Ok(value["data"].clone())
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn canonical_resource_helper_resolves_sibling_with_spaces_and_unicode() {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace");
        let directory = workspace
            .join(".work")
            .join(format!("native helper é {}", crate::storage::id()));
        std::fs::create_dir(&directory).unwrap();
        struct OwnedFixture(std::path::PathBuf);
        impl Drop for OwnedFixture {
            fn drop(&mut self) {
                for name in ["helper.ps1", "sibling.ps1"] {
                    let _ = std::fs::remove_file(self.0.join(name));
                }
                let _ = std::fs::remove_dir(&self.0);
            }
        }
        let _fixture = OwnedFixture(directory.clone());
        std::fs::write(directory.join("sibling.ps1"), "$sibling='OWNED_SIBLING_OK'").unwrap();
        let script = directory.join("helper.ps1");
        std::fs::write(
            &script,
            r#"$ErrorActionPreference='Stop'
[Console]::InputEncoding=[Text.UTF8Encoding]::new($false)
[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false)
$request=[Console]::In.ReadToEnd()|ConvertFrom-Json
. (Join-Path $PSScriptRoot 'sibling.ps1')
@{ok=$true;data=@{sibling=$sibling;text=$request.input.text;operation=$request.operation}}|ConvertTo-Json -Compress
"#,
        )
        .unwrap();
        let canonical = script.canonicalize().unwrap();
        assert!(canonical.to_string_lossy().starts_with(r"\\?\"));
        assert_eq!(
            std::fs::metadata(&canonical).unwrap().len(),
            std::fs::metadata(dunce::simplified(&canonical))
                .unwrap()
                .len()
        );
        let result = native(
            &canonical,
            "owned.native_path_fixture",
            &json!({"text":"Unicode é / spaces / literal quote \""}),
        )
        .await
        .unwrap();
        assert_eq!(
            result,
            json!({"sibling":"OWNED_SIBLING_OK","text":"Unicode é / spaces / literal quote \"","operation":"owned.native_path_fixture"})
        );
    }
}

//! One-shot elevated worker. Only a process authenticated at both pipe ends
//! receives the reviewed payload. No command files, listener service or UAC bypass.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub command: String,
    pub cwd: String,
    pub timeout: u64,
}
impl Request {
    fn validate(&self) -> Result<(), String> {
        if self.command.trim().is_empty()
            || self.command.len() > 128000
            || !(1..=3600).contains(&self.timeout)
            || self.cwd.len() > 4000
            || !std::path::Path::new(&self.cwd).is_absolute()
            || self.cwd.contains('\0')
        {
            return Err("ADMIN_REQUEST_INVALID".into());
        }
        Ok(())
    }
}
const FRAME_LIMIT: usize = 256 * 1024;
async fn receive<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Value, String> {
    let n = reader
        .read_u32_le()
        .await
        .map_err(|_| "ADMIN_PIPE_CLOSED")? as usize;
    if n == 0 || n > FRAME_LIMIT {
        return Err("ADMIN_FRAME_LIMIT".into());
    }
    let mut bytes = vec![0; n];
    reader
        .read_exact(&mut bytes)
        .await
        .map_err(|_| "ADMIN_PIPE_CLOSED")?;
    serde_json::from_slice(&bytes).map_err(|_| "ADMIN_FRAME_JSON".into())
}
async fn send<W: AsyncWrite + Unpin>(writer: &mut W, value: &Value) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(|_| "ADMIN_FRAME_JSON")?;
    if bytes.len() > FRAME_LIMIT {
        return Err("ADMIN_FRAME_LIMIT".into());
    }
    writer
        .write_u32_le(bytes.len() as u32)
        .await
        .map_err(|_| "ADMIN_PIPE_CLOSED")?;
    writer
        .write_all(&bytes)
        .await
        .map_err(|_| "ADMIN_PIPE_CLOSED")?;
    writer.flush().await.map_err(|_| "ADMIN_PIPE_CLOSED".into())
}

#[cfg(windows)]
mod native {
    use super::*;
    use std::{
        os::windows::{ffi::OsStrExt, io::AsRawHandle},
        path::PathBuf,
    };
    use tokio::net::windows::named_pipe::{ClientOptions, ServerOptions};
    use windows::{
        core::{w, PCWSTR, PWSTR},
        Win32::{
            Foundation::{CloseHandle, HANDLE},
            System::{
                Pipes::{GetNamedPipeClientProcessId, GetNamedPipeServerProcessId},
                Threading::{
                    GetProcessId, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
                    PROCESS_QUERY_LIMITED_INFORMATION,
                },
            },
            UI::{
                Shell::{
                    IsUserAnAdmin, ShellExecuteExW, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS,
                    SHELLEXECUTEINFOW,
                },
                WindowsAndMessaging::SW_HIDE,
            },
        },
    };
    struct Process(HANDLE);
    unsafe impl Send for Process {}
    impl Drop for Process {
        fn drop(&mut self) {
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
    fn wide(v: &std::ffi::OsStr) -> Vec<u16> {
        v.encode_wide().chain(Some(0)).collect()
    }
    fn same_program(pid: u32) -> Result<Process, String> {
        let handle = Process(
            unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
                .map_err(|_| "ADMIN_PEER_PROCESS")?,
        );
        let mut buffer = vec![0u16; 32768];
        let mut n = buffer.len() as u32;
        unsafe {
            QueryFullProcessImageNameW(
                handle.0,
                PROCESS_NAME_WIN32,
                PWSTR(buffer.as_mut_ptr()),
                &mut n,
            )
        }
        .map_err(|_| "ADMIN_PEER_IMAGE")?;
        let peer = dunce::canonicalize(PathBuf::from(String::from_utf16_lossy(
            &buffer[..n as usize],
        )))
        .map_err(|_| "ADMIN_PEER_IMAGE")?;
        let own = dunce::canonicalize(std::env::current_exe().map_err(|_| "ADMIN_IMAGE")?)
            .map_err(|_| "ADMIN_IMAGE")?;
        if peer != own {
            return Err("ADMIN_PEER_IMAGE_MISMATCH".into());
        }
        Ok(handle)
    }
    fn launch(id: &str) -> Result<Process, String> {
        let exe = std::env::current_exe().map_err(|_| "ADMIN_IMAGE")?;
        let path = wide(exe.as_os_str());
        let args = wide(std::ffi::OsStr::new(&format!(
            "--admin-worker {id} {}",
            std::process::id()
        )));
        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
            lpVerb: w!("runas"),
            lpFile: PCWSTR(path.as_ptr()),
            lpParameters: PCWSTR(args.as_ptr()),
            nShow: SW_HIDE.0,
            ..Default::default()
        };
        unsafe { ShellExecuteExW(&mut info) }
            .map_err(|error| format!("ADMIN_UAC_CANCELLED_OR_FAILED: {error}"))?;
        if info.hProcess.is_invalid() {
            return Err("ADMIN_PROCESS_HANDLE".into());
        }
        Ok(Process(info.hProcess))
    }
    pub async fn execute<F: FnMut(&str, &str) + Send>(
        request: Request,
        cancel: CancellationToken,
        mut emit: F,
    ) -> Result<Value, String> {
        request.validate()?;
        let id = uuid::Uuid::new_v4().to_string();
        let name = format!(r"\\.\pipe\AlfredPet.Admin.{id}");
        let mut pipe = ServerOptions::new()
            .first_pipe_instance(true)
            .reject_remote_clients(true)
            .max_instances(1)
            .create(&name)
            .map_err(|_| "ADMIN_PIPE_CREATE")?;
        let work = async {
            let child = tokio::task::spawn_blocking(move || launch(&id))
                .await
                .map_err(|_| "ADMIN_LAUNCH")??;
            pipe.connect().await.map_err(|_| "ADMIN_PIPE_CONNECT")?;
            let mut pid = 0;
            unsafe { GetNamedPipeClientProcessId(HANDLE(pipe.as_raw_handle()), &mut pid) }
                .map_err(|_| "ADMIN_PIPE_IDENTITY")?;
            if pid != unsafe { GetProcessId(child.0) } {
                return Err("ADMIN_PIPE_IDENTITY_MISMATCH".into());
            }
            let _identity = same_program(pid)?;
            // The elevated worker cannot execute until it receives this payload.
            send(
                &mut pipe,
                &serde_json::to_value(&request).map_err(|_| "ADMIN_REQUEST_JSON")?,
            )
            .await?;
            let mut stdout = String::new();
            let mut stderr = String::new();
            let mut total = 0;
            loop {
                let message = receive(&mut pipe).await?;
                match message["kind"].as_str() {
                    Some("line") => {
                        let channel = message["channel"].as_str().ok_or("ADMIN_CHANNEL")?;
                        let line = message["text"].as_str().ok_or("ADMIN_LINE")?;
                        total += line.len() + 1;
                        if total > 8 * 1024 * 1024 {
                            return Err("ADMIN_OUTPUT_LIMIT".into());
                        }
                        match channel {
                            "stdout" => {
                                stdout.push_str(line);
                                stdout.push('\n');
                            }
                            "stderr" => {
                                stderr.push_str(line);
                                stderr.push('\n');
                            }
                            _ => return Err("ADMIN_CHANNEL".into()),
                        }
                        emit(channel, line);
                    }
                    Some("done") => {
                        if let Some(error) = message["error"].as_str() {
                            return Err(error.into());
                        }
                        return Ok(
                            json!({"stdout":stdout,"stderr":stderr,"exit_code":0,"elevated":true}),
                        );
                    }
                    _ => return Err("ADMIN_PROTOCOL".into()),
                }
            }
        };
        tokio::select! {
            _=cancel.cancelled()=>Err("ADMIN_CANCELLED: dispatched effects may already exist".into()),
            result=tokio::time::timeout(std::time::Duration::from_secs(request.timeout+120),work)=>result.map_err(|_|"ADMIN_TIMEOUT: inspect result before retrying".to_string())?,
        }
        // Closing the pipe cancels the worker's process tree, including parent crash.
    }
    pub async fn worker(id: &str, parent: u32) -> Result<(), String> {
        uuid::Uuid::parse_str(id).map_err(|_| "ADMIN_PIPE_NAME")?;
        if !unsafe { IsUserAnAdmin() }.as_bool() {
            return Err("ADMIN_ELEVATION_REQUIRED".into());
        }
        let _parent = same_program(parent)?;
        let name = format!(r"\\.\pipe\AlfredPet.Admin.{id}");
        let mut pipe = ClientOptions::new()
            .open(name)
            .map_err(|_| "ADMIN_PIPE_OPEN")?;
        let mut pid = 0;
        unsafe { GetNamedPipeServerProcessId(HANDLE(pipe.as_raw_handle()), &mut pid) }
            .map_err(|_| "ADMIN_PIPE_IDENTITY")?;
        if pid != parent {
            return Err("ADMIN_PARENT_MISMATCH".into());
        }
        let request: Request = serde_json::from_value(
            tokio::time::timeout(std::time::Duration::from_secs(20), receive(&mut pipe))
                .await
                .map_err(|_| "ADMIN_REQUEST_TIMEOUT")??,
        )
        .map_err(|_| "ADMIN_REQUEST_JSON")?;
        request.validate()?;
        let (mut reader, mut writer) = tokio::io::split(pipe);
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        let watcher = tokio::spawn(async move {
            let mut byte = [0];
            let _ = reader.read(&mut byte).await;
            token.cancel();
        });
        struct Watcher(tokio::task::JoinHandle<()>);
        impl Drop for Watcher {
            fn drop(&mut self) {
                self.0.abort();
            }
        }
        let _watcher = Watcher(watcher);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let job = crate::terminal::elevated_run(request, cancel.clone(), move |channel, line| {
            let _ = tx.send(json!({"kind":"line","channel":channel,"text":line}));
        });
        tokio::pin!(job);
        let result = loop {
            tokio::select! {
                result=&mut job=>break result,
                line=rx.recv()=>if let Some(line)=line {send(&mut writer,&line).await?;},
            }
        };
        while let Ok(line) = rx.try_recv() {
            send(&mut writer, &line).await?;
        }
        send(&mut writer, &json!({"kind":"done","error":result.err()})).await
    }
}
pub async fn execute<F: FnMut(&str, &str) + Send>(
    request: Request,
    cancel: CancellationToken,
    emit: F,
) -> Result<Value, String> {
    #[cfg(windows)]
    {
        native::execute(request, cancel, emit).await
    }
    #[cfg(not(windows))]
    {
        let _ = (request, cancel, emit);
        Err("WINDOWS_REQUIRED".into())
    }
}
pub fn entry(args: &[String]) -> i32 {
    if args.len() != 4 {
        return 2;
    }
    let Ok(parent) = args[3].parse::<u32>() else {
        return 2;
    };
    let Ok(runtime) = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    else {
        return 2;
    };
    #[cfg(windows)]
    {
        if runtime.block_on(native::worker(&args[2], parent)).is_ok() {
            0
        } else {
            1
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (runtime, parent);
        2
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn protocol_bounds_and_roundtrip() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        let value = json!({"kind":"line","text":"Señor"});
        send(&mut a, &value).await.unwrap();
        assert_eq!(receive(&mut b).await.unwrap(), value);
        a.write_u32_le((FRAME_LIMIT + 1) as u32).await.unwrap();
        assert_eq!(receive(&mut b).await.unwrap_err(), "ADMIN_FRAME_LIMIT");
        assert!(Request {
            command: "Get-Date".into(),
            cwd: "relative".into(),
            timeout: 30
        }
        .validate()
        .is_err());
        assert!(serde_json::from_value::<Request>(
            json!({"command":"x","cwd":"C:/","timeout":30,"executable":"bad"})
        )
        .is_err());
    }
}

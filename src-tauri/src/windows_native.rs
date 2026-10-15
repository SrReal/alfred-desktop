use crate::geometry::{Monitor, Rect};

/// Observe one PID without enumerating the machine or starting a shell.
#[cfg(windows)]
pub(crate) fn watch_process(pid: u32) -> Result<serde_json::Value, String> {
    use windows::Win32::{
        Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, FILETIME, WAIT_OBJECT_0, WAIT_TIMEOUT},
        System::Threading::{
            GetProcessTimes, OpenProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION,
            PROCESS_SYNCHRONIZE,
        },
    };
    if pid == 0 || pid > i32::MAX as u32 {
        return Err("WATCH_TARGET_ID".into());
    }
    let handle = match unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            false,
            pid,
        )
    } {
        Ok(handle) => handle,
        Err(error)
            if error.code() == windows::core::HRESULT::from_win32(ERROR_INVALID_PARAMETER.0) =>
        {
            return Ok(serde_json::json!({"present":false}))
        }
        Err(_) => return Err("WATCH_PROCESS_UNAVAILABLE".into()),
    };
    // Keep the exact process object open across the liveness and creation-time
    // queries; a reused PID cannot supply the second half of this observation.
    let result = (|| {
        match unsafe { WaitForSingleObject(handle, 0) } {
            WAIT_OBJECT_0 => return Ok(serde_json::json!({"present":false})),
            WAIT_TIMEOUT => {}
            _ => return Err("WATCH_PROCESS_UNAVAILABLE".into()),
        }
        let (mut created, mut exited, mut kernel, mut user) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        unsafe { GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) }
            .map_err(|_| "WATCH_PROCESS_UNAVAILABLE")?;
        let filetime = (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
        let started = filetime
            .checked_add(504_911_232_000_000_000)
            .ok_or("WATCH_PROCESS_UNAVAILABLE")?;
        Ok(serde_json::json!({"present":true,"process_id":pid,"started":started}))
    })();
    let _ = unsafe { CloseHandle(handle) };
    result
}

/// A window watch needs only metadata; it must not send messages to its owner.
#[cfg(windows)]
pub(crate) fn watch_window(raw: i64) -> Result<serde_json::Value, String> {
    use windows::Win32::{
        Foundation::HWND,
        UI::WindowsAndMessaging::{GetWindowThreadProcessId, IsWindow, IsWindowVisible},
    };
    if raw <= 0 {
        return Err("WATCH_TARGET_ID".into());
    }
    let hwnd = HWND(raw as usize as *mut _);
    if !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
        return Ok(serde_json::json!({"present":false}));
    }
    let mut owner = 0;
    if unsafe { GetWindowThreadProcessId(hwnd, Some(&mut owner)) } == 0 || owner == 0 {
        return Ok(serde_json::json!({"present":false}));
    }
    let visible = unsafe { IsWindowVisible(hwnd) }.as_bool();
    Ok(serde_json::json!({"present":true,"process_id":owner,"visible":visible}))
}

#[cfg(not(windows))]
pub(crate) fn watch_process(_: u32) -> Result<serde_json::Value, String> {
    Err("WINDOWS_11_REQUIRED".into())
}
#[cfg(not(windows))]
pub(crate) fn watch_window(_: i64) -> Result<serde_json::Value, String> {
    Err("WINDOWS_11_REQUIRED".into())
}
#[cfg(windows)]
pub fn monitors() -> Result<Vec<Monitor>, String> {
    use windows::{
        core::BOOL,
        Win32::{
            Foundation::{LPARAM, RECT},
            Graphics::Gdi::{
                EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
            },
            UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI},
        },
    };
    unsafe extern "system" fn enumerate(
        monitor: HMONITOR,
        _: HDC,
        _: *mut RECT,
        data: LPARAM,
    ) -> BOOL {
        // EnumDisplayMonitors invokes synchronously, so the Vec pointer outlives every callback.
        let list = unsafe { &mut *(data.0 as *mut Vec<Monitor>) };
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if unsafe { GetMonitorInfoW(monitor, &mut info as *mut _ as *mut MONITORINFO) }.as_bool() {
            let mut dx = 96;
            let mut dy = 96;
            let _ = unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) };
            let name = String::from_utf16_lossy(
                &info.szDevice[..info
                    .szDevice
                    .iter()
                    .position(|c| *c == 0)
                    .unwrap_or(info.szDevice.len())],
            );
            let work = info.monitorInfo.rcWork;
            let context = Monitor {
                id: name,
                work: Rect {
                    x: work.left as f64,
                    y: work.top as f64,
                    width: (work.right - work.left) as f64,
                    height: (work.bottom - work.top) as f64,
                },
                scale: dx as f64 / 96.,
            };
            if info.monitorInfo.dwFlags & 1 != 0 {
                list.insert(0, context);
            } else {
                list.push(context);
            }
        }
        BOOL(1)
    }
    let mut list = Vec::<Monitor>::new();
    if !unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(enumerate),
            LPARAM(&mut list as *mut _ as isize),
        )
    }
    .as_bool()
    {
        return Err("MONITOR_ENUM_FAILED".into());
    }
    if list.is_empty() {
        return Err("NO_MONITORS".into());
    }
    Ok(list)
}
#[cfg(not(windows))]
pub fn monitors() -> Result<Vec<Monitor>, String> {
    Err("WINDOWS_11_REQUIRED".into())
}

#[cfg(windows)]
pub struct ProcessJob(windows::Win32::Foundation::HANDLE);
#[cfg(windows)]
unsafe impl Send for ProcessJob {}
#[cfg(windows)]
impl ProcessJob {
    pub fn attach(child: &tokio::process::Child) -> Result<Self, String> {
        use windows::{
            core::PCWSTR,
            Win32::{
                Foundation::{CloseHandle, HANDLE},
                System::JobObjects::*,
            },
        };
        let job = unsafe { CreateJobObjectW(None, PCWSTR::null()) }.map_err(|_| "JOB_CREATE")?;
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let result = unsafe {
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const _,
                std::mem::size_of_val(&info) as u32,
            )
        };
        let Some(raw) = child.raw_handle() else {
            let _ = unsafe { CloseHandle(job) };
            return Err("JOB_PROCESS_HANDLE".into());
        };
        if result.is_err() || unsafe { AssignProcessToJobObject(job, HANDLE(raw as _)) }.is_err() {
            let _ = unsafe { CloseHandle(job) };
            return Err("JOB_ASSIGN".into());
        }
        Ok(Self(job))
    }
}
#[cfg(windows)]
impl Drop for ProcessJob {
    fn drop(&mut self) {
        let _ = unsafe { windows::Win32::Foundation::CloseHandle(self.0) };
    }
}

//! Bounded metadata queries: no shell startup and no messages to target applications.
//! Window mutations remain in the cancellable, isolated native helper.
use serde_json::Value;

#[cfg(not(windows))]
pub(crate) fn capture(_: &Value, _: bool) -> Result<Vec<Value>, String> {
    Err("WINDOWS_11_REQUIRED".into())
}

#[cfg(windows)]
pub(crate) fn capture(targets: &Value, initialize: bool) -> Result<Vec<Value>, String> {
    use serde_json::json;
    use std::collections::HashSet;
    use windows::{
        core::w,
        Win32::{
            Foundation::{CloseHandle, FILETIME, HANDLE, HWND, RECT},
            Graphics::Gdi::{
                GetMonitorInfoW, MonitorFromWindow, MONITORINFOEXW, MONITOR_DEFAULTTONEAREST,
            },
            System::Threading::{GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
            UI::{
                HiDpi::{
                    SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT,
                    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
                },
                WindowsAndMessaging::{
                    GetPropW, GetWindowLongW, GetWindowPlacement, GetWindowRect,
                    GetWindowThreadProcessId, IsWindow, IsWindowVisible, SetPropW, GWL_EXSTYLE,
                    WINDOWPLACEMENT, WPF_RESTORETOMAXIMIZED, WS_EX_TOOLWINDOW,
                },
            },
        },
    };

    struct ProcessHandle(HANDLE);
    impl Drop for ProcessHandle {
        fn drop(&mut self) {
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
    struct DpiContext(DPI_AWARENESS_CONTEXT);
    impl Drop for DpiContext {
        fn drop(&mut self) {
            unsafe { SetThreadDpiAwarenessContext(self.0) };
        }
    }
    fn started(pid: u32) -> Result<String, String> {
        let handle = ProcessHandle(
            unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
                .map_err(|_| "LAYOUT_PROCESS_IDENTITY_UNAVAILABLE")?,
        );
        let (mut created, mut exited, mut kernel, mut user) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        unsafe { GetProcessTimes(handle.0, &mut created, &mut exited, &mut kernel, &mut user) }
            .map_err(|_| "LAYOUT_PROCESS_IDENTITY_UNAVAILABLE")?;
        // FILETIME uses the 1601 epoch; the persisted .NET contract uses 0001.
        let filetime = (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
        filetime
            .checked_add(504_911_232_000_000_000)
            .map(|ticks| ticks.to_string())
            .ok_or_else(|| "LAYOUT_PROCESS_IDENTITY_UNAVAILABLE".into())
    }
    fn owner(hwnd: HWND) -> Result<u32, String> {
        if !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
            return Err("WINDOW_TARGET_MISSING".into());
        }
        let mut pid = 0;
        if unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) } == 0 || pid == 0 {
            return Err("WINDOW_TARGET_MISSING".into());
        }
        Ok(pid)
    }
    fn identity(hwnd: HWND, initialize: bool) -> Result<Option<String>, String> {
        let name = w!("AlfredPet.WindowIdentity.v1");
        let mut value = unsafe { GetPropW(hwnd, name) };
        if value.0.is_null() && initialize {
            let nonce = (uuid::Uuid::new_v4().as_u128() as u64) & i64::MAX as u64;
            if nonce == 0 {
                return Err("WINDOW_IDENTITY_RANDOM".into());
            }
            unsafe { SetPropW(hwnd, name, Some(HANDLE(nonce as usize as *mut _))) }
                .map_err(|_| "WINDOW_IDENTITY_DENIED")?;
            value = unsafe { GetPropW(hwnd, name) };
            if value.0 as usize as u64 != nonce {
                return Err("WINDOW_IDENTITY_CHANGED".into());
            }
        }
        Ok((!value.0.is_null()).then(|| format!("{:016x}", value.0 as usize as u64)))
    }
    let targets = targets.as_array().ok_or("WINDOW_TARGETS")?;
    if targets.is_empty() || targets.len() > 32 {
        return Err("LAYOUT_COUNT".into());
    }
    // No await while this thread-local setting is active. Restore on every exit.
    let previous =
        unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    if previous.0.is_null() {
        return Err("WINDOW_DPI_CONTEXT".into());
    }
    let _dpi = DpiContext(previous);
    let mut seen = HashSet::new();
    let mut snapshots = Vec::with_capacity(targets.len());
    for target in targets {
        let raw = target["hwnd"]
            .as_i64()
            .filter(|value| *value > 0)
            .ok_or("WINDOW_TARGET_MISSING")?;
        if !seen.insert(raw) {
            return Err("LAYOUT_TARGET".into());
        }
        // A malformed supplied identity must not silently become an omitted one.
        let expected_pid = target
            .get("process_id")
            .filter(|v| !v.is_null())
            .map(|v| v.as_u64().ok_or("WINDOW_TARGET_CHANGED"))
            .transpose()?;
        let expected_created = target
            .get("process_started")
            .filter(|v| !v.is_null())
            .map(|v| v.as_str().ok_or("LAYOUT_PROCESS_RECREATED"))
            .transpose()?;
        let expected_identity = target
            .get("window_identity")
            .filter(|v| !v.is_null())
            .map(|v| v.as_str().ok_or("WINDOW_IDENTITY_CHANGED"))
            .transpose()?;
        let hwnd = HWND(raw as usize as *mut _);
        let pid = owner(hwnd)?;
        if !unsafe { IsWindowVisible(hwnd) }.as_bool() {
            return Err("WINDOW_NOT_VISIBLE".into());
        }
        if expected_pid.is_some_and(|expected| expected != 0 && expected != u64::from(pid)) {
            return Err("WINDOW_TARGET_CHANGED".into());
        }
        let created = started(pid)?;
        if expected_created.is_some_and(|expected| !expected.is_empty() && expected != created) {
            return Err("LAYOUT_PROCESS_RECREATED".into());
        }
        let token = identity(hwnd, initialize)?;
        if expected_identity
            .is_some_and(|expected| !expected.is_empty() && Some(expected) != token.as_deref())
        {
            return Err("WINDOW_IDENTITY_CHANGED".into());
        }
        let mut placement = WINDOWPLACEMENT {
            length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
            ..Default::default()
        };
        unsafe { GetWindowPlacement(hwnd, &mut placement) }.map_err(|_| "WINDOW_PLACEMENT_READ")?;
        let mut rect = RECT::default();
        unsafe { GetWindowRect(hwnd, &mut rect) }.map_err(|_| "WINDOW_BOUNDS")?;
        let second_pid = owner(hwnd)?;
        let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
        let mut screen = MONITORINFOEXW::default();
        screen.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if !unsafe { GetMonitorInfoW(monitor, &mut screen as *mut _ as *mut _) }.as_bool() {
            return Err("MONITOR_NOT_FOUND".into());
        }
        let info = screen.monitorInfo;
        let tool_window =
            unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) } as u32 & WS_EX_TOOLWINDOW.0 != 0;
        let (dx, dy) = if tool_window {
            (0, 0)
        } else {
            (
                i64::from(info.rcWork.left) - i64::from(info.rcMonitor.left),
                i64::from(info.rcWork.top) - i64::from(info.rcMonitor.top),
            )
        };
        // Fresh process handle and property read: HWND/PID reuse is not accepted.
        if second_pid != pid || started(second_pid)? != created || identity(hwnd, false)? != token {
            return Err("WINDOW_IDENTITY_CHANGED".into());
        }
        let show = placement.showCmd;
        let (min_x, min_y) = if show == 2 {
            (placement.ptMinPosition.x, placement.ptMinPosition.y)
        } else {
            (-1, -1)
        };
        let (max_x, max_y) = if show == 3 {
            (placement.ptMaxPosition.x, placement.ptMaxPosition.y)
        } else {
            (-1, -1)
        };
        let normal = placement.rcNormalPosition;
        let monitor_name = String::from_utf16_lossy(
            &screen.szDevice[..screen
                .szDevice
                .iter()
                .position(|v| *v == 0)
                .unwrap_or(screen.szDevice.len())],
        );
        let mut snapshot = json!({
            "hwnd": raw, "process_id": pid, "process_started": created,
            "window_identity": token, "monitor": monitor_name,
            "x": rect.left, "y": rect.top, "width": i64::from(rect.right) - i64::from(rect.left), "height": i64::from(rect.bottom) - i64::from(rect.top),
            "normal_screen": {"x":i64::from(normal.left) + dx,"y":i64::from(normal.top) + dy,"width":i64::from(normal.right) - i64::from(normal.left),"height":i64::from(normal.bottom) - i64::from(normal.top)},
            "placement": {"show_cmd":show,"min_x":min_x,"min_y":min_y,"max_x":max_x,"max_y":max_y,"left":normal.left,"top":normal.top,"right":normal.right,"bottom":normal.bottom}
        });
        if show == 2 && placement.flags.contains(WPF_RESTORETOMAXIMIZED) {
            snapshot["placement"]["restore_maximized"] = json!(true);
        }
        snapshots.push(snapshot);
    }
    Ok(snapshots)
}

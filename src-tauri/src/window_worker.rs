//! One bounded window mutation per owned, cancellable process. No shell startup.
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::{Read, Write};

pub(crate) const RESULT_PREFIX: &str = "ALFREDPET_WINDOW_RESULT:";
const INPUT_LIMIT: u64 = 64 * 1024;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    expected: Value,
    action: String,
    input: Option<Value>,
    restore: Option<Value>,
    #[serde(default)]
    restore_to_normal: bool,
}

pub(crate) fn entry() -> i32 {
    let result = (|| {
        let mut data = Vec::new();
        std::io::stdin()
            .lock()
            .take(INPUT_LIMIT + 1)
            .read_to_end(&mut data)
            .map_err(|_| "WINDOW_WORKER_INPUT")?;
        if data.len() as u64 > INPUT_LIMIT {
            return Err("WINDOW_WORKER_INPUT_LIMIT".to_string());
        }
        let request: Request =
            serde_json::from_slice(&data).map_err(|_| "WINDOW_WORKER_REQUEST")?;
        change(request)
    })();
    let response = match result {
        Ok(data) => json!({"ok":true,"data":data}),
        Err(error) => json!({"ok":false,"error":error}),
    };
    // A fixed frame also separates the response from the test harness banner.
    let mut output = std::io::stdout().lock();
    if writeln!(output, "\n{RESULT_PREFIX}{response}")
        .and_then(|_| output.flush())
        .is_err()
    {
        return 1;
    }
    0
}

#[cfg(not(windows))]
fn change(_: Request) -> Result<Value, String> {
    Err("WINDOWS_11_REQUIRED".into())
}

#[cfg(windows)]
fn change(request: Request) -> Result<Value, String> {
    use windows::Win32::{
        Foundation::{HWND, POINT, RECT},
        UI::{
            HiDpi::{
                SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT,
                DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
            },
            WindowsAndMessaging::{
                SetWindowPlacement, SetWindowPos, ShowWindow, SWP_NOACTIVATE, SWP_NOZORDER,
                SW_MAXIMIZE, SW_MINIMIZE, SW_RESTORE, SW_SHOWNOACTIVATE, WINDOWPLACEMENT,
                WINDOWPLACEMENT_FLAGS,
            },
        },
    };
    struct Dpi(DPI_AWARENESS_CONTEXT);
    impl Drop for Dpi {
        fn drop(&mut self) {
            unsafe { SetThreadDpiAwarenessContext(self.0) };
        }
    }
    fn integer(value: &Value, field: &str) -> Result<i32, String> {
        let n = value[field].as_i64().ok_or("WINDOW_BOUNDS")?;
        if !(-100000..=100000).contains(&n) {
            return Err("WINDOW_BOUNDS".into());
        }
        i32::try_from(n).map_err(|_| "WINDOW_BOUNDS".into())
    }
    crate::window_undo::snapshot(&request.expected, true)?;
    let input = request.input.as_ref().unwrap_or(&Value::Null);
    let mut placement = None;
    let mut bounds = None;
    let mut state = None;
    match request.action.as_str() {
        "restore_snapshot" => {
            if request.input.is_some() || request.restore_to_normal {
                return Err("WINDOW_WORKER_REQUEST".into());
            }
            let restore = request.restore.as_ref().ok_or("WINDOW_SAVED_STATE")?;
            crate::window_undo::snapshot(restore, true)?;
            for key in ["hwnd", "process_id", "process_started", "window_identity"] {
                if restore[key] != request.expected[key] {
                    return Err("WINDOW_IDENTITY_CHANGED".into());
                }
            }
            let saved = &restore["placement"];
            let show = saved["show_cmd"].as_u64().ok_or("WINDOW_SAVED_STATE")?;
            let latent = saved["restore_maximized"] == true;
            placement = Some(WINDOWPLACEMENT {
                length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
                flags: WINDOWPLACEMENT_FLAGS(if show == 2 {
                    1 | if latent { 2 } else { 0 }
                } else {
                    0
                }),
                showCmd: match show {
                    1 => 4,
                    2 => {
                        if latent {
                            2
                        } else {
                            7
                        }
                    }
                    3 => 3,
                    _ => return Err("WINDOW_SAVED_STATE".into()),
                },
                ptMinPosition: POINT {
                    x: integer(saved, "min_x")?,
                    y: integer(saved, "min_y")?,
                },
                ptMaxPosition: POINT {
                    x: integer(saved, "max_x")?,
                    y: integer(saved, "max_y")?,
                },
                rcNormalPosition: RECT {
                    left: integer(saved, "left")?,
                    top: integer(saved, "top")?,
                    right: integer(saved, "right")?,
                    bottom: integer(saved, "bottom")?,
                },
            });
        }
        "move" | "state" => {
            if request.restore.is_some() || input["hwnd"] != request.expected["hwnd"] {
                return Err("WINDOW_TARGET_CHANGED".into());
            }
            for key in ["process_id", "process_started", "window_identity"] {
                if let Some(value) = input.get(key).filter(|v| !v.is_null()) {
                    if value != &request.expected[key] && value != 0 && value != "" {
                        return Err("WINDOW_IDENTITY_CHANGED".into());
                    }
                }
            }
            if request.action == "move" {
                let b = [
                    integer(input, "x")?,
                    integer(input, "y")?,
                    integer(input, "width")?,
                    integer(input, "height")?,
                ];
                if b[2] < 1 || b[3] < 1 {
                    return Err("WINDOW_BOUNDS".into());
                }
                if input["process_started"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty())
                    && !request.restore_to_normal
                    && request.expected["placement"]["show_cmd"] != 1
                {
                    return Err("LAYOUT_WINDOW_NOT_NORMAL".into());
                }
                bounds = Some(b);
            } else {
                state = Some(match input["state"].as_str() {
                    Some("minimize") => SW_MINIMIZE,
                    Some("maximize") => SW_MAXIMIZE,
                    Some("restore") => SW_RESTORE,
                    _ => return Err("WINDOW_STATE".into()),
                });
            }
        }
        _ => return Err("WINDOW_RECOVERY_ACTION".into()),
    }
    let previous =
        unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    if previous.0.is_null() {
        return Err("WINDOW_DPI_CONTEXT".into());
    }
    let _dpi = Dpi(previous);
    let before = crate::window_snapshot::capture(&json!([request.expected]), false)?.remove(0);
    if before != request.expected {
        return Err("WINDOW_CHANGED_REVIEW_REQUIRED".into());
    }
    let hwnd = HWND(before["hwnd"].as_i64().ok_or("WINDOW_TARGET_MISSING")? as usize as *mut _);
    let outcome: Result<(), String> = (|| {
        if let Some(placement) = placement {
            unsafe { SetWindowPlacement(hwnd, &placement) }.map_err(|_| "WINDOW_RESTORE_FAILED")?;
        } else {
            if request.restore_to_normal && before["placement"]["show_cmd"] != 1 {
                let _ = unsafe { ShowWindow(hwnd, SW_SHOWNOACTIVATE) };
            }
            if let Some([x, y, width, height]) = bounds {
                // Recheck identity after restoring state and immediately before positioning.
                let current = crate::window_snapshot::capture(&json!([before]), false)?.remove(0);
                if input["process_started"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty())
                    && current["placement"]["show_cmd"] != 1
                {
                    return Err("LAYOUT_WINDOW_NOT_NORMAL".into());
                }
                unsafe {
                    SetWindowPos(
                        hwnd,
                        None,
                        x,
                        y,
                        width,
                        height,
                        SWP_NOACTIVATE | SWP_NOZORDER,
                    )
                }
                .map_err(|_| "WINDOW_MOVE_FAILED")?;
            } else if let Some(code) = state {
                let _ = unsafe { ShowWindow(hwnd, code) };
            }
        }
        Ok(())
    })();
    let after = crate::window_snapshot::capture(&json!([before]), false)?.remove(0);
    Ok(json!({"before":before,"after":after,"operation_error":outcome.err()}))
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "internal owned window worker; invoked by recovery tests"]
    fn worker_child() {
        std::process::exit(super::entry());
    }
}

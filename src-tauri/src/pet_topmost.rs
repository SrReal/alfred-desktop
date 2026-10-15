use crate::runtime::Core;
use std::sync::Arc;
use tauri::Manager;

pub async fn maintain(core: Arc<Core>) {
    let mut timer = tokio::time::interval(std::time::Duration::from_millis(750));
    loop {
        timer.tick().await;
        let app = core.app.lock().ok().and_then(|a| a.clone());
        let Some(pet) = app.and_then(|a| a.get_webview_window("pet")) else {
            continue;
        };
        if core.store.setting("always_on_top") == false {
            continue;
        }
        if pet.is_visible().unwrap_or(false) && !pet.is_minimized().unwrap_or(false) {
            if let Err(error) = raise(&pet) {
                tracing::debug!(code="PET_TOPMOST", %error);
            }
        }
    }
}

#[cfg(windows)]
fn raise(pet: &tauri::WebviewWindow) -> Result<(), String> {
    use windows::Win32::{
        Foundation::HWND,
        UI::WindowsAndMessaging::{
            SetWindowPos, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE,
        },
    };
    let hwnd = HWND(pet.hwnd().map_err(|_| "PET_HWND")?.0);
    // Raise within the topmost band too, without focus, movement or activation.
    unsafe {
        SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_NOOWNERZORDER,
        )
    }
    .map_err(|_| "PET_TOPMOST".into())
}
#[cfg(not(windows))]
fn raise(pet: &tauri::WebviewWindow) -> Result<(), String> {
    pet.set_always_on_top(true).map_err(|e| e.to_string())
}

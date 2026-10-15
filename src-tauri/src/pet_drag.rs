//! Wait for the OS gesture, not the asynchronous startDragging acknowledgement.
pub async fn finish(window: &tauri::WebviewWindow) -> Result<(), String> {
    #[cfg(windows)]
    {
        use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
        let pressed = || unsafe { GetAsyncKeyState(VK_LBUTTON.0 as i32) < 0 };
        if !pressed() {
            return Err("PET_DRAG_BUTTON_REQUIRED".into());
        }
        window.start_dragging().map_err(|_| "PET_DRAG_START")?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
        let mut released = Release::default();
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(16)).await;
            if released.sample(pressed()) {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                return Err("PET_DRAG_TIMEOUT".into());
            }
        }
    }
    #[cfg(not(windows))]
    {
        window.start_dragging().map_err(|_| "PET_DRAG_START".into())
    }
}
#[derive(Default)]
struct Release(u8);
impl Release {
    fn sample(&mut self, pressed: bool) -> bool {
        self.0 = if pressed { 0 } else { self.0 + 1 };
        self.0 >= 3
    }
}
#[cfg(test)]
mod tests {
    use super::Release;
    #[test]
    fn holding_and_transient_release_do_not_start_gravity() {
        let mut release = Release::default();
        for _ in 0..100 {
            assert!(!release.sample(true));
        }
        assert!(!release.sample(false));
        assert!(!release.sample(true));
        assert!(!release.sample(false));
        assert!(!release.sample(false));
        assert!(release.sample(false));
    }
}

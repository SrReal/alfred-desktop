//! Presentation intent survives a slow first WebView load and close/show races.
#[derive(Default)]
pub(crate) struct SurfaceState {
    pub ready: bool,
    pub visible: bool,
    pub view: String,
}

impl SurfaceState {
    pub fn select(&mut self, view: &str) -> bool {
        if self.view == view {
            return false;
        }
        self.view = view.into();
        self.ready = false;
        true
    }

    pub fn rendered(&mut self, view: &str) -> bool {
        if self.view != view {
            return false;
        }
        self.mark_ready()
    }
    pub fn request_show(&mut self) -> bool {
        self.visible = true;
        self.ready
    }

    pub fn toggle(&mut self) -> bool {
        self.visible = !self.visible;
        self.visible && self.ready
    }

    pub fn mark_ready(&mut self) -> bool {
        self.ready = true;
        self.visible
    }

    pub fn hide(&mut self) {
        self.visible = false;
    }
}

#[cfg(test)]
mod tests {
    use super::SurfaceState;

    #[test]
    fn a_second_click_cancels_a_pending_open() {
        let mut state = SurfaceState::default();
        assert!(!state.toggle());
        assert!(state.visible);
        assert!(!state.toggle());
        assert!(!state.mark_ready());
        assert!(state.toggle());
    }

    #[test]
    fn prewarming_and_native_close_never_reopen_a_window() {
        let mut state = SurfaceState::default();
        assert!(!state.mark_ready());
        assert!(state.request_show());
        state.hide();
        assert!(!state.mark_ready());
        assert!(state.request_show());
    }

    #[test]
    fn navigation_rejects_stale_ready_notifications_and_reuses_visibility() {
        let mut state = SurfaceState::default();
        assert!(state.select("chat"));
        state.request_show();
        assert!(state.rendered("chat"));
        assert!(state.select("settings"));
        assert!(!state.rendered("chat"));
        assert!(!state.ready);
        assert!(state.rendered("settings"));
        assert!(!state.select("settings"));
        state.hide();
        assert!(!state.rendered("settings"));
    }
}

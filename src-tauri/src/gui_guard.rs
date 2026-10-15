//! Tracks only tagged, injected button events for one bounded GUI action.
//! The callback never records coordinates, reads keys or suppresses user input.
#[cfg(windows)]
mod native {
    use std::{
        cell::RefCell,
        sync::{
            atomic::{AtomicBool, Ordering},
            mpsc, Arc,
        },
        time::Duration,
    };
    use windows::Win32::{
        Foundation::{LPARAM, LRESULT, WPARAM},
        System::Threading::GetCurrentThreadId,
        UI::{
            Input::KeyboardAndMouse::{
                SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_LEFTUP, MOUSEINPUT,
            },
            WindowsAndMessaging::*,
        },
    };
    pub struct State {
        tag: usize,
        down: AtomicBool,
    }
    impl State {
        fn observe(&self, tag: usize, injected: bool, message: u32) {
            if tag == self.tag && injected {
                if message == WM_LBUTTONDOWN {
                    self.down.store(true, Ordering::SeqCst);
                }
                if message == WM_LBUTTONUP {
                    self.down.store(false, Ordering::SeqCst);
                }
            }
        }
        fn release(&self, send: impl FnOnce(usize) -> bool) -> bool {
            if self.down.swap(false, Ordering::SeqCst) {
                return send(self.tag);
            }
            true
        }
    }
    thread_local! {static ACTIVE:RefCell<Option<Arc<State>>>=const{RefCell::new(None)};}
    unsafe extern "system" fn hook(code: i32, message: WPARAM, data: LPARAM) -> LRESULT {
        if code == 0 && data.0 != 0 {
            let event = unsafe { &*(data.0 as *const MSLLHOOKSTRUCT) };
            ACTIVE.with(|slot| {
                if let Some(state) = slot.borrow().as_ref() {
                    state.observe(event.dwExtraInfo, event.flags & 1 != 0, message.0 as u32);
                }
            });
        }
        unsafe { CallNextHookEx(None, code, message, data) }
    }
    fn release_button(tag: usize) -> bool {
        let packet = INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dwFlags: MOUSEEVENTF_LEFTUP,
                    dwExtraInfo: tag,
                    ..Default::default()
                },
            },
        };
        unsafe { SendInput(&[packet], std::mem::size_of::<INPUT>() as i32) == 1 }
    }
    pub struct Guard {
        state: Arc<State>,
        thread: u32,
    }
    impl Guard {
        pub fn new(lease: Option<tokio::sync::OwnedMutexGuard<()>>) -> Result<Self, String> {
            if usize::BITS != 64 {
                return Err("INPUT_GUARD_REQUIRES_64_BIT_WINDOWS".into());
            }
            let random = uuid::Uuid::new_v4();
            let mut bytes = [0u8; 8];
            bytes.copy_from_slice(&random.as_bytes()[..8]);
            let tag =
                ((u64::from_le_bytes(bytes) & 0x0000ffffffffffff) | 0x411f000000000000) as usize;
            let state = Arc::new(State {
                tag,
                down: AtomicBool::new(false),
            });
            let shared = state.clone();
            let (ready_tx, ready_rx) = mpsc::sync_channel(1);
            std::thread::Builder::new()
                .name("alfredpet-input-guard".into())
                .spawn(move || {
                    let _lease = lease;
                    let mut message = MSG::default();
                    unsafe {
                        let _ = PeekMessageW(&mut message, None, 0, 0, PM_NOREMOVE);
                    }
                    ACTIVE.with(|slot| *slot.borrow_mut() = Some(shared));
                    let installed = unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(hook), None, 0) };
                    match installed {
                        Ok(handle) => {
                            if ready_tx.send(Ok(unsafe { GetCurrentThreadId() })).is_ok() {
                                while unsafe { GetMessageW(&mut message, None, 0, 0) }.0 > 0 {
                                    unsafe {
                                        let _ = TranslateMessage(&message);
                                        DispatchMessageW(&message);
                                    }
                                }
                            }
                            // GetMessage dispatches pending sent hook callbacks before
                            // the posted WM_QUIT. Cleanup here observes a down packet
                            // queued immediately before the helper was terminated.
                            ACTIVE.with(|slot| {
                                let state = slot.borrow().clone();
                                if let Some(state) = state {
                                    if !state.release(release_button) {
                                        tracing::error!("INPUT_CANCEL_RELEASE_DENIED");
                                    }
                                }
                            });
                            let _ = unsafe { UnhookWindowsHookEx(handle) };
                        }
                        Err(_) => {
                            let _ = ready_tx.send(Err("INPUT_GUARD_UNAVAILABLE".to_string()));
                        }
                    }
                    ACTIVE.with(|slot| {
                        slot.borrow_mut().take();
                    });
                })
                .map_err(|_| "INPUT_GUARD_THREAD")?;
            let thread = ready_rx
                .recv_timeout(Duration::from_secs(3))
                .map_err(|_| "INPUT_GUARD_TIMEOUT")??;
            Ok(Self { state, thread })
        }
        pub fn tag(&self) -> u64 {
            self.state.tag as u64
        }
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            // Dropping process::output requests helper termination through its
            // Job Object. The hook thread drains sent callbacks before cleanup;
            // full application crash recovery still needs an external watchdog.
            let _ = unsafe { PostThreadMessageW(self.thread, WM_QUIT, WPARAM(0), LPARAM(0)) };
        }
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn only_our_injected_events_arm_cleanup() {
            let state = State {
                tag: 42,
                down: AtomicBool::new(false),
            };
            state.observe(41, true, WM_LBUTTONDOWN);
            state.observe(42, false, WM_LBUTTONDOWN);
            assert!(state.release(|_| panic!("foreign input must never be released")));
            state.observe(42, true, WM_LBUTTONDOWN);
            assert!(state.down.load(Ordering::SeqCst));
            assert!(state.release(|tag| tag == 42));
            assert!(!state.down.load(Ordering::SeqCst));
            state.observe(42, true, WM_LBUTTONDOWN);
            state.observe(42, true, WM_LBUTTONUP);
            assert!(state.release(|_| panic!("already released")));
        }
        #[test]
        fn real_hook_installs_and_tears_down_without_injecting_input() {
            let guard = Guard::new(None).unwrap();
            assert!(guard.tag() > 0);
            assert!(!guard.state.down.load(Ordering::SeqCst));
            drop(guard);
        }
        #[tokio::test]
        async fn input_lease_is_held_until_hook_cleanup_finishes() {
            let mutex = Arc::new(tokio::sync::Mutex::new(()));
            let guard = Guard::new(Some(mutex.clone().lock_owned().await)).unwrap();
            assert!(mutex.try_lock().is_err());
            drop(guard);
            let _lease = tokio::time::timeout(Duration::from_secs(5), mutex.lock())
                .await
                .unwrap();
        }
    }
}
#[cfg(windows)]
pub use native::Guard;

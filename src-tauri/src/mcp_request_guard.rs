//! Private per-request authority, including cancellation/drop cleanup. It
//! contains no serializable credentials and is not an approval by itself.
use std::{future::Future, sync::Arc, time::Duration};
#[derive(Clone)]
pub(crate) struct Guard(Arc<dyn Fn() -> Result<(), String> + Send + Sync>);
impl Guard {
    pub(crate) fn new(check: impl Fn() -> Result<(), String> + Send + Sync + 'static) -> Self {
        Self(Arc::new(check))
    }
    pub(crate) fn check(&self) -> Result<(), String> {
        (self.0)()
    }
    pub(crate) async fn run<T>(
        guard: Option<&Self>,
        future: impl Future<Output = Result<T, String>>,
    ) -> Result<T, String> {
        let Some(guard) = guard else {
            return future.await;
        };
        guard.check()?;
        tokio::pin!(future);
        let mut tick = tokio::time::interval(Duration::from_millis(50));
        loop {
            tokio::select! {biased; _=tick.tick()=>guard.check()?, result=&mut future=>{guard.check()?;return result;}}
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    #[tokio::test]
    async fn revocation_before_during_and_at_completion_denies_result() {
        let live = Arc::new(AtomicBool::new(false));
        let check = live.clone();
        let guard = Guard::new(move || {
            if check.load(Ordering::SeqCst) {
                Ok(())
            } else {
                Err("OWNED_REVOKED".into())
            }
        });
        let entered = AtomicBool::new(false);
        assert_eq!(
            Guard::run(Some(&guard), async {
                entered.store(true, Ordering::SeqCst);
                Ok(())
            })
            .await
            .unwrap_err(),
            "OWNED_REVOKED"
        );
        assert!(!entered.load(Ordering::SeqCst));
        live.store(true, Ordering::SeqCst);
        let cancel = async {
            tokio::time::sleep(Duration::from_millis(25)).await;
            live.store(false, Ordering::SeqCst);
        };
        let (result, _) = tokio::join!(
            Guard::run(Some(&guard), async {
                tokio::time::sleep(Duration::from_secs(1)).await;
                Ok(())
            }),
            cancel
        );
        assert_eq!(result.unwrap_err(), "OWNED_REVOKED");
        live.store(true, Ordering::SeqCst);
        assert_eq!(
            Guard::run(Some(&guard), async {
                live.store(false, Ordering::SeqCst);
                Ok(())
            })
            .await
            .unwrap_err(),
            "OWNED_REVOKED"
        );
    }
}

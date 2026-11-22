//! Result checks under the job's deadline (ADR 0016).
//!
//! Converting and checking a CAS answer is CPU-bound Rust code, run on a blocking
//! thread. [`check_result`] runs it under the same deadline and cancellation as
//! the rest of the job: when the deadline passes, the job fails with `TIMEOUT` at
//! once, never with an unchecked or late success.
//!
//! A blocking thread cannot be stopped from outside. Two things bound what a check
//! that overran still uses:
//! * The check is asked to stop through an [`Interrupt`], which it polls at its
//!   checkpoints (between factors, between series); it stops at the next one.
//! * At most [`max_concurrent_checks`] checks run at the same time in a process.
//!   A check that overran keeps its permit until it stops, so abandoned checks
//!   never occupy more threads than that; a new check waits for a permit, within
//!   its own deadline.

use std::sync::{Arc, OnceLock};

use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::backend::{CasError, ExecutionContext};

/// Asks a running check to stop. The check polls it with [`Interrupt::checkpoint`].
#[derive(Debug, Clone, Default)]
pub struct Interrupt(CancellationToken);

impl Interrupt {
    /// An interrupt that is never raised, for checks run outside a job.
    pub fn none() -> Self {
        Self::default()
    }

    /// Fails once the check should stop: its job's deadline passed or the job was
    /// cancelled. Nobody waits for the error; it only ends the check early.
    pub fn checkpoint(&self) -> Result<(), CasError> {
        if self.0.is_cancelled() {
            Err(CasError::Cancelled)
        } else {
            Ok(())
        }
    }

    fn raise(&self) {
        self.0.cancel();
    }
}

/// The number of result checks that may run at the same time in this process:
/// the number of CPUs.
pub fn max_concurrent_checks() -> usize {
    std::thread::available_parallelism().map_or(4, std::num::NonZero::get)
}

fn permits() -> Arc<Semaphore> {
    static PERMITS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    PERMITS
        .get_or_init(|| Arc::new(Semaphore::new(max_concurrent_checks())))
        .clone()
}

/// Runs `check` on a blocking thread under the deadline and cancellation of
/// `ctx`. Returns `TIMEOUT` when the deadline passes and `CANCELLED` when the job
/// is cancelled, whether the check is waiting for a permit or running; the check
/// is then interrupted and its result, if any, discarded.
pub async fn check_result<T: Send + 'static>(
    ctx: &ExecutionContext,
    check: impl FnOnce(&Interrupt) -> Result<T, CasError> + Send + 'static,
) -> Result<T, CasError> {
    let interrupt = Interrupt::default();
    let work = {
        let interrupt = interrupt.clone();
        async move {
            let permit = permits()
                .acquire_owned()
                .await
                .map_err(|e| CasError::Unavailable(format!("result check: {e}")))?;
            tokio::task::spawn_blocking(move || {
                // Held until the check returns, also after the job gave up on it.
                let _permit = permit;
                check(&interrupt)
            })
            .await
            .map_err(|e| CasError::UnsupportedResult(format!("result check failed: {e}")))?
        }
    };
    // Biased: a check that ends as the deadline passes is a timeout, not a late
    // success.
    tokio::select! {
        biased;
        () = ctx.cancel.cancelled() => {
            interrupt.raise();
            Err(CasError::Cancelled)
        }
        () = ctx.expired() => {
            interrupt.raise();
            Err(ctx.timed_out())
        }
        r = work => r,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn a_check_within_the_deadline_returns_its_result() {
        let ctx = ExecutionContext::new(Some(Duration::from_secs(10)));
        assert_eq!(check_result(&ctx, |_| Ok(7)).await, Ok(7));
        let failed: Result<(), _> =
            check_result(&ctx, |_| Err(CasError::UnsupportedResult("no".into()))).await;
        assert_eq!(failed, Err(CasError::UnsupportedResult("no".into())));
    }

    #[tokio::test]
    async fn a_check_past_the_deadline_times_out_and_is_interrupted() {
        let ctx = ExecutionContext::new(Some(Duration::from_millis(200)));
        let stopped = Arc::new(AtomicBool::new(false));
        let seen = stopped.clone();
        let started = std::time::Instant::now();
        let result = check_result(&ctx, move |interrupt| {
            // A long check with a checkpoint every 10 ms.
            for _ in 0..1000 {
                if interrupt.checkpoint().is_err() {
                    seen.store(true, Ordering::SeqCst);
                    return Err(CasError::Cancelled);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(())
        })
        .await;
        assert_eq!(result, Err(CasError::Timeout(0)));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
        // The check stops at its next checkpoint.
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(stopped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn a_cancelled_job_cancels_its_check() {
        let ctx = ExecutionContext::new(None);
        let cancel = ctx.cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            cancel.cancel();
        });
        let result: Result<(), _> = check_result(&ctx, |interrupt| {
            loop {
                interrupt.checkpoint()?;
                std::thread::sleep(Duration::from_millis(5));
            }
        })
        .await;
        assert_eq!(result, Err(CasError::Cancelled));
    }

    #[tokio::test]
    async fn an_expired_deadline_times_out_before_the_check_starts() {
        let ctx = ExecutionContext::new(Some(Duration::ZERO));
        let ran = Arc::new(AtomicBool::new(false));
        let flag = ran.clone();
        tokio::time::sleep(Duration::from_millis(10)).await;
        let result = check_result(&ctx, move |_| {
            flag.store(true, Ordering::SeqCst);
            Ok(())
        })
        .await;
        assert_eq!(result, Err(CasError::Timeout(0)));
    }
}

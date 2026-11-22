use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;

use crate::backend::{CasBackend, CasError, CasRequest, CasResult, ExecutionContext};
use crate::check::check_result;
use crate::guarantee::{EvidenceLevel, ResultVerifier};
use crate::job::{JobId, JobSpec};
use crate::provenance::JobRecord;
use crate::state::JobState;

/// Runs one job on `backend` in this process, checks its result with
/// `verifier`, and returns its provenance record.
///
/// The backend must support the operation; otherwise the job ends `FAILED` with
/// `UNSUPPORTED_OPERATION` without contacting the CAS.
pub async fn run_job(
    backend: &dyn CasBackend,
    spec: JobSpec,
    ctx: ExecutionContext,
    verifier: &Arc<dyn ResultVerifier>,
) -> JobRecord {
    let job_id = JobId::new();
    let mut record = JobRecord::new(job_id, spec.clone(), Utc::now());
    // These transitions are valid from QUEUED by construction.
    let _ = record.transition(JobState::Assigned, Utc::now());
    let _ = record.transition(JobState::Running, Utc::now());
    let outcome = execute(backend, job_id, &spec, ctx, verifier).await;
    let _ = record.complete(outcome, Utc::now());
    record
}

/// A backend gets this long after the deadline to stop on its own before the
/// runner drops it.
const STOP_GRACE: Duration = Duration::from_secs(5);

/// Checks that `backend` supports the job, applies the wall-clock limit of the spec,
/// executes it and checks the result with `verifier`. Shared by in-process runs
/// and worker agents.
///
/// The limit covers the whole execution, result check included (ADR 0016): the
/// deadline is fixed here, when the job starts, and a result that arrives after
/// it is `TIMEOUT`, never `SUCCEEDED`. A backend that does not stop by itself is
/// dropped [`STOP_GRACE`] after the deadline.
///
/// The guarantee of a result comes from `verifier` alone (ADR 0018): one the
/// backend wrote itself is discarded, and a backend that claims more than
/// `asserted` for itself has its result refused.
pub async fn execute(
    backend: &dyn CasBackend,
    job_id: JobId,
    spec: &JobSpec,
    ctx: ExecutionContext,
    verifier: &Arc<dyn ResultVerifier>,
) -> Result<CasResult, CasError> {
    let capabilities = backend.capabilities();
    if spec.backend != backend.name() || !capabilities.supports(&spec.operation) {
        return Err(CasError::UnsupportedOperation {
            backend: spec.backend.clone(),
            operation: spec.operation.clone(),
        });
    }
    // A limit that would not hold is refused, not silently ignored.
    let unenforced = capabilities.unenforced(&spec.limits);
    if !unenforced.is_empty() {
        return Err(CasError::InvalidArguments(format!(
            "backend {} does not enforce limits.{}",
            spec.backend,
            unenforced.join(", limits.")
        )));
    }
    let timeout = ctx
        .timeout
        .or(spec.limits.wall_seconds.map(Duration::from_secs));
    let deadline = match (ctx.deadline, timeout) {
        (Some(at), _) => Some(at),
        (None, None) => None,
        (None, Some(t)) => Some(tokio::time::Instant::now().checked_add(t).ok_or_else(|| {
            CasError::InvalidArguments(format!("a deadline {t:?} from now cannot be represented"))
        })?),
    };
    let ctx = ExecutionContext {
        timeout,
        cancel: ctx.cancel,
        deadline,
    };
    let request = CasRequest {
        job_id,
        operation: spec.operation.clone(),
        arguments: spec.arguments.clone(),
        limits: spec.limits.clone(),
    };
    let grace = async {
        match ctx.deadline {
            Some(at) => match at.checked_add(STOP_GRACE) {
                Some(end) => tokio::time::sleep_until(end).await,
                None => std::future::pending().await,
            },
            None => std::future::pending().await,
        }
    };
    let outcome = tokio::select! {
        r = backend.execute(request, ctx.clone()) => r,
        () = grace => return Err(ctx.timed_out()),
    };
    let mut result = match outcome {
        Ok(_) if ctx.is_expired() => return Err(ctx.timed_out()),
        // A backend may notice the deadline as a cancellation of its own work.
        Err(CasError::Cancelled) if ctx.is_expired() && !ctx.cancel.is_cancelled() => {
            return Err(ctx.timed_out());
        }
        other => other?,
    };
    if let Some(own) = result.guarantee.take()
        && let Some(claim) = own
            .claims
            .iter()
            .find(|c| c.level != EvidenceLevel::Asserted)
    {
        return Err(CasError::UnsupportedResult(format!(
            "backend {} claimed {} at the level {:?} for itself; only the verifier \
             checks results",
            backend.name(),
            claim.id,
            claim.level
        )));
    }
    let verifier = verifier.clone();
    let spec = spec.clone();
    check_result(&ctx, move |interrupt| {
        verifier.verify(&spec, result, interrupt)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use crate::backend::{BackendInfo, Capabilities};
    use crate::check::Interrupt;
    use crate::guarantee::{Claim, Guarantee, Scope, VerifierInfo};

    /// Writes one asserted claim; with `test.slow_verify`, checks for a long
    /// time and stops at a checkpoint when interrupted.
    struct Recorder;

    impl ResultVerifier for Recorder {
        fn verify(
            &self,
            spec: &JobSpec,
            mut result: CasResult,
            interrupt: &Interrupt,
        ) -> Result<CasResult, CasError> {
            if spec.operation == "test.slow_verify" {
                for _ in 0..3000 {
                    if interrupt.checkpoint().is_err() {
                        VERIFY_STOPPED.store(true, Ordering::SeqCst);
                        return Err(CasError::Cancelled);
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            result.guarantee = Some(Guarantee::new(
                info("recorder"),
                None,
                vec![claim("value", EvidenceLevel::Asserted)],
                vec![],
            ));
            Ok(result)
        }
    }

    fn info(name: &str) -> VerifierInfo {
        VerifierInfo {
            name: name.into(),
            version: "1".into(),
            commit: None,
        }
    }

    fn claim(id: &str, level: EvidenceLevel) -> Claim {
        Claim {
            id: id.into(),
            level,
            method: None,
            by: "echo".into(),
            scope: Scope::Exact,
            depends_on: vec![],
            reason: None,
        }
    }

    fn verifier() -> Arc<dyn ResultVerifier> {
        Arc::new(Recorder)
    }

    static VERIFY_STOPPED: std::sync::LazyLock<Arc<AtomicBool>> =
        std::sync::LazyLock::new(|| Arc::new(AtomicBool::new(false)));

    struct Echo;

    #[async_trait::async_trait]
    impl CasBackend for Echo {
        fn name(&self) -> &'static str {
            "echo"
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities {
                operations: [
                    "test.echo",
                    "test.slow",
                    "test.late",
                    "test.slow_check",
                    "test.stuck",
                    "test.slow_verify",
                    "test.claims_checked",
                    "test.claims_asserted",
                ]
                .map(String::from)
                .to_vec(),
                ..Capabilities::default()
            }
        }
        async fn execute(
            &self,
            job: CasRequest,
            ctx: ExecutionContext,
        ) -> Result<CasResult, CasError> {
            match job.operation.as_str() {
                "test.slow" => {
                    ctx.cancel.cancelled().await;
                    return Err(CasError::Cancelled);
                }
                // Answers after the deadline without looking at it.
                "test.late" => tokio::time::sleep(Duration::from_millis(1500)).await,
                // The CAS answers at once; checking the answer takes long and
                // stops at a checkpoint when interrupted.
                "test.slow_check" => {
                    let stopped = STOPPED.clone();
                    crate::check::check_result(&ctx, move |interrupt| {
                        for _ in 0..3000 {
                            if interrupt.checkpoint().is_err() {
                                stopped.store(true, Ordering::SeqCst);
                                return Err(CasError::Cancelled);
                            }
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        Ok(())
                    })
                    .await?;
                }
                // Never answers and ignores the deadline and cancellation.
                "test.stuck" => std::future::pending::<()>().await,
                _ => {}
            }
            // A guarantee the backend writes itself, which the runner must not
            // keep.
            let own = match job.operation.as_str() {
                "test.claims_checked" => Some(EvidenceLevel::Checked),
                "test.claims_asserted" => Some(EvidenceLevel::Asserted),
                _ => None,
            }
            .map(|level| Guarantee::new(info("echo"), None, vec![claim("own", level)], vec![]));
            Ok(CasResult {
                value: job.arguments,
                backend: BackendInfo {
                    name: "echo".into(),
                    backend_version: Some("1".into()),
                    driver_version: "0".into(),
                    build: None,
                    ..BackendInfo::default()
                },
                execution_host: Some("localhost".into()),
                log: vec![],
                guarantee: own,
            })
        }
    }

    static STOPPED: std::sync::LazyLock<Arc<AtomicBool>> =
        std::sync::LazyLock::new(|| Arc::new(AtomicBool::new(false)));

    fn limited(operation: &str, wall_seconds: u64) -> JobSpec {
        let mut spec = spec(operation);
        spec.limits.wall_seconds = Some(wall_seconds);
        spec
    }

    fn spec(operation: &str) -> JobSpec {
        JobSpec {
            schema_version: 1,
            backend: "echo".into(),
            operation: operation.into(),
            arguments: serde_json::json!({"n": 1}),
            limits: Default::default(),
            retry_policy: Default::default(),
            environment: Default::default(),
        }
    }

    fn states(record: &JobRecord) -> Vec<JobState> {
        record.history.iter().map(|c| c.state).collect()
    }

    #[tokio::test]
    async fn success_records_history_and_hash() {
        let record = run_job(
            &Echo,
            spec("test.echo"),
            ExecutionContext::new(None),
            &verifier(),
        )
        .await;
        assert_eq!(record.state, JobState::Succeeded);
        assert_eq!(
            states(&record),
            [
                JobState::Queued,
                JobState::Assigned,
                JobState::Running,
                JobState::Succeeded
            ]
        );
        assert_eq!(record.result, Some(serde_json::json!({"n": 1})));
        assert_eq!(record.input_sha256.len(), 64);
        assert_eq!(record.backend.backend_version.as_deref(), Some("1"));
    }

    #[tokio::test]
    async fn a_limit_the_backend_does_not_enforce_is_refused() {
        let mut job = spec("test.echo");
        job.limits.memory_mb = Some(256);
        let record = run_job(&Echo, job, ExecutionContext::new(None), &verifier()).await;
        assert_eq!(record.state, JobState::Failed);
        assert!(
            matches!(&record.error, Some(CasError::InvalidArguments(m)) if m.contains("limits.memory_mb")),
            "{:?}",
            record.error
        );
    }

    #[tokio::test]
    async fn unsupported_operation_fails_without_execution() {
        let record = run_job(
            &Echo,
            spec("test.other"),
            ExecutionContext::new(None),
            &verifier(),
        )
        .await;
        assert_eq!(record.state, JobState::Failed);
        assert!(matches!(
            record.error,
            Some(CasError::UnsupportedOperation { .. })
        ));
    }

    #[tokio::test]
    async fn a_job_within_its_limit_succeeds() {
        let record = run_job(
            &Echo,
            limited("test.echo", 5),
            ExecutionContext::new(None),
            &verifier(),
        )
        .await;
        assert_eq!(record.state, JobState::Succeeded);
    }

    #[tokio::test]
    async fn an_answer_after_the_deadline_is_a_timeout() {
        let record = run_job(
            &Echo,
            limited("test.late", 1),
            ExecutionContext::new(None),
            &verifier(),
        )
        .await;
        assert_eq!(record.state, JobState::TimedOut, "{:?}", record.error);
        assert_eq!(record.error, Some(CasError::Timeout(1)));
        assert_eq!(record.result, None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_result_check_past_the_deadline_is_a_timeout() {
        // `wall_seconds` covers the result check (ADR 0016): a CAS that answers
        // at once does not make a 30 s check a success.
        let started = std::time::Instant::now();
        let record = run_job(
            &Echo,
            limited("test.slow_check", 1),
            ExecutionContext::new(None),
            &verifier(),
        )
        .await;
        assert_eq!(record.state, JobState::TimedOut, "{:?}", record.error);
        assert_eq!(record.error, Some(CasError::Timeout(1)));
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "{:?}",
            started.elapsed()
        );
        // The check was asked to stop and stopped at its next checkpoint.
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(STOPPED.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn a_backend_that_ignores_the_deadline_is_dropped() {
        let started = std::time::Instant::now();
        let record = run_job(
            &Echo,
            limited("test.stuck", 1),
            ExecutionContext::new(None),
            &verifier(),
        )
        .await;
        assert_eq!(record.state, JobState::TimedOut, "{:?}", record.error);
        let elapsed = started.elapsed();
        assert!(
            elapsed >= Duration::from_secs(1) + STOP_GRACE,
            "{elapsed:?}"
        );
        assert!(elapsed < Duration::from_secs(3) + STOP_GRACE, "{elapsed:?}");
    }

    #[tokio::test]
    async fn cancellation_goes_through_cancelling() {
        let ctx = ExecutionContext::new(None);
        ctx.cancel.cancel();
        let record = run_job(&Echo, spec("test.slow"), ctx, &verifier()).await;
        assert_eq!(record.state, JobState::Cancelled);
        assert!(states(&record).contains(&JobState::Cancelling));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_verification_past_the_deadline_is_a_timeout() {
        let started = std::time::Instant::now();
        let record = run_job(
            &Echo,
            limited("test.slow_verify", 1),
            ExecutionContext::new(None),
            &verifier(),
        )
        .await;
        assert_eq!(record.state, JobState::TimedOut, "{:?}", record.error);
        assert_eq!(record.error, Some(CasError::Timeout(1)));
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(record.guarantee, None);
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(VERIFY_STOPPED.load(Ordering::SeqCst));
    }

    /// ADR 0018: the guarantee comes from the verifier. A backend that claims
    /// more than asserted for itself has its result refused; an asserted
    /// guarantee of its own is replaced.
    #[tokio::test]
    async fn a_backend_does_not_vouch_for_its_own_result() {
        let record = run_job(
            &Echo,
            spec("test.claims_checked"),
            ExecutionContext::new(None),
            &verifier(),
        )
        .await;
        assert_eq!(record.state, JobState::Failed);
        assert!(
            matches!(&record.error, Some(CasError::UnsupportedResult(m)) if m.contains("own")),
            "{:?}",
            record.error
        );
        assert_eq!(record.result, None);

        let record = run_job(
            &Echo,
            spec("test.claims_asserted"),
            ExecutionContext::new(None),
            &verifier(),
        )
        .await;
        assert_eq!(record.state, JobState::Succeeded, "{:?}", record.error);
        let guarantee = record.guarantee.unwrap();
        assert_eq!(guarantee.verifier.name, "recorder");
        assert_eq!(guarantee.claims[0].id, "value");
    }
}

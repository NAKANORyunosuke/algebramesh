//! A heartbeat sweep whose store fails is tried again (Issue #86): the jobs of
//! a silent agent that the failed sweep left active are declared `LOST` by a
//! later sweep, without a restart of the scheduler.

#![allow(clippy::unwrap_used)]

use std::path::Path;
use std::time::Duration;

use algebramesh_core::JobState;
use algebramesh_rpc::pb;
use algebramesh_server::scheduler::SchedulerError;
use algebramesh_server::{Scheduler, SchedulerOptions, Store};

const TIMEOUT: Duration = Duration::from_millis(200);

fn spec() -> String {
    serde_json::json!({
        "schema_version": 1, "backend": "test", "operation": "test.op",
        "arguments": {}, "retry_policy": "if_deterministic",
    })
    .to_string()
}

fn register(scheduler: &Scheduler) -> String {
    let capabilities = serde_json::json!({"operations": ["test.op"], "supported_types": []});
    scheduler
        .register(
            pb::RegisterRequest {
                name: "worker".into(),
                host: "h".into(),
                agent_version: "0".into(),
                backends: vec![pb::BackendOffer {
                    name: "test".into(),
                    capabilities_json: capabilities.to_string(),
                    slots: 3,
                    ..Default::default()
                }],
                ..Default::default()
            },
            None,
        )
        .unwrap()
        .agent_id
}

/// Makes every transaction that declares `job_id` `LOST` fail in `db`, as a
/// full disk would, until [`heal`].
fn break_lost(db: &Path, job_id: &str) {
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.execute_batch(&format!(
        "CREATE TRIGGER injected_fault BEFORE UPDATE OF state ON jobs
         WHEN NEW.state = 'LOST' AND OLD.job_id = '{job_id}'
         BEGIN SELECT RAISE(ABORT, 'injected fault'); END;"
    ))
    .unwrap();
}

fn heal(db: &Path) {
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.execute_batch("DROP TRIGGER injected_fault;").unwrap();
}

/// One agent with three active jobs, the third with its cancellation
/// requested; the transaction that declares job `failing` `LOST` fails.
async fn sweep_fails_at(failing: usize) {
    let temp = tempfile::Builder::new()
        .prefix("algebramesh-sweep-")
        .tempdir()
        .unwrap();
    let dir = temp.path().to_path_buf();
    let db = dir.join("metadata.sqlite3");
    let options = SchedulerOptions {
        heartbeat_timeout: TIMEOUT,
        ..SchedulerOptions::default()
    };
    let scheduler = Scheduler::new(
        Store::open(Some(&db), Some(dir.join("artifacts"))).unwrap(),
        options,
    )
    .unwrap();
    let agent = register(&scheduler);
    let mut jobs = Vec::new();
    for _ in 0..3 {
        jobs.push(scheduler.submit(&spec(), 0, "").unwrap().job_id);
    }
    for job in &jobs {
        let assigned = scheduler
            .poll(&agent, &["test".to_string()], Duration::from_millis(100))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&assigned.job_id, job);
    }
    assert!(scheduler.cancel(&jobs[2]).unwrap().cancel_requested);

    // The agent falls silent; the sweep fails at the job `failing`.
    break_lost(&db, &jobs[failing]);
    tokio::time::sleep(TIMEOUT + Duration::from_millis(100)).await;
    for _ in 0..2 {
        let e = scheduler.sweep().unwrap_err();
        assert!(matches!(e, SchedulerError::Store(_)), "{e}");
    }
    for (i, job) in jobs.iter().enumerate() {
        let state = scheduler.status(job).unwrap().state;
        let expected = if i < failing { "LOST" } else { "ASSIGNED" };
        assert_eq!(state, expected, "job {i} after the failed sweeps");
    }

    // Once the store works again, the next sweep declares the rest LOST.
    heal(&db);
    scheduler.sweep().unwrap();
    for job in &jobs {
        assert_eq!(scheduler.status(job).unwrap().state, "LOST");
    }
    scheduler.sweep().unwrap();

    // Each deterministic job is retried once; the cancelled one is not.
    let retries = scheduler
        .with_store(|s| s.jobs_in_states(&[JobState::Queued]))
        .unwrap();
    for (i, job) in jobs.iter().enumerate() {
        let children: Vec<_> = retries
            .iter()
            .filter(|r| r.record.retry_of.map(|p| p.to_string()).as_deref() == Some(job))
            .collect();
        let status = scheduler.status(job).unwrap();
        if i < 2 {
            assert_eq!(children.len(), 1, "retries of job {i}");
            assert_eq!(status.retried_as, children[0].record.job_id.to_string());
        } else {
            assert!(children.is_empty(), "the cancelled job was retried");
            assert!(status.retried_as.is_empty() && status.cancel_requested);
        }
    }
    assert_eq!(retries.len(), 2);

    // The agent is offline now, and is told to start over.
    let workers = scheduler.workers().unwrap().workers;
    assert!(workers.iter().all(|w| !w.online));
    let answer = scheduler
        .heartbeat(&pb::HeartbeatRequest {
            agent_id: agent,
            running_job_ids: jobs.clone(),
        })
        .unwrap();
    assert!(!answer.known);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_sweep_that_fails_at_the_first_job_is_tried_again() {
    sweep_fails_at(0).await;
}

#[tokio::test]
async fn a_sweep_that_fails_at_the_second_job_is_tried_again() {
    sweep_fails_at(1).await;
}

#[tokio::test]
async fn a_sweep_that_fails_at_a_cancelled_job_is_tried_again() {
    sweep_fails_at(2).await;
}

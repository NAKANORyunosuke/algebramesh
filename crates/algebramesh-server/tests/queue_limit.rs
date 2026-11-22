//! The number of queued jobs has an upper bound (Issue #40).

#![allow(clippy::unwrap_used)]

use std::time::Duration;

use algebramesh_rpc::pb;
use algebramesh_server::scheduler::SchedulerError;
use algebramesh_server::{Scheduler, SchedulerOptions, Store};

const LIMIT: u64 = 3;

fn scheduler(heartbeat_timeout: Duration) -> Scheduler {
    let options = SchedulerOptions {
        max_queued_jobs: LIMIT,
        heartbeat_timeout,
        ..SchedulerOptions::default()
    };
    Scheduler::new(Store::open(None, None).unwrap(), options).unwrap()
}

fn spec(retry: &str) -> String {
    serde_json::json!({
        "schema_version": 1, "backend": "test", "operation": "test.op",
        "arguments": {}, "retry_policy": retry,
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
                    backend_version: String::new(),
                    build: String::new(),
                    slots: 1,
                    ..Default::default()
                }],
                ..Default::default()
            },
            None,
        )
        .unwrap()
        .agent_id
}

async fn assign(scheduler: &Scheduler, agent: &str) -> String {
    scheduler
        .poll(agent, &["test".to_string()], Duration::from_millis(200))
        .await
        .unwrap()
        .unwrap()
        .job_id
}

#[tokio::test]
async fn a_full_queue_refuses_new_jobs_only() {
    let scheduler = scheduler(Duration::from_secs(15));
    let mut queued = Vec::new();
    for key in ["a", "b", "c"] {
        queued.push(scheduler.submit(&spec("never"), 0, key).unwrap().job_id);
    }
    let list = scheduler.backends().unwrap();
    assert_eq!((list.queued_jobs, list.max_queued_jobs), (3, LIMIT));

    // A new job, with a key or without one, is refused with a clear reason.
    for key in ["d", ""] {
        let err = scheduler.submit(&spec("never"), 0, key).unwrap_err();
        assert!(matches!(err, SchedulerError::Exhausted(_)), "{err:?}");
        assert!(err.to_string().contains("max_queued_jobs = 3"), "{err}");
    }
    // The queued jobs are untouched, and nothing was recorded for "d".
    for id in &queued {
        assert_eq!(scheduler.status(id).unwrap().state, "QUEUED");
    }
    assert_eq!(scheduler.backends().unwrap().queued_jobs, 3);

    // A known key is answered with its job although the queue is full.
    assert_eq!(
        scheduler.submit(&spec("never"), 0, "b").unwrap().job_id,
        queued[1]
    );
    // ... and the same key with another job is still a conflict, not a full queue.
    let err = scheduler.submit(&spec("never"), 1, "b").unwrap_err();
    assert!(matches!(err, SchedulerError::Conflict(_)), "{err:?}");

    // Once a job has started, there is room for one more.
    let agent = register(&scheduler);
    assign(&scheduler, &agent).await;
    let d = scheduler.submit(&spec("never"), 0, "d").unwrap();
    assert_eq!(d.state, "QUEUED");
    assert!(scheduler.submit(&spec("never"), 0, "e").is_err());
    // The key refused before was not taken: "d" is the job accepted now.
    assert_eq!(
        scheduler.submit(&spec("never"), 0, "d").unwrap().job_id,
        d.job_id
    );
}

/// The retry of a LOST job replaces a job already accepted, so a full queue
/// does not stop it.
#[tokio::test]
async fn the_retry_of_a_lost_job_is_queued_beyond_the_limit() {
    let timeout = Duration::from_millis(200);
    let scheduler = scheduler(timeout);
    scheduler.submit(&spec("if_deterministic"), 0, "").unwrap();
    let agent = register(&scheduler);
    let lost = assign(&scheduler, &agent).await;
    for _ in 0..LIMIT {
        scheduler.submit(&spec("never"), 0, "").unwrap();
    }
    // The agent goes silent: its job is LOST and retried.
    tokio::time::sleep(timeout * 2).await;
    scheduler.sweep().unwrap();
    let status = scheduler.status(&lost).unwrap();
    assert_eq!(status.state, "LOST", "{status:?}");
    assert!(!status.retried_as.is_empty());
    assert_eq!(
        scheduler.status(&status.retried_as).unwrap().state,
        "QUEUED"
    );
    assert_eq!(scheduler.backends().unwrap().queued_jobs, LIMIT + 1);
}

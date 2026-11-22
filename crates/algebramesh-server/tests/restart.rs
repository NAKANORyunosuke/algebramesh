//! A scheduler restart keeps queued jobs and declares assigned ones LOST, and
//! submissions with an idempotency key create one job however often they are sent.

#![allow(clippy::unwrap_used)]

use std::time::Duration;

use algebramesh_rpc::pb;
use algebramesh_server::{Scheduler, SchedulerOptions, Store};

fn spec(retry: &str) -> String {
    serde_json::json!({
        "schema_version": 1, "backend": "asir", "operation": "polynomial.factor",
        "arguments": {}, "retry_policy": retry,
    })
    .to_string()
}

#[tokio::test]
async fn restart_marks_assigned_jobs_lost_and_keeps_the_queue() {
    let temp = tempfile::Builder::new()
        .prefix("algebramesh-restart-")
        .tempdir()
        .unwrap();
    let dir = temp.path().to_path_buf();
    let db = dir.join("metadata.sqlite3");
    let open = || {
        Scheduler::new(
            Store::open(Some(&db), Some(dir.join("artifacts"))).unwrap(),
            SchedulerOptions::default(),
        )
        .unwrap()
    };

    let (assigned, retried, queued) = {
        let scheduler = open();
        let agent = scheduler
            .register(
                pb::RegisterRequest {
                    name: "w".into(),
                    host: "h".into(),
                    agent_version: "0".into(),
                    backends: vec![pb::BackendOffer {
                        name: "asir".into(),
                        capabilities_json:
                            r#"{"operations":["polynomial.factor"],"supported_types":[]}"#.into(),
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
            .agent_id;
        let assigned = scheduler.submit(&spec("never"), 0, "").unwrap().job_id;
        let retried = scheduler
            .submit(&spec("if_deterministic"), 0, "")
            .unwrap()
            .job_id;
        let queued = scheduler.submit(&spec("never"), -5, "").unwrap().job_id;
        for _ in 0..2 {
            scheduler
                .poll(&agent, &["asir".into()], Duration::from_millis(10))
                .await
                .unwrap()
                .unwrap();
        }
        (assigned, retried, queued)
    };

    let scheduler = open();
    assert_eq!(scheduler.status(&assigned).unwrap().state, "LOST");
    let lost = scheduler.status(&retried).unwrap();
    assert_eq!(lost.state, "LOST");
    assert!(!lost.retried_as.is_empty());
    assert_eq!(scheduler.status(&lost.retried_as).unwrap().state, "QUEUED");
    assert_eq!(scheduler.status(&lost.retried_as).unwrap().attempt, 2);
    assert_eq!(scheduler.status(&queued).unwrap().state, "QUEUED");
    assert!(
        scheduler
            .events(&assigned)
            .unwrap()
            .iter()
            .any(|e| e.contains("scheduler restarted"))
    );
    assert!(
        dir.join("artifacts")
            .join(&assigned)
            .join("record.json")
            .exists()
    );
    drop(scheduler);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_submission_sent_again_with_its_key_creates_one_job() {
    let temp = tempfile::Builder::new()
        .prefix("algebramesh-idem-")
        .tempdir()
        .unwrap();
    let dir = temp.path().to_path_buf();
    let db = dir.join("metadata.sqlite3");
    let open = || {
        Scheduler::new(
            Store::open(Some(&db), None).unwrap(),
            SchedulerOptions::default(),
        )
        .unwrap()
    };
    let (first, other) = {
        let scheduler = open();
        let first = scheduler.submit(&spec("never"), 0, "key-1").unwrap().job_id;
        // The answer was lost and the client sends the same submission again.
        let again = scheduler.submit(&spec("never"), 0, "key-1").unwrap().job_id;
        assert_eq!(first, again);
        // Another key is another job.
        let other = scheduler.submit(&spec("never"), 0, "key-2").unwrap().job_id;
        assert_ne!(first, other);
        // The same key with another specification or priority is refused.
        let err = scheduler
            .submit(&spec("if_deterministic"), 0, "key-1")
            .unwrap_err();
        assert!(err.to_string().contains("different specification"), "{err}");
        assert!(scheduler.submit(&spec("never"), 3, "key-1").is_err());
        // Without a key every call is a new job.
        let a = scheduler.submit(&spec("never"), 0, "").unwrap().job_id;
        let b = scheduler.submit(&spec("never"), 0, "").unwrap().job_id;
        assert_ne!(a, b);
        (first, other)
    };
    // The keys live in the store: a new scheduler over it (a restart, or a new
    // leader's copy) still returns the same jobs.
    let scheduler = open();
    assert_eq!(
        scheduler.submit(&spec("never"), 0, "key-1").unwrap().job_id,
        first
    );
    assert_eq!(
        scheduler.submit(&spec("never"), 0, "key-2").unwrap().job_id,
        other
    );
    drop(scheduler);
    let _ = std::fs::remove_dir_all(dir);
}

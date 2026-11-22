//! Invariants of jobs and of the store under failures, from the audit of
//! Issue #25 (the probes of Issues #26, #27 and #28, kept as regression tests).

#![allow(clippy::unwrap_used)]

use std::time::Duration;

use algebramesh_core::{JobId, JobRecord, JobSpec, JobState};
use algebramesh_rpc::pb;
use algebramesh_server::store::JobRow;
use algebramesh_server::{Scheduler, SchedulerOptions, Store};
use chrono::Utc;

fn spec() -> JobSpec {
    JobSpec::from_json(
        r#"{"schema_version":1,"backend":"test","operation":"test.op","arguments":{},"retry_policy":"if_deterministic"}"#,
    )
    .unwrap()
}

fn options() -> SchedulerOptions {
    SchedulerOptions {
        max_attempts: 2,
        ..SchedulerOptions::default()
    }
}

fn register(s: &Scheduler) -> String {
    s.register(
        pb::RegisterRequest {
            name: "worker".into(),
            host: "host".into(),
            agent_version: "0".into(),
            backends: vec![pb::BackendOffer {
                name: "test".into(),
                capabilities_json: r#"{"operations":["test.op"],"supported_types":[]}"#.into(),
                backend_version: "0".into(),
                build: "b".into(),
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

async fn assigned(s: &Scheduler) -> (String, pb::Assignment) {
    let agent = register(s);
    s.submit(&serde_json::to_string(&spec()).unwrap(), 0, "key")
        .unwrap();
    let a = s
        .poll(&agent, &["test".into()], Duration::from_millis(20))
        .await
        .unwrap()
        .unwrap();
    (agent, a)
}

fn queued(s: &Scheduler) -> Vec<JobRow> {
    s.with_store(|st| st.jobs_in_states(&[JobState::Queued]))
        .unwrap()
}

/// Issue #26: a cancelled job whose scheduler restarts is not run again.
#[tokio::test]
async fn a_cancelled_running_job_is_not_retried_after_a_restart() {
    let s = Scheduler::new(Store::open(None, None).unwrap(), options()).unwrap();
    let (agent, a) = assigned(&s).await;
    s.started(&pb::JobEvent {
        agent_id: agent,
        job_id: a.job_id.clone(),
        attempt: a.attempt,
    })
    .unwrap();
    assert_eq!(s.cancel(&a.job_id).unwrap().state, "CANCELLING");
    let bytes = s.with_store(|st| st.snapshot()).unwrap();
    let mut recovered = Store::open(None, None).unwrap();
    recovered.restore(&bytes).unwrap();
    let restarted = Scheduler::new(recovered, options()).unwrap();
    let status = restarted.status(&a.job_id).unwrap();
    assert_eq!(status.state, "LOST");
    assert!(status.retried_as.is_empty(), "{status:?}");
    assert!(queued(&restarted).is_empty());
    assert!(
        restarted
            .events(&a.job_id)
            .unwrap()
            .iter()
            .any(|e| e.contains("not retried, its cancellation was requested"))
    );
}

/// Issue #26: an assigned job cancelled before its start was reported (the
/// report was lost) ends CANCELLED when its result arrives.
#[tokio::test]
async fn a_cancellation_before_a_lost_start_report_is_kept() {
    let s = Scheduler::new(Store::open(None, None).unwrap(), options()).unwrap();
    let (agent, a) = assigned(&s).await;
    assert!(s.cancel(&a.job_id).unwrap().cancel_requested);
    let outcome = serde_json::json!({"Ok": {
        "value": 1, "backend": {"name": "test", "driver_version": "0"}, "log": [],
    }});
    let ack = s
        .finished(&pb::JobFinished {
            agent_id: agent,
            job_id: a.job_id.clone(),
            attempt: a.attempt,
            outcome_json: outcome.to_string(),
        })
        .unwrap();
    assert!(ack.accepted);
    assert_eq!(s.status(&a.job_id).unwrap().state, "CANCELLED");
}

/// Issue #26: a cancelled job whose worker is lost is not retried either.
#[tokio::test]
async fn a_cancelled_job_of_a_lost_worker_is_not_retried() {
    let opts = SchedulerOptions {
        heartbeat_timeout: Duration::from_millis(100),
        ..options()
    };
    let s = Scheduler::new(Store::open(None, None).unwrap(), opts).unwrap();
    let (_agent, a) = assigned(&s).await;
    s.cancel(&a.job_id).unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    s.sweep().unwrap();
    let status = s.status(&a.job_id).unwrap();
    assert_eq!(status.state, "LOST");
    assert!(status.retried_as.is_empty());
    assert!(queued(&s).is_empty());
}

/// Issue #27: declaring a job lost, inserting its retry and linking them is
/// one change. Applying any prefix of the changes a scheduler wrote, and
/// starting a scheduler on it, leaves at most one retry, linked from the job.
#[test]
fn a_lost_job_and_its_retry_are_one_change() {
    let state = Store::open(None, None).unwrap();
    let mut original = JobRecord::new(JobId::new(), spec(), Utc::now());
    original.transition(JobState::Assigned, Utc::now()).unwrap();
    original.transition(JobState::Running, Utc::now()).unwrap();
    state.insert_job(&JobRow::new(original.clone(), 0)).unwrap();
    let base = state.snapshot().unwrap();
    let mut working = Store::open(None, None).unwrap();
    working.restore(&base).unwrap();
    let (sink, mut rx) = tokio::sync::mpsc::unbounded_channel();
    working.set_change_sink(Some(sink));
    // Starting declares the RUNNING job LOST and creates attempt 2.
    let _leader = Scheduler::new(working, options()).unwrap();
    let mut changes = Vec::new();
    while let Ok(c) = rx.try_recv() {
        changes.push(c);
    }
    assert_eq!(changes.len(), 1, "one change for the job and its retry");
    for prefix in 0..=changes.len() {
        let mut store = Store::open(None, None).unwrap();
        store.restore(&base).unwrap();
        store.apply_batch(&changes[..prefix], &[]).unwrap();
        let recovered = Scheduler::new(store, options()).unwrap();
        let retries = queued(&recovered);
        assert_eq!(retries.len(), 1, "prefix {prefix}");
        assert_eq!(retries[0].record.attempt, 2);
        let parent = recovered.status(&original.job_id.to_string()).unwrap();
        assert_eq!(parent.state, "LOST");
        assert_eq!(parent.retried_as, retries[0].record.job_id.to_string());
    }
}

/// Issue #21: an artifact that cannot be written does not misreport the
/// committed state.
#[test]
fn an_artifact_error_does_not_misreport_the_committed_state() {
    // The artifact directory is replaced by a file below: it lives in a
    // temporary directory of its own, removed with the test.
    let temp = tempfile::Builder::new()
        .prefix("am-artifact-")
        .tempdir()
        .unwrap();
    let root = temp.path().join("artifacts");
    let store = Store::open(None, Some(root.clone())).unwrap();
    let mut row = JobRow::new(JobRecord::new(JobId::new(), spec(), Utc::now()), 0);
    store.insert_job(&row).unwrap();
    std::fs::remove_dir(&root).unwrap();
    std::fs::write(&root, b"a regular file, not a directory").unwrap();
    for state in [JobState::Assigned, JobState::Running, JobState::Succeeded] {
        row.record.transition(state, Utc::now()).unwrap();
    }
    assert!(store.update_job(&row).is_ok());
    let stored = store
        .get_job(&row.record.job_id.to_string())
        .unwrap()
        .unwrap();
    assert_eq!(stored.record.state, JobState::Succeeded);
    let _ = std::fs::remove_file(root);
}

/// Issue #28: a snapshot that is not a usable database leaves the current one
/// as it was, before and after it is opened again.
#[test]
fn an_invalid_snapshot_keeps_the_previous_database() {
    let temp = tempfile::Builder::new()
        .prefix("am-restore-")
        .tempdir()
        .unwrap();
    let dir = temp.path().to_path_buf();
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("metadata.sqlite3");
    let mut store = Store::open(Some(&path), None).unwrap();
    let row = JobRow::new(JobRecord::new(JobId::new(), spec(), Utc::now()), 0);
    store.insert_job(&row).unwrap();
    let id = row.record.job_id.to_string();

    // Not a database; cut short; an empty database (SQLite opens an empty
    // file as one), which has none of the tables of a store.
    let good = store.snapshot().unwrap();
    for bad in [
        b"this is not a SQLite database
"
        .to_vec(),
        good[..good.len() / 2].to_vec(),
        Vec::new(),
    ] {
        assert!(store.restore(&bad).is_err());
        assert!(store.get_job(&id).unwrap().is_some());
        assert!(!path.with_extension("restore").exists());
    }
    drop(store);
    let again = Store::open(Some(&path), None).unwrap();
    assert!(again.get_job(&id).unwrap().is_some());
    // A good snapshot still replaces it.
    let other = Store::open(None, None).unwrap();
    let mut again = again;
    again.restore(&other.snapshot().unwrap()).unwrap();
    assert!(again.get_job(&id).unwrap().is_none());
    drop(again);
    let _ = std::fs::remove_dir_all(dir);
}

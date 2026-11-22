//! A job its agent no longer lists in its heartbeats does not stay active
//! (Issue #10): an agent that could not deliver a result forgets the job, and
//! the scheduler declares it `LOST` after the heartbeat timeout.

#![allow(clippy::unwrap_used)]

use std::time::Duration;

use algebramesh_rpc::pb;
use algebramesh_server::{Scheduler, SchedulerOptions, Store};

const TIMEOUT: Duration = Duration::from_millis(300);

fn scheduler() -> Scheduler {
    let options = SchedulerOptions {
        heartbeat_timeout: TIMEOUT,
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

fn heartbeat(scheduler: &Scheduler, agent: &str, running: &[&str]) {
    let response = scheduler
        .heartbeat(&pb::HeartbeatRequest {
            agent_id: agent.into(),
            running_job_ids: running.iter().map(|s| s.to_string()).collect(),
        })
        .unwrap();
    assert!(response.known);
}

/// Heartbeats every 50 ms for `time`, listing `running`.
async fn beat_for(scheduler: &Scheduler, agent: &str, running: &[&str], time: Duration) {
    let started = std::time::Instant::now();
    while started.elapsed() < time {
        heartbeat(scheduler, agent, running);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn a_job_the_agent_stops_listing_becomes_lost_and_is_retried() {
    let scheduler = scheduler();
    let agent = register(&scheduler);
    let submitted = scheduler
        .submit(&spec("if_deterministic"), 0, "")
        .unwrap()
        .job_id;
    let job = assign(&scheduler, &agent).await;
    assert_eq!(job, submitted);

    // Right after the assignment, a heartbeat that does not list the job yet
    // (the agent has not started it) changes nothing.
    heartbeat(&scheduler, &agent, &[]);
    // Listed: the job stays active however long it runs.
    beat_for(&scheduler, &agent, &[&job], TIMEOUT * 3).await;
    assert_eq!(scheduler.status(&job).unwrap().state, "ASSIGNED");

    // The agent gave up delivering the result and no longer lists it, while
    // its own heartbeats go on: the job is LOST once the timeout has passed.
    beat_for(&scheduler, &agent, &[], TIMEOUT / 2).await;
    assert_eq!(scheduler.status(&job).unwrap().state, "ASSIGNED");
    beat_for(&scheduler, &agent, &[], TIMEOUT).await;
    let status = scheduler.status(&job).unwrap();
    assert_eq!(status.state, "LOST", "{status:?}");
    assert!(
        status.error_json.contains("not listed the job"),
        "{status:?}"
    );
    // Its retry is queued and runs on the same agent.
    assert!(!status.retried_as.is_empty());
    assert_eq!(assign(&scheduler, &agent).await, status.retried_as);
}

#[tokio::test]
async fn a_job_without_retries_ends_lost() {
    let scheduler = scheduler();
    let agent = register(&scheduler);
    scheduler.submit(&spec("never"), 0, "").unwrap();
    let job = assign(&scheduler, &agent).await;
    beat_for(&scheduler, &agent, &[], TIMEOUT * 2).await;
    let status = scheduler.status(&job).unwrap();
    assert_eq!(status.state, "LOST");
    assert!(status.retried_as.is_empty());
}

#[tokio::test]
async fn a_finished_job_is_not_declared_lost() {
    let scheduler = scheduler();
    let agent = register(&scheduler);
    scheduler.submit(&spec("never"), 0, "").unwrap();
    let job = assign(&scheduler, &agent).await;
    let outcome = serde_json::json!({"Ok": {
        "value": 1, "backend": {"name": "test", "driver_version": "0"}, "log": [],
    }});
    let report = pb::JobFinished {
        agent_id: agent.clone(),
        job_id: job.clone(),
        attempt: 1,
        outcome_json: outcome.to_string(),
    };
    let ack = scheduler.finished(&report).unwrap();
    assert!(ack.accepted, "{ack:?}");
    // The same report again (its answer was lost, Issue #14) changes nothing.
    let again = scheduler.finished(&report).unwrap();
    assert!(!again.accepted);
    assert!(again.reason.contains("already SUCCEEDED"), "{again:?}");
    beat_for(&scheduler, &agent, &[], TIMEOUT * 2).await;
    assert_eq!(scheduler.status(&job).unwrap().state, "SUCCEEDED");
}

#[tokio::test]
async fn a_heartbeat_reconciles_only_its_own_agent() {
    let scheduler = scheduler();
    let a = register(&scheduler);
    let b = register(&scheduler);
    scheduler.submit(&spec("never"), 0, "").unwrap();
    let job_a = assign(&scheduler, &a).await;
    scheduler.submit(&spec("never"), 0, "").unwrap();
    let job_b = assign(&scheduler, &b).await;
    // Agent a no longer lists its job; agent b lists its own. Only a's heartbeats
    // decide about a's job, and b's job is untouched.
    let started = std::time::Instant::now();
    while started.elapsed() < TIMEOUT * 2 {
        heartbeat(&scheduler, &a, &[]);
        heartbeat(&scheduler, &b, &[&job_b]);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(scheduler.status(&job_a).unwrap().state, "LOST");
    assert_eq!(scheduler.status(&job_b).unwrap().state, "ASSIGNED");
    // The store finds an agent's active jobs by their ids alone.
    let ids = scheduler.with_store(|s| {
        s.job_ids_for_agent(
            &b,
            &[
                algebramesh_core::JobState::Assigned,
                algebramesh_core::JobState::Running,
                algebramesh_core::JobState::Cancelling,
            ],
        )
        .unwrap()
    });
    assert_eq!(ids, [job_b]);
}

/// Issue #22: a report sent again after the job finished is recorded by its
/// size and digest, once, not with its outcome.
#[tokio::test]
async fn late_reports_are_recorded_small_and_once() {
    let scheduler = scheduler();
    let agent = register(&scheduler);
    scheduler.submit(&spec("never"), 0, "").unwrap();
    let job = assign(&scheduler, &agent).await;
    // A result of 10 MiB.
    let outcome = serde_json::json!({"Ok": {
        "value": "x".repeat(10 << 20), "backend": {"name": "test", "driver_version": "0"},
        "log": [],
    }})
    .to_string();
    let report = pb::JobFinished {
        agent_id: agent.clone(),
        job_id: job.clone(),
        attempt: 1,
        outcome_json: outcome.clone(),
    };
    assert!(scheduler.finished(&report).unwrap().accepted);
    let size = |s: &Scheduler| s.with_store(|st| st.snapshot().unwrap().len());
    let before = size(&scheduler);
    for _ in 0..20 {
        assert!(!scheduler.finished(&report).unwrap().accepted);
    }
    // One small event, not 20 copies of 10 MiB.
    let events = scheduler.events(&job).unwrap();
    let late: Vec<&String> = events
        .iter()
        .filter(|e| e.contains("late report"))
        .collect();
    assert_eq!(late.len(), 1, "{late:?}");
    assert!(late[0].len() < 1000, "{}", late[0].len());
    assert!(late[0].contains(&format!("{} bytes", outcome.len())));
    assert!(late[0].contains(&algebramesh_core::sha256_hex(outcome.as_bytes())));
    assert!(
        size(&scheduler) < before + (1 << 20),
        "{} -> {}",
        before,
        size(&scheduler)
    );
    // Another attempt's report is recorded separately.
    let other = pb::JobFinished {
        attempt: 2,
        ..report
    };
    assert!(!scheduler.finished(&other).unwrap().accepted);
    let n = scheduler
        .events(&job)
        .unwrap()
        .iter()
        .filter(|e| e.contains("late report"))
        .count();
    assert_eq!(n, 2);
}

/// Issue #20: a result or a specification too large for one Raft log entry
/// ends in a clear error, not in a write that cannot be replicated.
#[tokio::test]
async fn results_and_specifications_have_a_size_limit() {
    let scheduler = scheduler();
    let agent = register(&scheduler);
    // A specification over the limit is refused when it is submitted.
    let big = serde_json::json!({
        "schema_version": 1, "backend": "test", "operation": "test.op",
        "arguments": {"blob": "x".repeat(algebramesh_core::MAX_SPEC_BYTES)},
    })
    .to_string();
    let err = scheduler.submit(&big, 0, "").unwrap_err();
    assert!(err.to_string().contains("exceeds the limit"), "{err}");

    // A result over the limit fails the job with RESOURCE_LIMIT.
    scheduler.submit(&spec("never"), 0, "").unwrap();
    let job = assign(&scheduler, &agent).await;
    let outcome = serde_json::json!({"Ok": {
        "value": "x".repeat(algebramesh_core::MAX_RESULT_BYTES),
        "backend": {"name": "test", "driver_version": "0"}, "log": [],
    }})
    .to_string();
    let ack = scheduler
        .finished(&pb::JobFinished {
            agent_id: agent,
            job_id: job.clone(),
            attempt: 1,
            outcome_json: outcome,
        })
        .unwrap();
    assert!(ack.accepted);
    let status = scheduler.status(&job).unwrap();
    assert_eq!(status.state, "FAILED");
    assert!(status.error_json.contains("RESOURCE_LIMIT"), "{status:?}");
    assert!(
        status.error_json.contains("exceeds the scheduler"),
        "{status:?}"
    );
}

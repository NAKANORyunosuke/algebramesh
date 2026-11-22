//! Measures the time of one agent heartbeat against the number of agents and of
//! active jobs with large specifications (Issue #16). Not run by default:
//!
//! ```text
//! cargo test --release -p algebramesh-server --test heartbeat_scaling -- --ignored --nocapture
//! ```

#![allow(clippy::unwrap_used)]

use std::time::{Duration, Instant};

use algebramesh_core::JobState;
use algebramesh_rpc::pb;
use algebramesh_server::{Scheduler, SchedulerOptions, Store};

/// A job whose specification carries about `bytes` bytes of arguments, as a
/// large polynomial does.
fn spec(bytes: usize) -> String {
    serde_json::json!({
        "schema_version": 1, "backend": "test", "operation": "test.op",
        "arguments": {"blob": "x".repeat(bytes)},
    })
    .to_string()
}

fn register(scheduler: &Scheduler, i: usize) -> String {
    let capabilities = serde_json::json!({"operations": ["test.op"], "supported_types": []});
    scheduler
        .register(
            pb::RegisterRequest {
                name: format!("worker-{i}"),
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

#[tokio::test]
#[ignore = "a measurement, run by hand in release mode"]
async fn heartbeat_time_follows_the_agents_own_jobs() {
    const JOBS: usize = 1000;
    const BYTES: usize = 100 * 1024;
    println!(
        "| agents | active jobs (100 KiB each) | jobs per agent | heartbeat | reading every active record |"
    );
    println!("|---|---|---|---|---|");
    for agents in [1, 10, 100] {
        let options = SchedulerOptions {
            heartbeat_timeout: Duration::from_secs(600),
            ..SchedulerOptions::default()
        };
        let scheduler = Scheduler::new(Store::open(None, None).unwrap(), options).unwrap();
        let ids: Vec<String> = (0..agents).map(|i| register(&scheduler, i)).collect();
        let spec = spec(BYTES);
        let mut assigned: Vec<Vec<String>> = vec![Vec::new(); agents];
        for j in 0..JOBS {
            scheduler.submit(&spec, 0, "").unwrap();
            let a = j % agents;
            let job = scheduler
                .poll(&ids[a], &["test".to_string()], Duration::from_millis(10))
                .await
                .unwrap()
                .unwrap()
                .job_id;
            assigned[a].push(job);
        }
        // One heartbeat of every agent, listing its jobs.
        let rounds = 5;
        let started = Instant::now();
        for _ in 0..rounds {
            for (a, id) in ids.iter().enumerate() {
                scheduler
                    .heartbeat(&pb::HeartbeatRequest {
                        agent_id: id.clone(),
                        running_job_ids: assigned[a].clone(),
                    })
                    .unwrap();
            }
        }
        let per_heartbeat = started.elapsed() / (rounds * agents) as u32;
        // What each heartbeat did before: read and decode every active record.
        let started = Instant::now();
        let all = scheduler.with_store(|s| {
            s.jobs_in_states(&[JobState::Assigned, JobState::Running, JobState::Cancelling])
                .unwrap()
                .len()
        });
        let full_scan = started.elapsed();
        assert_eq!(all, JOBS);
        println!(
            "| {agents} | {JOBS} | {} | {:.3} ms | {:.3} ms |",
            JOBS / agents,
            per_heartbeat.as_secs_f64() * 1e3,
            full_scan.as_secs_f64() * 1e3,
        );
    }
}

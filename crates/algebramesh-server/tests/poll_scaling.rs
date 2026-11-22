//! Measures an idle worker's poll against queued jobs it cannot run (Issue
//! #19). Not run by default:
//!
//! ```text
//! cargo test --release -p algebramesh-server --test poll_scaling -- --ignored --nocapture
//! ```

#![allow(clippy::unwrap_used)]

use std::time::{Duration, Instant};

use algebramesh_core::JobState;
use algebramesh_rpc::pb;
use algebramesh_server::{Scheduler, SchedulerOptions, Store};

fn register(scheduler: &Scheduler, i: usize) -> String {
    let capabilities = serde_json::json!({"operations": ["op"], "supported_types": []});
    scheduler
        .register(
            pb::RegisterRequest {
                name: format!("asir-{i}"),
                host: "h".into(),
                agent_version: "0".into(),
                backends: vec![pb::BackendOffer {
                    name: "asir".into(),
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
async fn an_idle_poll_does_not_read_the_queued_records() {
    const JOBS: usize = 1000;
    println!(
        "| queued m2 jobs | size each | asir workers | one empty poll | placements for asir | placements for m2 | reading every queued record |"
    );
    println!("|---|---|---|---|---|---|---|");
    for bytes in [100 * 1024, 1024 * 1024] {
        let scheduler = Scheduler::new(
            Store::open(None, None).unwrap(),
            SchedulerOptions::default(),
        )
        .unwrap();
        let spec = serde_json::json!({
            "schema_version": 1, "backend": "m2", "operation": "op",
            "arguments": {"blob": "x".repeat(bytes)},
        })
        .to_string();
        for _ in 0..JOBS {
            scheduler.submit(&spec, 0, "").unwrap();
        }
        let started = Instant::now();
        let queued = scheduler.with_store(|s| s.jobs_in_states(&[JobState::Queued]).unwrap().len());
        let full = started.elapsed();
        assert_eq!(queued, JOBS);
        // What a poll reads to choose: the placements of the queued jobs of its
        // backends. None for asir; all 1000 (small) for an m2 worker that the
        // constraints exclude from every job.
        let time = |backend: &str| {
            let started = Instant::now();
            let n = scheduler
                .with_store(|s| s.queued_placements(&[backend.to_string()]).unwrap().len());
            (started.elapsed(), n)
        };
        let (asir_time, n) = time("asir");
        assert_eq!(n, 0);
        let (m2_time, n) = time("m2");
        assert_eq!(n, JOBS);
        for workers in [1, 10, 100] {
            let agents: Vec<String> = (0..workers).map(|i| register(&scheduler, i)).collect();
            let started = Instant::now();
            for agent in &agents {
                let got = scheduler
                    .poll(agent, &["asir".to_string()], Duration::ZERO)
                    .await
                    .unwrap();
                assert!(got.is_none());
            }
            let per_poll = started.elapsed() / workers as u32;
            println!(
                "| {JOBS} | {} KiB | {workers} | {:.3} ms | {:.3} ms | {:.3} ms | {:.1} ms |",
                bytes / 1024,
                per_poll.as_secs_f64() * 1e3,
                asir_time.as_secs_f64() * 1e3,
                m2_time.as_secs_f64() * 1e3,
                full.as_secs_f64() * 1e3,
            );
        }
    }
}

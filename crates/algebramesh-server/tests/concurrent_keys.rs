//! Submissions with the same idempotency key sent at the same time (Issue
//! #38): one job is created, and every submission that is not a conflict gets
//! its id.

#![allow(clippy::unwrap_used)]

use std::sync::{Arc, Barrier};

use algebramesh_core::JobState;
use algebramesh_server::scheduler::SchedulerError;
use algebramesh_server::{Scheduler, SchedulerOptions, Store};

const THREADS: usize = 16;

fn spec() -> String {
    serde_json::json!({
        "schema_version": 1, "backend": "test", "operation": "test.op", "arguments": {},
    })
    .to_string()
}

#[test]
fn the_same_key_at_the_same_time_creates_one_job() {
    let scheduler = Arc::new(
        Scheduler::new(
            Store::open(None, None).unwrap(),
            SchedulerOptions::default(),
        )
        .unwrap(),
    );
    for round in 0..20 {
        let key = format!("key-{round}");
        let barrier = Arc::new(Barrier::new(THREADS));
        let threads: Vec<_> = (0..THREADS)
            .map(|i| {
                let (scheduler, barrier, key) = (scheduler.clone(), barrier.clone(), key.clone());
                // Every other round, a quarter of the submissions use another
                // priority with the same key: those that come after the first
                // are conflicts, whichever comes first.
                let priority = i32::from(round % 2 == 1 && i % 4 == 0);
                std::thread::spawn(move || {
                    barrier.wait();
                    (priority, scheduler.submit(&spec(), priority, &key))
                })
            })
            .collect();
        let answers: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
        let accepted: Vec<_> = answers
            .iter()
            .filter_map(|(p, a)| a.as_ref().ok().map(|s| (*p, s.job_id.clone())))
            .collect();
        assert!(!accepted.is_empty(), "round {round}: {answers:?}");
        // One job, one priority among the accepted submissions.
        let (priority, job) = accepted[0].clone();
        assert!(
            accepted.iter().all(|a| *a == (priority, job.clone())),
            "round {round}: {accepted:?}"
        );
        for (p, answer) in &answers {
            match answer {
                Ok(_) => {}
                Err(SchedulerError::Conflict(_)) => assert_ne!(*p, priority),
                Err(e) => panic!("round {round}: {e}"),
            }
        }
        let queued = scheduler.with_store(|s| s.count_jobs(JobState::Queued).unwrap());
        assert_eq!(queued, round + 1, "round {round}");
    }
}

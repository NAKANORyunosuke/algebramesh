//! A worker's poll chooses from the placement columns (Issue #19) and keeps
//! the queue order, the backends and the environment constraints.

#![allow(clippy::unwrap_used)]

use std::time::Duration;

use algebramesh_rpc::pb;
use algebramesh_server::{Scheduler, SchedulerOptions, Store};

fn register(scheduler: &Scheduler, name: &str, backend: &str) -> String {
    let capabilities = serde_json::json!({"operations": ["op"], "supported_types": []});
    scheduler
        .register(
            pb::RegisterRequest {
                name: name.into(),
                host: "h".into(),
                agent_version: "0".into(),
                backends: vec![pb::BackendOffer {
                    name: backend.into(),
                    capabilities_json: capabilities.to_string(),
                    backend_version: String::new(),
                    build: "b1".into(),
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

fn submit(
    scheduler: &Scheduler,
    backend: &str,
    priority: i32,
    environment: serde_json::Value,
) -> String {
    let spec = serde_json::json!({
        "schema_version": 1, "backend": backend, "operation": "op", "arguments": {},
        "environment": environment,
    });
    scheduler
        .submit(&spec.to_string(), priority, "")
        .unwrap()
        .job_id
}

async fn poll(scheduler: &Scheduler, agent: &str, backend: &str) -> Option<String> {
    scheduler
        .poll(agent, &[backend.to_string()], Duration::from_millis(50))
        .await
        .unwrap()
        .map(|a| a.job_id)
}

#[tokio::test]
async fn polls_keep_priority_order_backends_and_constraints() {
    let scheduler = Scheduler::new(
        Store::open(None, None).unwrap(),
        SchedulerOptions::default(),
    )
    .unwrap();
    let none = serde_json::json!({});
    let m2_low = submit(&scheduler, "m2", 0, none.clone());
    let asir = submit(&scheduler, "asir", 9, none.clone());
    let m2_elsewhere = submit(
        &scheduler,
        "m2",
        9,
        serde_json::json!({"agents": ["other"]}),
    );
    let m2_other_build = submit(&scheduler, "m2", 8, serde_json::json!({"builds": ["b2"]}));
    let m2_high = submit(&scheduler, "m2", 5, none.clone());
    let m2_low2 = submit(&scheduler, "m2", 0, none);

    let a = register(&scheduler, "w-asir", "asir");
    let m = register(&scheduler, "w-m2", "m2");
    // The asir worker gets only the asir job.
    assert_eq!(poll(&scheduler, &a, "asir").await, Some(asir));
    assert_eq!(poll(&scheduler, &a, "asir").await, None);
    // The m2 worker: priority first, then age; jobs for another agent or
    // another build are left.
    assert_eq!(poll(&scheduler, &m, "m2").await, Some(m2_high));
    assert_eq!(poll(&scheduler, &m, "m2").await, Some(m2_low));
    assert_eq!(poll(&scheduler, &m, "m2").await, Some(m2_low2));
    assert_eq!(poll(&scheduler, &m, "m2").await, None);
    // The agent named in the constraint gets its job.
    let other = register(&scheduler, "other", "m2");
    assert_eq!(poll(&scheduler, &other, "m2").await, Some(m2_elsewhere));
    assert_eq!(scheduler.status(&m2_other_build).unwrap().state, "QUEUED");
}

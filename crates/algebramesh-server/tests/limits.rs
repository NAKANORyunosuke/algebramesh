//! Jobs with limits go only to workers whose backend enforces them (ADR 0014).

#![allow(clippy::unwrap_used)]

use std::time::Duration;

use algebramesh_rpc::pb;
use algebramesh_server::{Scheduler, SchedulerOptions, Store};

fn spec(memory_mb: Option<u64>) -> String {
    let mut spec = serde_json::json!({
        "schema_version": 1, "backend": "m2", "operation": "polynomial.factor",
        "arguments": {},
    });
    if let Some(mb) = memory_mb {
        spec["limits"] = serde_json::json!({"memory_mb": mb});
    }
    spec.to_string()
}

fn register(scheduler: &Scheduler, name: &str, enforced: &[&str]) -> String {
    register_with(scheduler, name, enforced, None)
}

fn register_with(
    scheduler: &Scheduler,
    name: &str,
    enforced: &[&str],
    precision: Option<u32>,
) -> String {
    let capabilities = serde_json::json!({
        "operations": ["polynomial.factor"], "supported_types": [],
        "enforced_limits": enforced, "max_precision": precision,
    });
    scheduler
        .register(
            pb::RegisterRequest {
                name: name.into(),
                host: "h".into(),
                agent_version: "0".into(),
                backends: vec![pb::BackendOffer {
                    name: "m2".into(),
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

async fn poll(scheduler: &Scheduler, agent: &str) -> Option<String> {
    scheduler
        .poll(agent, &["m2".to_string()], Duration::from_millis(200))
        .await
        .unwrap()
        .map(|a| a.job_id)
}

#[tokio::test]
async fn a_memory_limit_needs_a_worker_that_enforces_it() {
    let scheduler = Scheduler::new(
        Store::open(None, None).unwrap(),
        SchedulerOptions::default(),
    )
    .unwrap();
    // No worker yet: the job waits.
    let early = scheduler.submit(&spec(Some(512)), 0, "").unwrap().job_id;

    // Only a worker that does not enforce memory limits: new jobs with one are
    // refused, and the waiting one is not given to it.
    let plain = register(&scheduler, "plain", &["wall_seconds"]);
    let err = scheduler.submit(&spec(Some(512)), 0, "").unwrap_err();
    assert!(err.to_string().contains("limits.memory_mb"), "{err}");
    assert_eq!(poll(&scheduler, &plain).await, None);
    // A job without the limit still runs there.
    let free = scheduler.submit(&spec(None), 0, "").unwrap().job_id;
    assert_eq!(poll(&scheduler, &plain).await, Some(free));

    // A worker that enforces it gets the limited job, and new ones are accepted.
    let strict = register(&scheduler, "strict", &["wall_seconds", "memory_mb"]);
    assert_eq!(poll(&scheduler, &strict).await, Some(early));
    assert!(scheduler.submit(&spec(Some(512)), 0, "").is_ok());
}

/// Issues #76 and #77: the scheduler refuses, when a job is submitted, a key
/// twice in the arguments (a `serde_json::Value` would keep the last) and a
/// backend name longer than 64 bytes, even with no worker online, when a job
/// for an unknown backend would wait.
#[tokio::test]
async fn a_key_twice_and_a_long_name_are_refused_on_submission() {
    let scheduler = Scheduler::new(
        Store::open(None, None).unwrap(),
        SchedulerOptions::default(),
    )
    .unwrap();
    let twice = r#"{"schema_version": 1, "backend": "m2", "operation": "polynomial.factor",
        "arguments": {"ring": {"domain": "QQ", "variables": ["x", "y"],
                               "order": {"weights": [-1, 0], "weights": [1, 0], "then": "lex"}}}}"#;
    let err = scheduler.submit(twice, 0, "").unwrap_err();
    assert!(err.to_string().contains("twice"), "{err}");
    let long = spec(None).replace("\"m2\"", &format!("\"{}\"", "a".repeat(2 << 20)));
    let err = scheduler.submit(&long, 0, "").unwrap_err();
    assert!(err.to_string().contains("more than 64"), "{err}");
    assert!(
        scheduler.submit(&spec(None), 0, "").is_ok(),
        "a job for m2 waits"
    );
}

#[tokio::test]
async fn a_known_request_id_does_not_depend_on_the_workers_online() {
    let options = SchedulerOptions {
        heartbeat_timeout: Duration::from_millis(100),
        ..SchedulerOptions::default()
    };
    let scheduler = Scheduler::new(Store::open(None, None).unwrap(), options).unwrap();
    register(&scheduler, "strict", &["wall_seconds", "memory_mb"]);
    let first = scheduler.submit(&spec(Some(512)), 0, "K").unwrap().job_id;

    // The worker that enforces the limit goes away; only one that does not is
    // online.
    tokio::time::sleep(Duration::from_millis(200)).await;
    scheduler.sweep().unwrap();
    let _plain = register(&scheduler, "plain", &["wall_seconds"]);

    // The same key and job return the job it created ...
    assert_eq!(
        scheduler.submit(&spec(Some(512)), 0, "K").unwrap().job_id,
        first
    );
    // ... the same key with another job or priority is still a conflict ...
    let other = scheduler.submit(&spec(Some(1024)), 0, "K").unwrap_err();
    assert!(other.to_string().contains("different"), "{other}");
    let other = scheduler.submit(&spec(Some(512)), 1, "K").unwrap_err();
    assert!(other.to_string().contains("different"), "{other}");
    // ... and a new key is checked against the workers online now.
    let new = scheduler.submit(&spec(Some(512)), 0, "L").unwrap_err();
    assert!(new.to_string().contains("limits.memory_mb"), "{new}");
}

/// The `m2` entry of `ListBackends`: its combined capabilities and the limits
/// some worker enforces.
fn m2_backend(scheduler: &Scheduler) -> (serde_json::Value, Vec<String>) {
    let list = scheduler.backends().unwrap();
    let b = list.backends.iter().find(|b| b.name == "m2").unwrap();
    (
        serde_json::from_str(&b.capabilities_json).unwrap(),
        b.available_limits.clone(),
    )
}

#[tokio::test]
async fn list_backends_keeps_the_limits_and_precision_of_the_workers() {
    let scheduler = Scheduler::new(
        Store::open(None, None).unwrap(),
        SchedulerOptions::default(),
    )
    .unwrap();
    // One worker that enforces both limits: shown as it is.
    register_with(
        &scheduler,
        "strict",
        &["wall_seconds", "memory_mb"],
        Some(53),
    );
    let (caps, available) = m2_backend(&scheduler);
    assert_eq!(
        caps["enforced_limits"],
        serde_json::json!(["wall_seconds", "memory_mb"])
    );
    assert_eq!(caps["max_precision"], 53);
    assert_eq!(available, ["wall_seconds", "memory_mb"]);

    // A second one that does not enforce memory_mb and has less precision:
    // memory_mb no longer holds everywhere, but a job asking for it is still
    // accepted (it runs on the first worker).
    register_with(&scheduler, "plain", &["wall_seconds"], Some(30));
    let (caps, available) = m2_backend(&scheduler);
    assert_eq!(caps["enforced_limits"], serde_json::json!(["wall_seconds"]));
    assert_eq!(caps["max_precision"], 30);
    assert_eq!(available, ["wall_seconds", "memory_mb"]);
    assert!(scheduler.submit(&spec(Some(512)), 0, "").is_ok());

    // A worker without a precision: no precision holds everywhere.
    register_with(&scheduler, "plain-2", &["wall_seconds"], None);
    let (caps, _) = m2_backend(&scheduler);
    assert!(
        caps.get("max_precision")
            .is_none_or(serde_json::Value::is_null),
        "{caps}"
    );

    // ListWorkers still shows each worker's own offer.
    let workers = scheduler.workers().unwrap();
    let strict = workers.workers.iter().find(|w| w.name == "strict").unwrap();
    assert!(strict.backends[0].capabilities_json.contains("memory_mb"));
}

/// Registers a worker of `m2` with these capabilities.
fn register_capabilities(scheduler: &Scheduler, name: &str, capabilities: serde_json::Value) {
    scheduler
        .register(
            pb::RegisterRequest {
                name: name.into(),
                host: "h".into(),
                agent_version: "0".into(),
                backends: vec![pb::BackendOffer {
                    name: "m2".into(),
                    capabilities_json: capabilities.to_string(),
                    slots: 1,
                    ..Default::default()
                }],
                ..Default::default()
            },
            None,
        )
        .unwrap();
}

/// Issue #54: `ListBackends` shows the signature of each operation that the
/// workers declare alike, and lists the operations they declare differently.
/// A worker that declares no signatures (an agent from before them) is read
/// as before.
#[tokio::test]
async fn list_backends_combines_the_signatures_of_the_workers() {
    let scheduler = Scheduler::new(
        Store::open(None, None).unwrap(),
        SchedulerOptions::default(),
    )
    .unwrap();
    register_capabilities(
        &scheduler,
        "old",
        serde_json::json!({
            "operations": ["polynomial.factor"], "supported_types": ["factorization"],
        }),
    );
    let (caps, _) = m2_backend(&scheduler);
    assert_eq!(caps["operations"], serde_json::json!(["polynomial.factor"]));
    assert_eq!(
        caps["supported_types"],
        serde_json::json!(["factorization"])
    );
    assert!(caps.get("signatures").is_none(), "{caps}");
    assert!(caps.get("signature_conflicts").is_none(), "{caps}");

    let factor = serde_json::json!({
        "arguments": {"ring": "ring", "expression": "polynomial"}, "result": "factorization",
    });
    let generate = serde_json::json!({
        "arguments": {"A": "integer_matrix", "beta": "rational_vector",
                      "expected": "weyl_operator_list"},
        "result": "gkz_system",
    });
    register_capabilities(
        &scheduler,
        "new",
        serde_json::json!({
            "operations": ["polynomial.factor", "gkz.generate"], "supported_types": [],
            "signatures": {"polynomial.factor": factor, "gkz.generate": generate},
        }),
    );
    let (caps, _) = m2_backend(&scheduler);
    assert_eq!(
        caps["signatures"],
        serde_json::json!({"polynomial.factor": factor, "gkz.generate": generate})
    );
    assert!(caps.get("signature_conflicts").is_none(), "{caps}");

    // Another worker declares a different result for polynomial.factor: the
    // operation is a conflict, and neither signature is shown for it.
    let mut other = factor.clone();
    other["result"] = serde_json::json!("polynomial");
    register_capabilities(
        &scheduler,
        "other",
        serde_json::json!({
            "operations": ["polynomial.factor"], "supported_types": [],
            "signatures": {"polynomial.factor": other},
        }),
    );
    let (caps, _) = m2_backend(&scheduler);
    assert_eq!(
        caps["signature_conflicts"],
        serde_json::json!(["polynomial.factor"])
    );
    assert_eq!(
        caps["signatures"],
        serde_json::json!({"gkz.generate": generate})
    );
}

fn spec_of(backend: &str, operation: &str) -> String {
    serde_json::json!({
        "schema_version": 1, "backend": backend, "operation": operation, "arguments": {},
    })
    .to_string()
}

fn queued(scheduler: &Scheduler) -> u64 {
    scheduler.backends().unwrap().queued_jobs
}

/// Issue #55: a job whose operation no online worker of its backend offers is
/// refused when it is submitted, as one whose limits none enforces, and no
/// job is created. With no worker of the backend online it waits. A
/// scheduler that has just begun to lead knows of no worker until they
/// register again: the job waits, and afterwards the answer is as before.
#[tokio::test]
async fn an_operation_no_online_worker_offers_is_refused() {
    let scheduler = Scheduler::new(
        Store::open(None, None).unwrap(),
        SchedulerOptions::default(),
    )
    .unwrap();
    // No worker yet: the job waits.
    let early = scheduler
        .submit(&spec_of("m2", "gkz.generate"), 0, "")
        .unwrap();
    assert_eq!(early.state, "QUEUED");

    // A worker that offers only polynomial.factor.
    let only_factor = serde_json::json!({
        "operations": ["polynomial.factor"], "supported_types": [],
    });
    register_capabilities(&scheduler, "factor-only", only_factor.clone());
    let before = queued(&scheduler);
    let err = scheduler
        .submit(&spec_of("m2", "gkz.generate"), 0, "")
        .unwrap_err();
    assert!(
        matches!(
            err,
            algebramesh_server::scheduler::SchedulerError::Unplaceable(_)
        ),
        "{err}"
    );
    assert!(err.to_string().contains("gkz.generate"), "{err}");
    assert!(!err.to_string().contains("  "), "{err}");
    assert_eq!(queued(&scheduler), before, "no job was created");
    // What it offers is accepted, and another backend without workers waits.
    assert!(
        scheduler
            .submit(&spec_of("m2", "polynomial.factor"), 0, "")
            .is_ok()
    );
    assert!(
        scheduler
            .submit(&spec_of("asir", "gkz.generate"), 0, "")
            .is_ok()
    );

    // A new leader: a scheduler over the same state, which knows of no agent.
    let state = scheduler.with_store(|s| s.snapshot()).unwrap();
    let mut store = Store::open(None, None).unwrap();
    store.restore(&state).unwrap();
    let leader = Scheduler::new(store, SchedulerOptions::default()).unwrap();
    assert!(leader.submit(&spec_of("m2", "gkz.generate"), 0, "").is_ok());
    register_capabilities(&leader, "factor-only", only_factor);
    let err = leader
        .submit(&spec_of("m2", "gkz.generate"), 0, "")
        .unwrap_err();
    assert!(err.to_string().contains("gkz.generate"), "{err}");
}

/// Issue #55: the limits of a job count only on the workers that offer its
/// operation, since placement needs both of one worker.
#[tokio::test]
async fn limits_count_on_the_workers_that_offer_the_operation() {
    let scheduler = Scheduler::new(
        Store::open(None, None).unwrap(),
        SchedulerOptions::default(),
    )
    .unwrap();
    register_capabilities(
        &scheduler,
        "factor",
        serde_json::json!({"operations": ["polynomial.factor"], "supported_types": [],
                           "enforced_limits": ["wall_seconds"]}),
    );
    register_capabilities(
        &scheduler,
        "gkz",
        serde_json::json!({"operations": ["gkz.generate"], "supported_types": [],
                           "enforced_limits": ["wall_seconds", "memory_mb"]}),
    );
    // The worker that enforces memory_mb does not offer polynomial.factor.
    let err = scheduler.submit(&spec(Some(512)), 0, "").unwrap_err();
    assert!(err.to_string().contains("limits.memory_mb"), "{err}");
    assert!(scheduler.submit(&spec(None), 0, "").is_ok());
}

/// Issue #55: a key already seen returns its job whichever workers are online.
#[tokio::test]
async fn a_known_request_id_is_answered_whatever_the_workers_offer() {
    let options = SchedulerOptions {
        heartbeat_timeout: Duration::from_millis(100),
        ..SchedulerOptions::default()
    };
    let scheduler = Scheduler::new(Store::open(None, None).unwrap(), options).unwrap();
    register_capabilities(
        &scheduler,
        "gkz",
        serde_json::json!({"operations": ["gkz.generate"], "supported_types": []}),
    );
    let first = scheduler
        .submit(&spec_of("m2", "gkz.generate"), 0, "K")
        .unwrap()
        .job_id;
    tokio::time::sleep(Duration::from_millis(200)).await;
    scheduler.sweep().unwrap();
    register_capabilities(
        &scheduler,
        "factor-only",
        serde_json::json!({"operations": ["polynomial.factor"], "supported_types": []}),
    );
    assert_eq!(
        scheduler
            .submit(&spec_of("m2", "gkz.generate"), 0, "K")
            .unwrap()
            .job_id,
        first
    );
    assert!(
        scheduler
            .submit(&spec_of("m2", "gkz.generate"), 0, "L")
            .is_err()
    );
}

//! Blobs in the arguments of a job, as one scheduler decides about them (ADR
//! 0021, B2, Issue #95): markers are taken only once the cluster has enabled
//! `blob_arguments`; the request fingerprint of version 2 does not depend on
//! whether a value is inline or in a blob, and a request with markers is not
//! compared with a fingerprint of version 1; only an agent that reads blob
//! arguments gets such a job; the derived tables keep the blobs it refers to.

#![allow(clippy::unwrap_used)]

use std::time::Duration;

use algebramesh_core::blob::{BlobRef, canonical_ref};
use algebramesh_core::features::{BLOB_ARGUMENTS, BLOB_REFS};
use algebramesh_rpc::pb;
use algebramesh_server::scheduler::SchedulerError;
use algebramesh_server::{Scheduler, SchedulerOptions, Store};
use serde_json::{Value, json};

fn scheduler() -> Scheduler {
    Scheduler::new(
        Store::open(None, None).unwrap(),
        SchedulerOptions::default(),
    )
    .unwrap()
}

fn enable(s: &Scheduler, features: &[&str]) {
    let features: Vec<String> = features.iter().map(|f| (*f).to_owned()).collect();
    s.with_store(|st| st.enable_features(&features, "1.1.0"))
        .unwrap();
}

fn spec(rows: Value) -> String {
    json!({
        "schema_version": 1, "backend": "test", "operation": "test.op",
        "arguments": {"rows": rows, "n": 3},
    })
    .to_string()
}

fn rows() -> Value {
    json!(["a", "b", "c"])
}

fn marker() -> (BlobRef, Value) {
    let r = canonical_ref(&rows());
    let m = r.marker();
    (r, m)
}

/// Registers an agent offering `test.op`, naming `features`.
fn register(s: &Scheduler, name: &str, features: &[&str]) -> String {
    s.register(
        pb::RegisterRequest {
            name: name.into(),
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
            features: features.iter().map(|f| (*f).to_owned()).collect(),
        },
        None,
    )
    .unwrap()
    .agent_id
}

#[test]
fn before_the_feature_a_marker_is_a_plain_value() {
    let s = scheduler();
    enable(&s, &[BLOB_REFS]);
    let (_, m) = marker();
    s.submit(&spec(m), 0, "").unwrap();
    // No blob is recorded for it.
    assert!(s.with_store(|st| st.referenced_blobs()).unwrap().is_empty());
}

#[test]
fn a_malformed_marker_in_the_arguments_is_refused() {
    let s = scheduler();
    enable(&s, &[BLOB_ARGUMENTS, BLOB_REFS]);
    let e = s
        .submit(&spec(json!({"$blob": "sha256:00"})), 0, "")
        .unwrap_err();
    assert!(
        matches!(&e, SchedulerError::InvalidSpec(m) if m.contains("arguments")),
        "{e}"
    );
}

#[test]
fn the_blobs_of_the_arguments_are_kept() {
    let s = scheduler();
    enable(&s, &[BLOB_ARGUMENTS, BLOB_REFS]);
    let (r, m) = marker();
    let job = s.submit(&spec(m), 0, "").unwrap().job_id;
    assert_eq!(
        s.with_store(|st| st.referenced_blobs()).unwrap(),
        vec![r.clone()]
    );
    assert_eq!(
        s.with_store(|st| st.jobs_referring_to(&r.id)).unwrap(),
        vec![job]
    );
}

#[test]
fn the_same_arguments_inline_or_in_a_blob_are_one_request() {
    let s = scheduler();
    enable(&s, &[BLOB_ARGUMENTS, BLOB_REFS]);
    let (_, m) = marker();
    let inline = s.submit(&spec(rows()), 0, "same").unwrap().job_id;
    let in_blob = s.submit(&spec(m.clone()), 0, "same").unwrap().job_id;
    assert_eq!(inline, in_blob);
    // Another value or another priority is another request.
    assert!(matches!(
        s.submit(&spec(json!(["a"])), 0, "same"),
        Err(SchedulerError::Conflict(_))
    ));
    assert!(matches!(
        s.submit(&spec(m), 1, "same"),
        Err(SchedulerError::Conflict(_))
    ));
}

#[test]
fn a_request_with_markers_is_not_compared_with_an_old_fingerprint() {
    let s = scheduler();
    enable(&s, &[BLOB_REFS]);
    let first = s.submit(&spec(rows()), 0, "old").unwrap().job_id;
    enable(&s, &[BLOB_ARGUMENTS]);
    // The same request, inline, is still the same job.
    assert_eq!(s.submit(&spec(rows()), 0, "old").unwrap().job_id, first);
    let (_, m) = marker();
    let e = s.submit(&spec(m), 0, "old").unwrap_err();
    assert!(
        matches!(&e, SchedulerError::Conflict(msg) if msg.contains("new request_id")),
        "{e}"
    );
}

#[tokio::test]
async fn only_an_agent_that_reads_blob_arguments_gets_such_a_job() {
    let s = scheduler();
    enable(&s, &[BLOB_ARGUMENTS, BLOB_REFS]);
    let (_, m) = marker();
    // Queued while no worker is online.
    let with_blob = s.submit(&spec(m.clone()), 0, "").unwrap().job_id;
    let old = register(&s, "old", &[BLOB_REFS]);
    let poll = |agent: String| {
        let s = &s;
        async move {
            s.poll(&agent, &["test".into()], Duration::from_millis(50))
                .await
                .unwrap()
        }
    };
    assert!(poll(old.clone()).await.is_none());
    // A job with its arguments inline goes to the old agent.
    let inline = s.submit(&spec(rows()), 0, "").unwrap().job_id;
    assert_eq!(poll(old).await.unwrap().job_id, inline);
    let new = register(&s, "new", &[BLOB_ARGUMENTS, BLOB_REFS]);
    assert_eq!(poll(new).await.unwrap().job_id, with_blob);
}

#[test]
fn a_job_no_online_agent_could_read_is_refused() {
    let s = scheduler();
    enable(&s, &[BLOB_ARGUMENTS, BLOB_REFS]);
    let _old = register(&s, "old", &[BLOB_REFS]);
    let (_, m) = marker();
    let e = s.submit(&spec(m), 0, "").unwrap_err();
    assert!(
        matches!(&e, SchedulerError::Unplaceable(msg) if msg.contains("blobs in arguments")),
        "{e}"
    );
}

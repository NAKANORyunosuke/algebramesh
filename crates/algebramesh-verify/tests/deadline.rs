//! Issue #49: the check of `polynomial.factor` stops inside one long product
//! when the job's deadline passes.

#![allow(clippy::unwrap_used)]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use algebramesh_core::check::check_result;
use algebramesh_core::guarantee::ResultVerifier;
use algebramesh_core::{BackendInfo, CasError, CasResult, ExecutionContext, JobSpec};
use algebramesh_verify::Registry;
use serde_json::{Value, json};

/// A factor of 2025 terms in x and y.
fn big_factor() -> Value {
    let terms: Vec<Value> = (0..45u32)
        .flat_map(|i| {
            (0..45u32).map(
                move |j| json!({"coefficient": (i * 7 + j + 1).to_string(), "exponents": [i, j]}),
            )
        })
        .collect();
    json!({"type": "polynomial", "terms": terms})
}

#[tokio::test(flavor = "multi_thread")]
async fn a_long_product_in_a_factor_check_stops_at_the_deadline() {
    let ring = json!({"domain": "ZZ", "variables": ["x", "y"], "order": "grevlex"});
    // The factor to the power 4: its squares alone take far longer than the
    // deadline. The check never gets to compare the product with the input,
    // so the input need not be the product.
    let spec = JobSpec {
        schema_version: 1,
        backend: "test".into(),
        operation: "polynomial.factor".into(),
        arguments: json!({"ring": ring, "expression": big_factor()}),
        limits: Default::default(),
        retry_policy: Default::default(),
        environment: Default::default(),
    };
    let value = json!({
        "type": "factorization", "ring": ring, "constant": "1",
        "factors": [{"factor": big_factor(), "multiplicity": 4, "display": "f"}],
        "verified_product": true,
    });
    let result = CasResult {
        value,
        backend: BackendInfo::default(),
        execution_host: None,
        log: vec![],
        guarantee: None,
    };
    let ctx = ExecutionContext::new(Some(Duration::from_millis(300)));
    let ended: Arc<Mutex<Option<Instant>>> = Arc::default();
    let record = ended.clone();
    let outcome = check_result(&ctx, move |interrupt| {
        let outcome = Registry::standard().verify(&spec, result, interrupt);
        *record.lock().unwrap() = Some(Instant::now());
        outcome
    })
    .await;
    let timed_out = Instant::now();
    assert!(matches!(outcome, Err(CasError::Timeout(_))), "{outcome:?}");
    tokio::time::sleep(Duration::from_millis(200)).await;
    let ended = ended.lock().unwrap().expect("the check stopped");
    assert!(
        ended.saturating_duration_since(timed_out) < Duration::from_millis(100),
        "the check ran {:?} past the deadline",
        ended.saturating_duration_since(timed_out)
    );
}

/// Issue #58: a check of a large certificate stops at the deadline too.
#[tokio::test(flavor = "multi_thread")]
async fn a_long_certificate_check_stops_at_the_deadline() {
    let n = 200;
    // A dense A; P upper unitriangular of ones (det 1), with its inverse
    // I - (superdiagonal); Q = I. The products take far longer than 300 ms.
    let a: Vec<Vec<i64>> = (0..n)
        .map(|i| {
            (0..n)
                .map(|j| ((i * 31 + j * 17) % 23) as i64 - 11)
                .collect()
        })
        .collect();
    let p: Vec<Vec<i64>> = (0..n)
        .map(|i| (0..n).map(|j| i64::from(j >= i)).collect())
        .collect();
    let p_inverse: Vec<Vec<i64>> = (0..n)
        .map(|i| {
            (0..n)
                .map(|j| i64::from(j == i) - i64::from(j == i + 1))
                .collect()
        })
        .collect();
    let identity: Vec<Vec<i64>> = (0..n)
        .map(|i| (0..n).map(|j| i64::from(i == j)).collect())
        .collect();
    let spec = JobSpec {
        schema_version: 1,
        backend: "test".into(),
        operation: "matrix.smith_normal_form".into(),
        arguments: json!({"domain": "ZZ", "matrix": a}),
        limits: Default::default(),
        retry_policy: Default::default(),
        environment: Default::default(),
    };
    let value = json!({
        "type": "smith_normal_form",
        "diagonal": vec!["1"; n],
        "certificate": {
            "left": {"transform": p, "inverse": p_inverse},
            "right": {"transform": identity.clone(), "inverse": identity},
        },
    });
    let result = CasResult {
        value,
        backend: BackendInfo::default(),
        execution_host: None,
        log: vec![],
        guarantee: None,
    };
    let ctx = ExecutionContext::new(Some(Duration::from_millis(300)));
    let ended: Arc<Mutex<Option<Instant>>> = Arc::default();
    let record = ended.clone();
    let outcome = check_result(&ctx, move |interrupt| {
        let outcome = Registry::standard().verify(&spec, result, interrupt);
        *record.lock().unwrap() = Some(Instant::now());
        outcome
    })
    .await;
    let timed_out = Instant::now();
    assert!(matches!(outcome, Err(CasError::Timeout(_))), "{outcome:?}");
    tokio::time::sleep(Duration::from_millis(200)).await;
    let ended = ended.lock().unwrap().expect("the check stopped");
    assert!(
        ended.saturating_duration_since(timed_out) < Duration::from_millis(100),
        "the check ran {:?} past the deadline",
        ended.saturating_duration_since(timed_out)
    );
}

//! The full backend path (listener, mathcap check, executeFunction, conversion,
//! product check, timeout, cancellation) against the in-process fake server.

#![allow(clippy::unwrap_used)]

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::Duration;

use algebramesh_backend_asir::convert::{cmo_to_polynomial, polynomial_to_cmo};
use algebramesh_backend_asir::{AsirBackend, AsirConfig, AsirEndpoint};
use algebramesh_core::{CasError, ExecutionContext, JobSpec, JobState, run_job};
use algebramesh_math_types::{CoefficientDomain, MonomialOrder, Polynomial, Rational, RingSpec};
use algebramesh_openxm_client::SessionOptions;
use algebramesh_openxm_client::testing::FakeServer;
use algebramesh_openxm_protocol::Cmo;
use num_bigint::BigInt;

fn ring() -> RingSpec {
    RingSpec::new(
        CoefficientDomain::Rationals,
        vec!["x".into(), "y".into()],
        MonomialOrder::Grevlex,
    )
    .unwrap()
}

fn poly(terms: &[(&str, [u32; 2])]) -> Polynomial {
    Polynomial::from_terms(
        ring(),
        terms
            .iter()
            .map(|(c, e)| (e.to_vec(), c.parse::<Rational>().unwrap())),
    )
    .unwrap()
}

/// Answers `fctr(x^4-y^4)` the way ox_asir does and checks the argument it receives.
fn fake_asir(delay: Option<Duration>) -> FakeServer {
    fake_asir_answering(delay, right_factors())
}

/// The factors of x^4-y^4, each of multiplicity 1.
fn right_factors() -> Vec<(Polynomial, u32)> {
    vec![
        (poly(&[("1", [1, 0]), ("-1", [0, 1])]), 1),
        (poly(&[("1", [1, 0]), ("1", [0, 1])]), 1),
        (poly(&[("1", [2, 0]), ("1", [0, 2])]), 1),
    ]
}

/// Answers `fctr(x^4-y^4)` with `factors` and their multiplicities.
fn fake_asir_answering(delay: Option<Duration>, factors: Vec<(Polynomial, u32)>) -> FakeServer {
    let mut server = FakeServer::new(Arc::new(move |name, args| match name {
        "version" => Ok(Cmo::Zz(BigInt::from(20_260_331))),
        "fctr" => {
            let input = cmo_to_polynomial(&args[0], &ring()).map_err(|e| e.to_string())?;
            if input != poly(&[("1", [4, 0]), ("-1", [0, 4])]) {
                return Err(format!("unexpected input {input}"));
            }
            let pair = |p: Cmo, m: u32| Cmo::List(vec![p, Cmo::Zz(BigInt::from(m))]);
            let mut entries = vec![pair(Cmo::Zz(BigInt::from(1)), 1)];
            entries.extend(factors.iter().map(|(f, m)| pair(polynomial_to_cmo(f), *m)));
            Ok(Cmo::List(entries))
        }
        other => Err(format!("executeFunction : the function {other} not found")),
    }));
    server.delay = delay;
    server
}

fn backend(server: FakeServer) -> AsirBackend {
    AsirBackend::new(AsirConfig {
        endpoint: AsirEndpoint::ReverseConnect {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            control_port: 0,
            data_port: 0,
            on_listening: Some(Arc::new(move |c, d| {
                tokio::spawn(server.clone().run(c, d));
            })),
        },
        accept_timeout: Duration::from_secs(10),
        build: Some("test".into()),
        session: SessionOptions::default(),
    })
}

fn verifier() -> Arc<dyn algebramesh_core::guarantee::ResultVerifier> {
    Arc::new(algebramesh_verify::Registry::standard())
}

fn spec(wall_seconds: Option<u64>) -> JobSpec {
    let mut spec = JobSpec::from_json(include_str!("../../../examples/factor.json")).unwrap();
    spec.limits.wall_seconds = wall_seconds;
    spec
}

#[tokio::test]
async fn factor_job_succeeds_with_typed_result() {
    let record = run_job(
        &backend(fake_asir(None)),
        spec(Some(30)),
        ExecutionContext::new(None),
        &verifier(),
    )
    .await;
    assert_eq!(record.state, JobState::Succeeded, "{:?}", record.error);
    let result = record.result.unwrap();
    assert_eq!(result["type"], "factorization");
    assert_eq!(result["constant"], "1");
    assert_eq!(result["verified_product"], true);
    let displays: Vec<&str> = result["factors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["display"].as_str().unwrap())
        .collect();
    assert_eq!(displays, ["x-y", "x+y", "x^2+y^2"]);
    let claims: Vec<&str> = record
        .guarantee
        .as_ref()
        .unwrap()
        .claims
        .iter()
        .map(|c| c.id.as_str())
        .collect();
    assert!(claims.contains(&"factor.product"), "{claims:?}");
    assert_eq!(record.backend.backend_version.as_deref(), Some("20260331"));
    assert_eq!(record.backend.build.as_deref(), Some("test"));
    assert!(record.execution_host.unwrap().starts_with("127.0.0.1:"));
}

#[tokio::test]
async fn timeout_kills_session_and_marks_timed_out() {
    let record = run_job(
        &backend(fake_asir(Some(Duration::from_secs(30)))),
        spec(Some(1)),
        ExecutionContext::new(None),
        &verifier(),
    )
    .await;
    assert_eq!(record.state, JobState::TimedOut);
    assert_eq!(record.error, Some(CasError::Timeout(1)));
}

#[tokio::test]
async fn cancellation_is_reported() {
    let ctx = ExecutionContext::new(None);
    let cancel = ctx.cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        cancel.cancel();
    });
    let record = run_job(
        &backend(fake_asir(Some(Duration::from_secs(30)))),
        spec(None),
        ctx,
        &verifier(),
    )
    .await;
    assert_eq!(record.state, JobState::Cancelled);
}

/// Issue #54: each operation is declared with its signature from
/// `algebramesh-ops`.
#[test]
fn operations_are_declared_with_their_signatures() {
    use algebramesh_core::CasBackend;
    let caps = AsirBackend::new(AsirConfig {
        endpoint: AsirEndpoint::ReverseConnect {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            control_port: 0,
            data_port: 0,
            on_listening: None,
        },
        accept_timeout: Duration::from_secs(10),
        build: None,
        session: SessionOptions::default(),
    })
    .capabilities();
    assert!(!caps.operations.is_empty());
    for op in &caps.operations {
        let d = algebramesh_ops::descriptor(op).unwrap();
        assert_eq!(
            caps.signatures[op],
            algebramesh_core::Signature::new(d.arguments, d.result)
        );
    }
    assert_eq!(caps.signatures.len(), caps.operations.len());
}

/// Issue #47: a factorization whose product is not the input is refused on
/// the Asir path too, now that the check runs after the backend.
#[tokio::test]
async fn a_wrong_factorization_from_asir_is_refused() {
    let mut wrong = right_factors();
    wrong[2] = (poly(&[("1", [2, 0]), ("-1", [0, 2])]), 1);
    let record = run_job(
        &backend(fake_asir_answering(None, wrong)),
        spec(Some(30)),
        ExecutionContext::new(None),
        &verifier(),
    )
    .await;
    assert_eq!(record.state, JobState::Failed);
    assert!(
        matches!(&record.error, Some(CasError::UnsupportedResult(m)) if m.contains("multiply to")),
        "{:?}",
        record.error
    );
}

/// Issue #48: a factor of multiplicity 0 from Asir is refused, although the
/// product of the factors is still the input.
#[tokio::test]
async fn a_factor_of_multiplicity_zero_from_asir_is_refused() {
    let mut mutated = right_factors();
    mutated.push((poly(&[("1", [1, 0]), ("1", [0, 0])]), 0));
    let record = run_job(
        &backend(fake_asir_answering(None, mutated)),
        spec(Some(30)),
        ExecutionContext::new(None),
        &verifier(),
    )
    .await;
    assert_eq!(record.state, JobState::Failed);
    assert!(
        matches!(&record.error, Some(CasError::UnsupportedResult(m)) if m.contains("multiplicity 0")),
        "{:?}",
        record.error
    );
}

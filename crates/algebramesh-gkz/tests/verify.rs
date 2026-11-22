//! The checks of the GKZ package (Issue #99): the values a backend built from
//! recorded Macaulay2 replies (`testdata/values.json`, kept equal to them by
//! the backend's test `the_values_of_the_package_tests_are_those_of_the_recorded_replies`)
//! pass with the claims of `docs/claims.md`, and values changed so that a
//! claim is false are refused.

#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use algebramesh_core::check::{Interrupt, check_result};
use algebramesh_core::guarantee::{EvidenceLevel, ResultVerifier, Summary};
use algebramesh_core::{BackendInfo, CasError, CasResult, ExecutionContext, JobSpec};
use algebramesh_gkz::verify::registry;
use algebramesh_verify::catalogue;
use serde_json::{Value, json};

fn cases() -> Vec<Value> {
    serde_json::from_str(include_str!("../testdata/values.json")).unwrap()
}

fn case(name: &str) -> Value {
    cases().into_iter().find(|c| c["name"] == name).unwrap()
}

fn spec(case: &Value) -> JobSpec {
    JobSpec {
        schema_version: 1,
        backend: "macaulay2".into(),
        operation: case["operation"].as_str().unwrap().into(),
        arguments: case["arguments"].clone(),
        limits: Default::default(),
        retry_policy: Default::default(),
        environment: Default::default(),
    }
}

fn result(value: Value) -> CasResult {
    CasResult {
        value,
        backend: BackendInfo {
            name: "macaulay2".into(),
            ..BackendInfo::default()
        },
        execution_host: None,
        log: vec![],
        guarantee: None,
    }
}

/// `value` checked as the result of the job of `case`; what a passing result
/// claims is what the catalogue lists.
fn check(case: &Value, value: Value) -> Result<CasResult, CasError> {
    let spec = spec(case);
    let checked = registry().verify(&spec, result(value), &Interrupt::none());
    if let Ok(CasResult {
        guarantee: Some(guarantee),
        ..
    }) = &checked
    {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/claims.md");
        let catalogue = catalogue::parse(&std::fs::read_to_string(path).unwrap());
        let problems = catalogue.check_guarantee(&spec.operation, guarantee);
        assert!(problems.is_empty(), "{problems:#?}");
    }
    checked
}

fn refused(outcome: Result<CasResult, CasError>, why: &str) {
    match outcome {
        Err(CasError::UnsupportedResult(m)) => assert!(m.contains(why), "{m}"),
        other => panic!("expected UNSUPPORTED_RESULT ({why}), got {other:?}"),
    }
}

const LOCAL: [&str; 7] = [
    "local.exponent_equations",
    "local.exponent_roots",
    "local.indicial_ideal",
    "local.roots_complete",
    "local.rank",
    "local.rank_sum",
    "local.parameters",
];

const SERIES: [&str; 10] = [
    "gkz.toric_in_IA",
    "gkz.toric_generates_IA",
    "gkz.generated_by_toric_and_euler",
    "series.shape",
    "series.euler_exact",
    "series.toric_to_order",
    "series.not_a_solution",
    "series.canonical",
    "series.log_independent",
    "series.missing",
];

/// Issue #99: every recorded value passes, with the claims of its operation
/// and the levels of the catalogue; the effective levels follow the
/// premises (`series.toric_to_order` rests on `gkz.toric_generates_IA`,
/// asserted, and `series.not_a_solution` only on `gkz.toric_in_IA`).
#[test]
fn the_recorded_values_pass_with_the_claims_of_the_catalogue() {
    use EvidenceLevel::{Asserted, Checked};
    for case in cases() {
        let name = case["name"].as_str().unwrap();
        let checked = check(&case, case["value"].clone()).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(
            checked.value, case["value"],
            "{name}: the value is not changed"
        );
        let guarantee = checked.guarantee.unwrap();
        assert_eq!(guarantee.summary, Summary::PartiallyVerified, "{name}");
        let ids: Vec<&str> = guarantee.claims.iter().map(|c| c.id.as_str()).collect();
        let want: Vec<&str> = match case["operation"].as_str().unwrap() {
            "gkz.generate" => vec![
                "gkz.euler",
                "gkz.toric_in_IA",
                "gkz.toric_generates_IA",
                "gkz.generated_by_toric_and_euler",
                "gkz.membership[0]",
                "gkz.membership[1]",
            ],
            "gkz.indicial" => LOCAL.to_vec(),
            _ => LOCAL.iter().chain(&SERIES).copied().collect(),
        };
        assert_eq!(ids, want, "{name}");
        let effective = |id: &str| guarantee.effective_level(id);
        match case["operation"].as_str().unwrap() {
            "gkz.generate" => {
                assert_eq!(effective("gkz.euler"), Checked);
                assert_eq!(effective("gkz.toric_in_IA"), Checked);
                // Issue #102: d1 d3 - d2^2 comes with its coefficients and
                // is checked; d2 is not in the ideal.
                assert_eq!(effective("gkz.membership[0]"), Checked);
                assert_eq!(effective("gkz.membership[1]"), Asserted);
            }
            "gkz.indicial" => {
                assert_eq!(effective("local.exponent_equations"), Checked);
                assert_eq!(effective("local.exponent_roots"), Asserted);
                assert_eq!(effective("local.parameters"), Checked);
            }
            _ => {
                assert_eq!(effective("series.shape"), Checked);
                assert_eq!(effective("series.euler_exact"), Checked);
                assert_eq!(effective("series.toric_to_order"), Asserted);
                assert_eq!(effective("series.not_a_solution"), Checked);
                assert_eq!(effective("series.canonical"), Checked);
            }
        }
    }
}

/// The terms of a series of the value with integer exponents and
/// coefficients, as `(exponent, coefficient)`.
fn integer_terms(terms: &Value) -> Vec<([i64; 3], i64)> {
    let int = |s: &Value| s.as_str().unwrap().parse::<i64>().unwrap();
    terms
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            (
                std::array::from_fn(|j| int(&t["exponent"][j])),
                int(&t["coefficient"]),
            )
        })
        .collect()
}

/// (d1 d3 - d2^2) applied to sum c x^e, with integer exponents: the toric
/// operator of A = [[1,1,1],[0,1,2]].
fn toric_image(terms: &[([i64; 3], i64)]) -> BTreeMap<[i64; 3], i64> {
    let mut image = BTreeMap::new();
    for &(e, c) in terms {
        *image.entry([e[0] - 1, e[1], e[2] - 1]).or_insert(0) += c * e[0] * e[2];
        *image.entry([e[0], e[1] - 2, e[2]]).or_insert(0) -= c * e[1] * (e[1] - 1);
    }
    image.retain(|_, c| *c != 0);
    image
}

/// series.not_a_solution: for beta = (3,4), the value labels a solution of
/// the exponent (0,2,1), x2^2 x3 + x1 x3^2, not_a_solution, with it as the
/// evidence. The Euler operators annihilate it (each monomial has A e =
/// beta) and so does d1 d3 - d2^2, computed here: the label is false, and
/// the value is refused.
#[test]
fn a_solution_given_as_the_evidence_of_not_a_solution_is_refused() {
    let case = case("series_integer");
    let mut value = case["value"].clone();
    let entry = &mut value["series"][1];
    assert_eq!(entry["status"], "verified_to_order");
    let terms = entry["terms"].clone();
    let integer = integer_terms(&terms);
    assert!(toric_image(&integer).is_empty());
    assert!(
        integer
            .iter()
            .all(|(e, _)| [e[0] + e[1] + e[2], e[1] + 2 * e[2]] == [3, 4])
    );
    let entry = entry.as_object_mut().unwrap();
    for key in ["terms", "canonical", "normalization", "canonical_terms"] {
        entry.remove(key);
    }
    entry.insert("status".into(), json!("not_a_solution"));
    entry.insert("reason".into(), json!("labelled by hand"));
    entry.insert("evidence_terms".into(), terms);
    refused(check(&case, value), "its evidence solves the system");
}

/// series.toric_to_order: in the same value, the verified series
/// x2^2 x3 + x1 x3^2 with 2 in place of the coefficient 1 of x1 x3^2 is
/// mapped by d1 d3 - d2^2 to 2 x3 (computed here), a coefficient that
/// involves only terms of weight at most 6: the value, which still calls it
/// verified_to_order, is refused.
#[test]
fn a_verified_series_the_toric_operator_does_not_annihilate_is_refused() {
    let case = case("series_integer");
    let mut value = case["value"].clone();
    let terms = value["series"][1]["terms"].as_array_mut().unwrap();
    let k = terms
        .iter()
        .position(|t| t["shift"] == json!([1, -2, 1]))
        .unwrap();
    terms[k]["coefficient"] = json!("2");
    terms[k]["display"] = json!("2*x1*x3^2");
    assert_eq!(
        toric_image(&integer_terms(&value["series"][1]["terms"])),
        BTreeMap::from([([0, 0, 1], 2)])
    );
    refused(check(&case, value), "is verified_to_order, but");
}

/// series.canonical: for the removable zero (Issue #88), the second
/// solution has no canonical form, as its part -15 - 6 log z at the other
/// exponent is not a multiple of the leading part 1 of that exponent's
/// Gamma-series. A value that calls it canonical is refused.
#[test]
fn a_canonical_form_the_normalization_does_not_give_is_refused() {
    let case = case("series_removable_zero");
    let mut value = case["value"].clone();
    let solution = value["series"][0]["solutions"][1].as_object_mut().unwrap();
    assert_eq!(solution["canonical"], false);
    solution.remove("canonical_reason");
    solution.insert("canonical".into(), json!(true));
    solution.insert("normalization".into(), json!([]));
    refused(check(&case, value), "the field canonical of solution");
}

/// local.parameters: the parameters are computed again from A and beta. For
/// beta = (1/2, 1/3) the value is not resonant; a value that says it is is
/// refused.
#[test]
fn parameters_other_than_those_of_a_and_beta_are_refused() {
    let case = case("indicial_generic");
    let mut value = case["value"].clone();
    assert_eq!(value["parameters"]["resonant"], false);
    value["parameters"]["resonant"] = json!(true);
    refused(
        check(&case, value),
        "the parameters are not those of A and beta",
    );
}

/// A value whose field `checks.*` does not record the check made here is
/// refused, even when the check passes.
#[test]
fn a_check_field_that_is_not_true_is_refused() {
    for (name, field) in [
        ("generate", "euler_operators_match"),
        ("indicial_generic", "exponents_are_roots"),
        ("series_generic", "toric_binomials_valid"),
    ] {
        let case = case(name);
        let mut value = case["value"].clone();
        value["checks"][field] = json!(false);
        refused(check(&case, value), &format!("checks.{field}"));
    }
}

/// gkz.membership[j]: one answer for each operator of the job.
#[test]
fn a_membership_answer_for_no_operator_is_refused() {
    let case = case("generate");
    let mut value = case["value"].clone();
    value["checks"]["expected_in_ideal"]
        .as_array_mut()
        .unwrap()
        .pop();
    refused(check(&case, value), "1 membership answers for 2 operators");
}

/// The operators d1 d3 - d2^2 and d2 of the case `generate` and its toric
/// generator, Euler operators and certificate of the first operator, which
/// is 1 times the toric generator.
fn membership_case() -> (Value, Value) {
    let case = case("generate");
    let value = case["value"].clone();
    assert_eq!(value["toric_generators"][0]["display"], "d1*d3-d2^2");
    assert_eq!(value["checks"]["expected_in_ideal"][0]["in_ideal"], true);
    assert_eq!(value["checks"]["expected_in_ideal"][1]["in_ideal"], false);
    assert_eq!(
        value["certificate"]["membership"][0]["toric"][0]["display"],
        "1"
    );
    assert_eq!(value["certificate"]["membership"][1], Value::Null);
    (case, value)
}

/// Issue #102: coefficients whose combination is not the operator of the job
/// are refused. 2 (d1 d3 - d2^2) is not d1 d3 - d2^2, as its d1 d3 term has
/// the coefficient 2.
#[test]
fn a_membership_certificate_that_does_not_give_the_operator_is_refused() {
    let (case, mut value) = membership_case();
    value["certificate"]["membership"][0]["toric"][0] = json!({"display": "2", "terms": [{"coefficient": "2", "x_exponents": [0, 0, 0], "d_exponents": [0, 0, 0]}]});
    assert_eq!(
        case["arguments"]["expected"][0][0],
        json!({"coefficient": "1", "x_exponents": [0, 0, 0], "d_exponents": [1, 0, 1]})
    );
    // By hand: the toric generator has d1 d3 with coefficient 1, so 2 times
    // it has 2, and the operator of the job has 1.
    assert!(
        value["toric_generators"][0]["terms"]
            .as_array()
            .unwrap()
            .contains(
                &json!({"coefficient": "1", "x_exponents": [0, 0, 0], "d_exponents": [1, 0, 1]})
            )
    );
    refused(
        check(&case, value),
        "certificate.membership[0] gives 2*d1*d3-2*d2^2",
    );
    // A list of coefficients that does not match the generators.
    let (case, mut value) = membership_case();
    value["certificate"]["membership"][0]["euler"]
        .as_array_mut()
        .unwrap()
        .pop();
    refused(check(&case, value), "coefficients for 1 toric generators");
}

/// Issue #102: a certificate for an operator the CAS did not find in the
/// ideal contradicts its answer and is refused, whatever it proves.
#[test]
fn a_membership_certificate_for_an_operator_not_in_the_ideal_is_refused() {
    let (case, mut value) = membership_case();
    value["certificate"]["membership"][1] = value["certificate"]["membership"][0].clone();
    refused(
        check(&case, value),
        "certificate.membership[1] is given for an operator the CAS did not find",
    );
    // Without the certificate the first operator is asserted, as before.
    let (case, mut value) = membership_case();
    value.as_object_mut().unwrap().remove("certificate");
    let g = check(&case, value).unwrap().guarantee.unwrap();
    assert_eq!(
        g.effective_level("gkz.membership[0]"),
        EvidenceLevel::Asserted
    );
}

/// Issue #102: the logarithmic solutions of the twisted cubic come with a
/// certificate of their independence and the claim is checked; an inverse
/// that is not one is refused. With the rows (0,0,0,0), (1,0,0,0), (0,1,0,0)
/// of the shift-0 parts 1, l1 + ... and l2 + ..., B_R is read here by hand
/// from the terms, and the changed matrix fails B_R B' = 1 at (0, 0).
#[test]
fn an_independence_certificate_that_is_not_an_inverse_is_refused() {
    let case = case("series_twisted_cubic_logarithmic");
    let value = case["value"].clone();
    let g = check(&case, value.clone()).unwrap().guarantee.unwrap();
    assert_eq!(
        g.effective_level("series.log_independent"),
        EvidenceLevel::Checked
    );

    // B_R by hand: the coefficient of the term with shift 0 and the log
    // powers of row r, in solution c.
    let entry = &value["series"][0];
    let rows: Vec<Value> = entry["certificate"]["independence"]["log_powers"]
        .as_array()
        .unwrap()
        .clone();
    let coefficient = |c: usize, k: &Value| -> i64 {
        let item = &entry["solutions"][c];
        item["terms"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|t| t["shift"].as_array().unwrap().iter().all(|u| u == 0))
            .filter(|t| t.get("log_powers").unwrap_or(&json!([0, 0, 0, 0])) == k)
            .map(|t| t["coefficient"].as_str().unwrap().parse::<i64>().unwrap())
            .sum()
    };
    let b: Vec<Vec<i64>> = rows
        .iter()
        .map(|k| (0..3).map(|c| coefficient(c, k)).collect())
        .collect();
    // The first column is the Gamma-series, 1 at shift 0; doubling the first
    // row of the inverse makes (B_R B')_(0,0) = 2.
    assert_eq!(b[0], [1, 0, 0]);
    let mut changed = value;
    let inverse = &mut changed["series"][0]["certificate"]["independence"]["inverse"];
    assert_eq!(inverse[0], json!(["1", "0", "0"]));
    inverse[0] = json!(["2", "0", "0"]);
    refused(
        check(&case, changed),
        "the product with the inverse is not 1 at (0, 0)",
    );

    // Without the certificate the claim is asserted, as before.
    let other = self::case("series_removable_zero");
    let g = check(&other, other["value"].clone())
        .unwrap()
        .guarantee
        .unwrap();
    assert_eq!(
        g.effective_level("series.log_independent"),
        EvidenceLevel::Asserted
    );
}

/// Issue #99: a long check of the local data stops at the job's deadline.
/// The value lists 200,000 points v = (1/6 + t, 1/3 - 2t, t) with A v = beta
/// for beta = (1/2, 1/3), for an indicial ideal without generators, so that
/// each is a root and none repeats; the check goes through all of them.
#[tokio::test(flavor = "multi_thread")]
async fn a_long_check_of_the_exponents_stops_at_the_deadline() {
    let case = case("indicial_generic");
    let mut value = case["value"].clone();
    value["indicial_ideal"] = json!([]);
    let points: Vec<Value> = (0..200_000i64)
        .map(|t| {
            json!({"point": [format!("{}/6", 1 + 6 * t), format!("{}/3", 1 - 6 * t), t.to_string()],
                   "multiplicity": 1})
        })
        .collect();
    value["exponents"] = Value::Array(points);
    let spec = spec(&case);
    let ctx = ExecutionContext::new(Some(Duration::from_millis(300)));
    let ended: Arc<Mutex<Option<Instant>>> = Arc::default();
    let record = ended.clone();
    let outcome = check_result(&ctx, move |interrupt| {
        let outcome = registry().verify(&spec, result(value), interrupt);
        *record.lock().unwrap() = Some(Instant::now());
        outcome
    })
    .await;
    let timed_out = Instant::now();
    assert!(matches!(outcome, Err(CasError::Timeout(_))), "{outcome:?}");
    tokio::time::sleep(Duration::from_millis(500)).await;
    let ended = ended.lock().unwrap().expect("the check stopped");
    assert!(
        ended.saturating_duration_since(timed_out) < Duration::from_millis(200),
        "the check ran {:?} past the deadline",
        ended.saturating_duration_since(timed_out)
    );
}

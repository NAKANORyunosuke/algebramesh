//! The checks of `module.syzygies`, `complex.free_resolution` (Issue #97)
//! and `ideal.hilbert_series` (Issue #98), on the twisted cubic and small
//! ideals worked out by hand.
//!
//! For each claim at level checked, a test changes a correct result so that
//! the claim is false, keeping the other claims true where it can, and the
//! result is refused (ADR 0018). Each test first shows what its mutation
//! does with the arithmetic of the tests' own (`Poly`, `row_times`, `at` in
//! `common`), not with the polynomial code the checks use.

#![allow(clippy::unwrap_used)]

mod common;

use algebramesh_core::guarantee::EvidenceLevel;
use algebramesh_core::{CasResult, JobSpec};
use common::*;
use serde_json::{Value, json};

const SYZYGIES: &str = "module.syzygies";
const RESOLUTION: &str = "complex.free_resolution";
const SERIES: &str = "ideal.hilbert_series";

fn x(e: [u32; 4]) -> Value {
    p(&[("1", &e)])
}

fn minus(e: [u32; 4]) -> Value {
    p(&[("-1", &e)])
}

const X: [u32; 4] = [1, 0, 0, 0];
const Y: [u32; 4] = [0, 1, 0, 0];
const Z: [u32; 4] = [0, 0, 1, 0];
const W: [u32; 4] = [0, 0, 0, 1];

/// F = [xz - y^2, xw - yz, yw - z^2] as a 1 x 3 matrix.
fn f_row() -> Value {
    pm(1, f().into_iter().map(|g| vec![(0, g)]).collect())
}

/// The syzygies of F: x f_2 - y f_1 + z f_0 = 0 and y f_2 - z f_1 + w f_0 =
/// 0, from the 3 x 3 determinants of [[x, y, z], [y, z, w]] with a row
/// repeated.
fn d2() -> Value {
    pm(
        3,
        vec![
            vec![(0, x(Z)), (1, minus(Y)), (2, x(X))],
            vec![(0, x(W)), (1, minus(Z)), (2, x(Y))],
        ],
    )
}

fn identity(n: usize) -> Value {
    pm(n, (0..n).map(|j| vec![(j, constant("1"))]).collect())
}

/// Whether `F m = 0`, by the tests' own product.
fn annihilated(m: &Value) -> bool {
    let f: Vec<Poly> = f().iter().map(poly).collect();
    let columns = m["columns"].as_array().unwrap().len();
    (0..columns).all(|j| row_times(&f, m, j).is_empty())
}

fn ids(result: &CasResult) -> Vec<String> {
    result.guarantee.as_ref().unwrap().not_claimed.clone()
}

// ------------------------------------------------------------ syzygies

fn syzygies_spec() -> JobSpec {
    job(
        SYZYGIES,
        json!({"module": {"type": "submodule", "ring": ring(&XYZW, "grevlex"), "rank": 1,
                          "generators": f_row()}}),
    )
}

fn syzygies(s: Value) -> Value {
    json!({"type": "syzygies", "syzygies": s})
}

#[test]
fn the_syzygies_of_the_cubic_are_checked() {
    assert!(annihilated(&d2()));
    let checked = check(&syzygies_spec(), syzygies(d2())).unwrap();
    assert_eq!(
        levels(&checked),
        [
            ("syz.annihilates".to_owned(), EvidenceLevel::Checked),
            ("syz.generates".to_owned(), EvidenceLevel::Asserted),
        ]
    );
}

/// `syz.annihilates`: z in column 0 changed to w; w f_0 - y f_1 + x f_2 is
/// not 0.
#[test]
fn a_syzygy_that_does_not_annihilate_is_refused() {
    let mut s = d2();
    s["columns"][0][0]["entry"] = x(W);
    assert!(!annihilated(&s));
    refused(
        check(&syzygies_spec(), syzygies(s)),
        "F times column 0 of S is not 0",
    );
}

#[test]
fn syzygies_with_the_wrong_number_of_rows_are_refused() {
    let s = pm(2, vec![vec![(0, x(X))]]);
    refused(
        check(&syzygies_spec(), syzygies(s)),
        "S has 2 rows, and F has 3 columns",
    );
}

// ------------------------------------------------------ free resolution

fn resolution_spec(generators: &[Value], variables: &[&str], certificate: &str) -> JobSpec {
    job(
        RESOLUTION,
        arguments(ring(variables, "grevlex"), generators, certificate),
    )
}

fn cubic_spec() -> JobSpec {
    resolution_spec(&f(), &XYZW, "include")
}

fn betti(numbers: &[(usize, u64, u64)]) -> Value {
    numbers
        .iter()
        .map(|&(i, j, beta)| json!({"i": i, "j": j, "beta": beta}))
        .collect()
}

fn resolution(
    d: Vec<Value>,
    degrees: Option<Value>,
    betti: Option<Value>,
    c: Value,
    n: Value,
) -> Value {
    let mut value = json!({
        "type": "free_resolution",
        "differentials": d,
        "certificate": {"differential_in_generators": c, "generators_in_differential": n},
    });
    if let Some(degrees) = degrees {
        value["degrees"] = degrees;
    }
    if let Some(betti) = betti {
        value["betti"] = betti;
    }
    value
}

fn cubic_betti() -> Value {
    betti(&[(0, 0, 1), (1, 2, 3), (2, 3, 2)])
}

fn cubic_resolution() -> Value {
    resolution(
        vec![f_row(), d2()],
        Some(json!([[0], [2, 2, 2], [3, 3]])),
        Some(cubic_betti()),
        identity(3),
        identity(3),
    )
}

fn minimal_claims() -> Vec<(String, EvidenceLevel)> {
    [
        ("res.complex", EvidenceLevel::Checked),
        ("res.image", EvidenceLevel::Checked),
        ("res.input_homogeneous", EvidenceLevel::Checked),
        ("res.differentials_homogeneous", EvidenceLevel::Checked),
        ("res.exact", EvidenceLevel::Asserted),
        ("res.entries_in_maximal_ideal", EvidenceLevel::Checked),
        ("res.minimal", EvidenceLevel::Checked),
        ("res.betti", EvidenceLevel::Checked),
    ]
    .map(|(id, level)| (id.to_owned(), level))
    .to_vec()
}

/// #97, acceptance 1: the Betti table of the twisted cubic, beta_{0,0} = 1,
/// beta_{1,2} = 3, beta_{2,3} = 2.
#[test]
fn the_resolution_of_the_cubic_is_checked() {
    assert!(annihilated(&d2()));
    let checked = check(&cubic_spec(), cubic_resolution()).unwrap();
    assert_eq!(levels(&checked), minimal_claims());
    assert!(ids(&checked).is_empty());
    let guarantee = checked.guarantee.unwrap();
    let minimal = guarantee
        .claims
        .iter()
        .find(|c| c.id == "res.minimal")
        .unwrap();
    assert_eq!(
        minimal.depends_on,
        [
            "res.input_homogeneous",
            "res.differentials_homogeneous",
            "res.entries_in_maximal_ideal",
            "res.exact"
        ]
    );
}

#[test]
fn a_resolution_without_a_certificate_asserts_its_image() {
    let mut value = cubic_resolution();
    value.as_object_mut().unwrap().remove("certificate");
    let checked = check(&resolution_spec(&f(), &XYZW, "none"), value.clone()).unwrap();
    let mut expected = minimal_claims();
    expected[1].1 = EvidenceLevel::Asserted;
    assert_eq!(levels(&checked), expected);
    refused(
        check(&cubic_spec(), value),
        "the job asked for a certificate and the value has none",
    );
}

/// `res.complex`: z in column 0 of d_2 changed to w; d_1 d_2 is not 0.
/// The entry keeps its degree.
#[test]
fn a_resolution_that_is_not_a_complex_is_refused() {
    let mut value = cubic_resolution();
    value["differentials"][1]["columns"][0][0]["entry"] = x(W);
    assert!(!annihilated(&value["differentials"][1]));
    refused(check(&cubic_spec(), value), "d_1 d_2 is not 0");
}

/// `res.image`: d_1 = x F, whose image is not I: its elements vanish at
/// x = 0 and f_0 does not. d_1 d_2 = x F d_2 = 0, and the degrees follow
/// d_1. C = x I gives d_1 = F C; no N gives F = d_1 N.
#[test]
fn a_resolution_of_another_image_is_refused() {
    let xf: Vec<Value> = f()
        .iter()
        .map(|g| to_json(&mul(&poly(g), &poly(&x(X)))))
        .collect();
    let d1 = pm(1, xf.iter().cloned().map(|g| vec![(0, g)]).collect());
    assert_eq!(at(&poly(&f()[0]), &[0, 1, 0, 0]), Q(-1, 1));
    for g in &xf {
        assert_eq!(at(&poly(g), &[0, 1, 0, 0]), Q(0, 1));
        assert_eq!(at(&poly(g), &[0, 5, 7, 11]), Q(0, 1));
    }
    let x_identity = pm(3, (0..3).map(|j| vec![(j, x(X))]).collect());
    let value = resolution(
        vec![d1, d2()],
        Some(json!([[0], [3, 3, 3], [4, 4]])),
        Some(betti(&[(0, 0, 1), (1, 3, 3), (2, 4, 2)])),
        x_identity,
        identity(3),
    );
    refused(check(&cubic_spec(), value), "F is not d_1 N");
}

/// `res.image`, the certificate C: C_00 = 2 gives 2 f_0, not d_1 = f_0.
#[test]
fn a_resolution_with_a_changed_entry_of_c_is_refused() {
    let mut value = cubic_resolution();
    value["certificate"]["differential_in_generators"]["columns"][0][0]["entry"] = constant("2");
    let f: Vec<Poly> = f().iter().map(poly).collect();
    assert_ne!(
        row_times(&f, &value["certificate"]["differential_in_generators"], 0),
        f[0]
    );
    refused(check(&cubic_spec(), value), "d_1 is not F C");
}

/// `res.image`, the certificate N: N_00 = 2 gives 2 f_0, not f_0.
#[test]
fn a_resolution_with_a_changed_entry_of_n_is_refused() {
    let mut value = cubic_resolution();
    value["certificate"]["generators_in_differential"]["columns"][0][0]["entry"] = constant("2");
    refused(check(&cubic_spec(), value), "F is not d_1 N");
}

/// The 1 x 1 matrix (1) over QQ[x, y].
fn one_of_two() -> Value {
    pm(1, vec![vec![(0, p(&[("1", &[0, 0])]))]])
}

/// `res.input_homogeneous` is recomputed from the generators: xy - 1 is not
/// homogeneous, and degrees for it are refused.
#[test]
fn degrees_for_inhomogeneous_generators_are_refused() {
    let g = p(&[("1", &[1, 1]), ("-1", &[0, 0])]);
    let spec = resolution_spec(std::slice::from_ref(&g), &["x", "y"], "include");
    let d1 = pm(1, vec![vec![(0, g)]]);
    let value = resolution(
        vec![d1],
        Some(json!([[0], [2]])),
        None,
        one_of_two(),
        one_of_two(),
    );
    refused(
        check(&spec, value),
        "degrees or Betti numbers for generators that are not all homogeneous",
    );
}

/// #97, acceptance 4: for xy - 1, no degrees and no Betti numbers, and the
/// claims of the grading and of minimality are not claimed.
#[test]
fn an_inhomogeneous_ideal_claims_no_minimality() {
    let g = p(&[("1", &[1, 1]), ("-1", &[0, 0])]);
    let spec = resolution_spec(std::slice::from_ref(&g), &["x", "y"], "include");
    let d1 = pm(1, vec![vec![(0, g)]]);
    let checked = check(
        &spec,
        resolution(vec![d1], None, None, one_of_two(), one_of_two()),
    )
    .unwrap();
    assert_eq!(
        levels(&checked),
        [
            ("res.complex".to_owned(), EvidenceLevel::Checked),
            ("res.image".to_owned(), EvidenceLevel::Checked),
            ("res.exact".to_owned(), EvidenceLevel::Asserted),
        ]
    );
    assert_eq!(
        ids(&checked),
        [
            "res.input_homogeneous",
            "res.differentials_homogeneous",
            "res.entries_in_maximal_ideal",
            "res.minimal",
            "res.betti"
        ]
    );
}

/// `res.differentials_homogeneous`: F_2 given the degrees 3 and 4; the
/// entries w, -z, y of column 1 have degree 1, not 4 - 2.
#[test]
fn a_differential_of_the_wrong_degree_is_refused() {
    let mut value = cubic_resolution();
    value["degrees"] = json!([[0], [2, 2, 2], [3, 4]]);
    value["betti"] = betti(&[(0, 0, 1), (1, 2, 3), (2, 3, 1), (2, 4, 1)]);
    refused(
        check(&cubic_spec(), value),
        "the entry (0, 1) of d_2 is not homogeneous of degree 4 - 2",
    );
}

/// R/(x) resolved with a trivial pair: F_1 = R(-1) + R(-2), d_1 = [x, 0],
/// F_2 = R(-2), d_2 = (0, 1). It is a complex with the image (x), exact,
/// and not minimal: d_2 has the constant entry 1.
fn with_a_constant_entry(betti: Option<Value>) -> (JobSpec, Value) {
    let x1 = p(&[("1", &[1])]);
    let spec = resolution_spec(std::slice::from_ref(&x1), &["x"], "include");
    let d1 = pm(1, vec![vec![(0, x1)], vec![]]);
    let d2 = pm(2, vec![vec![(1, p(&[("1", &[0])]))]]);
    let c = pm(1, vec![vec![(0, p(&[("1", &[0])]))], vec![]]);
    let n = pm(2, vec![vec![(0, p(&[("1", &[0])]))]]);
    let value = resolution(vec![d1, d2], Some(json!([[0], [1, 2], [2]])), betti, c, n);
    (spec, value)
}

/// `res.entries_in_maximal_ideal` and `res.minimal`: the Betti numbers the
/// degrees of a resolution that is not minimal give are not those of R/(x),
/// beta_{0,0} = beta_{1,1} = 1; they are refused.
#[test]
fn betti_numbers_of_a_resolution_with_a_constant_entry_are_refused() {
    let given = betti(&[(0, 0, 1), (1, 1, 1), (1, 2, 1), (2, 2, 1)]);
    assert_ne!(given, betti(&[(0, 0, 1), (1, 1, 1)]));
    let (spec, value) = with_a_constant_entry(Some(given));
    refused(
        check(&spec, value),
        "Betti numbers for a resolution with a nonzero constant entry",
    );
}

#[test]
fn a_resolution_with_a_constant_entry_does_not_claim_minimality() {
    let (spec, value) = with_a_constant_entry(None);
    let checked = check(&spec, value).unwrap();
    assert_eq!(
        levels(&checked),
        [
            ("res.complex".to_owned(), EvidenceLevel::Checked),
            ("res.image".to_owned(), EvidenceLevel::Checked),
            ("res.input_homogeneous".to_owned(), EvidenceLevel::Checked),
            (
                "res.differentials_homogeneous".to_owned(),
                EvidenceLevel::Checked
            ),
            ("res.exact".to_owned(), EvidenceLevel::Asserted),
        ]
    );
    assert_eq!(
        ids(&checked),
        ["res.entries_in_maximal_ideal", "res.minimal", "res.betti"]
    );
}

/// `res.betti`: beta_{1,2} = 4, not the 3 basis elements of degree 2 of F_1.
#[test]
fn betti_numbers_other_than_those_of_the_degrees_are_refused() {
    let mut value = cubic_resolution();
    value["betti"] = betti(&[(0, 0, 1), (1, 2, 4), (2, 3, 2)]);
    refused(
        check(&cubic_spec(), value),
        "the Betti numbers are not those the degrees of the free modules give",
    );
}

/// #97, acceptance 5: the zero ideal has the resolution F_0 = R without
/// differentials; the unit ideal R <- R, d_1 = (1), which is not minimal.
#[test]
fn the_zero_ideal_and_the_unit_ideal_are_resolved() {
    let spec = resolution_spec(&[], &XYZW, "include");
    let value = resolution(
        vec![],
        Some(json!([[0]])),
        Some(betti(&[(0, 0, 1)])),
        pm(0, vec![]),
        pm(0, vec![]),
    );
    let checked = check(&spec, value).unwrap();
    assert_eq!(levels(&checked), minimal_claims());

    let one = constant("1");
    let spec = resolution_spec(std::slice::from_ref(&one), &XYZW, "include");
    let value = resolution(
        vec![pm(1, vec![vec![(0, one)]])],
        Some(json!([[0], [0]])),
        None,
        identity(1),
        identity(1),
    );
    let checked = check(&spec, value).unwrap();
    assert_eq!(
        ids(&checked),
        ["res.entries_in_maximal_ideal", "res.minimal", "res.betti"]
    );
}

// ------------------------------------------------------- Hilbert series

fn series_spec(betti_table: bool) -> JobSpec {
    job(
        SERIES,
        json!({"ideal": {"type": "ideal", "ring": ring(&XYZW, "grevlex"), "generators": f()},
               "betti_table": betti_table}),
    )
}

fn series(numerator: &[&str], d: u64, betti: Option<Value>) -> Value {
    let mut value =
        json!({"type": "hilbert_series", "numerator": numerator, "denominator_exponent": d});
    if let Some(betti) = betti {
        value["betti"] = betti;
    }
    value
}

/// #98, acceptance 1 and 2: (1 + 2t) / (1 - t)^2 and the form not reduced,
/// (1 - 3t^2 + 2t^3) / (1 - t)^4, both agree with the Betti numbers:
/// (1 + 2t)(1 - t)^2 = 1 - 3t^2 + 2t^3.
#[test]
fn the_hilbert_series_of_the_cubic_agrees_with_its_betti_numbers() {
    for value in [
        series(&["1", "2"], 2, Some(cubic_betti())),
        series(&["1", "0", "-3", "2"], 4, Some(cubic_betti())),
    ] {
        let checked = check(&series_spec(true), value).unwrap();
        assert_eq!(
            levels(&checked),
            [("hilbert.series".to_owned(), EvidenceLevel::Asserted)]
        );
        let claim = &checked.guarantee.unwrap().claims[0];
        assert_eq!(
            serde_json::to_value(claim.method).unwrap(),
            json!("consistency")
        );
    }
    let checked = check(&series_spec(false), series(&["1", "2"], 2, None)).unwrap();
    assert_eq!(
        reason(&checked, "hilbert.series").as_deref(),
        Some("the Hilbert series is the backend's answer")
    );
}

/// #98, acceptance 3: the numerator 1 + 3t; (1 + 3t)(1 - t)^2 = 1 + t - 5t^2
/// + 3t^3 is not 1 - 3t^2 + 2t^3.
#[test]
fn a_hilbert_series_with_a_changed_numerator_is_refused() {
    refused(
        check(
            &series_spec(true),
            series(&["1", "3"], 2, Some(cubic_betti())),
        ),
        "the Hilbert series is not the one the Betti numbers give",
    );
}

#[test]
fn betti_numbers_must_be_given_exactly_when_asked_for() {
    refused(
        check(&series_spec(true), series(&["1", "2"], 2, None)),
        "the job asked for the Betti numbers and the value has none",
    );
    refused(
        check(
            &series_spec(false),
            series(&["1", "2"], 2, Some(cubic_betti())),
        ),
        "Betti numbers the job did not ask for",
    );
    refused(
        check(&series_spec(false), series(&["1", "1/2"], 2, None)),
        "the coefficient of t^1 is not an integer",
    );
}

/// #98, acceptance 4.
#[test]
fn an_inhomogeneous_ideal_has_no_hilbert_series() {
    let spec = job(
        SERIES,
        json!({"ideal": {"type": "ideal", "ring": ring(&["x", "y"], "grevlex"),
                         "generators": [p(&[("1", &[1, 1]), ("-1", &[0, 0])])]}}),
    );
    refused(
        check(&spec, series(&["1"], 1, None)),
        "the arguments of the job",
    );
}

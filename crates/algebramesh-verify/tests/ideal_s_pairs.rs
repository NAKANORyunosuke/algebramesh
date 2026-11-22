//! The check of `gb.groebner` by lcm representations of the S-pairs (Issue
//! #68), in `ideal.groebner_basis`, `ideal.normal_form` and
//! `ideal.membership`, on bases whose S-pairs are worked out by hand.
//!
//! G = (g_1, ..., g_t) is a Groebner basis of (G) if and only if every
//! S(g_i, g_j) = (L / lt(g_i)) g_i - (L / lt(g_j)) g_j, with
//! L = lcm(lm(g_i), lm(g_j)), has an lcm representation
//! S(g_i, g_j) = sum_k h_k g_k with lm(h_k g_k) < L whenever h_k g_k != 0
//! (Cox, Little and O'Shea, Ideals, Varieties, and Algorithms, Chapter 2,
//! §9). Pairs with coprime leading monomials need none.
//!
//! For each mutation, the test first shows what it does with arithmetic of
//! its own on the JSON (`s_polynomial` and `lcm_representations` below, and
//! `Q`, `Poly`, `row_times`, `leading` in `common`), not with the polynomial
//! code the checks use; then the result is refused (ADR 0018).

#![allow(clippy::unwrap_used)]

mod common;

use std::cmp::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use algebramesh_core::check::{Interrupt, check_result};
use algebramesh_core::guarantee::{Claim, EvidenceLevel, Method, ResultVerifier, Summary};
use algebramesh_core::{CasError, CasResult, ExecutionContext, JobSpec};
use algebramesh_verify::ideal::{GroebnerVerifier, MembershipVerifier, NormalFormVerifier};
use algebramesh_verify::{Context, OperationVerifier, Registry, VALUE_BUDGET, catalogue};
use common::*;
use serde_json::{Value, json};

const OP: &str = "ideal.groebner_basis";
const NORMAL_FORM: &str = "ideal.normal_form";
const MEMBERSHIP: &str = "ideal.membership";

type Order = fn(&[u32], &[u32]) -> Ordering;

// ------------------------------------------ arithmetic of the tests' own

fn monomial(e: &[u32], c: Q) -> Poly {
    Poly::from([(e.to_vec(), c)])
}

/// S(g_i, g_j) and L = lcm(lm(g_i), lm(g_j)) in `order`, by the definition.
fn s_polynomial(gi: &Poly, gj: &Poly, order: Order) -> (Poly, Vec<u32>) {
    let (mi, ci) = leading(gi, order);
    let (mj, cj) = leading(gj, order);
    let l: Vec<u32> = mi.iter().zip(&mj).map(|(a, b)| *a.max(b)).collect();
    let quotient = |m: &[u32]| -> Vec<u32> { l.iter().zip(m).map(|(a, b)| a - b).collect() };
    let left = mul(&monomial(&quotient(&mi), Q::new(ci.1, ci.0)), gi);
    let right = mul(&monomial(&quotient(&mj), Q::new(cj.1, cj.0)), gj);
    (add(&left, &scale(&right, Q(-1, 1))), l)
}

/// For the p-th pair (i, j) of `pairs`: whether S(g_i, g_j) = sum_k g_k H_kp,
/// and whether lm(H_kp g_k) < L in `order` for every nonzero H_kp.
fn lcm_representations(
    g: &[Value],
    pairs: &[[usize; 2]],
    h: &Value,
    order: Order,
) -> Vec<(bool, bool)> {
    let g: Vec<Poly> = g.iter().map(poly).collect();
    pairs
        .iter()
        .enumerate()
        .map(|(p, &[i, j])| {
            let (s, l) = s_polynomial(&g[i], &g[j], order);
            let below = (0..g.len()).all(|k| {
                let product = mul(&entry(h, k, p), &g[k]);
                product.is_empty() || order(&leading(&product, order).0, &l) == Ordering::Less
            });
            (row_times(&g, h, p) == s, below)
        })
        .collect()
}

/// Whether the leading monomials of g_i and g_j in `order` are coprime.
fn coprime(g: &[Value], i: usize, j: usize, order: Order) -> bool {
    let (a, b) = (
        leading(&poly(&g[i]), order).0,
        leading(&poly(&g[j]), order).0,
    );
    a.iter().zip(&b).all(|(x, y)| *x == 0 || *y == 0)
}

// ------------------------------------------------------------- the JSON

fn s_pairs(pairs: &[[usize; 2]], h: Value) -> Value {
    json!({"pairs": pairs, "representations": h})
}

fn value(basis: &[Value], c: Value, n: Value, s: Value) -> Value {
    json!({
        "type": "groebner_basis",
        "basis": basis,
        "certificate": {"basis_in_generators": c, "generators_in_basis": n, "s_pairs": s},
    })
}

fn all_checked() -> Vec<(String, EvidenceLevel)> {
    [
        "gb.reduced_form",
        "gb.contained",
        "gb.contains",
        "gb.groebner",
    ]
    .into_iter()
    .map(|id| (id.to_owned(), EvidenceLevel::Checked))
    .collect()
}

fn claim<'a>(result: &'a CasResult, id: &str) -> &'a Claim {
    let claims = &result.guarantee.as_ref().unwrap().claims;
    claims.iter().find(|c| c.id == id).unwrap()
}

fn effective(result: &CasResult, id: &str) -> EvidenceLevel {
    result.guarantee.as_ref().unwrap().effective_level(id)
}

// -------------------------------------------------------- twisted cubic

/// The S-pairs of G = [z^2 - yw, yz - xw, y^2 - xz] in grevlex: z^2 and yz
/// share z, yz and y^2 share y; z^2 and y^2 are coprime.
const CUBIC_PAIRS: [[usize; 2]; 2] = [[0, 1], [1, 2]];

/// S(g_0, g_1) = y g_0 - z g_1 = xzw - y^2 w = -w g_2, and
/// S(g_1, g_2) = y g_1 - z g_2 = xz^2 - xyw = x g_0.
fn cubic_h() -> Value {
    pm(
        3,
        vec![
            vec![(2, p(&[("-1", &[0, 0, 0, 1])]))],
            vec![(0, p(&[("1", &[1, 0, 0, 0])]))],
        ],
    )
}

fn cubic_spec(certificate: &str) -> JobSpec {
    job(OP, arguments(ring(&XYZW, "grevlex"), &f(), certificate))
}

fn cubic_with(pairs: &[[usize; 2]], h: Value) -> Value {
    value(&g(), antidiagonal(), antidiagonal(), s_pairs(pairs, h))
}

fn cubic() -> Value {
    cubic_with(&CUBIC_PAIRS, cubic_h())
}

#[test]
fn the_twisted_cubic_basis_is_checked_with_its_s_pairs() {
    assert!(!coprime(&g(), 0, 1, grevlex) && !coprime(&g(), 1, 2, grevlex));
    assert!(coprime(&g(), 0, 2, grevlex));
    assert_eq!(
        lcm_representations(&g(), &CUBIC_PAIRS, &cubic_h(), grevlex),
        [(true, true), (true, true)]
    );
    let checked = check(&cubic_spec("s_pairs"), cubic()).unwrap();
    assert_eq!(levels(&checked), all_checked());
    let groebner = claim(&checked, "gb.groebner");
    assert_eq!(groebner.method, Some(Method::Certificate));
    assert_eq!(groebner.reason, None);
    // The certificate is the one the job asked for: the S-pairs stay in the
    // value.
    assert_eq!(
        checked.value["certificate"]["s_pairs"]["pairs"],
        json!(CUBIC_PAIRS)
    );
}

/// #68, acceptance 1: H_20 = -2w in place of -w. The basis is still a
/// Groebner basis; only the certificate is wrong: -2w g_2 is not S(g_0, g_1).
#[test]
fn a_changed_s_pair_representation_is_refused() {
    let mut h = cubic_h();
    h["columns"][0][0]["entry"] = p(&[("-2", &[0, 0, 0, 1])]);
    assert!(!lcm_representations(&g(), &CUBIC_PAIRS, &h, grevlex)[0].0);
    refused(
        check(&cubic_spec("s_pairs"), cubic_with(&CUBIC_PAIRS, h)),
        "S(g_0, g_1) is not sum_k g_k H_k0",
    );
}

/// #68, acceptance 2: S(g_0, g_1) = y g_0 - z g_1 is a representation by the
/// definition, but y g_0 has the leading monomial yz^2 = L: it is no lcm
/// representation.
#[test]
fn the_s_pair_representation_from_the_definition_is_refused() {
    let mut h = cubic_h();
    h["columns"][0] = json!([
        {"row": 0, "entry": p(&[("1", &[0, 1, 0, 0])])},
        {"row": 1, "entry": p(&[("-1", &[0, 0, 1, 0])])},
    ]);
    assert_eq!(
        lcm_representations(&g(), &CUBIC_PAIRS, &h, grevlex)[0],
        (true, false)
    );
    let y_g0 = mul(&monomial(&[0, 1, 0, 0], Q(1, 1)), &poly(&g()[0]));
    assert_eq!(leading(&y_g0, grevlex).0, [0, 1, 2, 0]);
    refused(
        check(&cubic_spec("s_pairs"), cubic_with(&CUBIC_PAIRS, h)),
        "H_00 gives no lcm representation of S(g_0, g_1): lm(H_00 g_0) is not below \
         lcm(lm(g_0), lm(g_1))",
    );
}

/// #68, acceptance 3: the pair (1, 2), whose leading monomials yz and y^2
/// are not coprime, left out with its column; or (0, 1) left out.
#[test]
fn an_s_pair_left_out_is_refused() {
    assert!(!coprime(&g(), 1, 2, grevlex));
    let first = pm(3, vec![vec![(2, p(&[("-1", &[0, 0, 0, 1])]))]]);
    assert_eq!(
        lcm_representations(&g(), &[[0, 1]], &first, grevlex),
        [(true, true)]
    );
    refused(
        check(&cubic_spec("s_pairs"), cubic_with(&[[0, 1]], first)),
        "s_pairs.pairs lacks the pair (1, 2), whose leading monomials are not coprime",
    );
    let second = pm(3, vec![vec![(0, p(&[("1", &[1, 0, 0, 0])]))]]);
    assert_eq!(
        lcm_representations(&g(), &[[1, 2]], &second, grevlex),
        [(true, true)]
    );
    refused(
        check(&cubic_spec("s_pairs"), cubic_with(&[[1, 2]], second)),
        "s_pairs.pairs[0] is (1, 2), not (0, 1)",
    );
}

/// #68, acceptance 3: the coprime pair (0, 2) listed as well, with a right
/// lcm representation: S(g_0, g_2) = y^2 g_0 - z^2 g_2 = xz g_0 - yw g_2.
#[test]
fn a_coprime_s_pair_listed_is_refused() {
    assert!(coprime(&g(), 0, 2, grevlex));
    let pairs = [[0, 1], [0, 2], [1, 2]];
    let h = pm(
        3,
        vec![
            vec![(2, p(&[("-1", &[0, 0, 0, 1])]))],
            vec![
                (0, p(&[("1", &[1, 0, 1, 0])])),
                (2, p(&[("-1", &[0, 1, 0, 1])])),
            ],
            vec![(0, p(&[("1", &[1, 0, 0, 0])]))],
        ],
    );
    assert!(
        lcm_representations(&g(), &pairs, &h, grevlex)
            .iter()
            .all(|r| *r == (true, true))
    );
    refused(
        check(&cubic_spec("s_pairs"), cubic_with(&pairs, h)),
        "s_pairs.pairs[1] = (0, 2): the leading monomials of g_0 and g_2 are coprime",
    );
}

/// #68, acceptance 3: the pairs in reverse order with their columns, each
/// still a right lcm representation; a pair twice; a pair beyond t.
#[test]
fn s_pairs_out_of_order_are_refused() {
    let reversed = [[1, 2], [0, 1]];
    let mut h = cubic_h();
    h["columns"].as_array_mut().unwrap().reverse();
    assert_eq!(
        lcm_representations(&g(), &reversed, &h, grevlex),
        [(true, true), (true, true)]
    );
    refused(
        check(&cubic_spec("s_pairs"), cubic_with(&reversed, h)),
        "s_pairs.pairs[0] is (1, 2), not (0, 1)",
    );
    let twice = [[0, 1], [0, 1], [1, 2]];
    let mut h = cubic_h();
    let first = h["columns"][0].clone();
    h["columns"].as_array_mut().unwrap().insert(0, first);
    assert!(
        lcm_representations(&g(), &twice, &h, grevlex)
            .iter()
            .all(|r| *r == (true, true))
    );
    refused(
        check(&cubic_spec("s_pairs"), cubic_with(&twice, h)),
        "s_pairs.pairs[1] is (0, 1), not (1, 2)",
    );
    refused(
        check(
            &cubic_spec("s_pairs"),
            cubic_with(&[[0, 1], [1, 3]], cubic_h()),
        ),
        "s_pairs.pairs[1] = (1, 3) is not a pair (i, j) of basis elements with i < j < 3",
    );
}

/// The S-pairs come exactly when the job asks for them, and H has t rows
/// and a column for each pair.
#[test]
fn s_pairs_missing_or_not_asked_for_are_refused() {
    let mut without = cubic();
    without["certificate"]
        .as_object_mut()
        .unwrap()
        .remove("s_pairs");
    refused(
        check(&cubic_spec("s_pairs"), without.clone()),
        "the job asked for the certificate of the S-pairs and the value has none",
    );
    refused(
        check(&cubic_spec("representation"), cubic()),
        "a certificate of the S-pairs the job did not ask for",
    );
    // Without S-pairs, `representation` leaves gb.groebner asserted.
    let checked = check(&cubic_spec("representation"), without).unwrap();
    assert_eq!(
        reason(&checked, "gb.groebner").as_deref(),
        Some("the job asked for no certificate of the S-pairs")
    );
    let mut short = cubic();
    short["certificate"]["s_pairs"]["representations"]["columns"]
        .as_array_mut()
        .unwrap()
        .pop();
    refused(
        check(&cubic_spec("s_pairs"), short),
        "H is 3 x 1, not 3 x 2",
    );
    let mut rows = cubic();
    rows["certificate"]["s_pairs"]["representations"]["rows"] = json!(4);
    refused(check(&cubic_spec("s_pairs"), rows), "H is 4 x 2, not 3 x 2");
}

// ------------------------------------------------------- other bases

/// A lex basis: the ideal of the points (0, 0), (1, 0), (0, 1) of QQ^2,
/// G = [y^2 - y, xy, x^2 - x] in increasing lex order y^2 < xy < x^2, from
/// F = [x^2 - x, xy, y^2 - y]: g_j = f_{2-j}. S(g_0, g_1) = x g_0 - y g_1 =
/// -xy = -g_1 and S(g_1, g_2) = x g_1 - y g_2 = xy = g_1.
#[test]
fn a_lex_basis_is_checked_with_its_s_pairs() {
    let xy = ["x", "y"];
    let f = [
        p(&[("1", &[2, 0]), ("-1", &[1, 0])]),
        p(&[("1", &[1, 1])]),
        p(&[("1", &[0, 2]), ("-1", &[0, 1])]),
    ];
    let basis = [f[2].clone(), f[1].clone(), f[0].clone()];
    let one = p(&[("1", &[0, 0])]);
    let permutation = pm(3, (0..3).map(|j| vec![(2 - j, one.clone())]).collect());
    assert_eq!(
        representations_hold(&f, &basis, &permutation, &permutation),
        (true, true)
    );
    let pairs = [[0, 1], [1, 2]];
    assert!(coprime(&basis, 0, 2, lex));
    let h = pm(
        3,
        vec![
            vec![(1, p(&[("-1", &[0, 0])]))],
            vec![(1, p(&[("1", &[0, 0])]))],
        ],
    );
    assert_eq!(
        lcm_representations(&basis, &pairs, &h, lex),
        [(true, true), (true, true)]
    );
    let spec = job(OP, arguments(ring(&xy, "lex"), &f, "s_pairs"));
    let v = value(&basis, permutation.clone(), permutation, s_pairs(&pairs, h));
    assert_eq!(levels(&check(&spec, v).unwrap()), all_checked());
}

/// The zero ideal (no element), the unit ideal (G = (1)) and an ideal of one
/// element have no pairs: H is t x 0, and gb.groebner is checked. A pair
/// listed for one element is refused.
#[test]
fn the_zero_ideal_the_unit_ideal_and_one_element_have_no_s_pairs() {
    let x = ["x"];
    let zero = p(&[]);
    let spec = job(
        OP,
        arguments(ring(&XYZW, "lex"), &[zero.clone(), zero], "s_pairs"),
    );
    let empty = value(
        &[],
        pm(2, vec![]),
        pm(0, vec![vec![], vec![]]),
        s_pairs(&[], pm(0, vec![])),
    );
    assert_eq!(levels(&check(&spec, empty).unwrap()), all_checked());

    // (x + 1) - x = 1.
    let f = [p(&[("1", &[1]), ("1", &[0])]), p(&[("1", &[1])])];
    let unit = [p(&[("1", &[0])])];
    let c = pm(
        2,
        vec![vec![(0, p(&[("1", &[0])])), (1, p(&[("-1", &[0])]))]],
    );
    let n = pm(1, vec![vec![(0, f[0].clone())], vec![(0, f[1].clone())]]);
    assert_eq!(representations_hold(&f, &unit, &c, &n), (true, true));
    let spec = job(OP, arguments(ring(&x, "grevlex"), &f, "s_pairs"));
    let v = value(&unit, c.clone(), n.clone(), s_pairs(&[], pm(1, vec![])));
    assert_eq!(levels(&check(&spec, v).unwrap()), all_checked());
    let v = value(&unit, c, n, s_pairs(&[[0, 1]], pm(1, vec![vec![]])));
    refused(
        check(&spec, v),
        "s_pairs.pairs[0] = (0, 1) is not a pair (i, j) of basis elements with i < j < 1",
    );

    // F = G = (x^2 - y) in QQ[x, y].
    let one_element = [p(&[("1", &[2, 0]), ("-1", &[0, 1])])];
    let identity = pm(1, vec![vec![(0, p(&[("1", &[0, 0])]))]]);
    let spec = job(
        OP,
        arguments(ring(&["x", "y"], "grevlex"), &one_element, "s_pairs"),
    );
    let v = value(
        &one_element,
        identity.clone(),
        identity,
        s_pairs(&[], pm(1, vec![])),
    );
    assert_eq!(levels(&check(&spec, v).unwrap()), all_checked());
}

/// An exponent of S(g_0, g_1) beyond u32::MAX refuses the value: in
/// QQ[x, y, z] with lex, g_0 = xz + y^(2^32 - 1) and g_1 = xy have the
/// S-pair y g_0 - z g_1, whose term y^(2^32) has no 32-bit exponent.
#[test]
fn an_s_pair_whose_exponents_overflow_is_refused() {
    let max = u32::MAX;
    let basis = [
        p(&[("1", &[1, 0, 1]), ("1", &[0, max, 0])]),
        p(&[("1", &[1, 1, 0])]),
    ];
    assert_eq!(leading(&poly(&basis[0]), lex).0, [1, 0, 1]);
    assert!(!coprime(&basis, 0, 1, lex));
    let one = p(&[("1", &[0, 0, 0])]);
    let identity = pm(2, vec![vec![(0, one.clone())], vec![(1, one)]]);
    let spec = job(
        OP,
        arguments(ring(&["x", "y", "z"], "lex"), &basis, "s_pairs"),
    );
    let v = value(
        &basis,
        identity.clone(),
        identity,
        s_pairs(&[[0, 1]], pm(2, vec![vec![]])),
    );
    refused(check(&spec, v), "an exponent of the result exceeds");
}

// ------------------------------------------------------ order mix-ups

const XYZ: [&str; 3] = ["x", "y", "z"];

/// F = (-xyz + z^2, -xyz - x^2 + xy) and its grevlex reduced basis
/// G = (x^2 - xy + z^2, xyz - z^2, yz^3 + xz^2 - yz^2), with C and N, as in
/// `an_order_mix_up_that_is_monic_and_interreduced_passes`.
fn mixup() -> (Vec<Value>, Vec<Value>, Value, Value) {
    let f = vec![
        p(&[("-1", &[1, 1, 1]), ("1", &[0, 0, 2])]),
        p(&[("-1", &[1, 1, 1]), ("-1", &[2, 0, 0]), ("1", &[1, 1, 0])]),
    ];
    let g = vec![
        p(&[("1", &[2, 0, 0]), ("-1", &[1, 1, 0]), ("1", &[0, 0, 2])]),
        p(&[("1", &[1, 1, 1]), ("-1", &[0, 0, 2])]),
        p(&[("1", &[0, 1, 3]), ("1", &[1, 0, 2]), ("-1", &[0, 1, 2])]),
    ];
    let one = |c: &str| p(&[(c, &[0, 0, 0])]);
    let c = pm(
        2,
        vec![
            vec![(0, one("1")), (1, one("-1"))],
            vec![(0, one("-1"))],
            vec![
                (
                    0,
                    p(&[("1", &[0, 1, 1]), ("1", &[1, 0, 0]), ("-1", &[0, 1, 0])]),
                ),
                (1, p(&[("-1", &[0, 1, 1])])),
            ],
        ],
    );
    let n = pm(
        3,
        vec![vec![(1, one("-1"))], vec![(0, one("-1")), (1, one("-1"))]],
    );
    (f, g, c, n)
}

/// The mix-up basis in increasing lex order (g_2, g_1, g_0), with the
/// columns of C and the rows of N in that order.
fn mixup_sorted() -> (Vec<Value>, Value, Value) {
    let (_, g, c, n) = mixup();
    let sorted = vec![g[2].clone(), g[1].clone(), g[0].clone()];
    let c_sorted = pm(
        2,
        (0..3)
            .rev()
            .map(|j| {
                (0..2)
                    .filter_map(|i| {
                        let e = entry(&c, i, j);
                        (!e.is_empty()).then(|| (i, to_json(&e)))
                    })
                    .collect()
            })
            .collect(),
    );
    let n_sorted = pm(
        3,
        (0..2)
            .map(|i| {
                (0..3)
                    .filter_map(|k| {
                        let e = entry(&n, 2 - k, i);
                        (!e.is_empty()).then(|| (k, to_json(&e)))
                    })
                    .collect()
            })
            .collect(),
    );
    (sorted, c_sorted, n_sorted)
}

/// #68, acceptance 4: a driver that computes in grevlex for a lex job.
///
/// In grevlex the basis is right and passes with its S-pairs: the leading
/// monomials x^2, xyz, yz^3 give the pairs (0, 1) and (1, 2), with
/// S(g_0, g_1) = yz g_0 - x g_1 = -y g_1 + g_2 and
/// S(g_1, g_2) = z^2 g_1 - x g_2 = -z^2 g_0.
///
/// For the lex job the basis, sorted in increasing lex order, passes with
/// `representation` (`an_order_mix_up_that_is_monic_and_interreduced_passes`)
/// but is no lex Groebner basis: z g_1 - y g_0 = -y^2 z^3 + y^2 z^2 - z^3 is
/// in I, and its lex leading monomial y^2 z^3 is divisible by none of the
/// lex leading monomials xz^2, xyz, x^2. By the theorem some S-pair then has
/// no lcm representation, so whatever S-pairs the driver sends, the check
/// refuses them. Two such: the grevlex S-pairs carried over to the sorted
/// basis lack the lex pair (0, 2); and the lex S-pairs, each divided by G in
/// grevlex (which gives S(g_0, g_1) = y g_0 - z g_1, S(g_0, g_2) =
/// x g_0 - z^2 g_2 and S(g_1, g_2) = -g_0 + y g_1), fail the lex inequality
/// at y g_0, whose lex leading monomial xyz^2 is L.
#[test]
fn an_order_mix_up_is_refused_by_its_s_pairs() {
    let (f, g, c, n) = mixup();
    let one = || p(&[("1", &[0, 0, 0])]);
    // In grevlex: right.
    let grevlex_pairs = [[0, 1], [1, 2]];
    assert!(coprime(&g, 0, 2, grevlex));
    let grevlex_h = pm(
        3,
        vec![
            vec![(1, p(&[("-1", &[0, 1, 0])])), (2, one())],
            vec![(0, p(&[("-1", &[0, 0, 2])]))],
        ],
    );
    assert_eq!(
        lcm_representations(&g, &grevlex_pairs, &grevlex_h, grevlex),
        [(true, true), (true, true)]
    );
    let grevlex_job = job(OP, arguments(ring(&XYZ, "grevlex"), &f, "s_pairs"));
    let right = check(
        &grevlex_job,
        value(&g, c, n, s_pairs(&grevlex_pairs, grevlex_h.clone())),
    )
    .unwrap();
    assert_eq!(levels(&right), all_checked());

    // In lex: no Groebner basis.
    let (sorted, c_sorted, n_sorted) = mixup_sorted();
    assert_eq!(
        representations_hold(&f, &sorted, &c_sorted, &n_sorted),
        (true, true)
    );
    let lex_leading: Vec<Vec<u32>> = sorted.iter().map(|g| leading(&poly(g), lex).0).collect();
    assert_eq!(lex_leading, [vec![1, 0, 2], vec![1, 1, 1], vec![2, 0, 0]]);
    let z = monomial(&[0, 0, 1], Q(1, 1));
    let y = monomial(&[0, 1, 0], Q(1, 1));
    let r = add(
        &mul(&z, &poly(&sorted[1])),
        &scale(&mul(&y, &poly(&sorted[0])), Q(-1, 1)),
    );
    assert_eq!(
        r,
        poly(&p(&[
            ("-1", &[0, 2, 3]),
            ("1", &[0, 2, 2]),
            ("-1", &[0, 0, 3])
        ]))
    );
    let lm_r = leading(&r, lex).0;
    assert_eq!(lm_r, [0, 2, 3]);
    assert!(lex_leading.iter().all(|m| !divides(m, &lm_r)));
    // Every pair shares x in lex.
    for (i, j) in [(0, 1), (0, 2), (1, 2)] {
        assert!(!coprime(&sorted, i, j, lex));
    }
    let lex_job = job(OP, arguments(ring(&XYZ, "lex"), &f, "s_pairs"));

    // The grevlex S-pairs on the sorted basis: g_k becomes g_(2-k), so the
    // pair (0, 1) becomes (1, 2) and (1, 2) becomes (0, 1).
    let carried = pm(
        3,
        vec![
            vec![(2, p(&[("-1", &[0, 0, 2])]))],
            vec![(0, one()), (1, p(&[("-1", &[0, 1, 0])]))],
        ],
    );
    refused(
        check(
            &lex_job,
            value(
                &sorted,
                c_sorted.clone(),
                n_sorted.clone(),
                s_pairs(&[[0, 1], [1, 2]], carried),
            ),
        ),
        "s_pairs.pairs[1] is (1, 2), not (0, 2)",
    );

    // The lex S-pairs divided in grevlex: equations that hold, and an
    // inequality that does not.
    let lex_pairs = [[0, 1], [0, 2], [1, 2]];
    let divided = pm(
        3,
        vec![
            vec![(0, p(&[("1", &[0, 1, 0])])), (1, p(&[("-1", &[0, 0, 1])]))],
            vec![(0, p(&[("1", &[1, 0, 0])])), (2, p(&[("-1", &[0, 0, 2])]))],
            vec![(0, p(&[("-1", &[0, 0, 0])])), (1, p(&[("1", &[0, 1, 0])]))],
        ],
    );
    assert_eq!(
        lcm_representations(&sorted, &lex_pairs, &divided, lex),
        [(true, false), (true, false), (true, true)]
    );
    // S(g_0, g_1) = -r: the leading monomial y^2 z^3 of S is below L =
    // xyz^2, but none of g_0, g_1, g_2 can reach it with a smaller product.
    let (s01, l01) = s_polynomial(&poly(&sorted[0]), &poly(&sorted[1]), lex);
    assert_eq!(s01, scale(&r, Q(-1, 1)));
    assert_eq!(l01, [1, 1, 2]);
    refused(
        check(
            &lex_job,
            value(&sorted, c_sorted, n_sorted, s_pairs(&lex_pairs, divided)),
        ),
        "H_00 gives no lcm representation of S(g_0, g_1)",
    );
}

// ------------------------------------------ normal form and membership

/// y^3 + xw, outside the cubic: its normal form is x^2 w + xw, with the
/// quotients (0, x, y).
fn outside() -> Value {
    p(&[("1", &[0, 3, 0, 0]), ("1", &[1, 0, 0, 1])])
}

fn outside_remainder() -> Value {
    p(&[("1", &[2, 0, 0, 1]), ("1", &[1, 0, 0, 1])])
}

fn outside_quotients() -> Vec<Value> {
    vec![
        p(&[]),
        p(&[("1", &[1, 0, 0, 0])]),
        p(&[("1", &[0, 1, 0, 0])]),
    ]
}

fn cubic_job(operation: &str, certificate: &str) -> JobSpec {
    let mut args = arguments(ring(&XYZW, "grevlex"), &f(), certificate);
    args["polynomial"] = outside();
    job(operation, args)
}

fn normal_form(h: Value) -> Value {
    json!({
        "type": "normal_form",
        "remainder": outside_remainder(),
        "basis": g(),
        "certificate": {
            "basis_in_generators": antidiagonal(),
            "generators_in_basis": antidiagonal(),
            "quotients": outside_quotients(),
            "s_pairs": s_pairs(&CUBIC_PAIRS, h),
        },
    })
}

fn not_in(h: Value) -> Value {
    json!({
        "type": "ideal_membership",
        "member": false,
        "certificate": {
            "basis": g(),
            "generators_in_basis": antidiagonal(),
            "quotients": outside_quotients(),
            "remainder": outside_remainder(),
            "s_pairs": s_pairs(&CUBIC_PAIRS, h),
        },
    })
}

/// With the S-pairs every premise of `normal_form.unique` is checked, and
/// so is the normal form in effect.
#[test]
fn a_normal_form_with_s_pairs_is_checked() {
    let checked = check(&cubic_job(NORMAL_FORM, "s_pairs"), normal_form(cubic_h())).unwrap();
    assert!(
        levels(&checked)
            .iter()
            .all(|(_, l)| *l == EvidenceLevel::Checked)
    );
    assert_eq!(levels(&checked).len(), 6);
    assert_eq!(
        claim(&checked, "gb.groebner").method,
        Some(Method::Certificate)
    );
    assert_eq!(
        effective(&checked, "normal_form.unique"),
        EvidenceLevel::Checked
    );
    // With `representation` it is asserted in effect.
    let mut without = normal_form(cubic_h());
    without["certificate"]
        .as_object_mut()
        .unwrap()
        .remove("s_pairs");
    let asserted = check(&cubic_job(NORMAL_FORM, "representation"), without.clone()).unwrap();
    assert_eq!(
        effective(&asserted, "normal_form.unique"),
        EvidenceLevel::Asserted
    );
    refused(
        check(&cubic_job(NORMAL_FORM, "s_pairs"), without),
        "the job asked for the certificate of the S-pairs and the value has none",
    );
}

/// `gb.groebner` in a normal form: H_00 = x in place of the zero entry
/// adds x g_0 to S(g_0, g_1).
#[test]
fn a_normal_form_with_a_changed_s_pair_representation_is_refused() {
    let mut h = cubic_h();
    h["columns"][0] = json!([
        {"row": 0, "entry": p(&[("1", &[1, 0, 0, 0])])},
        {"row": 2, "entry": p(&[("-1", &[0, 0, 0, 1])])},
    ]);
    assert!(!lcm_representations(&g(), &CUBIC_PAIRS, &h, grevlex)[0].0);
    refused(
        check(&cubic_job(NORMAL_FORM, "s_pairs"), normal_form(h)),
        "S(g_0, g_1) is not sum_k g_k H_k0",
    );
}

/// With the S-pairs every premise of `membership.not_in` is checked, and so
/// is f ∉ I in effect.
#[test]
fn a_polynomial_outside_the_cubic_is_checked_with_its_s_pairs() {
    let checked = check(&cubic_job(MEMBERSHIP, "s_pairs"), not_in(cubic_h())).unwrap();
    assert_eq!(
        levels(&checked),
        [
            ("gb.reduced_form".into(), EvidenceLevel::Checked),
            ("gb.contains".into(), EvidenceLevel::Checked),
            ("gb.groebner".into(), EvidenceLevel::Checked),
            ("normal_form.reduced".into(), EvidenceLevel::Checked),
            ("membership.not_in".into(), EvidenceLevel::Checked),
        ]
    );
    assert_eq!(
        effective(&checked, "membership.not_in"),
        EvidenceLevel::Checked
    );
    // f in the ideal: the coefficients, whatever the job asked for.
    let inside = p(&[("1", &[2, 0, 0, 1]), ("-1", &[0, 3, 0, 0])]);
    let mut args = arguments(ring(&XYZW, "grevlex"), &f(), "s_pairs");
    args["polynomial"] = inside;
    let coefficients = json!({"type": "ideal_membership", "member": true, "certificate": {
        "coefficients": [p(&[("1", &[0, 1, 0, 0])]), p(&[("1", &[1, 0, 0, 0])]), p(&[])]}});
    let checked = check(&job(MEMBERSHIP, args), coefficients).unwrap();
    assert_eq!(
        levels(&checked),
        [("membership.in".into(), EvidenceLevel::Checked)]
    );
}

/// `gb.groebner` in the certificate of `member: false`: H_01 = 2x in place
/// of x gives 2x g_0, not S(g_1, g_2) = x g_0.
#[test]
fn a_membership_certificate_with_a_changed_s_pair_representation_is_refused() {
    let mut h = cubic_h();
    h["columns"][1][0]["entry"] = p(&[("2", &[1, 0, 0, 0])]);
    assert!(!lcm_representations(&g(), &CUBIC_PAIRS, &h, grevlex)[1].0);
    refused(
        check(&cubic_job(MEMBERSHIP, "s_pairs"), not_in(h)),
        "S(g_1, g_2) is not sum_k g_k H_k1",
    );
}

// ------------------------------------------------------- the summary

/// A checker whose contract the user has confirmed, for this test only:
/// the real contracts stay unconfirmed (`docs/claims.md`).
struct Confirmed<V>(V);

impl<V: OperationVerifier> OperationVerifier for Confirmed<V> {
    fn operation(&self) -> &'static str {
        self.0.operation()
    }

    fn verify(
        &self,
        spec: &JobSpec,
        value: &Value,
        context: &Context<'_>,
    ) -> Result<Vec<Claim>, CasError> {
        self.0.verify(spec, value, context)
    }

    fn catalog(&self) -> Option<&'static str> {
        Some("docs/claims.md")
    }
}

fn summary(registry: &Registry, spec: &JobSpec, value: Value) -> Summary {
    registry
        .verify(spec, result(value), &Interrupt::default())
        .unwrap()
        .guarantee
        .unwrap()
        .summary
}

/// #68, acceptance 5: with the S-pairs every claim of a Groebner basis, a
/// normal form and f ∉ I is checked, so the summary is `verified` once the
/// user confirms the contract; without them it stays `partially_verified`.
#[test]
fn a_basis_with_s_pairs_is_verified_under_a_confirmed_contract() {
    let mut confirmed = Registry::empty();
    confirmed.register(Box::new(Confirmed(GroebnerVerifier)));
    confirmed.register(Box::new(Confirmed(NormalFormVerifier)));
    confirmed.register(Box::new(Confirmed(MembershipVerifier)));
    assert_eq!(
        summary(&confirmed, &cubic_spec("s_pairs"), cubic()),
        Summary::Verified
    );
    assert_eq!(
        summary(
            &confirmed,
            &cubic_job(NORMAL_FORM, "s_pairs"),
            normal_form(cubic_h())
        ),
        Summary::Verified
    );
    assert_eq!(
        summary(
            &confirmed,
            &cubic_job(MEMBERSHIP, "s_pairs"),
            not_in(cubic_h())
        ),
        Summary::Verified
    );
    let mut representation = cubic();
    representation["certificate"]
        .as_object_mut()
        .unwrap()
        .remove("s_pairs");
    assert_eq!(
        summary(&confirmed, &cubic_spec("representation"), representation),
        Summary::PartiallyVerified
    );
    // The contracts themselves are not confirmed yet.
    let text =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/claims.md"))
            .unwrap();
    let contracts = catalogue::parse(&text);
    for op in [OP, NORMAL_FORM, MEMBERSHIP] {
        assert!(!contracts.operation(op).unwrap().confirmed(), "{op}");
    }
    assert_eq!(
        summary(&Registry::standard(), &cubic_spec("s_pairs"), cubic()),
        Summary::PartiallyVerified
    );
}

// --------------------------------------------- certificates too large

/// F = [x, y] and G = [y, x] in QQ[x, y] with grevlex, with
/// x = x (1 + y P) - y (x P) in C for P of many terms. The leading
/// monomials y and x are coprime, so there are no pairs. The checked
/// certificate is left out for its size, and `gb.groebner` becomes asserted
/// with the reason; a coprime pair listed is refused before that.
/// In QQ[x, y]: `c` P times x^shift[0] y^shift[1], plus 1 when `one`, for
/// P = sum_{k < 150000} x^(k / 400) y^(k % 400).
fn many_terms(c: &str, shift: [u32; 2], one: bool) -> Value {
    let mut terms: Vec<Value> = (0..150_000u32)
        .map(|k| json!({"coefficient": c, "exponents": [k / 400 + shift[0], k % 400 + shift[1]]}))
        .collect();
    if one {
        terms.push(json!({"coefficient": "1", "exponents": [0, 0]}));
    }
    json!({"type": "polynomial", "terms": terms})
}

/// C with x = x (1 + y P) - y (x P), for F = [x, y] and G = [y, x].
fn large_c() -> Value {
    let one = p(&[("1", &[0, 0])]);
    pm(
        2,
        vec![
            vec![(1, one)],
            vec![
                (0, many_terms("1", [0, 1], true)),
                (1, many_terms("-1", [1, 0], false)),
            ],
        ],
    )
}

#[test]
fn an_s_pairs_certificate_too_large_is_left_out() {
    let xy = ["x", "y"];
    let x = p(&[("1", &[1, 0])]);
    let y = p(&[("1", &[0, 1])]);
    let one = p(&[("1", &[0, 0])]);
    let c = large_c();
    let n = pm(2, vec![vec![(1, one.clone())], vec![(0, one)]]);
    let (f, g) = (vec![x.clone(), y.clone()], vec![y, x]);
    assert_eq!(representations_hold(&f, &g, &c, &n), (true, true));
    assert!(coprime(&g, 0, 1, grevlex));
    let spec = job(OP, arguments(ring(&xy, "grevlex"), &f, "s_pairs"));
    let listed = value(
        &g,
        c.clone(),
        n.clone(),
        s_pairs(&[[0, 1]], pm(2, vec![vec![]])),
    );
    refused(check(&spec, listed), "are coprime");
    let v = value(&g, c, n, s_pairs(&[], pm(2, vec![])));
    assert!(serde_json::to_vec(&v).unwrap().len() > VALUE_BUDGET);
    let checked = check(&spec, v).unwrap();
    assert_eq!(
        checked.value["certificate"],
        json!({"omitted": "certificate_too_large"})
    );
    assert_eq!(
        levels(&checked),
        [
            ("gb.reduced_form".into(), EvidenceLevel::Checked),
            ("gb.contained".into(), EvidenceLevel::Asserted),
            ("gb.contains".into(), EvidenceLevel::Asserted),
            ("gb.groebner".into(), EvidenceLevel::Asserted),
        ]
    );
    for id in ["gb.contained", "gb.contains", "gb.groebner"] {
        assert_eq!(
            reason(&checked, id).as_deref(),
            Some("certificate_too_large")
        );
    }
}

/// The normal form of x + 1 modulo F = [x, y] (G = [y, x], no pairs) with
/// the C of `an_s_pairs_certificate_too_large_is_left_out`: x + 1 - 1 =
/// 0 y + 1 x. The certificate is left out for its size, and `gb.groebner`,
/// asked for by the S-pairs, becomes asserted with that reason.
#[test]
fn an_s_pairs_certificate_of_a_normal_form_too_large_is_left_out() {
    let xy = ["x", "y"];
    let x = p(&[("1", &[1, 0])]);
    let y = p(&[("1", &[0, 1])]);
    let one = p(&[("1", &[0, 0])]);
    let (f, g) = (vec![x.clone(), y.clone()], vec![y, x]);
    let c = large_c();
    let n = pm(2, vec![vec![(1, one.clone())], vec![(0, one.clone())]]);
    assert_eq!(representations_hold(&f, &g, &c, &n), (true, true));
    let x_plus_one = p(&[("1", &[1, 0]), ("1", &[0, 0])]);
    let mut args = arguments(ring(&xy, "grevlex"), &f, "s_pairs");
    args["polynomial"] = x_plus_one;
    let spec = job(NORMAL_FORM, args);
    let v = json!({
        "type": "normal_form",
        "remainder": one.clone(),
        "basis": g,
        "certificate": {
            "basis_in_generators": c,
            "generators_in_basis": n,
            "quotients": [p(&[]), one],
            "s_pairs": s_pairs(&[], pm(2, vec![])),
        },
    });
    assert!(serde_json::to_vec(&v).unwrap().len() > VALUE_BUDGET);
    let checked = check(&spec, v).unwrap();
    assert_eq!(
        checked.value["certificate"],
        json!({"omitted": "certificate_too_large"})
    );
    for id in [
        "gb.contained",
        "gb.contains",
        "gb.groebner",
        "normal_form.reduced",
    ] {
        assert_eq!(
            reason(&checked, id).as_deref(),
            Some("certificate_too_large")
        );
    }
}

// ----------------------------------------------------------- deadline

/// A polynomial of 3600 terms in x and y, the last two of the variables
/// z, x, y.
fn dense() -> Value {
    let terms: Vec<Value> = (0..60u32)
        .flat_map(|i| {
            (0..60u32).map(
                move |j| json!({"coefficient": format!("{}/{}", i * 7 + j + 1, j + 2), "exponents": [0, i, j]}),
            )
        })
        .collect();
    json!({"type": "polynomial", "terms": terms})
}

/// `p` plus the monomial with exponents `e`.
fn plus(p: &Value, e: [u32; 3]) -> Value {
    let mut p = p.clone();
    p["terms"]
        .as_array_mut()
        .unwrap()
        .push(json!({"coefficient": "1", "exponents": e}));
    p
}

/// Issue #49: the check of the S-pairs stops at the job's deadline. In
/// QQ[z, x, y] with lex, F = G = (zy + D, zx + D) for D of 3600 terms in x
/// and y: the leading monomials zy < zx share z, no term of D is divisible
/// by either, and C = N = I, so every check before the S-pairs is quick.
/// H is one column of two entries of 3600 terms: G H takes some 26 million
/// products of terms, far longer than the deadline. The check never gets to
/// compare it with S(g_0, g_1), so H need not be a representation.
#[tokio::test(flavor = "multi_thread")]
async fn a_long_s_pair_check_stops_at_the_deadline() {
    let basis = vec![plus(&dense(), [1, 0, 1]), plus(&dense(), [1, 1, 0])];
    let one = p(&[("1", &[0, 0, 0])]);
    let identity = pm(2, vec![vec![(0, one.clone())], vec![(1, one)]]);
    let h = pm(2, vec![vec![(0, dense()), (1, dense())]]);
    let spec = job(
        OP,
        arguments(ring(&["z", "x", "y"], "lex"), &basis, "s_pairs"),
    );
    let result = result(value(
        &basis,
        identity.clone(),
        identity,
        s_pairs(&[[0, 1]], h),
    ));
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

/// The check of the S-pairs holds the S-polynomials of a bounded number of
/// pairs at once, so a reply that lists many pairs cheaply cannot make it
/// build all of them before it compares the first. In QQ[z, x, y] with
/// lex, G = F = (g_0, ..., g_9) with g_a = z x^a y^(9-a) + D for D of 3600
/// terms in x and y: the leading monomials share z, so all 45 pairs are
/// listed, and their S-polynomials have some 7200 terms each, more than one
/// group of pairs holds. Column 0 of H is zero, but
/// S(g_0, g_1) = x g_0 - y g_1 = (x - y) D is not. Column 44 holds
/// x^(2^32 - 1), whose product with g_0 overflows an exponent: a check that
/// built the whole product G H before comparing would refuse at that
/// overflow instead. The value is refused at S(g_0, g_1).
#[test]
fn a_wrong_s_pair_is_refused_before_the_later_pairs_are_built() {
    let t = 10u32;
    let basis: Vec<Value> = (0..t).map(|a| plus(&dense(), [1, a, t - 1 - a])).collect();
    let pairs: Vec<[usize; 2]> = (0..t as usize)
        .flat_map(|i| (i + 1..t as usize).map(move |j| [i, j]))
        .collect();
    assert_eq!(pairs.len(), 45);
    let one = p(&[("1", &[0, 0, 0])]);
    let identity = pm(
        t as usize,
        (0..t as usize).map(|k| vec![(k, one.clone())]).collect(),
    );
    let mut columns: Vec<Vec<(usize, Value)>> = vec![vec![]; pairs.len()];
    columns[44] = vec![(0, p(&[("1", &[0, u32::MAX, 0])]))];
    let h = pm(t as usize, columns);
    let g: Vec<Poly> = basis.iter().map(poly).collect();
    let (s, _) = s_polynomial(&g[0], &g[1], lex);
    assert!(!s.is_empty());
    assert!(row_times(&g, &h, 0).is_empty());
    let spec = job(
        OP,
        arguments(ring(&["z", "x", "y"], "lex"), &basis, "s_pairs"),
    );
    refused(
        check(
            &spec,
            value(&basis, identity.clone(), identity, s_pairs(&pairs, h)),
        ),
        "S(g_0, g_1) is not sum_k g_k H_k0",
    );
}

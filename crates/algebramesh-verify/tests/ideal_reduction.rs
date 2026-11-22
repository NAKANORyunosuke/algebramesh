//! The checks of `ideal.normal_form` and `ideal.membership` (Issue #67), on
//! the twisted cubic and other ideals whose normal forms, quotients and
//! coefficients are worked out by hand.
//!
//! For each claim at level checked, a test changes a correct result so that
//! the claim is false, keeping the other claims true where it can, and the
//! result is refused (ADR 0018). Each test first shows what its mutation
//! does with arithmetic of its own on the JSON (`Q`, `Poly`, `row_times`,
//! `at`, `grevlex` in `common`), not with the polynomial code the checks
//! use.

#![allow(clippy::unwrap_used)]

mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use algebramesh_core::check::check_result;
use algebramesh_core::guarantee::{EvidenceLevel, ResultVerifier};
use algebramesh_core::{CasError, CasResult, ExecutionContext, JobSpec};
use algebramesh_verify::{Registry, VALUE_BUDGET};
use common::*;
use serde_json::{Value, json};

const NORMAL_FORM: &str = "ideal.normal_form";
const MEMBERSHIP: &str = "ideal.membership";

// ------------------------------------------------------------- the JSON

fn arguments_with(
    ring: Value,
    generators: &[Value],
    polynomial: Value,
    certificate: &str,
) -> Value {
    let mut args = arguments(ring, generators, certificate);
    args["polynomial"] = polynomial;
    args
}

/// A job of `operation` on the twisted cubic in grevlex, for `polynomial`.
fn cubic_job(operation: &str, polynomial: Value, certificate: &str) -> JobSpec {
    job(
        operation,
        arguments_with(ring(&XYZW, "grevlex"), &f(), polynomial, certificate),
    )
}

fn normal_form(
    remainder: Value,
    basis: &[Value],
    c: Value,
    n: Value,
    quotients: &[Value],
) -> Value {
    json!({
        "type": "normal_form",
        "remainder": remainder,
        "basis": basis,
        "certificate": {
            "basis_in_generators": c,
            "generators_in_basis": n,
            "quotients": quotients,
        },
    })
}

fn in_ideal(coefficients: &[Value]) -> Value {
    json!({
        "type": "ideal_membership",
        "member": true,
        "certificate": {"coefficients": coefficients},
    })
}

fn not_in_ideal(basis: &[Value], n: Value, quotients: &[Value], remainder: Value) -> Value {
    json!({
        "type": "ideal_membership",
        "member": false,
        "certificate": {
            "basis": basis,
            "generators_in_basis": n,
            "quotients": quotients,
            "remainder": remainder,
        },
    })
}

fn zero() -> Value {
    p(&[])
}

// ------------------------------------------ arithmetic of the tests' own

fn sub(a: &Poly, b: &Poly) -> Poly {
    add(a, &scale(b, Q(-1, 1)))
}

/// Whether `f - r = sum_k q_k g_k`.
fn quotients_hold(f: &Value, r: &Value, g: &[Value], q: &[Value]) -> bool {
    let sum = g.iter().zip(q).fold(Poly::new(), |sum, (g, q)| {
        add(&sum, &mul(&poly(q), &poly(g)))
    });
    sub(&poly(f), &poly(r)) == sum
}

/// Whether `f = sum_i c_i f_i`.
fn coefficients_hold(generators: &[Value], f: &Value, c: &[Value]) -> bool {
    let sum = generators.iter().zip(c).fold(Poly::new(), |sum, (g, c)| {
        add(&sum, &mul(&poly(c), &poly(g)))
    });
    poly(f) == sum
}

/// Whether no term of `r` is divisible by the grevlex leading monomial of an
/// element of `g`.
fn reduced(r: &Value, g: &[Value]) -> bool {
    let leading: Vec<Vec<u32>> = g.iter().map(|g| leading(&poly(g), grevlex).0).collect();
    poly(r)
        .keys()
        .all(|e| leading.iter().all(|m| !divides(m, e)))
}

/// Every generator of the twisted cubic vanishes at [`ON_THE_CUBIC`], so
/// a polynomial that does not is outside the ideal.
fn outside_the_cubic(p: &Value) -> bool {
    assert!(f().iter().all(|f| at(&poly(f), &ON_THE_CUBIC) == Q(0, 1)));
    at(&poly(p), &ON_THE_CUBIC) != Q(0, 1)
}

// -------------------------------------------------------- twisted cubic

/// y^3 + xw, outside the ideal: y^3 = x g_1 + y g_2 + x^2 w.
fn outside() -> Value {
    p(&[("1", &[0, 3, 0, 0]), ("1", &[1, 0, 0, 1])])
}

/// The normal form of y^3 + xw: x^2 w + xw.
fn outside_remainder() -> Value {
    p(&[("1", &[2, 0, 0, 1]), ("1", &[1, 0, 0, 1])])
}

/// (0, x, y).
fn outside_quotients() -> Vec<Value> {
    vec![
        zero(),
        p(&[("1", &[1, 0, 0, 0])]),
        p(&[("1", &[0, 1, 0, 0])]),
    ]
}

/// x^2 w - y^3 = y f_0 + x f_1 = -x g_1 - y g_2, in the ideal.
fn inside() -> Value {
    p(&[("1", &[2, 0, 0, 1]), ("-1", &[0, 3, 0, 0])])
}

/// (0, -x, -y).
fn inside_quotients() -> Vec<Value> {
    vec![
        zero(),
        p(&[("-1", &[1, 0, 0, 0])]),
        p(&[("-1", &[0, 1, 0, 0])]),
    ]
}

/// (y, x, 0).
fn inside_coefficients() -> Vec<Value> {
    vec![
        p(&[("1", &[0, 1, 0, 0])]),
        p(&[("1", &[1, 0, 0, 0])]),
        zero(),
    ]
}

fn outside_normal_form() -> Value {
    normal_form(
        outside_remainder(),
        &g(),
        antidiagonal(),
        antidiagonal(),
        &outside_quotients(),
    )
}

fn outside_not_in() -> Value {
    not_in_ideal(
        &g(),
        antidiagonal(),
        &outside_quotients(),
        outside_remainder(),
    )
}

fn normal_form_checked() -> Vec<(String, EvidenceLevel)> {
    vec![
        ("gb.reduced_form".into(), EvidenceLevel::Checked),
        ("gb.contained".into(), EvidenceLevel::Checked),
        ("gb.contains".into(), EvidenceLevel::Checked),
        ("gb.groebner".into(), EvidenceLevel::Asserted),
        ("normal_form.reduced".into(), EvidenceLevel::Checked),
        ("normal_form.unique".into(), EvidenceLevel::Checked),
    ]
}

fn not_in_checked() -> Vec<(String, EvidenceLevel)> {
    vec![
        ("gb.reduced_form".into(), EvidenceLevel::Checked),
        ("gb.contains".into(), EvidenceLevel::Checked),
        ("gb.groebner".into(), EvidenceLevel::Asserted),
        ("normal_form.reduced".into(), EvidenceLevel::Checked),
        ("membership.not_in".into(), EvidenceLevel::Checked),
    ]
}

fn depends_on(result: &CasResult, id: &str) -> Vec<String> {
    let claims = &result.guarantee.as_ref().unwrap().claims;
    claims
        .iter()
        .find(|c| c.id == id)
        .unwrap()
        .depends_on
        .clone()
}

fn effective(result: &CasResult, id: &str) -> EvidenceLevel {
    result.guarantee.as_ref().unwrap().effective_level(id)
}

// ---------------------------------------------------------- normal form

#[test]
fn the_normal_form_of_a_polynomial_outside_the_cubic_is_checked() {
    assert!(quotients_hold(
        &outside(),
        &outside_remainder(),
        &g(),
        &outside_quotients()
    ));
    assert!(reduced(&outside_remainder(), &g()));
    assert!(outside_the_cubic(&outside()));
    let spec = cubic_job(NORMAL_FORM, outside(), "representation");
    let checked = check(&spec, outside_normal_form()).unwrap();
    assert_eq!(levels(&checked), normal_form_checked());
    assert_eq!(
        depends_on(&checked, "normal_form.unique"),
        [
            "normal_form.reduced",
            "gb.contained",
            "gb.contains",
            "gb.groebner"
        ]
    );
    assert_eq!(
        effective(&checked, "normal_form.unique"),
        EvidenceLevel::Asserted
    );
    // The certificate is the default.
    let mut default = spec.clone();
    default
        .arguments
        .as_object_mut()
        .unwrap()
        .remove("certificate");
    assert_eq!(
        levels(&check(&default, outside_normal_form()).unwrap()),
        normal_form_checked()
    );
}

#[test]
fn the_normal_form_of_a_member_of_the_cubic_is_zero() {
    assert!(quotients_hold(
        &inside(),
        &zero(),
        &g(),
        &inside_quotients()
    ));
    let spec = cubic_job(NORMAL_FORM, inside(), "representation");
    let value = normal_form(
        zero(),
        &g(),
        antidiagonal(),
        antidiagonal(),
        &inside_quotients(),
    );
    assert_eq!(levels(&check(&spec, value).unwrap()), normal_form_checked());
}

/// #67, acceptance 5: the effective level of `normal_form.unique` is the
/// weakest of the levels of its four premises.
#[test]
fn the_unique_normal_form_takes_the_weakest_level_of_its_premises() {
    let spec = cubic_job(NORMAL_FORM, outside(), "representation");
    let checked = check(&spec, outside_normal_form()).unwrap();
    let guarantee = checked.guarantee.unwrap();
    let premises = [
        "normal_form.reduced",
        "gb.contained",
        "gb.contains",
        "gb.groebner",
    ];
    let with = |levels: [EvidenceLevel; 4]| {
        let mut g = guarantee.clone();
        for c in &mut g.claims {
            if let Some(i) = premises.iter().position(|p| *p == c.id) {
                c.level = levels[i];
            }
        }
        g.effective_level("normal_form.unique")
    };
    use EvidenceLevel::{Asserted, Checked, CrossChecked};
    // As checked with `representation`: gb.groebner is asserted without the
    // S-pairs (#68).
    assert_eq!(with([Checked, Checked, Checked, Asserted]), Asserted);
    assert_eq!(with([Checked; 4]), Checked);
    for i in 0..4 {
        for weaker in [CrossChecked, Asserted] {
            let mut levels = [Checked; 4];
            levels[i] = weaker;
            assert_eq!(with(levels), weaker, "{} at {weaker:?}", premises[i]);
        }
    }
    // Without a certificate the premises from it are asserted.
    let mut bare = outside_normal_form();
    bare.as_object_mut().unwrap().remove("certificate");
    let none = check(&cubic_job(NORMAL_FORM, outside(), "none"), bare).unwrap();
    assert_eq!(effective(&none, "normal_form.unique"), Asserted);
}

/// `gb.contained` in a normal form: -2 f_2 is not g_0 = -f_2.
#[test]
fn a_normal_form_with_a_changed_entry_of_c_is_refused() {
    let mut c = antidiagonal();
    c["columns"][0][0]["entry"] = constant("-2");
    let generators: Vec<Poly> = f().iter().map(poly).collect();
    assert_ne!(row_times(&generators, &c, 0), poly(&g()[0]));
    let value = normal_form(
        outside_remainder(),
        &g(),
        c,
        antidiagonal(),
        &outside_quotients(),
    );
    refused(
        check(&cubic_job(NORMAL_FORM, outside(), "representation"), value),
        "g_0 is not sum_i f_i C_i0",
    );
}

/// `gb.contains` in a normal form: (x - 1) g_0 is not f_2 = -g_0.
#[test]
fn a_normal_form_with_a_changed_entry_of_n_is_refused() {
    let mut n = antidiagonal();
    n["columns"][2][0]["entry"] = p(&[("1", &[1, 0, 0, 0]), ("-1", &[0, 0, 0, 0])]);
    let basis: Vec<Poly> = g().iter().map(poly).collect();
    assert_ne!(row_times(&basis, &n, 2), poly(&f()[2]));
    let value = normal_form(
        outside_remainder(),
        &g(),
        antidiagonal(),
        n,
        &outside_quotients(),
    );
    refused(
        check(&cubic_job(NORMAL_FORM, outside(), "representation"), value),
        "f_2 is not sum_k g_k N_k2",
    );
}

/// `gb.reduced_form` in a normal form: 2 g_0 is not monic. Column 0 of C is
/// doubled and row 0 of N halved; q_0 = 0 stays, so every representation
/// holds and the remainder is still reduced.
#[test]
fn a_normal_form_with_a_non_monic_basis_element_is_refused() {
    let mut basis = g();
    basis[0] = p(&[("2", &[0, 0, 2, 0]), ("-2", &[0, 1, 0, 1])]);
    let mut c = antidiagonal();
    c["columns"][0][0]["entry"] = constant("-2");
    let mut n = antidiagonal();
    n["columns"][2][0]["entry"] = constant("-1/2");
    assert_eq!(representations_hold(&f(), &basis, &c, &n), (true, true));
    assert!(quotients_hold(
        &outside(),
        &outside_remainder(),
        &basis,
        &outside_quotients()
    ));
    assert!(reduced(&outside_remainder(), &basis));
    assert_eq!(
        leading(&poly(&basis[0]), grevlex),
        (vec![0, 0, 2, 0], Q(2, 1))
    );
    let value = normal_form(outside_remainder(), &basis, c, n, &outside_quotients());
    refused(
        check(&cubic_job(NORMAL_FORM, outside(), "representation"), value),
        "g_0 is not monic",
    );
}

/// `normal_form.reduced`: q_1 = x + 1 in place of x. The remainder is the
/// normal form still; only the certificate is wrong.
#[test]
fn a_changed_quotient_is_refused() {
    let mut quotients = outside_quotients();
    quotients[1] = p(&[("1", &[1, 0, 0, 0]), ("1", &[0, 0, 0, 0])]);
    assert!(!quotients_hold(
        &outside(),
        &outside_remainder(),
        &g(),
        &quotients
    ));
    let value = normal_form(
        outside_remainder(),
        &g(),
        antidiagonal(),
        antidiagonal(),
        &quotients,
    );
    refused(
        check(&cubic_job(NORMAL_FORM, outside(), "representation"), value),
        "f - r is not sum_k q_k g_k",
    );
}

/// `normal_form.reduced` and `normal_form.unique`: r + 1 is still reduced,
/// but f - (r + 1) is not in the ideal: it is -1 at a point where the
/// generators vanish.
#[test]
fn a_changed_remainder_is_refused() {
    let remainder = p(&[
        ("1", &[2, 0, 0, 1]),
        ("1", &[1, 0, 0, 1]),
        ("1", &[0, 0, 0, 0]),
    ]);
    assert!(reduced(&remainder, &g()));
    let difference = to_json(&sub(&poly(&outside()), &poly(&remainder)));
    assert!(outside_the_cubic(&difference));
    let value = normal_form(
        remainder,
        &g(),
        antidiagonal(),
        antidiagonal(),
        &outside_quotients(),
    );
    refused(
        check(&cubic_job(NORMAL_FORM, outside(), "representation"), value),
        "f - r is not sum_k q_k g_k",
    );
}

/// `normal_form.reduced` and `normal_form.unique`: r + g_0 with q_0 - 1 in
/// place of q_0 keeps f - r = sum_k q_k g_k, but the term z^2 of r + g_0 is
/// the leading monomial of g_0. Refused with the certificate, without it,
/// and in the certificate of `member: false`.
#[test]
fn a_remainder_not_reduced_is_refused() {
    let remainder = to_json(&add(&poly(&outside_remainder()), &poly(&g()[0])));
    let mut quotients = outside_quotients();
    quotients[0] = constant("-1");
    assert!(quotients_hold(&outside(), &remainder, &g(), &quotients));
    assert!(!reduced(&remainder, &g()));
    let why = "a term of the remainder is divisible by the leading monomial of g_0";
    let value = normal_form(
        remainder.clone(),
        &g(),
        antidiagonal(),
        antidiagonal(),
        &quotients,
    );
    let spec = cubic_job(NORMAL_FORM, outside(), "representation");
    refused(check(&spec, value.clone()), why);
    let mut bare = value;
    bare.as_object_mut().unwrap().remove("certificate");
    refused(check(&cubic_job(NORMAL_FORM, outside(), "none"), bare), why);
    let not_in = not_in_ideal(&g(), antidiagonal(), &quotients, remainder);
    refused(
        check(&cubic_job(MEMBERSHIP, outside(), "representation"), not_in),
        why,
    );
}

/// The counts and shapes of a normal form.
#[test]
fn quotients_of_another_number_are_refused() {
    let spec = cubic_job(NORMAL_FORM, outside(), "representation");
    let mut two = outside_normal_form();
    two["certificate"]["quotients"]
        .as_array_mut()
        .unwrap()
        .pop();
    refused(check(&spec, two), "2 quotients, not 3");
    let mut shape = outside_normal_form();
    shape["certificate"]["basis_in_generators"]["rows"] = json!(4);
    refused(check(&spec, shape), "C is 4 x 3, not 3 x 3");
    let mut ring = outside_normal_form();
    ring["remainder"]["terms"][0]["exponents"] = json!([1, 0, 0]);
    refused(check(&spec, ring), "the remainder");
}

// ----------------------------------------------------------- membership

#[test]
fn a_member_of_the_cubic_is_checked_by_its_coefficients() {
    assert!(coefficients_hold(&f(), &inside(), &inside_coefficients()));
    let spec = cubic_job(MEMBERSHIP, inside(), "representation");
    let checked = check(&spec, in_ideal(&inside_coefficients())).unwrap();
    assert_eq!(
        levels(&checked),
        [("membership.in".into(), EvidenceLevel::Checked)]
    );
}

/// `membership.in`: c_0 = y + 1 in place of y adds f_0 to the sum.
#[test]
fn a_changed_coefficient_is_refused() {
    let mut coefficients = inside_coefficients();
    coefficients[0] = p(&[("1", &[0, 1, 0, 0]), ("1", &[0, 0, 0, 0])]);
    assert!(!coefficients_hold(&f(), &inside(), &coefficients));
    refused(
        check(
            &cubic_job(MEMBERSHIP, inside(), "representation"),
            in_ideal(&coefficients),
        ),
        "f is not sum_i c_i f_i",
    );
}

/// `membership.in`: y^3 + xw is not in the ideal (it is 2 at a point where
/// the generators vanish), whatever the coefficients.
#[test]
fn a_polynomial_outside_the_ideal_claimed_as_a_member_is_refused() {
    assert!(outside_the_cubic(&outside()));
    refused(
        check(
            &cubic_job(MEMBERSHIP, outside(), "representation"),
            in_ideal(&inside_coefficients()),
        ),
        "f is not sum_i c_i f_i",
    );
}

#[test]
fn a_polynomial_outside_the_cubic_is_checked_by_its_normal_form() {
    let spec = cubic_job(MEMBERSHIP, outside(), "representation");
    let checked = check(&spec, outside_not_in()).unwrap();
    assert_eq!(levels(&checked), not_in_checked());
    assert_eq!(
        depends_on(&checked, "membership.not_in"),
        ["normal_form.reduced", "gb.contains", "gb.groebner"]
    );
    assert_eq!(
        effective(&checked, "membership.not_in"),
        EvidenceLevel::Asserted
    );
    // Were gb.groebner checked (#68, with `s_pairs`), so would be
    // membership.not_in.
    let mut guarantee = checked.guarantee.unwrap();
    for c in &mut guarantee.claims {
        if c.id == "gb.groebner" {
            c.level = EvidenceLevel::Checked;
        }
    }
    assert_eq!(
        guarantee.effective_level("membership.not_in"),
        EvidenceLevel::Checked
    );
}

/// #67, acceptance 4: x^2 w - y^3 = y f_0 + x f_1 is in the ideal; a value
/// that says it is not, with the remainder 0 and quotients that hold, is
/// refused.
#[test]
fn a_zero_remainder_claiming_not_in_is_refused() {
    assert!(coefficients_hold(&f(), &inside(), &inside_coefficients()));
    assert!(quotients_hold(
        &inside(),
        &zero(),
        &g(),
        &inside_quotients()
    ));
    let value = not_in_ideal(&g(), antidiagonal(), &inside_quotients(), zero());
    refused(
        check(&cubic_job(MEMBERSHIP, inside(), "representation"), value),
        "member is false and the remainder is 0",
    );
}

/// `normal_form.reduced` in the certificate of `member: false`: q_2 = 2y.
#[test]
fn a_membership_certificate_with_a_changed_quotient_is_refused() {
    let mut quotients = outside_quotients();
    quotients[2] = p(&[("2", &[0, 1, 0, 0])]);
    assert!(!quotients_hold(
        &outside(),
        &outside_remainder(),
        &g(),
        &quotients
    ));
    let value = not_in_ideal(&g(), antidiagonal(), &quotients, outside_remainder());
    refused(
        check(&cubic_job(MEMBERSHIP, outside(), "representation"), value),
        "f - r is not sum_k q_k g_k",
    );
}

/// `normal_form.reduced` in the certificate of `member: false`: r + 1 is
/// reduced and not 0, but f - (r + 1) is not in the ideal.
#[test]
fn a_membership_certificate_with_a_changed_remainder_is_refused() {
    let remainder = p(&[
        ("1", &[2, 0, 0, 1]),
        ("1", &[1, 0, 0, 1]),
        ("1", &[0, 0, 0, 0]),
    ]);
    assert!(reduced(&remainder, &g()));
    let difference = to_json(&sub(&poly(&outside()), &poly(&remainder)));
    assert!(outside_the_cubic(&difference));
    let value = not_in_ideal(&g(), antidiagonal(), &outside_quotients(), remainder);
    refused(
        check(&cubic_job(MEMBERSHIP, outside(), "representation"), value),
        "f - r is not sum_k q_k g_k",
    );
}

/// `gb.contains` in the certificate of `member: false`: (x - 1) g_0 is not
/// f_2 = -g_0.
#[test]
fn a_membership_certificate_with_a_changed_entry_of_n_is_refused() {
    let mut n = antidiagonal();
    n["columns"][2][0]["entry"] = p(&[("1", &[1, 0, 0, 0]), ("-1", &[0, 0, 0, 0])]);
    let basis: Vec<Poly> = g().iter().map(poly).collect();
    assert_ne!(row_times(&basis, &n, 2), poly(&f()[2]));
    let value = not_in_ideal(&g(), n, &outside_quotients(), outside_remainder());
    refused(
        check(&cubic_job(MEMBERSHIP, outside(), "representation"), value),
        "f_2 is not sum_k g_k N_k2",
    );
}

/// `gb.reduced_form` in the certificate of `member: false`: G in decreasing
/// order y^2 > yz > z^2, with N = -I and the quotients reversed, so that
/// every representation holds and the remainder is still reduced.
#[test]
fn a_membership_certificate_with_a_basis_out_of_order_is_refused() {
    let mut basis = g();
    basis.reverse();
    let mut quotients = outside_quotients();
    quotients.reverse();
    let n = pm(3, (0..3).map(|j| vec![(j, constant("-1"))]).collect());
    let c = n.clone();
    assert_eq!(representations_hold(&f(), &basis, &c, &n), (true, true));
    assert!(quotients_hold(
        &outside(),
        &outside_remainder(),
        &basis,
        &quotients
    ));
    assert!(reduced(&outside_remainder(), &basis));
    let lm = |j: usize| leading(&poly(&basis[j]), grevlex).0;
    assert_eq!(grevlex(&lm(0), &lm(1)), std::cmp::Ordering::Greater);
    let value = not_in_ideal(&basis, n, &quotients, outside_remainder());
    refused(
        check(&cubic_job(MEMBERSHIP, outside(), "representation"), value),
        "the leading monomials of g_0 and g_1 are not in strictly increasing order",
    );
}

/// The certificate of `member: false` carries no C, so (G) ⊆ I is not
/// checked and G need not be a basis of I (not_claimed in `docs/claims.md`).
/// I = (x^2) and f = y with G = (x): x^2 = x * x, y - y = 0 * x, and y is
/// reduced, so the value is accepted. f ∉ (x) ⊇ I still holds, and the
/// result claims neither `gb.contained` nor `normal_form.unique`.
#[test]
fn a_membership_certificate_whose_basis_generates_a_larger_ideal_is_accepted() {
    let generators = [p(&[("1", &[2, 0])])];
    let polynomial = p(&[("1", &[0, 1])]);
    let basis = [p(&[("1", &[1, 0])])];
    let n = pm(1, vec![vec![(0, basis[0].clone())]]);
    let as_poly: Vec<Poly> = basis.iter().map(poly).collect();
    assert_eq!(row_times(&as_poly, &n, 0), poly(&generators[0]));
    assert!(quotients_hold(&polynomial, &polynomial, &basis, &[zero()]));
    assert!(reduced(&polynomial, &basis));
    let spec = job(
        MEMBERSHIP,
        arguments_with(
            ring(&["x", "y"], "grevlex"),
            &generators,
            polynomial.clone(),
            "representation",
        ),
    );
    let value = not_in_ideal(&basis, n, &[zero()], polynomial);
    let checked = check(&spec, value).unwrap();
    assert_eq!(levels(&checked), not_in_checked());
}

/// A certificate of the kind of the other answer is refused.
#[test]
fn a_membership_certificate_of_the_wrong_kind_is_refused() {
    let mut member = outside_not_in();
    member["member"] = json!(true);
    refused(
        check(&cubic_job(MEMBERSHIP, outside(), "representation"), member),
        "member is true and the certificate is not one of coefficients",
    );
    let mut not_member = in_ideal(&inside_coefficients());
    not_member["member"] = json!(false);
    refused(
        check(
            &cubic_job(MEMBERSHIP, inside(), "representation"),
            not_member,
        ),
        "member is false and the certificate is one of coefficients",
    );
    // The same without a certificate requested.
    let mut member = outside_not_in();
    member["member"] = json!(true);
    refused(
        check(&cubic_job(MEMBERSHIP, outside(), "none"), member),
        "member is true",
    );
}

#[test]
fn coefficients_of_another_number_are_refused() {
    let mut coefficients = inside_coefficients();
    coefficients.pop();
    refused(
        check(
            &cubic_job(MEMBERSHIP, inside(), "representation"),
            in_ideal(&coefficients),
        ),
        "2 coefficients, not 3",
    );
    let mut value = outside_not_in();
    value["certificate"]["quotients"] = json!([zero()]);
    refused(
        check(&cubic_job(MEMBERSHIP, outside(), "representation"), value),
        "1 quotients, not 3",
    );
}

// --------------------------------------------- certificates left out

#[test]
fn without_a_certificate_the_claims_it_supports_are_asserted() {
    let mut bare = outside_normal_form();
    bare.as_object_mut().unwrap().remove("certificate");
    let checked = check(&cubic_job(NORMAL_FORM, outside(), "none"), bare).unwrap();
    assert_eq!(
        levels(&checked),
        [
            ("gb.reduced_form".into(), EvidenceLevel::Checked),
            ("gb.contained".into(), EvidenceLevel::Asserted),
            ("gb.contains".into(), EvidenceLevel::Asserted),
            ("gb.groebner".into(), EvidenceLevel::Asserted),
            ("normal_form.reduced".into(), EvidenceLevel::Asserted),
            ("normal_form.unique".into(), EvidenceLevel::Checked),
        ]
    );
    for id in ["gb.contained", "gb.contains", "normal_form.reduced"] {
        assert_eq!(
            reason(&checked, id).as_deref(),
            Some("the job asked for no certificate")
        );
    }
    assert_eq!(
        reason(&checked, "gb.groebner").as_deref(),
        Some("the job asked for no certificate of the S-pairs")
    );
    assert_eq!(depends_on(&checked, "normal_form.unique").len(), 4);

    let bare = json!({"type": "ideal_membership", "member": true});
    let checked = check(&cubic_job(MEMBERSHIP, inside(), "none"), bare).unwrap();
    assert_eq!(
        levels(&checked),
        [("membership.in".into(), EvidenceLevel::Asserted)]
    );
    assert_eq!(
        reason(&checked, "membership.in").as_deref(),
        Some("the job asked for no certificate")
    );
    let bare = json!({"type": "ideal_membership", "member": false});
    let checked = check(&cubic_job(MEMBERSHIP, outside(), "none"), bare).unwrap();
    assert_eq!(
        levels(&checked),
        [("membership.not_in".into(), EvidenceLevel::Asserted)]
    );
    assert!(depends_on(&checked, "membership.not_in").is_empty());
}

/// ADR 0018: only the job (certificate: none) or the checker itself (for the
/// size) may leave a certificate out.
#[test]
fn a_certificate_the_backend_leaves_out_of_a_normal_form_or_a_membership_is_refused() {
    let omitted = json!({"omitted": "certificate_too_large"});
    let normal = cubic_job(NORMAL_FORM, outside(), "representation");
    let mut without = outside_normal_form();
    without.as_object_mut().unwrap().remove("certificate");
    refused(check(&normal, without), "has none");
    let mut marked = outside_normal_form();
    marked["certificate"] = omitted.clone();
    refused(check(&normal, marked), "only the checker");

    let member = cubic_job(MEMBERSHIP, outside(), "representation");
    refused(
        check(
            &member,
            json!({"type": "ideal_membership", "member": false}),
        ),
        "has none",
    );
    refused(
        check(
            &member,
            json!({"type": "ideal_membership", "member": false, "certificate": omitted}),
        ),
        "only the checker",
    );
}

// ------------------------------------------------------ refused inputs

/// The arguments are refused under the rules of `ideal.groebner_basis`, and
/// the polynomial must belong to the ring of the ideal.
#[test]
fn normal_form_and_membership_jobs_are_refused_as_groebner_jobs() {
    for (op, value) in [
        (NORMAL_FORM, outside_normal_form()),
        (MEMBERSHIP, outside_not_in()),
    ] {
        let mut zz = cubic_job(op, outside(), "representation");
        zz.arguments["ideal"]["ring"]["domain"] = json!("ZZ");
        refused(check(&zz, value.clone()), "over QQ only");
        let mut missing = cubic_job(op, outside(), "representation");
        missing
            .arguments
            .as_object_mut()
            .unwrap()
            .remove("polynomial");
        refused(check(&missing, value.clone()), "the arguments of the job");
        let short = p(&[("1", &[1, 0, 0])]);
        refused(
            check(&cubic_job(op, short, "representation"), value),
            "the polynomial",
        );
    }
}

// ----------------------------------------------------- the zero and unit

/// The zero ideal: G is empty, the normal form of f is f itself with no
/// quotients, and f is a member only when it is 0. The unit ideal: G = (1),
/// every polynomial reduces to 0 and is a member.
#[test]
fn the_zero_ideal_and_the_unit_ideal_give_normal_forms_and_memberships() {
    let x = ["x"];
    let polynomial = p(&[("1", &[2]), ("3", &[0])]);
    let zero_ideal = [zero()];
    let c = pm(1, vec![]);
    let n = pm(0, vec![vec![]]);
    let spec = |op: &str, f: Value| {
        job(
            op,
            arguments_with(ring(&x, "lex"), &zero_ideal, f, "representation"),
        )
    };
    let value = normal_form(polynomial.clone(), &[], c, n.clone(), &[]);
    assert_eq!(
        levels(&check(&spec(NORMAL_FORM, polynomial.clone()), value).unwrap()),
        normal_form_checked()
    );
    let value = not_in_ideal(&[], n, &[], polynomial.clone());
    assert_eq!(
        levels(&check(&spec(MEMBERSHIP, polynomial.clone()), value).unwrap()),
        not_in_checked()
    );
    let value = in_ideal(&[zero()]);
    assert_eq!(
        levels(&check(&spec(MEMBERSHIP, zero()), value).unwrap()),
        [("membership.in".into(), EvidenceLevel::Checked)]
    );

    // (x + 1) - x = 1; x^2 + 3 = 1 (x^2 + 3) = 3 (x + 1) + (x - 3) x.
    let generators = [p(&[("1", &[1]), ("1", &[0])]), p(&[("1", &[1])])];
    let basis = [p(&[("1", &[0])])];
    let c = pm(
        2,
        vec![vec![(0, p(&[("1", &[0])])), (1, p(&[("-1", &[0])]))]],
    );
    let n = pm(
        1,
        vec![
            vec![(0, generators[0].clone())],
            vec![(0, generators[1].clone())],
        ],
    );
    assert_eq!(
        representations_hold(&generators, &basis, &c, &n),
        (true, true)
    );
    assert!(quotients_hold(
        &polynomial,
        &zero(),
        &basis,
        std::slice::from_ref(&polynomial)
    ));
    let coefficients = [p(&[("3", &[0])]), p(&[("1", &[1]), ("-3", &[0])])];
    assert!(coefficients_hold(&generators, &polynomial, &coefficients));
    let spec = |op: &str| {
        job(
            op,
            arguments_with(
                ring(&x, "grevlex"),
                &generators,
                polynomial.clone(),
                "representation",
            ),
        )
    };
    let value = normal_form(zero(), &basis, c, n, std::slice::from_ref(&polynomial));
    assert_eq!(
        levels(&check(&spec(NORMAL_FORM), value).unwrap()),
        normal_form_checked()
    );
    assert_eq!(
        levels(&check(&spec(MEMBERSHIP), in_ideal(&coefficients)).unwrap()),
        [("membership.in".into(), EvidenceLevel::Checked)]
    );
}

// --------------------------------------------- certificates too large

/// sum_{k < size} c x^(k / 400 + shift_x) y^(k % 400 + shift_y), and 1 as
/// well when `one`.
fn many_terms(size: u32, c: &str, shift: [u32; 2], one: bool) -> Value {
    let mut terms: Vec<Value> = (0..size)
        .map(|k| json!({"coefficient": c, "exponents": [k / 400 + shift[0], k % 400 + shift[1]]}))
        .collect();
    if one {
        terms.push(json!({"coefficient": "1", "exponents": [0, 0]}));
    }
    json!({"type": "polynomial", "terms": terms})
}

/// #67, acceptance 6: F = [x, y] and G = [y, x] in QQ[x, y] with grevlex,
/// with x = x (1 + y P) - y (x P) for P of many terms in C or in the
/// coefficients, and quotients of many terms: each value exceeds the limit
/// of a result, the checked certificate is left out, and the claims it
/// supports become asserted.
#[test]
fn certificates_of_normal_forms_and_memberships_too_large_are_left_out() {
    let xy = ["x", "y"];
    let x = p(&[("1", &[1, 0])]);
    let y = p(&[("1", &[0, 1])]);
    let one = p(&[("1", &[0, 0])]);
    let (generators, basis) = (vec![x.clone(), y.clone()], vec![y, x.clone()]);
    let size = 150_000;
    let one_plus_yp = many_terms(size, "1", [0, 1], true);
    let minus_xp = many_terms(size, "-1", [1, 0], false);
    let c = pm(
        2,
        vec![
            vec![(1, one.clone())],
            vec![(0, one_plus_yp.clone()), (1, minus_xp.clone())],
        ],
    );
    let n = pm(2, vec![vec![(1, one.clone())], vec![(0, one.clone())]]);
    assert_eq!(
        representations_hold(&generators, &basis, &c, &n),
        (true, true)
    );
    let spec = |op: &str, f: Value| {
        job(
            op,
            arguments_with(ring(&xy, "grevlex"), &generators, f, "representation"),
        )
    };
    let left_out = |checked: &CasResult| {
        assert_eq!(
            checked.value["certificate"],
            json!({"omitted": "certificate_too_large"})
        );
    };

    // x + 1 = 0 y + 1 x + 1.
    let f = p(&[("1", &[1, 0]), ("1", &[0, 0])]);
    let quotients = [zero(), one.clone()];
    assert!(quotients_hold(&f, &one, &basis, &quotients));
    let value = normal_form(one.clone(), &basis, c, n.clone(), &quotients);
    assert!(serde_json::to_vec(&value).unwrap().len() > VALUE_BUDGET);
    let checked = check(&spec(NORMAL_FORM, f), value).unwrap();
    left_out(&checked);
    assert_eq!(
        levels(&checked),
        [
            ("gb.reduced_form".into(), EvidenceLevel::Checked),
            ("gb.contained".into(), EvidenceLevel::Asserted),
            ("gb.contains".into(), EvidenceLevel::Asserted),
            ("gb.groebner".into(), EvidenceLevel::Asserted),
            ("normal_form.reduced".into(), EvidenceLevel::Asserted),
            ("normal_form.unique".into(), EvidenceLevel::Checked),
        ]
    );
    for id in ["gb.contained", "gb.contains", "normal_form.reduced"] {
        assert_eq!(
            reason(&checked, id).as_deref(),
            Some("certificate_too_large")
        );
    }
    // The job asked for no S-pairs, so gb.groebner keeps that reason.
    assert_eq!(
        reason(&checked, "gb.groebner").as_deref(),
        Some("the job asked for no certificate of the S-pairs")
    );

    // x = (1 + y P) x - (x P) y.
    let coefficients = [one_plus_yp, minus_xp];
    assert!(coefficients_hold(&generators, &x, &coefficients));
    let value = in_ideal(&coefficients);
    assert!(serde_json::to_vec(&value).unwrap().len() > VALUE_BUDGET);
    let checked = check(&spec(MEMBERSHIP, x.clone()), value).unwrap();
    left_out(&checked);
    assert_eq!(
        levels(&checked),
        [("membership.in".into(), EvidenceLevel::Asserted)]
    );
    assert_eq!(
        reason(&checked, "membership.in").as_deref(),
        Some("certificate_too_large")
    );

    // 1 + x P' = 0 y + P' x + 1, for P' of more terms.
    let f = many_terms(250_000, "1", [1, 0], true);
    let quotients = [zero(), many_terms(250_000, "1", [0, 0], false)];
    assert!(quotients_hold(&f, &one, &basis, &quotients));
    let value = not_in_ideal(&basis, n, &quotients, one);
    assert!(serde_json::to_vec(&value).unwrap().len() > VALUE_BUDGET);
    let checked = check(&spec(MEMBERSHIP, f), value).unwrap();
    left_out(&checked);
    assert_eq!(
        levels(&checked),
        [("membership.not_in".into(), EvidenceLevel::Asserted)]
    );
    assert_eq!(
        reason(&checked, "membership.not_in").as_deref(),
        Some("certificate_too_large")
    );
}

// ----------------------------------------------------------- deadline

/// A polynomial of 2025 terms in x and y.
fn dense() -> Value {
    let terms: Vec<Value> = (0..45u32)
        .flat_map(|i| {
            (0..45u32).map(
                move |j| json!({"coefficient": format!("{}/{}", i * 7 + j + 1, j + 2), "exponents": [i, j]}),
            )
        })
        .collect();
    json!({"type": "polynomial", "terms": terms})
}

/// Issue #49: the check of large coefficients stops at the job's deadline.
/// F is four dense polynomials and so are the coefficients: the sum
/// sum_i c_i f_i takes some 16 million products of terms, far longer than
/// the deadline. The check never gets to compare it with f = y.
#[tokio::test(flavor = "multi_thread")]
async fn a_long_membership_check_stops_at_the_deadline() {
    let generators = vec![dense(); 4];
    let spec = job(
        MEMBERSHIP,
        arguments_with(
            ring(&["x", "y"], "grevlex"),
            &generators,
            p(&[("1", &[0, 1])]),
            "representation",
        ),
    );
    let result = result(in_ideal(&vec![dense(); 4]));
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

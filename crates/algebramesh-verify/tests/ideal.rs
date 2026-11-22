//! The checks of `ideal.groebner_basis` (Issue #66), on bases and
//! transformation matrices worked out by hand or checked by expansion.
//!
//! For each claim at level checked, a test changes a correct result so that
//! the claim is false, keeping the other claims true where it can, and the
//! result is refused (ADR 0018). Other mutations keep the claims true and
//! make only the certificate wrong. Each test first shows what its mutation
//! does with arithmetic of its own on the JSON (`Q`, `Poly`, `row_times`,
//! `at`, `grevlex` in `common`), not with the polynomial code the checks
//! use.

#![allow(clippy::unwrap_used)]

mod common;

use std::cmp::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use algebramesh_core::check::check_result;
use algebramesh_core::guarantee::{EvidenceLevel, ResultVerifier};
use algebramesh_core::{CasError, ExecutionContext, JobSpec};
use algebramesh_verify::{Registry, VALUE_BUDGET};
use common::*;
use serde_json::{Value, json};

const OP: &str = "ideal.groebner_basis";

fn spec(arguments: Value) -> JobSpec {
    job(OP, arguments)
}

/// The levels of a basis whose certificate `representation` is checked:
/// all but `gb.groebner`, which needs the S-pairs (#68, in
/// `ideal_s_pairs.rs`).
fn representation_checked() -> Vec<(String, EvidenceLevel)> {
    vec![
        ("gb.reduced_form".into(), EvidenceLevel::Checked),
        ("gb.contained".into(), EvidenceLevel::Checked),
        ("gb.contains".into(), EvidenceLevel::Checked),
        ("gb.groebner".into(), EvidenceLevel::Asserted),
    ]
}

fn value(basis: &[Value], c: Value, n: Value) -> Value {
    json!({
        "type": "groebner_basis",
        "basis": basis,
        "certificate": {"basis_in_generators": c, "generators_in_basis": n},
    })
}

fn cubic_spec(certificate: &str) -> JobSpec {
    spec(arguments(ring(&XYZW, "grevlex"), &f(), certificate))
}

fn cubic() -> Value {
    value(&g(), antidiagonal(), antidiagonal())
}

#[test]
fn the_twisted_cubic_basis_is_checked() {
    assert_eq!(
        representations_hold(&f(), &g(), &antidiagonal(), &antidiagonal()),
        (true, true)
    );
    let lms: Vec<Vec<u32>> = g().iter().map(|g| leading(&poly(g), grevlex).0).collect();
    assert_eq!(lms, [vec![0, 0, 2, 0], vec![0, 1, 1, 0], vec![0, 2, 0, 0]]);
    assert!(
        lms.windows(2)
            .all(|w| grevlex(&w[0], &w[1]) == Ordering::Less)
    );
    let checked = check(&cubic_spec("representation"), cubic()).unwrap();
    assert_eq!(levels(&checked), representation_checked());
    assert_eq!(
        reason(&checked, "gb.groebner").as_deref(),
        Some("the job asked for no certificate of the S-pairs")
    );
    // The certificate is the default.
    let mut default = cubic_spec("representation");
    default
        .arguments
        .as_object_mut()
        .unwrap()
        .remove("certificate");
    assert_eq!(
        levels(&check(&default, cubic()).unwrap()),
        representation_checked()
    );
}

/// #66, acceptance 3: one entry of C changed. The claims stay true; only
/// the certificate is wrong: -2 f_2 is not g_0 = -f_2.
#[test]
fn a_changed_entry_of_c_is_refused() {
    let mut c = antidiagonal();
    c["columns"][0][0]["entry"] = constant("-2");
    let f: Vec<Poly> = f().iter().map(poly).collect();
    assert_ne!(row_times(&f, &c, 0), poly(&g()[0]));
    refused(
        check(
            &cubic_spec("representation"),
            value(&g(), c, antidiagonal()),
        ),
        "g_0 is not sum_i f_i C_i0",
    );
}

/// #66, acceptance 3: one entry of N changed, to x - 1: (x - 1) g_0 is not
/// f_2 = -g_0.
#[test]
fn a_changed_entry_of_n_is_refused() {
    let mut n = antidiagonal();
    n["columns"][2][0]["entry"] = p(&[("1", &[1, 0, 0, 0]), ("-1", &[0, 0, 0, 0])]);
    let basis: Vec<Poly> = g().iter().map(poly).collect();
    assert_ne!(row_times(&basis, &n, 2), poly(&f()[2]));
    refused(
        check(
            &cubic_spec("representation"),
            value(&g(), antidiagonal(), n),
        ),
        "f_2 is not sum_k g_k N_k2",
    );
}

/// `gb.contained`: g_0 + 1 is not in I, as the generators vanish at
/// (1, 1, 1, 1) and it does not. The rest of the reduced form is kept.
#[test]
fn a_basis_element_outside_the_ideal_is_refused() {
    let mut basis = g();
    basis[0] = p(&[
        ("1", &[0, 0, 2, 0]),
        ("-1", &[0, 1, 0, 1]),
        ("1", &[0, 0, 0, 0]),
    ]);
    for f in f() {
        assert_eq!(at(&poly(&f), &ON_THE_CUBIC), Q(0, 1));
    }
    assert_eq!(at(&poly(&basis[0]), &ON_THE_CUBIC), Q(1, 1));
    refused(
        check(
            &cubic_spec("representation"),
            value(&basis, antidiagonal(), antidiagonal()),
        ),
        "g_0 is not sum_i f_i C_i0",
    );
}

/// `gb.contained`, and the input comes from the specification: the basis of
/// the twisted cubic is refused for F' = [xz - y^2, xw - yz, yw], whose
/// generators vanish at (0, 0, 1, 0) where g_0 = z^2 - yw is 1.
#[test]
fn a_basis_of_another_ideal_is_refused() {
    let mut other = f();
    other[2] = p(&[("1", &[0, 1, 0, 1])]);
    let point = [0, 0, 1, 0];
    for f in &other {
        assert_eq!(at(&poly(f), &point), Q(0, 1));
    }
    assert_eq!(at(&poly(&g()[0]), &point), Q(1, 1));
    let spec = spec(arguments(ring(&XYZW, "grevlex"), &other, "representation"));
    refused(check(&spec, cubic()), "g_0 is not sum_i f_i C_i0");
}

/// `gb.contains`: without y^2 - xz, G spans a smaller ideal. At (0, 1, 0, 0)
/// g_0 and g_1 vanish but f_0 = -1. C keeps its first two columns, which
/// are still right, and N loses its last row.
#[test]
fn a_basis_of_a_smaller_ideal_is_refused() {
    let basis = g()[..2].to_vec();
    let c = pm(
        3,
        vec![vec![(2, constant("-1"))], vec![(1, constant("-1"))]],
    );
    let n = pm(
        2,
        vec![vec![], vec![(1, constant("-1"))], vec![(0, constant("-1"))]],
    );
    let point = [0, 1, 0, 0];
    assert!(basis.iter().all(|g| at(&poly(g), &point) == Q(0, 1)));
    assert_eq!(at(&poly(&f()[0]), &point), Q(-1, 1));
    assert!(representations_hold(&f(), &basis, &c, &n).0);
    refused(
        check(&cubic_spec("representation"), value(&basis, c, n)),
        "f_0 is not sum_k g_k N_k0",
    );
}

/// `gb.reduced_form`: 2 g_0 is not monic. Column 0 of C is doubled and row 0
/// of N halved, so that both representations hold.
#[test]
fn a_non_monic_basis_element_is_refused() {
    let mut basis = g();
    basis[0] = p(&[("2", &[0, 0, 2, 0]), ("-2", &[0, 1, 0, 1])]);
    let mut c = antidiagonal();
    c["columns"][0][0]["entry"] = constant("-2");
    let mut n = antidiagonal();
    n["columns"][2][0]["entry"] = constant("-1/2");
    assert_eq!(representations_hold(&f(), &basis, &c, &n), (true, true));
    assert_eq!(
        leading(&poly(&basis[0]), grevlex),
        (vec![0, 0, 2, 0], Q(2, 1))
    );
    refused(
        check(&cubic_spec("representation"), value(&basis, c, n)),
        "g_0 is not monic",
    );
}

/// `gb.reduced_form`: g_2 + g_1 = y^2 + yz - xz - xw keeps the leading
/// monomial y^2, but its term yz is the leading monomial of g_1. C and N
/// follow: column 2 of C gains column 1, and f_0 = -g_2 = g_1 - (g_2 + g_1).
#[test]
fn a_basis_not_in_reduced_form_is_refused() {
    let mut basis = g();
    basis[2] = p(&[
        ("1", &[0, 2, 0, 0]),
        ("1", &[0, 1, 1, 0]),
        ("-1", &[1, 0, 1, 0]),
        ("-1", &[1, 0, 0, 1]),
    ]);
    assert_eq!(poly(&basis[2]), add(&poly(&g()[2]), &poly(&g()[1])));
    let c = pm(
        3,
        vec![
            vec![(2, constant("-1"))],
            vec![(1, constant("-1"))],
            vec![(0, constant("-1")), (1, constant("-1"))],
        ],
    );
    let n = pm(
        3,
        vec![
            vec![(1, constant("1")), (2, constant("-1"))],
            vec![(1, constant("-1"))],
            vec![(0, constant("-1"))],
        ],
    );
    assert_eq!(representations_hold(&f(), &basis, &c, &n), (true, true));
    let g2 = poly(&basis[2]);
    let lm1 = leading(&poly(&g()[1]), grevlex).0;
    assert_eq!(leading(&g2, grevlex), (vec![0, 2, 0, 0], Q(1, 1)));
    assert!(g2.keys().any(|e| divides(&lm1, e)));
    refused(
        check(&cubic_spec("representation"), value(&basis, c, n)),
        "a term of g_2 is divisible by the leading monomial of g_1",
    );
}

/// `gb.reduced_form`: G in decreasing order, y^2 > yz > z^2. C and N are
/// then -I.
#[test]
fn basis_elements_out_of_order_are_refused() {
    let mut basis = g();
    basis.reverse();
    let minus_identity = || pm(3, (0..3).map(|j| vec![(j, constant("-1"))]).collect());
    let (c, n) = (minus_identity(), minus_identity());
    assert_eq!(representations_hold(&f(), &basis, &c, &n), (true, true));
    let lm = |j: usize| leading(&poly(&basis[j]), grevlex).0;
    assert_eq!(grevlex(&lm(0), &lm(1)), Ordering::Greater);
    refused(
        check(&cubic_spec("representation"), value(&basis, c, n)),
        "the leading monomials of g_0 and g_1 are not in strictly increasing order",
    );
}

/// `gb.reduced_form`: a zero element, with a zero column of C and a zero
/// row of N, keeps both representations.
#[test]
fn a_zero_basis_element_is_refused() {
    let mut basis = g();
    basis.push(p(&[]));
    let c = pm(
        3,
        vec![
            vec![(2, constant("-1"))],
            vec![(1, constant("-1"))],
            vec![(0, constant("-1"))],
            vec![],
        ],
    );
    let mut n = antidiagonal();
    n["rows"] = json!(4);
    assert_eq!(representations_hold(&f(), &basis, &c, &n), (true, true));
    assert!(poly(&basis[3]).is_empty());
    refused(
        check(&cubic_spec("representation"), value(&basis, c, n)),
        "the basis element g_3 is zero",
    );
}

// ----------------------------------------------------- the zero and unit

/// The zero ideal has the empty basis, with or without zero generators; the
/// unit ideal has the basis 1.
#[test]
fn the_zero_ideal_and_the_unit_ideal_are_checked() {
    let zero = p(&[]);
    let spec_zero = spec(arguments(
        ring(&XYZW, "lex"),
        &[zero.clone(), zero],
        "representation",
    ));
    let empty = value(&[], pm(2, vec![]), pm(0, vec![vec![], vec![]]));
    assert_eq!(
        levels(&check(&spec_zero, empty).unwrap()),
        representation_checked()
    );
    let spec_none = spec(arguments(ring(&XYZW, "lex"), &[], "representation"));
    let empty = value(&[], pm(0, vec![]), pm(0, vec![]));
    assert_eq!(
        levels(&check(&spec_none, empty).unwrap()),
        representation_checked()
    );

    // (x + 1) - x = 1.
    let x = ["x"];
    let f = [p(&[("1", &[1]), ("1", &[0])]), p(&[("1", &[1])])];
    let basis = [p(&[("1", &[0])])];
    let c = pm(
        2,
        vec![vec![(0, p(&[("1", &[0])])), (1, p(&[("-1", &[0])]))]],
    );
    let n = pm(1, vec![vec![(0, f[0].clone())], vec![(0, f[1].clone())]]);
    assert_eq!(representations_hold(&f, &basis, &c, &n), (true, true));
    let spec_unit = spec(arguments(ring(&x, "grevlex"), &f, "representation"));
    assert_eq!(
        levels(&check(&spec_unit, value(&basis, c, n)).unwrap()),
        representation_checked()
    );
}

// ------------------------------------------------------ order mix-ups

const XYZ: [&str; 3] = ["x", "y", "z"];

/// #66, acceptance 4: a driver that sends the grevlex basis for a lex job.
/// F = (x^2 y - z/2, 2xy^2 - x, 3xz - y); its grevlex reduced basis, with C
/// and N, checked by expansion here. Under lex the first element, z^2 - x/3,
/// has the leading term -x/3, so it is not monic: `gb.reduced_form` is false
/// (a reduced basis is monic in the declared order).
#[test]
fn an_order_mix_up_that_is_not_monic_is_refused() {
    let f = vec![
        p(&[("1", &[2, 1, 0]), ("-1/2", &[0, 0, 1])]),
        p(&[("2", &[1, 2, 0]), ("-1", &[1, 0, 0])]),
        p(&[("3", &[1, 0, 1]), ("-1", &[0, 1, 0])]),
    ];
    let g = vec![
        p(&[("1", &[0, 0, 2]), ("-1/3", &[1, 0, 0])]),
        p(&[("1", &[1, 0, 1]), ("-1/3", &[0, 1, 0])]),
        p(&[("1", &[2, 0, 0]), ("-1", &[0, 1, 1])]),
        p(&[("1", &[0, 2, 1]), ("-1/2", &[0, 0, 1])]),
        p(&[("1", &[0, 3, 0]), ("-1/2", &[0, 1, 0])]),
        p(&[("1", &[1, 2, 0]), ("-1/2", &[1, 0, 0])]),
    ];
    let c = pm(
        3,
        vec![
            vec![
                (
                    0,
                    p(&[("-8", &[2, 3, 0]), ("4", &[2, 1, 0]), ("-2", &[0, 0, 1])]),
                ),
                (
                    1,
                    p(&[("12", &[4, 1, 1]), ("-6", &[2, 0, 2]), ("1/3", &[0, 0, 0])]),
                ),
                (
                    2,
                    p(&[
                        ("-8", &[4, 3, 0]),
                        ("4", &[4, 1, 0]),
                        ("4", &[2, 2, 1]),
                        ("-2", &[2, 0, 1]),
                        ("2/3", &[1, 1, 0]),
                    ]),
                ),
            ],
            vec![(2, p(&[("1/3", &[0, 0, 0])]))],
            vec![
                (
                    0,
                    p(&[("24", &[3, 3, 0]), ("-12", &[3, 1, 0]), ("6", &[1, 0, 1])]),
                ),
                (
                    1,
                    p(&[("-36", &[5, 1, 1]), ("18", &[3, 0, 2]), ("-1", &[1, 0, 0])]),
                ),
                (
                    2,
                    p(&[
                        ("24", &[5, 3, 0]),
                        ("-12", &[5, 1, 0]),
                        ("-12", &[3, 2, 1]),
                        ("6", &[3, 0, 1]),
                        ("-2", &[2, 1, 0]),
                        ("1", &[0, 0, 1]),
                    ]),
                ),
            ],
            vec![
                (0, p(&[("-2", &[0, 2, 0]), ("1", &[0, 0, 0])])),
                (1, p(&[("3", &[2, 0, 1])])),
                (2, p(&[("-2", &[2, 2, 0]), ("1", &[2, 0, 0])])),
            ],
            vec![
                (1, p(&[("3/2", &[0, 0, 1])])),
                (2, p(&[("-1", &[0, 2, 0]), ("1/2", &[0, 0, 0])])),
            ],
            vec![(1, p(&[("1/2", &[0, 0, 0])]))],
        ],
    );
    let n = pm(
        6,
        vec![
            vec![(2, p(&[("1", &[0, 1, 0])])), (3, p(&[("1", &[0, 0, 0])]))],
            vec![(5, p(&[("2", &[0, 0, 0])]))],
            vec![(1, p(&[("3", &[0, 0, 0])]))],
        ],
    );
    assert_eq!(representations_hold(&f, &g, &c, &n), (true, true));
    // In grevlex, the order the driver used, the value is right.
    let lms: Vec<Vec<u32>> = g.iter().map(|g| leading(&poly(g), grevlex).0).collect();
    assert!(
        lms.windows(2)
            .all(|w| grevlex(&w[0], &w[1]) == Ordering::Less)
    );
    let grevlex_job = spec(arguments(ring(&XYZ, "grevlex"), &f, "representation"));
    let right = check(&grevlex_job, value(&g, c.clone(), n.clone())).unwrap();
    assert_eq!(levels(&right), representation_checked());
    // In lex, the order of the job, g_0 = z^2 - x/3 leads with -x/3.
    assert_eq!(leading(&poly(&g[0]), lex), (vec![1, 0, 0], Q(-1, 3)));
    let lex_job = spec(arguments(ring(&XYZ, "lex"), &f, "representation"));
    refused(check(&lex_job, value(&g, c, n)), "g_0 is not monic");
}

/// The other side of #66, acceptance 4: a representation alone cannot tell
/// every mix-up. F = (-xyz + z^2, -xyz - x^2 + xy) has the grevlex reduced
/// basis G = (x^2 - xy + z^2, xyz - z^2, yz^3 + xz^2 - yz^2), monic and
/// interreduced under lex too. In grevlex order the lex leading monomials
/// x^2 > xyz > xz^2 decrease and the value is refused; sorted in increasing
/// lex order it passes with `gb.groebner` asserted, though it is not a lex
/// Groebner basis: z g_1 - y g_2 lies in I and its lex leading monomial
/// y^2 z^3 is divisible by none of x^2, xyz, xz^2.
#[test]
fn an_order_mix_up_that_is_monic_and_interreduced_passes() {
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
    // g_2 = (yz + x - y) f_0 - yz f_1.
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
    assert_eq!(representations_hold(&f, &g, &c, &n), (true, true));
    let grevlex_job = spec(arguments(ring(&XYZ, "grevlex"), &f, "representation"));
    assert_eq!(
        levels(&check(&grevlex_job, value(&g, c.clone(), n.clone())).unwrap()),
        representation_checked()
    );

    // Under lex: monic and interreduced, but decreasing.
    let lex_leading: Vec<(Vec<u32>, Q)> = g.iter().map(|g| leading(&poly(g), lex)).collect();
    assert_eq!(
        lex_leading,
        [
            (vec![2, 0, 0], Q(1, 1)),
            (vec![1, 1, 1], Q(1, 1)),
            (vec![1, 0, 2], Q(1, 1)),
        ]
    );
    for (i, g) in g.iter().enumerate() {
        for e in poly(g).keys() {
            for (j, (m, _)) in lex_leading.iter().enumerate() {
                assert!(
                    i == j || !divides(m, e),
                    "g_{i} has a term divisible by lm g_{j}"
                );
            }
        }
    }
    let lex_job = spec(arguments(ring(&XYZ, "lex"), &f, "representation"));
    refused(
        check(&lex_job, value(&g, c.clone(), n.clone())),
        "the leading monomials of g_0 and g_1 are not in strictly increasing order",
    );

    // Sorted in increasing lex order: (g_2, g_1, g_0), with the columns of C
    // and the rows of N in that order.
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
    assert_eq!(
        representations_hold(&f, &sorted, &c_sorted, &n_sorted),
        (true, true)
    );
    let passed = check(&lex_job, value(&sorted, c_sorted, n_sorted)).unwrap();
    assert_eq!(levels(&passed), representation_checked());

    // Yet G is no lex Groebner basis: r = z g_1 - y g_2 is in (G) = I, and
    // lm(r) = y^2 z^3 in lex is divisible by no lex leading monomial of G.
    let z = Poly::from([(vec![0, 0, 1], Q(1, 1))]);
    let y = Poly::from([(vec![0, 1, 0], Q(1, 1))]);
    let r = add(
        &mul(&z, &poly(&g[1])),
        &scale(&mul(&y, &poly(&g[2])), Q(-1, 1)),
    );
    let expected = poly(&p(&[
        ("-1", &[0, 2, 3]),
        ("1", &[0, 2, 2]),
        ("-1", &[0, 0, 3]),
    ]));
    assert_eq!(r, expected);
    let (lm_r, _) = leading(&r, lex);
    assert_eq!(lm_r, [0, 2, 3]);
    assert!(lex_leading.iter().all(|(m, _)| !divides(m, &lm_r)));
}

// ------------------------------------------------------ refused inputs

/// #66, acceptance 5, and the other refusals of the arguments: the checker
/// applies the rules the backends apply before any CAS runs.
#[test]
fn integer_coefficients_are_refused() {
    let mut zz = cubic_spec("representation");
    zz.arguments["ideal"]["ring"]["domain"] = json!("ZZ");
    refused(
        check(&zz, cubic()),
        "the ideal.* operations are over QQ only",
    );
}

#[test]
fn a_ring_without_variables_is_refused() {
    let spec = spec(arguments(ring(&[], "lex"), &[], "representation"));
    refused(
        check(&spec, value(&[], pm(0, vec![]), pm(0, vec![]))),
        "over QQ only, in a ring with variables",
    );
}

/// The shapes of C (s x t) and N (t x s).
#[test]
fn matrices_of_another_shape_are_refused() {
    let mut value = cubic();
    value["certificate"]["basis_in_generators"]["rows"] = json!(4);
    refused(
        check(&cubic_spec("representation"), value),
        "C is 4 x 3, not 3 x 3",
    );
    let mut value = cubic();
    value["certificate"]["generators_in_basis"]["columns"]
        .as_array_mut()
        .unwrap()
        .pop();
    refused(
        check(&cubic_spec("representation"), value),
        "N is 3 x 2, not 3 x 3",
    );
}

// --------------------------------------------- certificates left out

/// ADR 0018: only the job (certificate: none) or the checker itself (for the
/// size) may leave a certificate out.
#[test]
fn a_groebner_certificate_the_backend_leaves_out_is_refused() {
    let spec = cubic_spec("representation");
    let mut without = cubic();
    without.as_object_mut().unwrap().remove("certificate");
    refused(check(&spec, without), "has none");
    let mut marked = cubic();
    marked["certificate"] = json!({"omitted": "certificate_too_large"});
    refused(check(&spec, marked), "only the checker");
}

#[test]
fn without_a_certificate_only_the_reduced_form_is_checked() {
    let mut bare = cubic();
    bare.as_object_mut().unwrap().remove("certificate");
    let checked = check(&cubic_spec("none"), bare).unwrap();
    assert_eq!(
        levels(&checked),
        [
            ("gb.reduced_form".into(), EvidenceLevel::Checked),
            ("gb.contained".into(), EvidenceLevel::Asserted),
            ("gb.contains".into(), EvidenceLevel::Asserted),
            ("gb.groebner".into(), EvidenceLevel::Asserted),
        ]
    );
    assert_eq!(
        reason(&checked, "gb.contained").as_deref(),
        Some("the job asked for no certificate")
    );
    assert_eq!(
        reason(&checked, "gb.groebner").as_deref(),
        Some("the job asked for no certificate of the S-pairs")
    );
    // The reduced form is still checked without a certificate.
    let mut not_monic = cubic();
    not_monic.as_object_mut().unwrap().remove("certificate");
    not_monic["basis"][0] = p(&[("2", &[0, 0, 2, 0]), ("-2", &[0, 1, 0, 1])]);
    refused(check(&cubic_spec("none"), not_monic), "g_0 is not monic");
}

/// #66, acceptance 6: F = [x, y] and G = [y, x] in QQ[x, y] with grevlex,
/// with x = x (1 + y P) - y (x P) for P of many terms: the value exceeds the
/// limit of a result, the checked certificate is left out, and the claims it
/// supports become asserted.
#[test]
fn a_groebner_certificate_too_large_is_left_out() {
    let xy = ["x", "y"];
    let x = p(&[("1", &[1, 0])]);
    let y = p(&[("1", &[0, 1])]);
    let size = 150_000u32;
    // P = sum_{k < size} x^(k / 400) y^(k % 400).
    let monomials: Vec<[u32; 2]> = (0..size).map(|k| [k / 400, k % 400]).collect();
    let terms = |c: &str, shift: [u32; 2]| -> Value {
        let terms: Vec<Value> = monomials
            .iter()
            .map(|m| json!({"coefficient": c, "exponents": [m[0] + shift[0], m[1] + shift[1]]}))
            .collect();
        json!({"type": "polynomial", "terms": terms})
    };
    // 1 + y P and -x P.
    let mut one_plus_yp = terms("1", [0, 1]);
    one_plus_yp["terms"]
        .as_array_mut()
        .unwrap()
        .push(json!({"coefficient": "1", "exponents": [0, 0]}));
    let minus_xp = terms("-1", [1, 0]);
    let one = p(&[("1", &[0, 0])]);
    let c = pm(
        2,
        vec![
            vec![(1, one.clone())],
            vec![(0, one_plus_yp), (1, minus_xp)],
        ],
    );
    let n = pm(2, vec![vec![(1, one.clone())], vec![(0, one)]]);
    let (f, g) = (vec![x.clone(), y.clone()], vec![y, x]);
    assert_eq!(representations_hold(&f, &g, &c, &n), (true, true));
    let value = value(&g, c, n);
    assert!(serde_json::to_vec(&value).unwrap().len() > VALUE_BUDGET);
    let spec = spec(arguments(ring(&xy, "grevlex"), &f, "representation"));
    let checked = check(&spec, value).unwrap();
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
    for id in ["gb.contained", "gb.contains"] {
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

/// Issue #49: the check of a large certificate stops at the job's deadline.
/// F is four dense polynomials and C one column of four dense entries: the
/// product F C takes some 16 million products of terms, far longer than the
/// deadline. The check never gets to compare it with G, so G = [y] need not
/// be F C.
#[tokio::test(flavor = "multi_thread")]
async fn a_long_groebner_check_stops_at_the_deadline() {
    let f = vec![dense(); 4];
    let c = pm(4, vec![(0..4).map(|i| (i, dense())).collect()]);
    let n = pm(1, vec![vec![], vec![], vec![], vec![]]);
    let spec = spec(arguments(
        ring(&["x", "y"], "grevlex"),
        &f,
        "representation",
    ));
    let result = result(value(&[p(&[("1", &[0, 1])])], c, n));
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

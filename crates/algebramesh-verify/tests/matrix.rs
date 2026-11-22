//! The checks of `matrix.*` (Issue #58), on certificates worked out by hand.
//!
//! For each claim at level checked, a test changes a correct result so that
//! the claim is false, keeping the other claims true where it can, and the
//! result is refused (ADR 0018). Other mutations keep the claims true and
//! make only the certificate wrong. Each test first shows what its mutation
//! does with plain integer arithmetic on the JSON (`ints`, `product`, `det2`),
//! not with the matrix code the checks use.

#![allow(clippy::unwrap_used)]

use algebramesh_core::check::Interrupt;
use algebramesh_core::guarantee::{EvidenceLevel, ResultVerifier};
use algebramesh_core::{BackendInfo, CasError, CasResult, JobSpec};
use algebramesh_verify::{Registry, VALUE_BUDGET, catalogue};
use serde_json::{Value, json};

fn spec(operation: &str, arguments: Value) -> JobSpec {
    JobSpec {
        schema_version: 1,
        backend: "oscar".into(),
        operation: operation.into(),
        arguments,
        limits: Default::default(),
        retry_policy: Default::default(),
        environment: Default::default(),
    }
}

fn check(spec: &JobSpec, value: Value) -> Result<CasResult, CasError> {
    let result = CasResult {
        value,
        backend: BackendInfo {
            name: "oscar".into(),
            ..BackendInfo::default()
        },
        execution_host: None,
        log: vec![],
        guarantee: None,
    };
    let checked = Registry::standard().verify(spec, result, &Interrupt::default());
    // What a result claims is what the catalogue lists (Issue #61).
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

fn levels(result: &CasResult) -> Vec<(String, EvidenceLevel)> {
    result
        .guarantee
        .as_ref()
        .unwrap()
        .claims
        .iter()
        .map(|c| (c.id.clone(), c.level))
        .collect()
}

fn all_checked(result: &CasResult) -> bool {
    levels(result)
        .iter()
        .all(|(_, l)| *l == EvidenceLevel::Checked)
}

fn refused(result: Result<CasResult, CasError>, why: &str) {
    match result {
        Err(CasError::UnsupportedResult(m)) => assert!(m.contains(why), "{m}"),
        other => panic!("expected UNSUPPORTED_RESULT ({why}), got {other:?}"),
    }
}

// ---------------------------------------------- plain integer arithmetic

/// An integer written in JSON, as a number or a string.
fn int(v: &Value) -> i64 {
    v.as_i64()
        .unwrap_or_else(|| v.as_str().unwrap().parse().unwrap())
}

/// The entries of a matrix of integers written in JSON.
fn ints(m: &Value) -> Vec<Vec<i64>> {
    m.as_array()
        .unwrap()
        .iter()
        .map(|row| row.as_array().unwrap().iter().map(int).collect())
        .collect()
}

/// `a b`, by the definition of the product.
fn product(a: &Value, b: &Value) -> Vec<Vec<i64>> {
    let (a, b) = (ints(a), ints(b));
    assert!(a.iter().all(|row| row.len() == b.len()), "shapes");
    let ncols = b.first().map_or(0, Vec::len);
    a.iter()
        .map(|row| {
            (0..ncols)
                .map(|j| row.iter().zip(&b).map(|(x, b_row)| x * b_row[j]).sum())
                .collect()
        })
        .collect()
}

fn is_zero(m: &[Vec<i64>]) -> bool {
    m.iter().flatten().all(|&v| v == 0)
}

/// The determinant of a 2 x 2 matrix.
fn det2(m: &Value) -> i64 {
    let m = ints(m);
    m[0][0] * m[1][1] - m[0][1] * m[1][0]
}

// ------------------------------------------------------------ Smith (ZZ)

const A: &str = "[[2,4],[6,8]]";

fn a() -> Value {
    serde_json::from_str(A).unwrap()
}

fn smith() -> Value {
    json!({
        "type": "smith_normal_form",
        "diagonal": ["2", "4"],
        "certificate": {
            "left": {"transform": [[1, 0], [3, -1]], "inverse": [[1, 0], [3, -1]]},
            "right": {"transform": [[1, -2], [0, 1]], "inverse": [[1, 2], [0, 1]]},
        },
    })
}

fn snf_spec() -> JobSpec {
    spec(
        "matrix.smith_normal_form",
        json!({"domain": "ZZ", "matrix": a()}),
    )
}

#[test]
fn the_smith_form_of_2_4_6_8_is_diag_2_4() {
    let checked = check(&snf_spec(), smith()).unwrap();
    assert!(all_checked(&checked), "{:?}", levels(&checked));
}

/// `snf.equivalent`: diag(2, 8) keeps the form (2 | 8), but P A Q has the
/// determinant of A up to sign for unimodular P and Q, and 2 * 8 = 16 while
/// det A = -8.
#[test]
fn a_smith_form_that_is_not_equivalent_is_refused() {
    let mut wrong = smith();
    wrong["diagonal"] = json!(["2", "8"]);
    let (d1, d2) = (int(&wrong["diagonal"][0]), int(&wrong["diagonal"][1]));
    assert_eq!(d2 % d1, 0, "the form is kept");
    assert_ne!(d1 * d2, det2(&a()).abs());
    refused(check(&snf_spec(), wrong), "not the diagonal");

    // P with a wrong inverse: P P' is not I. The claims stay true; only the
    // certificate is wrong.
    let mut not_unimodular = smith();
    not_unimodular["certificate"]["left"]["inverse"] = json!([[1, 0], [3, 1]]);
    let left = &not_unimodular["certificate"]["left"];
    assert_ne!(
        product(&left["transform"], &left["inverse"]),
        [[1, 0], [0, 1]]
    );
    refused(check(&snf_spec(), not_unimodular), "not its inverse");
}

/// `snf.form`: diag(4, 2) is P A Q, with the rows of P and the columns of Q
/// swapped, but 4 does not divide 2.
#[test]
fn a_smith_diagonal_that_does_not_divide_is_refused() {
    let mut swapped = smith();
    swapped["diagonal"] = json!(["4", "2"]);
    swapped["certificate"] = json!({
        "left": {"transform": [[3, -1], [1, 0]], "inverse": [[0, 1], [-1, 3]]},
        "right": {"transform": [[-2, 1], [1, 0]], "inverse": [[0, 1], [1, 2]]},
    });
    let c = &swapped["certificate"];
    let (p, q) = (&c["left"]["transform"], &c["right"]["transform"]);
    assert_eq!(product(p, &c["left"]["inverse"]), [[1, 0], [0, 1]]);
    assert_eq!(product(q, &c["right"]["inverse"]), [[1, 0], [0, 1]]);
    assert_eq!(
        product(&json!(product(p, &a())), q),
        [[4, 0], [0, 2]],
        "the equivalence is kept"
    );
    let (d1, d2) = (int(&swapped["diagonal"][0]), int(&swapped["diagonal"][1]));
    assert_ne!(d2 % d1, 0);
    refused(check(&snf_spec(), swapped), "dividing the next");
}

/// The matrix comes from the specification: a certificate for another matrix
/// is refused. diag(2, 4) is not the Smith form of [[2, 4], [6, 10]], whose
/// determinant is -4, not 8 up to sign.
#[test]
fn a_smith_form_of_another_matrix_is_refused() {
    let matrix = json!([[2, 4], [6, 10]]);
    assert_eq!(det2(&matrix), -4);
    let other = spec(
        "matrix.smith_normal_form",
        json!({"domain": "ZZ", "matrix": matrix}),
    );
    refused(check(&other, smith()), "not the diagonal");
}

#[test]
fn without_a_certificate_only_the_form_is_checked() {
    let none = spec(
        "matrix.smith_normal_form",
        json!({"domain": "ZZ", "matrix": a(), "certificate": "none"}),
    );
    let mut value = smith();
    value.as_object_mut().unwrap().remove("certificate");
    let checked = check(&none, value).unwrap();
    assert_eq!(
        levels(&checked),
        [
            ("snf.form".into(), EvidenceLevel::Checked),
            ("snf.equivalent".into(), EvidenceLevel::Asserted)
        ]
    );
}

// ---------------------------------------------------------- Hermite (ZZ)

fn hermite() -> Value {
    // H = U A with U = [[-2, 1], [3, -1]] (det -1) and V = U^-1.
    json!({
        "type": "hermite_normal_form",
        "form": [[2, 0], [0, 4]],
        "certificate": {"transform": [[-2, 1], [3, -1]], "inverse": [[1, 1], [3, 2]]},
    })
}

fn hnf_spec() -> JobSpec {
    spec(
        "matrix.hermite_normal_form",
        json!({"domain": "ZZ", "matrix": a()}),
    )
}

#[test]
fn the_hermite_form_is_checked() {
    let value = hermite();
    assert_eq!(
        product(&value["certificate"]["transform"], &a()),
        ints(&value["form"])
    );
    let checked = check(&hnf_spec(), value).unwrap();
    assert!(all_checked(&checked), "{:?}", levels(&checked));
}

#[test]
fn a_wrong_hermite_form_is_refused() {
    // hnf.form: the entry 4 above the pivot 4 is not below it. H = U A is
    // kept, with U = [[1, 0], [3, -1]], its own inverse.
    let mut above = hermite();
    above["form"] = json!([[2, 4], [0, 4]]);
    above["certificate"] = json!({"transform": [[1, 0], [3, -1]], "inverse": [[1, 0], [3, -1]]});
    let u = &above["certificate"];
    assert_eq!(product(&u["transform"], &u["inverse"]), [[1, 0], [0, 1]]);
    assert_eq!(product(&u["transform"], &a()), ints(&above["form"]));
    let h = ints(&above["form"]);
    assert!(h[0][1] >= h[1][1]);
    refused(check(&hnf_spec(), above), "Hermite normal form");

    // hnf.equivalent: diag(2, 8) keeps the form, but U A has the determinant
    // of A up to sign for unimodular U: 16 is not 8.
    let mut other = hermite();
    other["form"] = json!([[2, 0], [0, 8]]);
    assert_ne!(det2(&other["form"]).abs(), det2(&a()).abs());
    refused(check(&hnf_spec(), other), "U A is not H");

    // Over QQ there is no Hermite normal form here.
    let qq = spec(
        "matrix.hermite_normal_form",
        json!({"domain": "QQ", "matrix": a()}),
    );
    refused(check(&qq, hermite()), "over ZZ only");
}

// ------------------------------------------------------------------ rank

fn b() -> Value {
    json!([[1, 2, 3], [2, 4, 6], [1, 0, 1]])
}

fn rank() -> Value {
    // The minor at rows 0, 2 and columns 0, 1 is [[1, 2], [1, 0]].
    json!({
        "type": "matrix_rank",
        "rank": 2,
        "certificate": {
            "minor": {"rows": [0, 2], "columns": [0, 1], "inverse": [["0", "1"], ["1/2", "-1/2"]]},
            "kernel": [["-1"], ["-1"], ["1"]],
        },
    })
}

/// The minor of B at rows 0, 2 and columns 0, 1 is not singular: rank B >= 2.
fn rank_b_is_at_least_2() {
    let b = ints(&b());
    assert_ne!(b[0][0] * b[2][1] - b[0][1] * b[2][0], 0);
}

/// The right rank with a wrong certificate: the claims stay true, and each
/// wrong part of the certificate is refused.
#[test]
fn a_rank_is_bounded_from_both_sides() {
    let spec = spec("matrix.rank", json!({"domain": "QQ", "matrix": b()}));
    let checked = check(&spec, rank()).unwrap();
    assert!(all_checked(&checked), "{:?}", levels(&checked));

    // B' = I is not the inverse of the minor [[1, 2], [1, 0]].
    let mut lower = rank();
    lower["certificate"]["minor"]["inverse"] = json!([["1", "0"], ["0", "1"]]);
    let inverse = &lower["certificate"]["minor"]["inverse"];
    assert_ne!(product(&json!([[1, 2], [1, 0]]), inverse), [[1, 0], [0, 1]]);
    refused(check(&spec, lower), "not its inverse");

    let mut upper = rank();
    upper["certificate"]["kernel"] = json!([["-1"], ["1"], ["1"]]);
    assert!(!is_zero(&product(&b(), &upper["certificate"]["kernel"])));
    refused(check(&spec, upper), "not zero");
}

/// `rank.lower`: B sends the nonzero (-1, -1, 1) to zero, so its rank is
/// below 3. rank B <= 3 stays true (B has 3 columns, and K has none), so only
/// the minor refuses it: B itself is singular, and no B' is its inverse.
#[test]
fn a_rank_too_high_is_refused() {
    assert!(is_zero(&product(&b(), &json!([[-1], [-1], [1]]))));
    let spec = spec("matrix.rank", json!({"domain": "QQ", "matrix": b()}));
    let high = json!({
        "type": "matrix_rank",
        "rank": 3,
        "certificate": {
            "minor": {"rows": [0, 1, 2], "columns": [0, 1, 2],
                      "inverse": [["1", "0", "0"], ["0", "1", "0"], ["0", "0", "1"]]},
            "kernel": [[], [], []],
        },
    });
    refused(check(&spec, high), "not its inverse");
}

/// `rank.upper`: a rank of 1 is below the rank 2 of B. rank B >= 1 stays
/// true with the minor [1] at row 0 and column 0, so only the kernel vectors
/// refuse it: no two independent vectors lie in the kernel.
#[test]
fn a_rank_too_low_is_refused() {
    rank_b_is_at_least_2();
    let spec = spec("matrix.rank", json!({"domain": "QQ", "matrix": b()}));
    let mut low = rank();
    low["rank"] = json!(1);
    low["certificate"]["minor"] = json!({"rows": [0], "columns": [0], "inverse": [["1"]]});
    // In column echelon form, so independent; B (0, 1, 0) = (2, 4, 0).
    low["certificate"]["kernel"] = json!([["-1", "0"], ["-1", "1"], ["1", "0"]]);
    assert_eq!(
        product(&b(), &low["certificate"]["kernel"]),
        [[0, 2], [0, 4], [0, 0]]
    );
    refused(check(&spec, low), "not zero");
}

// ---------------------------------------------------------------- kernel

#[test]
fn a_kernel_over_qq_is_a_basis() {
    let spec = spec("matrix.kernel", json!({"domain": "QQ", "matrix": b()}));
    let value = json!({
        "type": "matrix_kernel",
        "basis": [["-1"], ["-1"], ["1"]],
        "certificate": {"minor": {"rows": [0, 2], "columns": [0, 1],
                                  "inverse": [["0", "1"], ["1/2", "-1/2"]]}},
    });
    let checked = check(&spec, value.clone()).unwrap();
    assert!(all_checked(&checked), "{:?}", levels(&checked));

    let mut not_in = value.clone();
    not_in["basis"] = json!([["1"], ["-1"], ["1"]]);
    assert_eq!(product(&b(), &not_in["basis"]), [[2], [4], [2]]);
    refused(check(&spec, not_in), "not zero");
    // The basis is right, but a minor of size 1 bounds the dimension of the
    // kernel by 2, not by k = 1: only the certificate is wrong.
    let mut short = value;
    short["certificate"]["minor"] = json!({"rows": [0], "columns": [0], "inverse": [["1"]]});
    refused(check(&spec, short), "n - k");
}

#[test]
fn a_kernel_over_zz_is_a_basis_of_the_lattice() {
    // 2x + 4y = 0: the lattice is spanned by (-2, 1). U A^T = [[2], [0]].
    let a = json!([[2, 4]]);
    let spec = spec("matrix.kernel", json!({"domain": "ZZ", "matrix": a}));
    let value = json!({
        "type": "matrix_kernel",
        "basis": [["-2"], ["1"]],
        "certificate": {"transform": [[1, 0], [-2, 1]], "inverse": [[1, 0], [2, 1]]},
    });
    let checked = check(&spec, value.clone()).unwrap();
    assert!(all_checked(&checked), "{:?}", levels(&checked));

    // kernel.spanning: (-4, 2) is a nonzero vector of the kernel, so the
    // basis stays contained and independent, but it spans a sublattice of
    // index 2: the lattice vector (-2, 1) is c (-4, 2) only for 2 c = 1.
    let mut half = value;
    half["basis"] = json!([["-4"], ["2"]]);
    assert!(is_zero(&product(&a, &half["basis"])));
    let lattice_vector = json!([[-2], [1]]);
    assert!(is_zero(&product(&a, &lattice_vector)));
    assert_ne!(ints(&lattice_vector)[1][0] % ints(&half["basis"])[1][0], 0);
    refused(check(&spec, half), "rows of U");
}

/// `x * m` for a matrix `m` written in JSON whose entries are integers or
/// fractions `p/q`, when every entry of `x * m` is an integer.
fn scaled(m: &Value, x: i64) -> Vec<Vec<i64>> {
    let entry = |v: &Value| -> i64 {
        let text = v.as_str().map_or_else(|| v.to_string(), str::to_owned);
        let (p, q) = text.split_once('/').unwrap_or((&text, "1"));
        let (p, q): (i64, i64) = (p.parse().unwrap(), q.parse().unwrap());
        assert_eq!((x * p) % q, 0, "{x} * {text} is not an integer");
        x * p / q
    };
    m.as_array()
        .unwrap()
        .iter()
        .map(|row| row.as_array().unwrap().iter().map(entry).collect())
        .collect()
}

/// Issue #80: `kernel.spanning` over ZZ, `hnf.equivalent` and
/// `snf.equivalent` rest on transforms that are unimodular: integral, with an
/// integral inverse. Each case below gives an integral transform of
/// determinant 2 with its true inverse, which has a half, and keeps every
/// other condition of its certificate; so only the check that the inverse is
/// integral refuses it.
#[test]
fn a_transform_whose_inverse_is_not_integral_is_refused() {
    // kernel.spanning: 2x + 4y = 0. U A^T = [[2], [0]] and K is the row of U
    // at the zero row, but det U = 2: K = (-4, 2) spans a sublattice of index
    // 2, without the lattice vector (-2, 1).
    let row = json!([[2, 4]]);
    let spec_zz = spec("matrix.kernel", json!({"domain": "ZZ", "matrix": row}));
    let value = json!({
        "type": "matrix_kernel",
        "basis": [["-4"], ["2"]],
        "certificate": {"transform": [[1, 0], [-4, 2]], "inverse": [["1", "0"], ["2", "1/2"]]},
    });
    let (u, v) = (
        &value["certificate"]["transform"],
        &value["certificate"]["inverse"],
    );
    assert_eq!(
        product(u, &json!(scaled(v, 2))),
        [[2, 0], [0, 2]],
        "U V = I"
    );
    assert_eq!(det2(u), 2);
    assert_eq!(product(u, &json!([[2], [4]])), [[2], [0]], "U A^T");
    assert_eq!(
        ints(&value["basis"]),
        [[-4], [2]],
        "the row of U at the zero row"
    );
    assert_eq!(ints(u)[1], [-4, 2]);
    assert!(is_zero(&product(&row, &value["basis"])));
    let lattice_vector = json!([[-2], [1]]);
    assert!(is_zero(&product(&row, &lattice_vector)));
    // (-2, 1) = c (-4, 2) only for c = 1/2.
    let (k, l) = (ints(&value["basis"]), ints(&lattice_vector));
    assert_ne!(l[1][0] % k[1][0], 0);
    assert_eq!(2 * l[1][0], k[1][0]);
    refused(check(&spec_zz, value), "not integral");

    // hnf.equivalent: U' = diag(1, 2) U, so U' A = diag(2, 8), which is in
    // Hermite normal form; but |det| = 16, not |det A| = 8, so it is not the
    // Hermite form of A.
    let mut hnf = hermite();
    hnf["form"] = json!([[2, 0], [0, 8]]);
    hnf["certificate"] =
        json!({"transform": [[-2, 1], [6, -2]], "inverse": [["1", "1/2"], ["3", "1"]]});
    let c = &hnf["certificate"];
    assert_eq!(
        product(&c["transform"], &json!(scaled(&c["inverse"], 2))),
        [[2, 0], [0, 2]]
    );
    assert_eq!(product(&c["transform"], &a()), ints(&hnf["form"]));
    assert_ne!(det2(&hnf["form"]).abs(), det2(&a()).abs());
    refused(check(&hnf_spec(), hnf), "not integral");

    // snf.equivalent: P' = diag(1, 2) P, so P' A Q = diag(2, 8), and 2 divides
    // 8; but |det| = 16, not 8.
    let mut snf = smith();
    snf["diagonal"] = json!(["2", "8"]);
    snf["certificate"]["left"] =
        json!({"transform": [[1, 0], [6, -2]], "inverse": [["1", "0"], ["3", "-1/2"]]});
    let c = &snf["certificate"];
    let (p, q) = (&c["left"]["transform"], &c["right"]["transform"]);
    assert_eq!(
        product(p, &json!(scaled(&c["left"]["inverse"], 2))),
        [[2, 0], [0, 2]]
    );
    assert_eq!(product(q, &c["right"]["inverse"]), [[1, 0], [0, 1]]);
    assert_eq!(product(&json!(product(p, &a())), q), [[2, 0], [0, 8]]);
    assert_ne!(2 * 8, det2(&a()).abs());
    refused(check(&snf_spec(), snf), "not integral");
}

/// `kernel.contained`, over QQ and over ZZ: a second column that A does not
/// send to zero. The columns stay independent and still span the kernel, so
/// only the product refuses them.
#[test]
fn a_vector_outside_the_kernel_is_refused() {
    // Over QQ: the kernel of B is spanned by (-1, -1, 1), the first column.
    let spec_qq = spec("matrix.kernel", json!({"domain": "QQ", "matrix": b()}));
    let basis = json!([["-1", "0"], ["-1", "1"], ["1", "0"]]);
    assert_eq!(product(&b(), &basis), [[0, 2], [0, 4], [0, 0]]);
    assert_ne!(det2(&json!(ints(&basis)[..2])), 0, "independent");
    let value = json!({
        "type": "matrix_kernel", "basis": basis,
        "certificate": {"minor": {"rows": [0], "columns": [0], "inverse": [["1"]]}},
    });
    refused(check(&spec_qq, value), "not zero");

    // Over ZZ: the lattice of 2x + 4y = 0 is spanned by (-2, 1), the first
    // column.
    let a = json!([[2, 4]]);
    let spec_zz = spec("matrix.kernel", json!({"domain": "ZZ", "matrix": a}));
    let basis = json!([["-2", "0"], ["1", "1"]]);
    assert_eq!(product(&a, &basis), [[0, 4]]);
    assert_ne!(det2(&basis), 0, "independent");
    let value = json!({
        "type": "matrix_kernel", "basis": basis,
        "certificate": {"transform": [[1, 0], [-2, 1]], "inverse": [[1, 0], [2, 1]]},
    });
    refused(check(&spec_zz, value), "not zero");
}

/// `kernel.spanning` over QQ: (-1, 1, 0) lies in the kernel of [[1, 1, 1]],
/// and so does (-1, 0, 1), which is no multiple of it (the last entries are
/// 1 and 0). A minor of size n - k = 2 would bound the kernel by k = 1; A has
/// one row, and the minor of size 1 is refused.
#[test]
fn a_basis_that_spans_too_little_is_refused() {
    let a = json!([[1, 1, 1]]);
    let basis = json!([["-1"], ["1"], ["0"]]);
    let other = json!([[-1], [0], [1]]);
    assert!(is_zero(&product(&a, &basis)), "contained");
    assert!(is_zero(&product(&a, &other)));
    assert_eq!((ints(&basis)[2][0], ints(&other)[2][0]), (0, 1));
    let spec = spec("matrix.kernel", json!({"domain": "QQ", "matrix": a}));
    let value = json!({
        "type": "matrix_kernel", "basis": basis,
        "certificate": {"minor": {"rows": [0], "columns": [0], "inverse": [["1"]]}},
    });
    refused(check(&spec, value), "n - k");
}

// ----------------------------------------------------------------- solve

/// `x` as a column.
fn column(x: &Value) -> Value {
    json!(
        x.as_array()
            .unwrap()
            .iter()
            .map(|v| [v])
            .collect::<Vec<_>>()
    )
}

/// `solve.solution`, over ZZ and over QQ.
#[test]
fn a_solution_is_checked() {
    let a = json!([[2, 4]]);
    let zz = spec(
        "matrix.solve",
        json!({"domain": "ZZ", "matrix": a, "rhs": [6]}),
    );
    let value = json!({"type": "matrix_solution", "solution": ["1", "1"]});
    assert!(all_checked(&check(&zz, value).unwrap()));
    let wrong = json!({"type": "matrix_solution", "solution": ["1", "2"]});
    assert_eq!(product(&a, &column(&wrong["solution"])), [[10]]);
    refused(check(&zz, wrong), "A x is not b");
    // A x = b holds, as 4 x = (2, 5) and 2 * 2 + 4 * 5 = 4 * 6, but 1/2 is
    // not an integer.
    assert_eq!(product(&a, &json!([[2], [5]])), [[24]]);
    let fraction = json!({"type": "matrix_solution", "solution": ["1/2", "5/4"]});
    refused(check(&zz, fraction), "not integral");

    // Over QQ, 2x + 4y = 1 is solved by (1/2, 0), and not by (1, 0).
    let qq = spec(
        "matrix.solve",
        json!({"domain": "QQ", "matrix": a, "rhs": [1]}),
    );
    let half = json!({"type": "matrix_solution", "solution": ["1/2", "0"]});
    assert!(all_checked(&check(&qq, half).unwrap()));
    let wrong = json!({"type": "matrix_solution", "solution": ["1", "0"]});
    assert_eq!(product(&a, &column(&wrong["solution"])), [[2]]);
    refused(check(&qq, wrong), "A x is not b");
}

/// Issue #58: without a solution over ZZ, y A is integral and y b is not.
#[test]
fn no_solution_over_zz_is_shown_by_y() {
    let a = json!([[2, 4]]);
    let odd = spec(
        "matrix.solve",
        json!({"domain": "ZZ", "matrix": a, "rhs": [1]}),
    );
    let none = json!({"type": "matrix_solution", "solution": null, "certificate": {"y": ["1/2"]}});
    assert!(all_checked(&check(&odd, none.clone()).unwrap()));
    // 2x + 4y = 1 has no integral solution, as 2x + 4y is even; but y = 1,
    // with y A = (2, 4) integral and y b = 1 an integer, does not show it.
    let integral =
        json!({"type": "matrix_solution", "solution": null, "certificate": {"y": ["1"]}});
    assert_eq!(product(&json!([[1]]), &a), [[2, 4]]);
    refused(check(&odd, integral), "no solution");

    // solve.no_solution: 2x + 4y = 6 has the integral solution (1, 1); the
    // result says none, with y = 1/2: y A = (1, 2) and y b = 3.
    let even = spec(
        "matrix.solve",
        json!({"domain": "ZZ", "matrix": a, "rhs": [6]}),
    );
    let solution = json!({"type": "matrix_solution", "solution": ["1", "1"]});
    assert_eq!(product(&a, &column(&solution["solution"])), [[6]]);
    assert!(all_checked(&check(&even, solution).unwrap()));
    refused(check(&even, none), "no solution");
}

#[test]
fn no_solution_over_qq_is_shown_by_y() {
    let a = json!([[1, 2], [2, 4]]);
    let spec = spec(
        "matrix.solve",
        json!({"domain": "QQ", "matrix": a, "rhs": [1, 3]}),
    );
    let none = json!({"type": "matrix_solution", "solution": null,
                      "certificate": {"y": ["2", "-1"]}});
    assert!(all_checked(&check(&spec, none.clone()).unwrap()));
    // y = (1, 1) is not in the left kernel: it does not show what is true.
    let not_left_kernel = json!({"type": "matrix_solution", "solution": null,
                                 "certificate": {"y": ["1", "1"]}});
    assert_eq!(product(&json!([[1, 1]]), &a), [[3, 6]]);
    refused(check(&spec, not_left_kernel), "no solution");

    // solve.no_solution: with b = (1, 2), x = (1, 0) is a solution; the
    // result says none, with y = (2, -1): y A = 0, and y b = 0.
    let solvable = spec_with_rhs(&spec, json!([1, 2]));
    let solution = json!({"type": "matrix_solution", "solution": ["1", "0"]});
    assert_eq!(product(&a, &column(&solution["solution"])), [[1], [2]]);
    assert!(all_checked(&check(&solvable, solution).unwrap()));
    refused(check(&solvable, none), "no solution");

    // Without the certificate the job asked for, the result is refused; with a
    // job that asked for none, the claim is asserted.
    let without = json!({"type": "matrix_solution", "solution": null});
    refused(check(&spec, without.clone()), "has none");
    let none = spec_with_certificate(&spec, "none");
    let asserted = check(&none, without).unwrap();
    assert_eq!(
        levels(&asserted),
        [("solve.no_solution".into(), EvidenceLevel::Asserted)]
    );
}

/// `spec` with the right-hand side `rhs`.
fn spec_with_rhs(spec: &JobSpec, rhs: Value) -> JobSpec {
    let mut spec = spec.clone();
    spec.arguments["rhs"] = rhs;
    spec
}

// ------------------------------------------------------------------ size

/// Before #36, a certificate that makes the value too large for a result is
/// left out, and the claims it supports are asserted; a value too large
/// without it is a RESOURCE_LIMIT.
#[test]
fn a_certificate_too_large_is_left_out() {
    let n = 1500;
    let zeros: Vec<i64> = vec![0; n];
    let identity: Vec<Vec<&str>> = (0..n)
        .map(|i| (0..n).map(|j| if i == j { "1" } else { "0" }).collect())
        .collect();
    let spec_rank = spec(
        "matrix.rank",
        json!({"domain": "QQ", "matrix": [zeros.clone()]}),
    );
    let value = json!({
        "type": "matrix_rank", "rank": 0,
        "certificate": {"minor": {"rows": [], "columns": [], "inverse": []}, "kernel": identity.clone()},
    });
    assert!(serde_json::to_vec(&value).unwrap().len() > VALUE_BUDGET);
    let checked = check(&spec_rank, value).unwrap();
    assert_eq!(
        checked.value["certificate"],
        json!({"omitted": "certificate_too_large"})
    );
    assert!(
        checked
            .guarantee
            .unwrap()
            .claims
            .iter()
            .all(|c| c.level == EvidenceLevel::Asserted
                && c.reason.as_deref() == Some("certificate_too_large"))
    );

    let spec_kernel = spec("matrix.kernel", json!({"domain": "QQ", "matrix": [zeros]}));
    let big = json!({
        "type": "matrix_kernel", "basis": identity,
        "certificate": {"minor": {"rows": [], "columns": [], "inverse": []}},
    });
    assert!(matches!(
        check(&spec_kernel, big),
        Err(CasError::ResourceLimit(_))
    ));
}

/// `spec` with the certificate request `request`.
fn spec_with_certificate(spec: &JobSpec, request: &str) -> JobSpec {
    let mut spec = spec.clone();
    spec.arguments["certificate"] = json!(request);
    spec
}

/// ADR 0018: only the job (certificate: none) or the checker itself (for the
/// size) may leave a certificate out. For every operation, a value without the
/// certificate the job asked for (the default), or with one the backend marked
/// as left out, is refused; the level is never lowered silently.
#[test]
fn a_certificate_the_backend_leaves_out_is_refused() {
    let qq_minor =
        json!({"rows": [0, 2], "columns": [0, 1], "inverse": [["0", "1"], ["1/2", "-1/2"]]});
    let cases = [
        (
            spec("matrix.rank", json!({"domain": "QQ", "matrix": b()})),
            rank(),
        ),
        (
            spec("matrix.kernel", json!({"domain": "QQ", "matrix": b()})),
            json!({"type": "matrix_kernel", "basis": [["-1"], ["-1"], ["1"]],
                   "certificate": {"minor": qq_minor}}),
        ),
        (
            spec("matrix.kernel", json!({"domain": "ZZ", "matrix": [[2, 4]]})),
            json!({"type": "matrix_kernel", "basis": [["-2"], ["1"]],
                   "certificate": {"transform": [[1, 0], [-2, 1]], "inverse": [[1, 0], [2, 1]]}}),
        ),
        (
            spec(
                "matrix.solve",
                json!({"domain": "ZZ", "matrix": [[2, 4]], "rhs": [1]}),
            ),
            json!({"type": "matrix_solution", "solution": null, "certificate": {"y": ["1/2"]}}),
        ),
        (hnf_spec(), hermite()),
        (snf_spec(), smith()),
    ];
    for (spec, value) in cases {
        let checked = check(&spec, value.clone()).unwrap();
        assert!(
            all_checked(&checked),
            "{}: {:?}",
            spec.operation,
            levels(&checked)
        );
        let mut without = value.clone();
        without.as_object_mut().unwrap().remove("certificate");
        refused(check(&spec, without), "has none");
        let mut marked = value;
        marked["certificate"] = json!({"omitted": "certificate_too_large"});
        refused(check(&spec, marked), "only the checker");
    }
}

/// The independence half of rank.upper and kernel.independent: columns in
/// A's kernel that are not independent are refused, though A K = 0.
#[test]
fn dependent_kernel_vectors_are_refused() {
    // rank.upper: a rank of 1 is below the rank 2 of B.
    rank_b_is_at_least_2();
    let spec_rank = spec("matrix.rank", json!({"domain": "QQ", "matrix": b()}));
    let twice = json!([["-1", "-1"], ["-1", "-1"], ["1", "1"]]);
    assert!(
        is_zero(&product(&b(), &twice)),
        "the mutation keeps A K = 0"
    );
    let mut low = rank();
    low["rank"] = json!(1);
    low["certificate"]["minor"] = json!({"rows": [0], "columns": [0], "inverse": [["1"]]});
    low["certificate"]["kernel"] = twice;
    refused(check(&spec_rank, low), "column echelon");

    // kernel.independent over QQ: in the kernel of [[1, 1, 1]], the first two
    // columns span it, and the first minus the second plus the third is 0.
    let spec_kernel = spec(
        "matrix.kernel",
        json!({"domain": "QQ", "matrix": [[1, 1, 1]]}),
    );
    let dependent = json!([["-1", "-1", "0"], ["1", "0", "-1"], ["0", "1", "1"]]);
    assert!(is_zero(&product(&json!([[1, 1, 1]]), &dependent)));
    assert!(is_zero(&product(&dependent, &json!([[1], [-1], [1]]))));
    // With k = 3 = n, a minor of size n - k = 0 bounds nothing; only the
    // shape of the basis refuses it.
    let value = json!({
        "type": "matrix_kernel", "basis": dependent,
        "certificate": {"minor": {"rows": [], "columns": [], "inverse": []}},
    });
    refused(check(&spec_kernel, value), "column echelon");

    // kernel.independent over ZZ: (-2, 1) spans the lattice of 2x + 4y = 0,
    // and (-4, 2) is twice it.
    let a = json!([[2, 4]]);
    let spec_zz = spec("matrix.kernel", json!({"domain": "ZZ", "matrix": a}));
    let dependent = json!([["-2", "-4"], ["1", "2"]]);
    assert!(is_zero(&product(&a, &dependent)));
    assert!(is_zero(&product(&dependent, &json!([[2], [-1]]))));
    let value = json!({
        "type": "matrix_kernel", "basis": dependent,
        "certificate": {"transform": [[1, 0], [-2, 1]], "inverse": [[1, 0], [2, 1]]},
    });
    refused(check(&spec_zz, value), "rows of U");
}

/// The other half of the column echelon form: no zero column. Kernel vectors
/// with a zero column are refused, though A sends them to zero.
#[test]
fn a_zero_kernel_vector_is_refused() {
    // I_2 has determinant 1, so its rank is 2; a rank of 1 with the zero
    // vector as its kernel vector is false.
    let identity = json!([[1, 0], [0, 1]]);
    assert_eq!(det2(&identity), 1);
    let zero = json!([["0"], ["0"]]);
    assert!(is_zero(&product(&identity, &zero)));
    let spec_rank = spec("matrix.rank", json!({"domain": "QQ", "matrix": identity}));
    let low = json!({
        "type": "matrix_rank", "rank": 1,
        "certificate": {"minor": {"rows": [0], "columns": [0], "inverse": [["1"]]}, "kernel": zero},
    });
    refused(check(&spec_rank, low), "column echelon");

    // A zero column first: these columns span one dimension of the kernel of
    // [[1, 1, 1]], which has two.
    let spec_kernel = spec(
        "matrix.kernel",
        json!({"domain": "QQ", "matrix": [[1, 1, 1]]}),
    );
    let basis = json!([["0", "-1"], ["0", "1"], ["0", "0"]]);
    assert!(is_zero(&product(&json!([[1, 1, 1]]), &basis)));
    let value = json!({
        "type": "matrix_kernel", "basis": basis,
        "certificate": {"minor": {"rows": [0], "columns": [0], "inverse": [["1"]]}},
    });
    refused(check(&spec_kernel, value), "column echelon");
}

//! Issue #65: matrices over a polynomial ring, their JSON form and their
//! product.

#![allow(clippy::unwrap_used)]

use algebramesh_math_types::{
    CoefficientDomain, MathError, MonomialOrder, Polynomial, PolynomialMatrix,
    PolynomialMatrixData, Rational, RingSpec,
};
use proptest::prelude::*;
use serde_json::json;

fn ring() -> RingSpec {
    RingSpec::new(
        CoefficientDomain::Rationals,
        vec!["x".into(), "y".into()],
        MonomialOrder::Grevlex,
    )
    .unwrap()
}

fn poly(terms: &[([u32; 2], i64)]) -> Polynomial {
    Polynomial::from_terms(
        ring(),
        terms
            .iter()
            .map(|(e, c)| (e.to_vec(), Rational::integer(*c))),
    )
    .unwrap()
}

fn entry(row: usize, terms: serde_json::Value) -> serde_json::Value {
    json!({"row": row, "entry": {"type": "polynomial", "terms": terms}})
}

/// A 2 x 3 matrix with an empty column.
fn matrix_json() -> serde_json::Value {
    json!({
        "type": "polynomial_matrix",
        "rows": 2,
        "columns": [
            [entry(0, json!([{"coefficient": "1", "exponents": [1, 0]}])),
             entry(1, json!([{"coefficient": "-1/2", "exponents": [0, 2]}]))],
            [],
            [entry(1, json!([{"coefficient": "3", "exponents": [0, 0]}]))]
        ]
    })
}

fn read(value: serde_json::Value) -> Result<PolynomialMatrix, String> {
    let data: PolynomialMatrixData = serde_json::from_value(value).map_err(|e| e.to_string())?;
    PolynomialMatrix::from_data(ring(), &data).map_err(|e| e.to_string())
}

#[test]
fn a_matrix_round_trips_through_json_in_a_canonical_form() {
    let m = read(matrix_json()).unwrap();
    assert_eq!((m.nrows(), m.ncols()), (2, 3));
    assert_eq!(m.get(0, 0), Some(&poly(&[([1, 0], 1)])));
    assert_eq!(
        m.get(1, 0)
            .unwrap()
            .leading_coefficient()
            .unwrap()
            .to_string(),
        "-1/2"
    );
    assert_eq!(m.get(0, 1), None);
    assert_eq!(m.get(5, 0), None);
    assert_eq!(m.column(1).unwrap().count(), 0);
    assert!(m.column(3).is_none());
    assert_eq!(serde_json::to_value(m.to_data()).unwrap(), matrix_json());
    // Entries out of order, and a zero entry, are read into the same matrix.
    let mut shuffled = matrix_json();
    shuffled["columns"][0] = json!([
        entry(1, json!([{"coefficient": "-1/2", "exponents": [0, 2]}])),
        entry(0, json!([{"coefficient": "1", "exponents": [1, 0]}]))
    ]);
    shuffled["columns"][1] = json!([entry(1, json!([]))]);
    assert_eq!(read(shuffled).unwrap(), m);
    // Matrices with no rows or no columns have a shape.
    for (rows, cols) in [(0, 2), (2, 0), (0, 0)] {
        let z = PolynomialMatrix::zero(ring(), rows, cols);
        assert!(z.is_zero());
        let back = PolynomialMatrix::from_data(ring(), &z.to_data()).unwrap();
        assert_eq!((back.nrows(), back.ncols()), (rows, cols));
    }
}

#[test]
fn a_malformed_matrix_is_refused() {
    let with = |edit: &dyn Fn(&mut serde_json::Value)| {
        let mut v = matrix_json();
        edit(&mut v);
        v
    };
    for (bad, why) in [
        (with(&|v| v["ring"] = json!({})), "unknown field"),
        (
            with(&|v| v["type"] = json!("polynomial")),
            "unknown variant",
        ),
        (
            with(&|v| v["columns"][0][0]["extra"] = json!(1)),
            "unknown field",
        ),
        (
            with(&|v| v["columns"][2][0]["row"] = json!(2)),
            "row 2 of column 2",
        ),
        (
            with(&|v| {
                v["columns"][2] = json!([
                    entry(1, json!([{"coefficient": "3", "exponents": [0, 0]}])),
                    entry(1, json!([]))
                ])
            }),
            "two entries",
        ),
        (
            with(&|v| v["columns"][2][0]["entry"]["terms"][0]["exponents"] = json!([0])),
            "exponent vector of length 1",
        ),
    ] {
        let error = read(bad.clone()).unwrap_err();
        assert!(error.contains(why), "{bad}: {error}");
    }
}

#[test]
fn the_product_of_matrices() {
    // (x, y) times [[y, 1], [-x, 0]] is (0, x).
    let x = poly(&[([1, 0], 1)]);
    let y = poly(&[([0, 1], 1)]);
    let one = poly(&[([0, 0], 1)]);
    let row = PolynomialMatrix::from_row(ring(), &[x.clone(), y.clone()]).unwrap();
    let c = read(json!({
        "type": "polynomial_matrix",
        "rows": 2,
        "columns": [
            [{"row": 0, "entry": y.to_data()}, {"row": 1, "entry": x.neg().to_data()}],
            [{"row": 0, "entry": one.to_data()}]
        ]
    }))
    .unwrap();
    let product = row.mul(&c).unwrap();
    assert_eq!((product.nrows(), product.ncols()), (1, 2));
    assert_eq!(product.get(0, 0), None);
    assert_eq!(product.get(0, 1), Some(&x));
    assert_eq!(
        product,
        PolynomialMatrix::from_row(ring(), &[Polynomial::zero(ring()), x.clone()]).unwrap()
    );
    // A row times a column is the sum of the products.
    let column = PolynomialMatrix::from_column(ring(), &[y.clone(), x.clone()]).unwrap();
    let sum = row.mul(&column).unwrap();
    assert_eq!(
        sum.get(0, 0),
        Some(&x.mul(&y).unwrap().add(&y.mul(&x).unwrap()).unwrap())
    );
    // Shapes and rings must agree.
    assert!(matches!(row.mul(&row), Err(MathError::Invalid(_))));
    let other = RingSpec::new(
        CoefficientDomain::Rationals,
        vec!["x".into(), "y".into()],
        MonomialOrder::Lex,
    )
    .unwrap();
    let foreign = PolynomialMatrix::zero(other.clone(), 2, 1);
    assert_eq!(row.mul(&foreign), Err(MathError::RingMismatch));
    assert_eq!(
        PolynomialMatrix::from_row(other, std::slice::from_ref(&x)),
        Err(MathError::RingMismatch)
    );
    // An exponent past u32::MAX is refused.
    let high = poly(&[([u32::MAX, 0], 1)]);
    let a = PolynomialMatrix::from_row(ring(), std::slice::from_ref(&high)).unwrap();
    let b = PolynomialMatrix::from_column(ring(), &[high]).unwrap();
    assert_eq!(a.mul(&b), Err(MathError::ExponentOverflow));
}

#[test]
fn a_raised_checkpoint_ends_the_product() {
    let x = poly(&[([1, 0], 1), ([0, 1], 2)]);
    let a = PolynomialMatrix::from_row(ring(), &[x.clone(), x.clone()]).unwrap();
    let b = PolynomialMatrix::from_column(ring(), &[x.clone(), x]).unwrap();
    let mut calls = 0;
    let result = a.mul_with(&b, &mut || {
        calls += 1;
        if calls == 2 {
            Err(MathError::Invalid("raised".into()))
        } else {
            Ok(())
        }
    });
    assert_eq!(result, Err(MathError::Invalid("raised".into())));
    assert_eq!(calls, 2);
}

/// The checkpoint reaches the product of two entries. A product of 17 x 17
/// products of terms calls it 71 times: twice for each of the 17 + 17 terms
/// while it clears their denominators, once before the product, once inside
/// it at the 256th product of terms, and once after 256 of the 289 terms of
/// the result while it reduces them (Issue #66). Raised inside, at the 70th
/// call, it ends the product.
#[test]
fn the_checkpoint_reaches_inside_the_product_of_two_entries() {
    let x = Polynomial::from_terms(ring(), (0..17).map(|i| (vec![i, 0], Rational::integer(1))))
        .unwrap();
    let y = Polynomial::from_terms(ring(), (0..17).map(|j| (vec![0, j], Rational::integer(1))))
        .unwrap();
    let a = PolynomialMatrix::from_row(ring(), std::slice::from_ref(&x)).unwrap();
    let b = PolynomialMatrix::from_column(ring(), std::slice::from_ref(&y)).unwrap();
    let mut calls = 0;
    let product = a
        .mul_with(&b, &mut || {
            calls += 1;
            Ok::<(), MathError>(())
        })
        .unwrap();
    assert_eq!(calls, 71);
    assert_eq!(product.get(0, 0), Some(&x.mul(&y).unwrap()));
    let mut calls = 0;
    let result = a.mul_with(&b, &mut || {
        calls += 1;
        if calls == 70 {
            Err(MathError::Invalid("raised".into()))
        } else {
            Ok(())
        }
    });
    assert_eq!(result, Err(MathError::Invalid("raised".into())));
    assert_eq!(calls, 70);
}

/// An invalid ring is refused even when no entry reads it, and an entry
/// outside the coefficient domain is refused.
#[test]
fn an_invalid_ring_or_a_foreign_entry_is_refused() {
    let invalid = RingSpec {
        domain: CoefficientDomain::Rationals,
        variables: vec!["x".into(), "x".into()],
        order: MonomialOrder::Lex,
    };
    for (rows, columns) in [(1, json!([[]])), (0, json!([]))] {
        let data: PolynomialMatrixData = serde_json::from_value(json!({
            "type": "polynomial_matrix", "rows": rows, "columns": columns
        }))
        .unwrap();
        assert!(PolynomialMatrix::from_data(invalid.clone(), &data).is_err());
        assert!(PolynomialMatrix::from_data(ring(), &data).is_ok());
    }
    let integers = RingSpec::new(
        CoefficientDomain::Integers,
        vec!["x".into(), "y".into()],
        MonomialOrder::Grevlex,
    )
    .unwrap();
    let data: PolynomialMatrixData = serde_json::from_value(matrix_json()).unwrap();
    assert!(matches!(
        PolynomialMatrix::from_data(integers, &data),
        Err(MathError::CoefficientNotInDomain { .. })
    ));
}

/// Zero entries cost no check of the ring: a matrix of 5000 zero entries over
/// a ring of 5000 variables is read at once. Checking the ring for each entry
/// took about 1.6 s in a release build.
#[test]
fn zero_entries_over_a_large_ring_are_read_in_linear_time() {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let n = 5000;
        let large = RingSpec::new(
            CoefficientDomain::Rationals,
            (0..n).map(|i| format!("x{i}")).collect(),
            MonomialOrder::Grevlex,
        )
        .unwrap();
        let data: PolynomialMatrixData = serde_json::from_value(json!({
            "type": "polynomial_matrix",
            "rows": n,
            "columns": [(0..n).map(|i| entry(i, json!([]))).collect::<Vec<_>>()]
        }))
        .unwrap();
        let _ = tx.send(PolynomialMatrix::from_data(large, &data).map(|m| m.is_zero()));
    });
    let read = rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("the matrix was not read within 10 s");
    assert_eq!(read, Ok(true));
}

fn polynomial() -> impl Strategy<Value = Polynomial> {
    proptest::collection::btree_map(proptest::collection::vec(0u32..3, 2), -3i64..=3, 0..3)
        .prop_map(|t| {
            Polynomial::from_terms(
                ring(),
                t.into_iter().map(|(e, c)| (e, Rational::integer(c))),
            )
            .unwrap()
        })
}

/// A dense description of a matrix, read through JSON.
fn matrix(rows: usize, cols: usize) -> impl Strategy<Value = PolynomialMatrix> {
    proptest::collection::vec(proptest::collection::vec(polynomial(), rows), cols).prop_map(
        move |columns| {
            let data = json!({
                "type": "polynomial_matrix",
                "rows": rows,
                "columns": columns.iter().map(|c| c.iter().enumerate()
                    .map(|(i, p)| json!({"row": i, "entry": p.to_data()}))
                    .collect::<Vec<_>>()).collect::<Vec<_>>(),
            });
            read(data).unwrap()
        },
    )
}

fn entry_or_zero(m: &PolynomialMatrix, i: usize, j: usize) -> Polynomial {
    m.get(i, j)
        .cloned()
        .unwrap_or_else(|| Polynomial::zero(ring()))
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 200, ..ProptestConfig::default() })]

    /// Each entry of a product is the sum of the products of entries, and the
    /// product is associative.
    #[test]
    fn the_product_is_the_matrix_product(
        (a, b, c) in (0usize..3, 0usize..3, 0usize..3, 0usize..3)
            .prop_flat_map(|(m, k, n, p)| (matrix(m, k), matrix(k, n), matrix(n, p))),
    ) {
        let ab = a.mul(&b).unwrap();
        prop_assert_eq!((ab.nrows(), ab.ncols()), (a.nrows(), b.ncols()));
        for i in 0..a.nrows() {
            for j in 0..b.ncols() {
                let mut sum = Polynomial::zero(ring());
                for k in 0..a.ncols() {
                    sum = sum.add(&entry_or_zero(&a, i, k).mul(&entry_or_zero(&b, k, j)).unwrap()).unwrap();
                }
                prop_assert_eq!(entry_or_zero(&ab, i, j), sum);
            }
        }
        prop_assert_eq!(ab.mul(&c).unwrap(), a.mul(&b.mul(&c).unwrap()).unwrap());
        // The canonical form: no zero entry is kept.
        let data = ab.to_data();
        prop_assert!(data.columns.iter().flatten().all(|e| !e.entry.terms.is_empty()));
        let rows: Vec<Vec<usize>> = data.columns.iter().map(|c| c.iter().map(|e| e.row).collect()).collect();
        for r in rows {
            let mut sorted = r.clone();
            sorted.sort_unstable();
            sorted.dedup();
            prop_assert_eq!(r, sorted);
        }
    }
}

//! Monomial orders (Issue #64): `MonomialOrder::compare`, its validation and
//! its JSON form.
//!
//! The property test checks that every order is a monomial order (a total
//! order, compatible with multiplication, with 1 the smallest monomial) and
//! that it is the order of an independent definition by a matrix. The first
//! properties alone hold for every monomial order: a grevlex whose tie-break
//! is reversed (which is a monomial order too) passes them, and only the
//! comparison with the matrix and the orders printed by the CAS find it.

#![allow(clippy::unwrap_used)]

use std::cmp::Ordering;

use algebramesh_math_types::{
    BaseOrder, CoefficientDomain, MAX_WEIGHT, MathError, MonomialOrder, OrderBlock, Polynomial,
    Rational, RingSpec, WeylAlgebraSpec,
};
use proptest::prelude::*;

fn base() -> impl Strategy<Value = BaseOrder> {
    prop_oneof![
        Just(BaseOrder::Lex),
        Just(BaseOrder::Grlex),
        Just(BaseOrder::Grevlex)
    ]
}

fn weight() -> impl Strategy<Value = u32> {
    prop_oneof![4 => 0u32..4, 1 => Just(MAX_WEIGHT), 1 => Just(MAX_WEIGHT - 1)]
}

/// A composition of `n` into positive parts.
fn composition(n: usize) -> impl Strategy<Value = Vec<usize>> {
    proptest::collection::vec(any::<bool>(), n.saturating_sub(1)).prop_map(|cuts| {
        let mut parts = vec![];
        let mut size = 1;
        for cut in cuts {
            if cut {
                parts.push(size);
                size = 1;
            } else {
                size += 1;
            }
        }
        parts.push(size);
        parts
    })
}

fn order(n: usize) -> impl Strategy<Value = MonomialOrder> {
    prop_oneof![
        base().prop_map(MonomialOrder::from),
        (proptest::collection::vec(weight(), n), base())
            .prop_map(|(weights, then)| MonomialOrder::Weights { weights, then }),
        (composition(n), proptest::collection::vec(base(), n)).prop_map(|(parts, orders)| {
            MonomialOrder::Block(
                parts
                    .into_iter()
                    .zip(orders)
                    .map(|(count, order)| OrderBlock { count, order })
                    .collect(),
            )
        }),
    ]
}

/// Mostly small exponents, so that ties are frequent, and some large ones,
/// so that the degrees pass `u32` and the weighted degrees pass `u64`.
fn exponent() -> impl Strategy<Value = u32> {
    prop_oneof![8 => 0u32..4, 1 => Just(u32::MAX), 1 => Just(u32::MAX / 2)]
}

fn monomial(n: usize) -> impl Strategy<Value = Vec<u32>> {
    proptest::collection::vec(exponent(), n)
}

/// The rows of the matrix of `order` on the variables `offset..offset + n`
/// of `total` variables.
fn rows_of_base(order: BaseOrder, n: usize, offset: usize, total: usize) -> Vec<Vec<i128>> {
    let unit = |i: usize, v: i128| {
        let mut row = vec![0i128; total];
        row[offset + i] = v;
        row
    };
    let mut ones = vec![0i128; total];
    for entry in &mut ones[offset..offset + n] {
        *entry = 1;
    }
    match order {
        BaseOrder::Lex => (0..n).map(|i| unit(i, 1)).collect(),
        BaseOrder::Grlex => std::iter::once(ones)
            .chain((0..n).map(|i| unit(i, 1)))
            .collect(),
        BaseOrder::Grevlex => std::iter::once(ones)
            .chain((0..n).rev().map(|i| unit(i, -1)))
            .collect(),
    }
}

/// An independent definition of the orders: `a > b` when `M a > M b`
/// lexicographically, for the matrix `M` of the order (Robbiano):
/// lex is `I`, grlex is `[1 ... 1; I]`, grevlex is
/// `[1 ... 1; -e_n; ...; -e_1]`, `Weights` puts `w` above the matrix of
/// `then` and `Block` puts the matrices of its blocks one below the other.
fn matrix(order: &MonomialOrder, n: usize) -> Vec<Vec<i128>> {
    match order {
        MonomialOrder::Lex => rows_of_base(BaseOrder::Lex, n, 0, n),
        MonomialOrder::Grlex => rows_of_base(BaseOrder::Grlex, n, 0, n),
        MonomialOrder::Grevlex => rows_of_base(BaseOrder::Grevlex, n, 0, n),
        MonomialOrder::Weights { weights, then } => {
            std::iter::once(weights.iter().map(|&w| i128::from(w)).collect())
                .chain(rows_of_base(*then, n, 0, n))
                .collect()
        }
        MonomialOrder::Block(blocks) => {
            let mut rows = vec![];
            let mut offset = 0;
            for b in blocks {
                rows.extend(rows_of_base(b.order, b.count, offset, n));
                offset += b.count;
            }
            rows
        }
    }
}

fn by_matrix(m: &[Vec<i128>], a: &[u32], b: &[u32]) -> Ordering {
    let apply = |e: &[u32]| -> Vec<i128> {
        m.iter()
            .map(|row| row.iter().zip(e).map(|(w, &x)| w * i128::from(x)).sum())
            .collect()
    };
    apply(a).cmp(&apply(b))
}

type Case = (MonomialOrder, Vec<u32>, Vec<u32>, Vec<u32>);

fn case() -> impl Strategy<Value = Case> {
    (1usize..=5).prop_flat_map(|n| (order(n), monomial(n), monomial(n), monomial(n)))
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 2000, ..ProptestConfig::default() })]

    #[test]
    fn every_order_is_a_monomial_order((order, a, b, c) in case()) {
        let n = a.len();
        order.validate(n).unwrap();
        // 1. Antisymmetric.
        prop_assert_eq!(order.compare(&a, &b), order.compare(&b, &a).reverse());
        // 2. Total: only equal monomials compare equal.
        prop_assert_eq!(order.compare(&a, &b) == Ordering::Equal, a == b);
        // 3. Transitive.
        if order.compare(&a, &b).is_le() && order.compare(&b, &c).is_le() {
            prop_assert!(order.compare(&a, &c).is_le());
        }
        // 4. Compatible with multiplication, when the products fit in u32.
        let times = |x: &[u32], y: &[u32]| -> Option<Vec<u32>> {
            x.iter().zip(y).map(|(p, q)| p.checked_add(*q)).collect()
        };
        if let (Some(ac), Some(bc)) = (times(&a, &c), times(&b, &c)) {
            prop_assert_eq!(order.compare(&ac, &bc), order.compare(&a, &b));
        }
        // 5. 1 is the smallest monomial.
        let one = vec![0u32; n];
        if a != one {
            prop_assert_eq!(order.compare(&one, &a), Ordering::Less);
        }
        // 6. The order of its matrix.
        prop_assert_eq!(order.compare(&a, &b), by_matrix(&matrix(&order, n), &a, &b));
    }
}

fn weights(weights: &[u32], then: BaseOrder) -> MonomialOrder {
    MonomialOrder::Weights {
        weights: weights.to_vec(),
        then,
    }
}

fn block(parts: &[(usize, BaseOrder)]) -> MonomialOrder {
    MonomialOrder::Block(
        parts
            .iter()
            .map(|&(count, order)| OrderBlock { count, order })
            .collect(),
    )
}

/// The terms of f = x y^2 + x^2 z + y^3 + z^4 + x^3 from the largest down, as
/// Macaulay2 1.24.11 (`terms`) and OSCAR 1.8.2 (`terms(f; ordering)`) listed
/// them in the images.
#[test]
fn the_orders_of_the_cas() {
    use BaseOrder::{Grevlex, Grlex, Lex};
    let (x3, x2z, xy2, y3, z4) = (
        vec![3, 0, 0],
        vec![2, 0, 1],
        vec![1, 2, 0],
        vec![0, 3, 0],
        vec![0, 0, 4],
    );
    let f = [xy2.clone(), x2z.clone(), y3.clone(), z4.clone(), x3.clone()];
    let descending = |order: &MonomialOrder| {
        let mut terms = f.to_vec();
        terms.sort_by(|a, b| order.compare(b, a));
        terms
    };
    let cases = [
        // Macaulay2 Lex, OSCAR lex.
        (MonomialOrder::Lex, [&x3, &x2z, &xy2, &y3, &z4]),
        // GLex (which Macaulay2 writes as Weights => {1, 1, 1}, Lex => 3), deglex.
        (MonomialOrder::Grlex, [&z4, &x3, &x2z, &xy2, &y3]),
        // GRevLex, degrevlex.
        (MonomialOrder::Grevlex, [&z4, &x3, &xy2, &y3, &x2z]),
        // weight_ordering([1, 0, 0], lex).
        (weights(&[1, 0, 0], Lex), [&x3, &x2z, &xy2, &y3, &z4]),
        // {Weights => {0, 0, 1}, GRevLex => 3}.
        (weights(&[0, 0, 1], Grevlex), [&z4, &x2z, &x3, &xy2, &y3]),
        // {Weights => {0, 0, 1}, Weights => {1, 1, 1}, Lex => 3}.
        (weights(&[0, 0, 1], Grlex), [&z4, &x2z, &x3, &xy2, &y3]),
        // {GRevLex => 2, Lex => 1}, degrevlex([x, y]) * lex([z]).
        (
            block(&[(2, Grevlex), (1, Lex)]),
            [&x3, &xy2, &y3, &x2z, &z4],
        ),
        // {Lex => 1, GRevLex => 2}, lex([x]) * degrevlex([y, z]).
        (
            block(&[(1, Lex), (2, Grevlex)]),
            [&x3, &x2z, &xy2, &z4, &y3],
        ),
        // {Weights => {1, 1}, Lex => 2, Weights => {1}, Lex => 1},
        // deglex([x, y]) * deglex([z]).
        (
            block(&[(2, Grlex), (1, Grlex)]),
            [&x3, &xy2, &y3, &x2z, &z4],
        ),
    ];
    for (order, expected) in cases {
        let expected: Vec<Vec<u32>> = expected.into_iter().cloned().collect();
        assert_eq!(descending(&order), expected, "{order:?}");
    }
    // x z + y^2: grevlex puts y^2 first, grlex x z.
    assert_eq!(
        MonomialOrder::Grevlex.compare(&[0, 2, 0], &[1, 0, 1]),
        Ordering::Greater
    );
    assert_eq!(
        MonomialOrder::Grlex.compare(&[0, 2, 0], &[1, 0, 1]),
        Ordering::Less
    );
}

/// The weighted degree is summed in u128: in u64 the weighted degrees below
/// would wrap and compare the wrong way.
#[test]
fn large_weighted_degrees_do_not_wrap() {
    let order = weights(&[MAX_WEIGHT; 5], BaseOrder::Lex);
    let a = [u32::MAX; 5];
    let b = [u32::MAX, u32::MAX, u32::MAX, u32::MAX, u32::MAX - 1];
    assert!(5 * u128::from(MAX_WEIGHT) * u128::from(u32::MAX) > u128::from(u64::MAX));
    assert_eq!(order.compare(&a, &b), Ordering::Greater);
    assert_eq!(order.compare(&b, &a), Ordering::Less);
    // The degree of grlex and grevlex passes u32 as well.
    let c = [u32::MAX, 1];
    let d = [0, u32::MAX];
    assert_eq!(MonomialOrder::Grlex.compare(&c, &d), Ordering::Greater);
    assert_eq!(MonomialOrder::Grevlex.compare(&c, &d), Ordering::Greater);
}

/// Exponent vectors of another length than the order was validated for give
/// some ordering, without a panic.
#[test]
fn vectors_of_the_wrong_length_do_not_panic() {
    let orders = [
        MonomialOrder::Lex,
        MonomialOrder::Grevlex,
        weights(&[1, 2], BaseOrder::Grevlex),
        block(&[(2, BaseOrder::Grevlex), (3, BaseOrder::Lex)]),
    ];
    for order in &orders {
        for (a, b) in [
            (&[1u32, 2, 3][..], &[1u32][..]),
            (&[][..], &[4u32, 5, 6, 7, 8, 9][..]),
        ] {
            let _ = order.compare(a, b);
            let _ = order.compare(b, a);
        }
    }
}

fn ring(order: serde_json::Value) -> Result<RingSpec, String> {
    let ring: RingSpec = serde_json::from_value(serde_json::json!({
        "domain": "QQ", "variables": ["x", "y", "z"], "order": order}))
    .map_err(|e| e.to_string())?;
    ring.validate().map_err(|e| e.to_string())?;
    Ok(ring)
}

/// The JSON of the three orders that existed before Issue #64 does not change
/// by a byte, both ways.
#[test]
fn the_existing_json_is_unchanged() {
    for (order, name) in [
        (MonomialOrder::Lex, "lex"),
        (MonomialOrder::Grlex, "grlex"),
        (MonomialOrder::Grevlex, "grevlex"),
    ] {
        let text = format!(r#"{{"domain":"QQ","variables":["x","y","z"],"order":"{name}"}}"#);
        let r: RingSpec = serde_json::from_str(&text).unwrap();
        assert_eq!(r.order, order);
        assert_eq!(serde_json::to_string(&r).unwrap(), text);
        assert_eq!(
            serde_json::to_string(&order).unwrap(),
            format!("\"{name}\"")
        );
    }
}

#[test]
fn weights_and_blocks_round_trip() {
    let cases = [
        (
            r#"{"weights":[1,0,2],"then":"grevlex"}"#,
            weights(&[1, 0, 2], BaseOrder::Grevlex),
        ),
        (
            r#"{"weights":[0,2147483647,0],"then":"lex"}"#,
            weights(&[0, MAX_WEIGHT, 0], BaseOrder::Lex),
        ),
        (
            r#"{"block":[{"count":2,"order":"grevlex"},{"count":1,"order":"lex"}]}"#,
            block(&[(2, BaseOrder::Grevlex), (1, BaseOrder::Lex)]),
        ),
        (
            r#"{"block":[{"count":3,"order":"grlex"}]}"#,
            block(&[(3, BaseOrder::Grlex)]),
        ),
    ];
    for (text, order) in cases {
        let read: MonomialOrder = serde_json::from_str(text).unwrap();
        assert_eq!(read, order);
        assert_eq!(serde_json::to_string(&order).unwrap(), text);
        let r = ring(serde_json::from_str(text).unwrap()).unwrap();
        assert_eq!(r.order, order);
        let back: RingSpec = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(back, r);
    }
    // The keys may come in any order.
    let read: MonomialOrder =
        serde_json::from_str(r#"{"then":"grlex","weights":[1,1,1]}"#).unwrap();
    assert_eq!(read, weights(&[1, 1, 1], BaseOrder::Grlex));
    let read: MonomialOrder =
        serde_json::from_str(r#"{"block":[{"order":"lex","count":3}]}"#).unwrap();
    assert_eq!(read, block(&[(3, BaseOrder::Lex)]));
}

/// Each malformed order is refused with a message that says why.
#[test]
fn malformed_orders_are_refused() {
    use serde_json::json;
    let refused = |order: serde_json::Value, why: &str| {
        let e = ring(order.clone()).unwrap_err();
        assert!(e.contains(why), "{order}: {e}");
    };
    refused(
        json!({"weights": [-1, 0, 0], "then": "lex"}),
        "the weight -1 is negative: only global orders are accepted",
    );
    refused(
        json!({"weights": [2147483648u64, 0, 0], "then": "lex"}),
        "the weight 2147483648 is above 2147483647",
    );
    refused(
        json!({"weights": [4294967296u64, 0, 0], "then": "lex"}),
        "the weight 4294967296 is above 2147483647",
    );
    refused(
        json!({"weights": [1, 0], "then": "lex"}),
        "2 weights for 3 variables",
    );
    refused(
        json!({"weights": [1, 0, 0, 0], "then": "lex"}),
        "4 weights for 3 variables",
    );
    refused(json!({"weights": [1, 0, 0]}), "missing field `then`");
    refused(
        json!({"weights": [1, 0, 0], "then": "lex", "x": 1}),
        "unknown field `x`",
    );
    refused(
        json!({"weights": [1, 0, 0], "then": "lex", "block": []}),
        "unknown field `block`",
    );
    refused(
        json!({"weights": [1, 0, 0], "then": {"weights": [1, 1, 1], "then": "lex"}}),
        r#"invalid type: map, expected "lex", "grlex" or "grevlex""#,
    );
    refused(
        json!({"weights": [1, 0, 0], "then": "deglex"}),
        "unknown variant `deglex`",
    );
    refused(
        json!({"weights": [1.5, 0, 0], "then": "lex"}),
        "expected a weight, an integer from 0 to 2147483647",
    );
    refused(
        json!({"weights": [1.0, 0, 0], "then": "lex"}),
        "expected a weight, an integer from 0 to 2147483647",
    );
    refused(
        json!({"weights": ["1", 0, 0], "then": "lex"}),
        "expected a weight, an integer from 0 to 2147483647",
    );
    refused(
        json!({"block": [{"count": 2, "order": "lex"}]}),
        "the blocks do not cover the 3 variables exactly",
    );
    refused(
        json!({"block": [{"count": 2, "order": "lex"}, {"count": 2, "order": "lex"}]}),
        "the blocks do not cover the 3 variables exactly",
    );
    refused(
        json!({"block": [{"count": 0, "order": "lex"}, {"count": 3, "order": "lex"}]}),
        "at least one block, of at least one variable each",
    );
    refused(
        json!({"block": []}),
        "at least one block, of at least one variable each",
    );
    refused(
        json!({"block": [{"count": -1, "order": "lex"}, {"count": 4, "order": "lex"}]}),
        "invalid value: integer `-1`",
    );
    refused(
        json!({"block": [{"count": 3, "order": "lex", "x": 1}]}),
        "unknown field `x`",
    );
    refused(
        json!({"block": [{"count": 3, "order": {"lex": null}}]}),
        "invalid type: map",
    );
    refused(json!({"block": [{"count": 3}]}), "missing field `order`");
    refused(
        json!({"block": [{"order": "lex"}]}),
        "missing field `count`",
    );
    refused(
        json!({"block": [{"count": 3, "order": "lex"}], "then": "lex"}),
        "unknown field `then`",
    );
    // A block entry has only the object form: the array form of a derived
    // struct is not a second spelling.
    let entry = r#"invalid type: sequence, expected a block {"count": ..., "order": ...}"#;
    refused(json!({"block": [[3, "lex"]]}), entry);
    refused(
        json!({"block": [{"count": 1, "order": "lex"}, [2, "grevlex"]]}),
        entry,
    );
    let forms = r#"an order is "lex", "grlex", "grevlex""#;
    refused(json!("deglex"), "unknown order \"deglex\"");
    refused(json!("Lex"), "unknown order \"Lex\"");
    refused(json!("deglex"), forms);
    // A derived enum would take this for the unit variant `lex`.
    refused(json!({"lex": null}), forms);
    refused(json!({}), forms);
    refused(json!(null), forms);
    refused(json!(1), forms);
    refused(json!(["lex"]), forms);
    // A key given twice, which a JSON value cannot hold: neither value is
    // taken silently, in an order or in a block entry.
    let refused_text = |order: &str, why: &str| {
        let text = format!(r#"{{"domain":"QQ","variables":["x","y","z"],"order":{order}}}"#);
        let e = serde_json::from_str::<RingSpec>(&text)
            .unwrap_err()
            .to_string();
        assert!(e.contains(why), "{order}: {e}");
        let e = serde_json::from_str::<MonomialOrder>(order)
            .unwrap_err()
            .to_string();
        assert!(e.contains(why), "{order}: {e}");
    };
    refused_text(
        r#"{"weights":[2,0,0],"weights":[1,0,0],"then":"lex"}"#,
        "duplicate field `weights`",
    );
    refused_text(
        r#"{"weights":[1,0,0],"then":"lex","weights":[1,0,0]}"#,
        "duplicate field `weights`",
    );
    refused_text(
        r#"{"then":"grlex","weights":[1,0,0],"then":"lex"}"#,
        "duplicate field `then`",
    );
    refused_text(
        r#"{"block":[{"count":3,"order":"lex"}],"block":[{"count":3,"order":"lex"}]}"#,
        "duplicate field `block`",
    );
    refused_text(
        r#"{"block":[{"count":3,"count":3,"order":"lex"}]}"#,
        "duplicate field `count`",
    );
    refused_text(
        r#"{"block":[{"count":3,"order":"grlex","order":"lex"}]}"#,
        "duplicate field `order`",
    );
    // The value of the first key is read before the second key is seen.
    refused_text(
        r#"{"weights":[-1,0,0],"weights":[1,0,0],"then":"lex"}"#,
        "the weight -1 is negative",
    );
    // The other errors read from text as from a JSON value.
    refused_text(r#"{"block":[[3,"lex"]]}"#, entry);
    refused_text(
        r#"{"weights":[1,0,0],"then":"lex","block":[]}"#,
        "unknown field `block`",
    );
    refused_text(r#"{"lex":null}"#, forms);
    refused_text("null", forms);
}

#[test]
fn validation_counts_the_variables() {
    let invalid = |order: &MonomialOrder, nvars: usize| {
        matches!(order.validate(nvars), Err(MathError::Invalid(_)))
    };
    // A ring without variables: the three orders and empty weights.
    for order in [
        MonomialOrder::Lex,
        MonomialOrder::Grlex,
        MonomialOrder::Grevlex,
        weights(&[], BaseOrder::Grevlex),
    ] {
        order.validate(0).unwrap();
    }
    assert!(invalid(&MonomialOrder::Block(vec![]), 0));
    assert!(invalid(&block(&[(0, BaseOrder::Lex)]), 0));
    assert!(invalid(&weights(&[1], BaseOrder::Lex), 0));
    // A weight above MAX_WEIGHT can be built in Rust, and is refused.
    weights(&[MAX_WEIGHT, 0], BaseOrder::Lex)
        .validate(2)
        .unwrap();
    let e = weights(&[0, MAX_WEIGHT + 1], BaseOrder::Lex).validate(2);
    assert_eq!(
        e,
        Err(MathError::Invalid(
            "the weight 2147483648 is above 2147483647".into()
        ))
    );
    assert!(invalid(&weights(&[1, 2, 3], BaseOrder::Lex), 2));
    // Block counts that overflow when added.
    assert!(invalid(
        &block(&[(usize::MAX, BaseOrder::Lex), (2, BaseOrder::Lex)]),
        1
    ));
    block(&[(1, BaseOrder::Lex), (1, BaseOrder::Grlex)])
        .validate(2)
        .unwrap();
}

/// A ring is checked with its order wherever it is checked: built, or used
/// for a polynomial.
#[test]
fn rings_and_polynomials_check_their_order() {
    let names = || vec!["x".to_owned(), "y".to_owned()];
    let bad = weights(&[1], BaseOrder::Lex);
    assert!(RingSpec::new(CoefficientDomain::Rationals, names(), bad.clone()).is_err());
    let ring = RingSpec {
        domain: CoefficientDomain::Rationals,
        variables: names(),
        order: bad,
    };
    assert!(ring.validate().is_err());
    assert!(Polynomial::from_terms(ring, [(vec![1, 0], Rational::integer(1))]).is_err());
    let good = RingSpec::new(
        CoefficientDomain::Rationals,
        names(),
        block(&[(1, BaseOrder::Lex), (1, BaseOrder::Lex)]),
    )
    .unwrap();
    assert!(Polynomial::from_terms(good, [(vec![1, 0], Rational::integer(1))]).is_ok());
}

/// The ordering of a Weyl algebra is on its 2n variables x_1..x_n, d_1..d_n.
#[test]
fn the_ordering_of_a_weyl_algebra_is_on_both_kinds_of_variables() {
    let mut algebra = WeylAlgebraSpec::standard(CoefficientDomain::Rationals, 2).unwrap();
    algebra.ordering = Some(weights(&[0, 0, 1, 1], BaseOrder::Grevlex));
    algebra.validate().unwrap();
    algebra.ordering = Some(weights(&[0, 1], BaseOrder::Grevlex));
    assert!(algebra.validate().is_err());
    algebra.ordering = Some(block(&[(2, BaseOrder::Lex), (2, BaseOrder::Grevlex)]));
    algebra.validate().unwrap();
    algebra.ordering = Some(block(&[(2, BaseOrder::Lex)]));
    assert!(algebra.validate().is_err());
}

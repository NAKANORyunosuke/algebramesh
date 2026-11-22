//! Issue #65: leading terms in the order of the ring, and the JSON forms of
//! ideals and factorizations.

#![allow(clippy::unwrap_used)]

use algebramesh_math_types::{
    BaseOrder, CoefficientDomain, Factorization, Ideal, IdealData, MonomialOrder, OrderBlock,
    Polynomial, Rational, RingSpec,
};
use proptest::prelude::*;
use serde_json::json;

fn ring(order: MonomialOrder) -> RingSpec {
    RingSpec::new(
        CoefficientDomain::Rationals,
        vec!["x".into(), "y".into(), "z".into()],
        order,
    )
    .unwrap()
}

fn integers() -> RingSpec {
    RingSpec::new(
        CoefficientDomain::Integers,
        vec!["x".into(), "y".into()],
        MonomialOrder::Grevlex,
    )
    .unwrap()
}

fn poly(ring: &RingSpec, terms: &[([u32; 3], i64)]) -> Polynomial {
    Polynomial::from_terms(
        ring.clone(),
        terms
            .iter()
            .map(|(e, c)| (e.to_vec(), Rational::integer(*c))),
    )
    .unwrap()
}

#[test]
fn the_leading_term_follows_the_order_of_the_ring() {
    // x z + 2 y^2: grevlex puts y^2 first, grlex and lex put x z first.
    let terms = [([1, 0, 1], 1), ([0, 2, 0], 2)];
    for (order, monomial, coefficient) in [
        (MonomialOrder::Grevlex, [0, 2, 0], 2),
        (MonomialOrder::Grlex, [1, 0, 1], 1),
        (MonomialOrder::Lex, [1, 0, 1], 1),
    ] {
        let p = poly(&ring(order.clone()), &terms);
        assert_eq!(p.leading_monomial(), Some(&monomial[..]), "{order:?}");
        assert_eq!(
            p.leading_coefficient(),
            Some(&Rational::integer(coefficient)),
            "{order:?}"
        );
        assert_eq!(
            p.leading_term(),
            Some((&monomial[..], &Rational::integer(coefficient)))
        );
    }
    // y^3 + x z: lex puts x z first, against the degree.
    let p = poly(&ring(MonomialOrder::Lex), &[([0, 3, 0], 1), ([1, 0, 1], 1)]);
    assert_eq!(p.leading_monomial(), Some(&[1, 0, 1][..]));
    assert_eq!(p.total_degree(), Some(3));
    // A weight on z alone, and an elimination order for x.
    let weights = MonomialOrder::Weights {
        weights: vec![0, 0, 1],
        then: BaseOrder::Grevlex,
    };
    let block = MonomialOrder::Block(vec![
        OrderBlock {
            count: 1,
            order: BaseOrder::Lex,
        },
        OrderBlock {
            count: 2,
            order: BaseOrder::Grevlex,
        },
    ]);
    let p = poly(&ring(weights), &[([3, 0, 0], 1), ([0, 0, 1], 1)]);
    assert_eq!(p.leading_monomial(), Some(&[0, 0, 1][..]));
    let p = poly(&ring(block), &[([0, 5, 0], 1), ([1, 0, 0], 1)]);
    assert_eq!(p.leading_monomial(), Some(&[1, 0, 0][..]));
    // Changing the order of the ring changes the leading term.
    let p = poly(&ring(MonomialOrder::Grevlex), &terms);
    let q = p.with_ring(ring(MonomialOrder::Lex)).unwrap();
    assert_eq!(q.leading_monomial(), Some(&[1, 0, 1][..]));
}

#[test]
fn the_zero_polynomial_has_no_leading_term() {
    let zero = Polynomial::zero(ring(MonomialOrder::Grevlex));
    assert_eq!(zero.leading_term(), None);
    assert_eq!(zero.leading_monomial(), None);
    assert_eq!(zero.leading_coefficient(), None);
    assert_eq!(zero.total_degree(), None);
    let one = Polynomial::constant(ring(MonomialOrder::Lex), Rational::integer(-4)).unwrap();
    assert_eq!(
        one.leading_term(),
        Some((&[0, 0, 0][..], &Rational::integer(-4)))
    );
    assert_eq!(one.total_degree(), Some(0));
}

fn order() -> impl Strategy<Value = MonomialOrder> {
    let base = prop_oneof![
        Just(BaseOrder::Lex),
        Just(BaseOrder::Grlex),
        Just(BaseOrder::Grevlex)
    ];
    prop_oneof![
        Just(MonomialOrder::Lex),
        Just(MonomialOrder::Grlex),
        Just(MonomialOrder::Grevlex),
        (proptest::collection::vec(0u32..3, 3), base.clone())
            .prop_map(|(weights, then)| MonomialOrder::Weights { weights, then }),
        (1usize..3, base.clone(), base).prop_map(|(k, a, b)| MonomialOrder::Block(vec![
            OrderBlock { count: k, order: a },
            OrderBlock {
                count: 3 - k,
                order: b
            },
        ])),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 500, ..ProptestConfig::default() })]

    /// The leading term is a term, and no term is larger in the order.
    #[test]
    fn the_leading_term_is_the_largest(
        order in order(),
        terms in proptest::collection::btree_map(
            proptest::collection::vec(0u32..4, 3), -3i64..=3, 0..6),
    ) {
        let p = Polynomial::from_terms(
            ring(order.clone()),
            terms.into_iter().map(|(e, c)| (e, Rational::integer(c))),
        )
        .unwrap();
        match p.leading_term() {
            None => prop_assert!(p.is_zero()),
            Some((lm, lc)) => {
                prop_assert!(p.terms().any(|(e, c)| e == lm && c == lc));
                for (e, _) in p.terms() {
                    prop_assert_ne!(order.compare(e, lm), std::cmp::Ordering::Greater);
                }
                let degree = p.terms().map(|(e, _)| e.iter().map(|&d| u64::from(d)).sum::<u64>()).max();
                prop_assert_eq!(p.total_degree(), degree);
            }
        }
    }
}

fn ideal_json() -> serde_json::Value {
    json!({
        "type": "ideal",
        "ring": {"domain": "QQ", "variables": ["x", "y", "z"], "order": "grevlex"},
        "generators": [
            {"type": "polynomial", "terms": [
                {"coefficient": "1", "exponents": [2, 0, 0]},
                {"coefficient": "-1/2", "exponents": [0, 1, 0]}]},
            {"type": "polynomial", "terms": []},
            {"type": "polynomial", "terms": [{"coefficient": "3", "exponents": [0, 0, 1]}]}
        ]
    })
}

#[test]
fn an_ideal_round_trips_through_json() {
    let data: IdealData = serde_json::from_value(ideal_json()).unwrap();
    let ideal = Ideal::from_data(&data).unwrap();
    // The zero generator and the order of the generators are kept.
    assert_eq!(ideal.generators.len(), 3);
    assert!(ideal.generators[1].is_zero());
    assert_eq!(serde_json::to_value(ideal.to_data()).unwrap(), ideal_json());
    let zero: IdealData = serde_json::from_value(json!({
        "type": "ideal",
        "ring": {"domain": "ZZ", "variables": [], "order": "lex"},
        "generators": []
    }))
    .unwrap();
    assert!(Ideal::from_data(&zero).unwrap().generators.is_empty());
}

#[test]
fn an_ideal_with_an_unknown_key_or_a_foreign_generator_is_refused() {
    let with = |edit: &dyn Fn(&mut serde_json::Value)| {
        let mut v = ideal_json();
        edit(&mut v);
        v
    };
    for bad in [
        with(&|v| v["extra"] = json!(1)),
        with(&|v| v["type"] = json!("polynomial")),
        with(&|v| {
            v.as_object_mut().unwrap().remove("type");
        }),
        with(&|v| v["generators"][0]["extra"] = json!(1)),
        with(&|v| v["ring"]["order"] = json!("deglex")),
    ] {
        assert!(
            serde_json::from_value::<IdealData>(bad.clone()).is_err(),
            "{bad}"
        );
    }
    for bad in [
        // An exponent vector of the wrong length.
        with(&|v| v["generators"][2]["terms"][0]["exponents"] = json!([0, 1])),
        // A fraction over ZZ.
        with(&|v| v["ring"]["domain"] = json!("ZZ")),
        // A repeated exponent vector.
        with(&|v| {
            v["generators"][2]["terms"] = json!([
                {"coefficient": "1", "exponents": [0, 0, 1]},
                {"coefficient": "2", "exponents": [0, 0, 1]}])
        }),
        // A weight vector of the wrong length.
        with(&|v| v["ring"]["order"] = json!({"weights": [1, 2], "then": "lex"})),
    ] {
        let data: IdealData = serde_json::from_value(bad.clone()).unwrap();
        assert!(Ideal::from_data(&data).is_err(), "{bad}");
    }
    // An invalid ring is refused even with no generator to read it.
    for ring in [
        json!({"domain": "QQ", "variables": ["x", "x"], "order": "lex"}),
        json!({"domain": "QQ", "variables": ["x", "y"], "order": {"weights": [1], "then": "lex"}}),
    ] {
        let data: IdealData = serde_json::from_value(json!({
            "type": "ideal", "ring": ring, "generators": []
        }))
        .unwrap();
        assert!(Ideal::from_data(&data).is_err(), "{ring}");
    }
    // Generators of another ring.
    let p = poly(&ring(MonomialOrder::Lex), &[([1, 0, 0], 1)]);
    assert!(Ideal::new(ring(MonomialOrder::Grevlex), vec![p]).is_err());
}

#[test]
fn a_factorization_round_trips_through_json() {
    let r = integers();
    let x = Polynomial::from_terms(r.clone(), [(vec![1, 0], Rational::integer(1))]).unwrap();
    let f = Polynomial::from_terms(
        r.clone(),
        [
            (vec![1, 0], Rational::integer(1)),
            (vec![0, 1], Rational::integer(-1)),
        ],
    )
    .unwrap();
    let factorization = Factorization::new(r, Rational::integer(-6), vec![(x, 2), (f, 1)]).unwrap();
    let value = serde_json::to_value(&factorization).unwrap();
    assert_eq!(value["type"], "factorization");
    let read: Factorization = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(read, factorization);
    assert_eq!(serde_json::to_value(&read).unwrap(), value);

    let with = |edit: &dyn Fn(&mut serde_json::Value)| {
        let mut v = value.clone();
        edit(&mut v);
        v
    };
    for bad in [
        with(&|v| v["verified_product"] = json!(true)),
        with(&|v| v["type"] = json!("ideal")),
        with(&|v| {
            v.as_object_mut().unwrap().remove("type");
        }),
        with(&|v| v["factors"][0]["extra"] = json!(1)),
        // A constant outside ZZ.
        with(&|v| v["constant"] = json!("1/2")),
        // A factor with an exponent vector of the wrong length.
        with(&|v| v["factors"][0]["factor"]["terms"][0]["exponents"] = json!([1])),
        // A factor with a coefficient outside ZZ.
        with(&|v| v["factors"][1]["factor"]["terms"][0]["coefficient"] = json!("1/3")),
        // An invalid ring.
        with(&|v| v["ring"]["variables"] = json!(["x", "x"])),
    ] {
        assert!(
            serde_json::from_value::<Factorization>(bad.clone()).is_err(),
            "{bad}"
        );
    }
    // An invalid ring is refused even with no factor to read it.
    let bare = json!({
        "type": "factorization",
        "ring": {"domain": "ZZ", "variables": ["x", "x"], "order": "lex"},
        "constant": "1",
        "factors": []
    });
    assert!(serde_json::from_value::<Factorization>(bare).is_err());
}

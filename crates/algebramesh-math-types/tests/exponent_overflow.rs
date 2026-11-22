//! Products never wrap an exponent around: either every exponent of the result is
//! the exact sum, or the product fails with `ExponentOverflow` (Issue #7).

#![allow(clippy::unwrap_used)]

use algebramesh_math_types::{
    CoefficientDomain, DifferentialOperator, MathError, MonomialOrder, Polynomial, Rational,
    RingSpec, WeylAlgebraSpec,
};
use proptest::prelude::*;

/// Exponents spread over the whole range, with many near `u32::MAX / 2` and
/// `u32::MAX`, where sums start to overflow.
fn exponent() -> impl Strategy<Value = u32> {
    prop_oneof![
        0..16u32,
        (u32::MAX / 2 - 8)..=(u32::MAX / 2 + 8),
        (u32::MAX - 16)..=u32::MAX,
        any::<u32>(),
    ]
}

fn ring() -> RingSpec {
    RingSpec::new(
        CoefficientDomain::Rationals,
        vec!["x".into(), "y".into()],
        MonomialOrder::Grevlex,
    )
    .unwrap()
}

fn monomial(e: [u32; 2], c: &str) -> Polynomial {
    Polynomial::from_terms(ring(), [(e.to_vec(), c.parse::<Rational>().unwrap())]).unwrap()
}

proptest! {
    #[test]
    fn polynomial_products_are_exact_or_refused(
        a in [exponent(), exponent()],
        b in [exponent(), exponent()],
        c in prop_oneof![Just("3"), Just("1/3")],
    ) {
        let product = monomial(a, c).mul(&monomial(b, "2"));
        let sums: Vec<u64> = a.iter().zip(&b).map(|(p, q)| u64::from(*p) + u64::from(*q)).collect();
        if sums.iter().all(|&s| s <= u64::from(u32::MAX)) {
            let product = product.unwrap();
            let (e, _) = product.terms().next().unwrap();
            let e: Vec<u64> = e.iter().map(|&d| u64::from(d)).collect();
            prop_assert_eq!(e, sums);
        } else {
            prop_assert_eq!(product, Err(MathError::ExponentOverflow));
        }
    }

    #[test]
    fn powers_are_exact_or_refused(a in exponent(), k in prop_oneof![0..8u32, any::<u32>()]) {
        let power = monomial([a, 1], "1").pow(k);
        let degree = u64::from(a) * u64::from(k);
        if degree <= u64::from(u32::MAX) {
            let power = power.unwrap();
            let (e, _) = power.terms().next().unwrap();
            prop_assert_eq!(e, &[u32::try_from(degree).unwrap(), k][..]);
        } else {
            prop_assert_eq!(power, Err(MathError::ExponentOverflow));
        }
    }

    #[test]
    fn weyl_products_are_exact_or_refused(
        x1 in exponent(), d1 in 0..3u32, x2 in 0..3u32, d2 in exponent(),
    ) {
        // (x^x1 d^d1) (x^x2 d^d2): the leading term is x^(x1+x2) d^(d1+d2).
        let algebra = WeylAlgebraSpec::standard(CoefficientDomain::Rationals, 1).unwrap();
        let op = |x: u32, d: u32| {
            DifferentialOperator::from_terms(algebra.clone(), [(vec![x], vec![d], Rational::one())])
                .unwrap()
        };
        let product = op(x1, d1).mul(&op(x2, d2));
        let fits = |p: u32, q: u32| u64::from(p) + u64::from(q) <= u64::from(u32::MAX);
        if fits(x1, x2) && fits(d1, d2) {
            let data = product.unwrap().to_data();
            // `to_data` lists the terms in descending order; the first one is the
            // term k = 0.
            prop_assert_eq!(&data[0].x_exponents, &vec![x1 + x2]);
            prop_assert_eq!(&data[0].d_exponents, &vec![d1 + d2]);
        } else {
            prop_assert_eq!(product, Err(MathError::ExponentOverflow));
        }
    }
}

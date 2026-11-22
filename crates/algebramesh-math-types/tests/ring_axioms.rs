//! The ring axioms for polynomials and for the Weyl algebra (Issue #39).
//!
//! Polynomials over QQ in three variables, and differential operators of the
//! Weyl algebra in two variables, of small degree and with small coefficients
//! (integers, so that the integer path of the polynomial product is used, and
//! fractions, for the rational path). The product of differential operators is
//! also checked against their action on polynomials, which the test computes
//! by differentiating term by term: the Weyl algebra acts faithfully on the
//! polynomial ring in characteristic zero, so `(PQ) f = P (Q f)` for every `f`
//! determines the product.

#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;

use algebramesh_math_types::{
    CoefficientDomain, DifferentialOperator, MonomialOrder, Polynomial, Rational, RingSpec,
    WeylAlgebraSpec,
};
use num_bigint::BigInt;
use num_rational::BigRational;
use proptest::prelude::*;

const NVARS: usize = 3;
const N: usize = 2;

fn ring() -> RingSpec {
    RingSpec::new(
        CoefficientDomain::Rationals,
        vec!["x".into(), "y".into(), "z".into()],
        MonomialOrder::Grevlex,
    )
    .unwrap()
}

fn algebra() -> WeylAlgebraSpec {
    WeylAlgebraSpec::standard(CoefficientDomain::Rationals, N).unwrap()
}

/// Small integers, or fractions with a small denominator.
fn coefficient() -> impl Strategy<Value = Rational> {
    prop_oneof![
        (-5i64..=5).prop_map(Rational::integer),
        (-5i64..=5, 1i64..=4)
            .prop_map(|(p, q)| { Rational::new(BigInt::from(p), BigInt::from(q)).unwrap() }),
    ]
}

fn integer() -> impl Strategy<Value = Rational> {
    (-5i64..=5).prop_map(Rational::integer)
}

fn terms(
    c: impl Strategy<Value = Rational>,
) -> impl Strategy<Value = BTreeMap<Vec<u32>, Rational>> {
    proptest::collection::btree_map(proptest::collection::vec(0u32..3, NVARS), c, 0..5)
}

fn polynomial() -> impl Strategy<Value = Polynomial> {
    prop_oneof![terms(coefficient()), terms(integer())]
        .prop_map(|t| Polynomial::from_terms(ring(), t).unwrap())
}

fn operator() -> impl Strategy<Value = DifferentialOperator> {
    let term = (
        proptest::collection::vec(0u32..3, N),
        proptest::collection::vec(0u32..3, N),
        coefficient(),
    );
    proptest::collection::vec(term, 0..4)
        .prop_map(|t| DifferentialOperator::from_terms(algebra(), t).unwrap())
}

fn constant(c: i64) -> Polynomial {
    Polynomial::constant(ring(), Rational::integer(c)).unwrap()
}

fn op(x: [u32; N], d: [u32; N], c: i64) -> DifferentialOperator {
    DifferentialOperator::from_terms(algebra(), [(x.to_vec(), d.to_vec(), Rational::integer(c))])
        .unwrap()
}

fn neg(p: &Polynomial) -> Polynomial {
    p.scale(&Rational::integer(-1)).unwrap()
}

/// A polynomial in `x_1..x_N`, as exponent vectors and coefficients.
type Function = BTreeMap<Vec<u32>, BigRational>;

/// `P f`, computed from the definition: the term `c x^a d^b` differentiates
/// `x^e` to `c * prod_i e_i!/(e_i-b_i)! * x^(a+e-b)`, or zero if some
/// `b_i > e_i`.
fn act(p: &DifferentialOperator, f: &Function) -> Function {
    let mut out = Function::new();
    for t in p.to_data() {
        for (e, c) in f {
            if t.d_exponents.iter().zip(e).any(|(b, e)| b > e) {
                continue;
            }
            let mut value = &t.coefficient.0 * c;
            let mut exponent = Vec::with_capacity(N);
            for i in 0..N {
                let (a, b, e) = (t.x_exponents[i], t.d_exponents[i], e[i]);
                for k in 0..b {
                    value *= BigRational::from_integer(BigInt::from(e - k));
                }
                exponent.push(a + e - b);
            }
            *out.entry(exponent).or_default() += value;
        }
    }
    out.retain(|_, c| *c != BigRational::from_integer(0.into()));
    out
}

fn function() -> impl Strategy<Value = Function> {
    proptest::collection::btree_map(
        proptest::collection::vec(0u32..5, N),
        (-5i64..=5).prop_map(|c| BigRational::from_integer(c.into())),
        0..5,
    )
    .prop_map(|mut f| {
        f.retain(|_, c| *c != BigRational::from_integer(0.into()));
        f
    })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 200, ..ProptestConfig::default() })]

    #[test]
    fn polynomials_form_a_commutative_ring(
        a in polynomial(), b in polynomial(), c in polynomial(),
    ) {
        let zero = Polynomial::zero(ring());
        let one = constant(1);
        // Addition: an abelian group.
        prop_assert_eq!(a.add(&b).unwrap().add(&c).unwrap(), a.add(&b.add(&c).unwrap()).unwrap());
        prop_assert_eq!(a.add(&b).unwrap(), b.add(&a).unwrap());
        prop_assert_eq!(a.add(&zero).unwrap(), a.clone());
        prop_assert!(a.add(&neg(&a)).unwrap().is_zero());
        // Multiplication: associative, commutative, with unit 1, and
        // distributive over addition.
        let ab = a.mul(&b).unwrap();
        prop_assert_eq!(ab.mul(&c).unwrap(), a.mul(&b.mul(&c).unwrap()).unwrap());
        prop_assert_eq!(&ab, &b.mul(&a).unwrap());
        prop_assert_eq!(a.mul(&one).unwrap(), a.clone());
        prop_assert!(a.mul(&zero).unwrap().is_zero());
        prop_assert_eq!(
            a.mul(&b.add(&c).unwrap()).unwrap(),
            ab.add(&a.mul(&c).unwrap()).unwrap()
        );
        // The integer and the rational paths of the product agree:
        // (a/2)(b/3) * 6 = ab.
        let half = Rational::new(1.into(), 2.into()).unwrap();
        let third = Rational::new(1.into(), 3.into()).unwrap();
        let scaled = a.scale(&half).unwrap().mul(&b.scale(&third).unwrap()).unwrap();
        prop_assert_eq!(scaled.scale(&Rational::integer(6)).unwrap(), ab);
    }

    /// Issue #65: the difference and the negative.
    #[test]
    fn subtraction_adds_the_negative(a in polynomial(), b in polynomial()) {
        prop_assert!(a.sub(&a).unwrap().is_zero());
        prop_assert_eq!(a.sub(&b).unwrap().add(&b).unwrap(), a.clone());
        prop_assert_eq!(a.sub(&b).unwrap(), a.add(&b.neg()).unwrap());
        prop_assert_eq!(a.neg().neg(), a.clone());
        prop_assert_eq!(a.neg(), neg(&a));
        prop_assert_eq!(b.sub(&a).unwrap(), a.sub(&b).unwrap().neg());
    }

    #[test]
    fn powers_are_repeated_products(a in polynomial(), m in 0u32..4, n in 0u32..4) {
        let power = |k: u32| a.pow(k).unwrap();
        prop_assert_eq!(power(m + n), power(m).mul(&power(n)).unwrap());
        let mut product = constant(1);
        for _ in 0..m {
            product = product.mul(&a).unwrap();
        }
        prop_assert_eq!(power(m), product);
    }

    #[test]
    fn differential_operators_form_a_ring(
        p in operator(), q in operator(), r in operator(),
    ) {
        let one = op([0; N], [0; N], 1);
        let pq = p.mul(&q).unwrap();
        prop_assert_eq!(pq.mul(&r).unwrap(), p.mul(&q.mul(&r).unwrap()).unwrap());
        prop_assert_eq!(
            p.mul(&q.add(&r).unwrap()).unwrap(),
            pq.add(&p.mul(&r).unwrap()).unwrap()
        );
        prop_assert_eq!(
            p.add(&q).unwrap().mul(&r).unwrap(),
            p.mul(&r).unwrap().add(&q.mul(&r).unwrap()).unwrap()
        );
        prop_assert_eq!(p.mul(&one).unwrap(), p.clone());
        prop_assert_eq!(one.mul(&p).unwrap(), p.clone());
        prop_assert!(p.add(&p.neg()).unwrap().is_zero());
        prop_assert_eq!(p.add(&q).unwrap(), q.add(&p).unwrap());
    }

    #[test]
    fn the_product_of_operators_is_their_composition(
        p in operator(), q in operator(), f in function(),
    ) {
        let pq = p.mul(&q).unwrap();
        prop_assert_eq!(act(&pq, &f), act(&p, &act(&q, &f)));
    }
}

/// `d_i x_j - x_j d_i = delta_ij`, and the `x_i` commute, as do the `d_i`.
#[test]
fn the_commutation_relations_hold() {
    let x = |i: usize| {
        let mut e = [0; N];
        e[i] = 1;
        op(e, [0; N], 1)
    };
    let d = |i: usize| {
        let mut e = [0; N];
        e[i] = 1;
        op([0; N], e, 1)
    };
    let bracket = |a: &DifferentialOperator, b: &DifferentialOperator| {
        a.mul(b).unwrap().add(&b.mul(a).unwrap().neg()).unwrap()
    };
    let one = op([0; N], [0; N], 1);
    for i in 0..N {
        for j in 0..N {
            let expected = if i == j {
                one.clone()
            } else {
                DifferentialOperator::from_terms(algebra(), []).unwrap()
            };
            assert_eq!(bracket(&d(i), &x(j)), expected, "[d{i}, x{j}]");
            assert!(bracket(&x(i), &x(j)).is_zero());
            assert!(bracket(&d(i), &d(j)).is_zero());
        }
    }
    // d^2 x^3 = x^3 d^2 + 6 x^2 d + 6 x (one variable of two).
    let product = op([0, 0], [2, 0], 1).mul(&op([3, 0], [0, 0], 1)).unwrap();
    let expected = DifferentialOperator::from_terms(
        algebra(),
        [
            (vec![3, 0], vec![2, 0], Rational::integer(1)),
            (vec![2, 0], vec![1, 0], Rational::integer(6)),
            (vec![1, 0], vec![0, 0], Rational::integer(6)),
        ],
    )
    .unwrap();
    assert_eq!(product, expected);
}

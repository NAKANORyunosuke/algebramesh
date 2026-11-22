use std::collections::BTreeMap;
use std::fmt;

use num_bigint::BigInt;
use serde::{Deserialize, Serialize};

use crate::error::MathError;
use crate::ring::{CoefficientDomain, MonomialOrder, validate_variables};
use crate::scalar::Rational;

/// The Weyl algebra `K<x_1..x_n, d_1..d_n>` with `d_i x_j - x_j d_i = delta_ij`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WeylAlgebraSpec {
    pub coefficient_domain: CoefficientDomain,
    pub x_variables: Vec<String>,
    pub d_variables: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ordering: Option<MonomialOrder>,
}

impl WeylAlgebraSpec {
    pub fn new(
        coefficient_domain: CoefficientDomain,
        x_variables: Vec<String>,
        d_variables: Vec<String>,
    ) -> Result<Self, MathError> {
        let spec = Self {
            coefficient_domain,
            x_variables,
            d_variables,
            ordering: None,
        };
        spec.validate()?;
        Ok(spec)
    }

    /// `x1..xn`, `d1..dn`.
    pub fn standard(coefficient_domain: CoefficientDomain, n: usize) -> Result<Self, MathError> {
        Self::new(
            coefficient_domain,
            (1..=n).map(|i| format!("x{i}")).collect(),
            (1..=n).map(|i| format!("d{i}")).collect(),
        )
    }

    pub fn validate(&self) -> Result<(), MathError> {
        if self.x_variables.len() != self.d_variables.len() {
            return Err(MathError::Invalid(
                "a Weyl algebra needs as many d variables as x variables".into(),
            ));
        }
        validate_variables(self.x_variables.iter().chain(&self.d_variables))?;
        // The ordering, when given, is on the 2n variables x_1..x_n, d_1..d_n.
        match &self.ordering {
            Some(order) => order.validate(self.x_variables.len() + self.d_variables.len()),
            None => Ok(()),
        }
    }

    pub fn n(&self) -> usize {
        self.x_variables.len()
    }
}

/// One term `coefficient * x^x_exponents * d^d_exponents` in the JSON form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WeylTermData {
    pub coefficient: Rational,
    pub x_exponents: Vec<u32>,
    pub d_exponents: Vec<u32>,
}

type Monomial = (Vec<u32>, Vec<u32>);

/// An element of a Weyl algebra in normally ordered form (all `x` to the left of all
/// `d`). This type is deliberately unrelated to [`crate::Polynomial`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DifferentialOperator {
    algebra: WeylAlgebraSpec,
    terms: BTreeMap<Monomial, Rational>,
}

impl DifferentialOperator {
    pub fn from_terms(
        algebra: WeylAlgebraSpec,
        terms: impl IntoIterator<Item = (Vec<u32>, Vec<u32>, Rational)>,
    ) -> Result<Self, MathError> {
        algebra.validate()?;
        let n = algebra.n();
        let mut map: BTreeMap<Monomial, Rational> = BTreeMap::new();
        for (x, d, c) in terms {
            if x.len() != n || d.len() != n {
                return Err(MathError::ExponentLength {
                    expected: n,
                    got: x.len().max(d.len()),
                });
            }
            algebra.coefficient_domain.check(&c)?;
            map.entry((x, d)).or_insert_with(Rational::zero).0 += c.0;
        }
        map.retain(|_, c| !c.is_zero());
        Ok(Self {
            algebra,
            terms: map,
        })
    }

    pub fn algebra(&self) -> &WeylAlgebraSpec {
        &self.algebra
    }

    pub fn to_data(&self) -> Vec<WeylTermData> {
        self.terms
            .iter()
            .rev()
            .map(|((x, d), c)| WeylTermData {
                coefficient: c.clone(),
                x_exponents: x.clone(),
                d_exponents: d.clone(),
            })
            .collect()
    }

    pub fn is_zero(&self) -> bool {
        self.terms.is_empty()
    }

    pub fn add(&self, other: &Self) -> Result<Self, MathError> {
        if self.algebra != other.algebra {
            return Err(MathError::RingMismatch);
        }
        let mut terms = self.terms.clone();
        for (m, c) in &other.terms {
            terms.entry(m.clone()).or_insert_with(Rational::zero).0 += &c.0;
        }
        terms.retain(|_, c| !c.is_zero());
        Ok(Self {
            algebra: self.algebra.clone(),
            terms,
        })
    }

    pub fn neg(&self) -> Self {
        Self {
            algebra: self.algebra.clone(),
            terms: self
                .terms
                .iter()
                .map(|(m, c)| (m.clone(), Rational(-c.0.clone())))
                .collect(),
        }
    }

    /// Product in the Weyl algebra. Uses, for each variable,
    /// `d^b x^a = sum_k binom(b,k) a!/(a-k)! x^(a-k) d^(b-k)`.
    ///
    /// Fails with [`MathError::ExponentOverflow`] if an exponent of the product
    /// would exceed `u32::MAX`. The largest exponent of `x_i` in the product is at
    /// most the sum of the largest ones in the operands (the term `k = 0`), and
    /// the same holds for `d_i`; this is checked before any term is computed.
    pub fn mul(&self, other: &Self) -> Result<Self, MathError> {
        self.mul_with(other, &mut || Ok::<(), MathError>(()))
    }

    /// [`Self::mul`], calling `checkpoint` after every few hundred terms it
    /// computes; an error from it ends the product (Issue #49).
    pub fn mul_with<E: From<MathError>>(
        &self,
        other: &Self,
        checkpoint: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<Self, E> {
        if self.algebra != other.algebra {
            return Err(MathError::RingMismatch.into());
        }
        let n = self.algebra.n();
        if !self.is_zero() && !other.is_zero() {
            let (x1, d1) = self.max_exponents();
            let (x2, d2) = other.max_exponents();
            let overflows =
                |a: &[u32], b: &[u32]| a.iter().zip(b).any(|(p, q)| p.checked_add(*q).is_none());
            if overflows(&x1, &x2) || overflows(&d1, &d2) {
                return Err(MathError::ExponentOverflow.into());
            }
        }
        let mut computed = 0usize;
        let mut terms: BTreeMap<Monomial, Rational> = BTreeMap::new();
        for ((x1, d1), c1) in &self.terms {
            for ((x2, d2), c2) in &other.terms {
                // Expand d^{d1} x^{x2} variable by variable; variables commute
                // across different indices.
                let mut partial: Vec<(Vec<u32>, Vec<u32>, BigInt)> =
                    vec![(x1.clone(), vec![0; n], BigInt::from(1))];
                for i in 0..n {
                    let (a, b) = (x2[i], d1[i]);
                    let mut next = Vec::new();
                    for (x, d, coeff) in &partial {
                        for k in 0..=a.min(b) {
                            let factor = binomial(b, k) * falling(a, k);
                            let mut x = x.clone();
                            let mut d = d.clone();
                            x[i] = x[i].checked_add(a - k).ok_or(MathError::ExponentOverflow)?;
                            d[i] = d[i].checked_add(b - k).ok_or(MathError::ExponentOverflow)?;
                            next.push((x, d, coeff * &factor));
                        }
                    }
                    partial = next;
                }
                // One call per CHECK_EVERY terms, as in a product of polynomials.
                let before = computed / crate::polynomial::CHECK_EVERY;
                computed += partial.len();
                if computed / crate::polynomial::CHECK_EVERY > before {
                    checkpoint()?;
                }
                for (x, d, coeff) in partial {
                    let d = d
                        .iter()
                        .zip(d2)
                        .map(|(p, q)| p.checked_add(*q).ok_or(MathError::ExponentOverflow))
                        .collect::<Result<Vec<u32>, _>>()?;
                    let value = &c1.0 * &c2.0 * num_rational::BigRational::from_integer(coeff);
                    terms.entry((x, d)).or_insert_with(Rational::zero).0 += value;
                }
            }
        }
        terms.retain(|_, c| !c.is_zero());
        Ok(Self {
            algebra: self.algebra.clone(),
            terms,
        })
    }

    /// The largest exponent of each `x_i` and of each `d_i` over the terms.
    fn max_exponents(&self) -> (Vec<u32>, Vec<u32>) {
        let n = self.algebra.n();
        let (mut x_max, mut d_max) = (vec![0; n], vec![0; n]);
        for (x, d) in self.terms.keys() {
            for i in 0..n {
                x_max[i] = x_max[i].max(x[i]);
                d_max[i] = d_max[i].max(d[i]);
            }
        }
        (x_max, d_max)
    }
}

fn binomial(n: u32, k: u32) -> BigInt {
    let mut r = BigInt::from(1);
    for i in 0..k {
        r = r * (n - i) / (i + 1);
    }
    r
}

fn falling(a: u32, k: u32) -> BigInt {
    (0..k).fold(BigInt::from(1), |acc, i| acc * (a - i))
}

/// Human-readable form for logs only, e.g. `x1*d1+x2*d2-3`.
impl fmt::Display for DifferentialOperator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.terms.is_empty() {
            return f.write_str("0");
        }
        for (i, ((x, d), c)) in self.terms.iter().rev().enumerate() {
            let mut factors = Vec::new();
            for (e, v) in x.iter().zip(&self.algebra.x_variables) {
                match e {
                    0 => {}
                    1 => factors.push(v.clone()),
                    _ => factors.push(format!("{v}^{e}")),
                }
            }
            for (e, v) in d.iter().zip(&self.algebra.d_variables) {
                match e {
                    0 => {}
                    1 => factors.push(v.clone()),
                    _ => factors.push(format!("{v}^{e}")),
                }
            }
            let negative = c.is_negative();
            let magnitude = Rational(num_traits::Signed::abs(&c.0));
            if i > 0 {
                f.write_str(if negative { "-" } else { "+" })?;
            } else if negative {
                f.write_str("-")?;
            }
            match (factors.is_empty(), magnitude == Rational::one()) {
                (true, _) => write!(f, "{magnitude}")?,
                (false, true) => write!(f, "{}", factors.join("*"))?,
                (false, false) => write!(f, "{magnitude}*{}", factors.join("*"))?,
            }
        }
        Ok(())
    }
}

/// A left ideal of a Weyl algebra given by generators.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DModuleIdeal {
    pub algebra: WeylAlgebraSpec,
    pub left_generators: Vec<DifferentialOperator>,
}

impl DModuleIdeal {
    pub fn new(
        algebra: WeylAlgebraSpec,
        left_generators: Vec<DifferentialOperator>,
    ) -> Result<Self, MathError> {
        if left_generators.iter().any(|g| g.algebra() != &algebra) {
            return Err(MathError::RingMismatch);
        }
        Ok(Self {
            algebra,
            left_generators,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Issue #49: `product` runs on a thread with a checkpoint that fails once
    /// a flag is raised; the flag is raised after 50 ms. Returns whether the
    /// product was stopped, and how long after the flag it ended.
    fn stops_after_raise(
        product: impl FnOnce(&mut dyn FnMut() -> Result<(), Stop>) -> Result<(), Stop> + Send + 'static,
    ) -> (bool, std::time::Duration) {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        let raised = Arc::new(AtomicBool::new(false));
        let seen = raised.clone();
        let worker = std::thread::spawn(move || {
            let mut checkpoint = || {
                if seen.load(Ordering::SeqCst) {
                    Err(Stop::Raised)
                } else {
                    Ok(())
                }
            };
            let outcome = product(&mut checkpoint);
            (outcome, std::time::Instant::now())
        });
        std::thread::sleep(std::time::Duration::from_millis(50));
        let at = std::time::Instant::now();
        raised.store(true, Ordering::SeqCst);
        let (outcome, ended) = worker.join().unwrap();
        (
            matches!(outcome, Err(Stop::Raised)),
            ended.saturating_duration_since(at),
        )
    }

    #[derive(Debug)]
    enum Stop {
        Raised,
        Math(#[allow(dead_code)] MathError),
    }

    impl From<MathError> for Stop {
        fn from(e: MathError) -> Self {
            Self::Math(e)
        }
    }

    /// A product of two operators with 400 terms each stops within 100 ms of
    /// the interruption.
    #[test]
    fn an_interrupted_product_stops_within_100_ms() {
        let algebra = WeylAlgebraSpec::standard(crate::CoefficientDomain::Rationals, 2).unwrap();
        let dense = |shift: u32| {
            DifferentialOperator::from_terms(
                algebra.clone(),
                (0..20u32).flat_map(move |i| {
                    (0..20u32).map(move |j| {
                        (
                            vec![i % 5, j % 4],
                            vec![(i + shift) % 6, (j + shift) % 5],
                            Rational::integer(i64::from(i + j + 1)),
                        )
                    })
                }),
            )
            .unwrap()
        };
        let (a, b) = (dense(0), dense(1));
        let (stopped, after) = stops_after_raise(move |checkpoint| {
            let mut checkpoint = || checkpoint();
            a.mul_with(&b, &mut checkpoint).map(|_| ())
        });
        assert!(stopped, "the product ended before the interruption");
        assert!(after < std::time::Duration::from_millis(100), "{after:?}");
    }

    fn algebra(n: usize) -> WeylAlgebraSpec {
        WeylAlgebraSpec::standard(CoefficientDomain::Rationals, n).unwrap()
    }

    fn op(n: usize, terms: &[(&[u32], &[u32], i64)]) -> DifferentialOperator {
        DifferentialOperator::from_terms(
            algebra(n),
            terms
                .iter()
                .map(|(x, d, c)| (x.to_vec(), d.to_vec(), Rational::integer(*c))),
        )
        .unwrap()
    }

    #[test]
    fn commutator_is_kronecker_delta() {
        let x = |i: usize| {
            let mut e = vec![0; 2];
            e[i] = 1;
            op(2, &[(&e, &[0, 0], 1)])
        };
        let d = |i: usize| {
            let mut e = vec![0; 2];
            e[i] = 1;
            op(2, &[(&[0, 0], &e, 1)])
        };
        for i in 0..2 {
            for j in 0..2 {
                let commutator = d(i)
                    .mul(&x(j))
                    .unwrap()
                    .add(&x(j).mul(&d(i)).unwrap().neg())
                    .unwrap();
                if i == j {
                    assert_eq!(commutator, op(2, &[(&[0, 0], &[0, 0], 1)]));
                } else {
                    assert!(commutator.is_zero());
                }
            }
        }
    }

    #[test]
    fn higher_powers() {
        // d^2 x^2 = x^2 d^2 + 4 x d + 2
        let d2 = op(1, &[(&[0], &[2], 1)]);
        let x2 = op(1, &[(&[2], &[0], 1)]);
        let expected = op(1, &[(&[2], &[2], 1), (&[1], &[1], 4), (&[0], &[0], 2)]);
        assert_eq!(d2.mul(&x2).unwrap(), expected);
        assert_eq!(expected.to_string(), "x1^2*d1^2+4*x1*d1+2");
    }

    #[test]
    fn exponent_overflow_is_an_error() {
        let top_x = op(1, &[(&[u32::MAX], &[0], 1)]);
        let top_d = op(1, &[(&[0], &[u32::MAX], 1)]);
        let x = op(1, &[(&[1], &[0], 1)]);
        let d = op(1, &[(&[0], &[1], 1)]);
        // x^(2^32 - 1) * x and d * d^(2^32 - 1).
        assert_eq!(top_x.mul(&x), Err(MathError::ExponentOverflow));
        assert_eq!(d.mul(&top_d), Err(MathError::ExponentOverflow));
        // d * x^(2^32 - 1) = x^(2^32 - 1) d + (2^32 - 1) x^(2^32 - 2): fits.
        let product = d.mul(&top_x).unwrap();
        assert_eq!(
            product,
            op(
                1,
                &[
                    (&[u32::MAX], &[1], 1),
                    (&[u32::MAX - 1], &[0], i64::from(u32::MAX))
                ]
            )
        );
        // x * d^(2^32 - 1) is already normally ordered: fits.
        assert!(x.mul(&top_d).is_ok());
    }

    #[test]
    fn algebra_mismatch_is_rejected() {
        assert_eq!(
            op(1, &[(&[1], &[0], 1)]).mul(&op(2, &[(&[1, 0], &[0, 0], 1)])),
            Err(MathError::RingMismatch)
        );
    }
}

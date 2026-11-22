use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, HashMap};
use std::fmt;

use num_bigint::BigInt;
use num_traits::Zero;
use serde::{Deserialize, Serialize};

use crate::error::MathError;
use crate::ring::RingSpec;
use crate::scalar::Rational;

/// One term in the JSON form of a polynomial.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TermData {
    pub coefficient: Rational,
    pub exponents: Vec<u32>,
}

/// The JSON form of a polynomial without its ring:
/// `{"type": "polynomial", "terms": [{"coefficient": "1", "exponents": [4, 0]}]}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolynomialData {
    #[serde(rename = "type")]
    pub kind: PolynomialKind,
    pub terms: Vec<TermData>,
}

/// The `"type"` field of [`PolynomialData`], which must be `"polynomial"`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PolynomialKind {
    #[default]
    #[serde(rename = "polynomial")]
    Polynomial,
}

/// How many products of terms a product computes between two calls of its
/// checkpoint (Issue #49): few enough that an interrupted product stops within
/// 100 ms, many enough that the calls cost nothing measurable.
pub(crate) const CHECK_EVERY: usize = 256;

/// A polynomial in a [`RingSpec`], in canonical sparse form: no zero coefficients and
/// every coefficient in the coefficient domain. Structural equality is therefore
/// equality of polynomials in the same ring.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Polynomial {
    ring: RingSpec,
    terms: BTreeMap<Vec<u32>, Rational>,
}

impl Polynomial {
    pub fn zero(ring: RingSpec) -> Self {
        Self {
            ring,
            terms: BTreeMap::new(),
        }
    }

    pub fn constant(ring: RingSpec, value: Rational) -> Result<Self, MathError> {
        let n = ring.nvars();
        Self::from_terms(ring, [(vec![0; n], value)])
    }

    /// Builds a polynomial, rejecting wrong exponent lengths, repeated exponent
    /// vectors and coefficients outside the domain. Zero coefficients are dropped.
    pub fn from_terms(
        ring: RingSpec,
        terms: impl IntoIterator<Item = (Vec<u32>, Rational)>,
    ) -> Result<Self, MathError> {
        ring.validate()?;
        Self::from_terms_in_valid_ring(ring, terms)
    }

    /// [`Self::from_terms`] in a ring the caller has validated: reading many
    /// polynomials of one ring checks the ring once, not once per polynomial.
    pub(crate) fn from_terms_in_valid_ring(
        ring: RingSpec,
        terms: impl IntoIterator<Item = (Vec<u32>, Rational)>,
    ) -> Result<Self, MathError> {
        let mut map = BTreeMap::new();
        for (exponents, coefficient) in terms {
            if exponents.len() != ring.nvars() {
                return Err(MathError::ExponentLength {
                    expected: ring.nvars(),
                    got: exponents.len(),
                });
            }
            ring.domain.check(&coefficient)?;
            match map.entry(exponents) {
                Entry::Occupied(e) => return Err(MathError::DuplicateExponent(e.key().clone())),
                Entry::Vacant(e) => {
                    e.insert(coefficient);
                }
            }
        }
        map.retain(|_, c: &mut Rational| !c.is_zero());
        Ok(Self { ring, terms: map })
    }

    pub fn from_data(ring: RingSpec, data: &PolynomialData) -> Result<Self, MathError> {
        ring.validate()?;
        Self::from_data_in_valid_ring(ring, data)
    }

    /// [`Self::from_data`] in a ring the caller has validated.
    pub(crate) fn from_data_in_valid_ring(
        ring: RingSpec,
        data: &PolynomialData,
    ) -> Result<Self, MathError> {
        Self::from_terms_in_valid_ring(
            ring,
            data.terms
                .iter()
                .map(|t| (t.exponents.clone(), t.coefficient.clone())),
        )
    }

    pub fn to_data(&self) -> PolynomialData {
        PolynomialData {
            kind: PolynomialKind::Polynomial,
            terms: self
                .terms
                .iter()
                .rev()
                .map(|(e, c)| TermData {
                    coefficient: c.clone(),
                    exponents: e.clone(),
                })
                .collect(),
        }
    }

    pub fn ring(&self) -> &RingSpec {
        &self.ring
    }

    /// Terms in ascending lexicographic order of exponent vectors.
    pub fn terms(&self) -> impl DoubleEndedIterator<Item = (&[u32], &Rational)> {
        self.terms.iter().map(|(e, c)| (e.as_slice(), c))
    }

    pub fn is_zero(&self) -> bool {
        self.terms.is_empty()
    }

    pub fn is_constant(&self) -> bool {
        self.terms.keys().all(|e| e.iter().all(|&d| d == 0))
    }

    /// Re-interprets the polynomial over a domain containing the current one
    /// (for example ZZ -> QQ). Fails if some coefficient is not in the new domain.
    pub fn with_ring(&self, ring: RingSpec) -> Result<Self, MathError> {
        if ring.variables != self.ring.variables {
            return Err(MathError::RingMismatch);
        }
        Self::from_terms(ring, self.terms.clone())
    }

    fn same_ring(&self, other: &Self) -> Result<(), MathError> {
        if self.ring == other.ring {
            Ok(())
        } else {
            Err(MathError::RingMismatch)
        }
    }

    pub fn add(&self, other: &Self) -> Result<Self, MathError> {
        self.same_ring(other)?;
        let mut terms = self.terms.clone();
        for (e, c) in &other.terms {
            let entry = terms.entry(e.clone()).or_insert_with(Rational::zero);
            entry.0 += &c.0;
        }
        terms.retain(|_, c| !c.is_zero());
        Ok(Self {
            ring: self.ring.clone(),
            terms,
        })
    }

    /// The product. Fails with [`MathError::ExponentOverflow`] if an exponent of
    /// the product would exceed `u32::MAX`; checked before any term is computed.
    pub fn mul(&self, other: &Self) -> Result<Self, MathError> {
        self.mul_with(other, &mut || Ok::<(), MathError>(()))
    }

    /// [`Self::mul`], calling `checkpoint` after every [`CHECK_EVERY`]
    /// products of terms; an error from it ends the product (Issue #49). A
    /// caller under a deadline uses it to stop one long product.
    pub fn mul_with<E: From<MathError>>(
        &self,
        other: &Self,
        checkpoint: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<Self, E> {
        self.same_ring(other)?;
        // The largest exponent of each variable in the product is the sum of the
        // largest ones in the operands.
        if !self.is_zero()
            && !other.is_zero()
            && add_exponents(&self.max_exponents(), &other.max_exponents()).is_err()
        {
            return Err(MathError::ExponentOverflow.into());
        }
        let mut count = 0usize;
        let mut tick = || {
            count += 1;
            if count.is_multiple_of(CHECK_EVERY) {
                checkpoint()
            } else {
                Ok(())
            }
        };
        let integral = |p: &Self| p.terms.values().all(Rational::is_integer);
        let terms = if integral(self) && integral(other) {
            // Integer arithmetic avoids a gcd per operation.
            let mut acc: HashMap<Vec<u32>, BigInt> = HashMap::new();
            for (e1, c1) in &self.terms {
                for (e2, c2) in &other.terms {
                    tick()?;
                    *acc.entry(add_exponents(e1, e2)?).or_default() += c1.numer() * c2.numer();
                }
            }
            acc.into_iter()
                .filter(|(_, c)| !c.is_zero())
                .map(|(e, c)| (e, Rational::integer(c)))
                .collect()
        } else {
            let mut acc: HashMap<Vec<u32>, Rational> = HashMap::new();
            for (e1, c1) in &self.terms {
                for (e2, c2) in &other.terms {
                    tick()?;
                    acc.entry(add_exponents(e1, e2)?)
                        .or_insert_with(Rational::zero)
                        .0 += &c1.0 * &c2.0;
                }
            }
            acc.into_iter().filter(|(_, c)| !c.is_zero()).collect()
        };
        Ok(Self {
            ring: self.ring.clone(),
            terms,
        })
    }

    /// `self^exponent` by repeated squaring. Fails with
    /// [`MathError::ExponentOverflow`] if an exponent of the power would exceed
    /// `u32::MAX`; never returns a wrong polynomial instead.
    pub fn pow(&self, exponent: u32) -> Result<Self, MathError> {
        self.pow_with(exponent, &mut || Ok::<(), MathError>(()))
    }

    /// [`Self::pow`], passing `checkpoint` to each product (Issue #49).
    pub fn pow_with<E: From<MathError>>(
        &self,
        mut exponent: u32,
        checkpoint: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<Self, E> {
        let n = self.ring.nvars();
        let mut result = Self {
            ring: self.ring.clone(),
            terms: BTreeMap::from([(vec![0; n], Rational::one())]),
        };
        // The largest exponent of each variable in the power is `exponent` times
        // the largest one in `self`.
        if exponent > 0
            && !self.is_zero()
            && self
                .max_exponents()
                .iter()
                .any(|e| e.checked_mul(exponent).is_none())
        {
            return Err(MathError::ExponentOverflow.into());
        }
        let mut base = self.clone();
        // Every intermediate power divides the result, so its exponents fit too.
        while exponent > 0 {
            if exponent & 1 == 1 {
                result = result.mul_with(&base, checkpoint)?;
            }
            exponent >>= 1;
            if exponent > 0 {
                base = base.mul_with(&base, checkpoint)?;
            }
        }
        Ok(result)
    }

    /// The largest exponent of each variable over the terms (zeros for the zero
    /// polynomial).
    fn max_exponents(&self) -> Vec<u32> {
        let mut max = vec![0; self.ring.nvars()];
        for e in self.terms.keys() {
            for (m, &d) in max.iter_mut().zip(e) {
                *m = (*m).max(d);
            }
        }
        max
    }

    pub fn scale(&self, factor: &Rational) -> Result<Self, MathError> {
        let terms = self
            .terms
            .iter()
            .map(|(e, c)| (e.clone(), Rational(&c.0 * &factor.0)));
        Self::from_terms(self.ring.clone(), terms)
    }

    /// `-self`.
    pub fn neg(&self) -> Self {
        Self {
            ring: self.ring.clone(),
            terms: self
                .terms
                .iter()
                .map(|(e, c)| (e.clone(), Rational(-&c.0)))
                .collect(),
        }
    }

    /// `self - other`.
    pub fn sub(&self, other: &Self) -> Result<Self, MathError> {
        self.same_ring(other)?;
        let mut terms = self.terms.clone();
        for (e, c) in &other.terms {
            let entry = terms.entry(e.clone()).or_insert_with(Rational::zero);
            entry.0 -= &c.0;
        }
        terms.retain(|_, c| !c.is_zero());
        Ok(Self {
            ring: self.ring.clone(),
            terms,
        })
    }

    /// The largest total degree of a term, or none for the zero polynomial.
    /// The exponents are added in `u64`, which cannot overflow for fewer than
    /// 2^32 variables.
    pub fn total_degree(&self) -> Option<u64> {
        self.terms
            .keys()
            .map(|e| e.iter().map(|&d| u64::from(d)).sum())
            .max()
    }

    /// The exponents and the coefficient of the largest term in the monomial
    /// order of the ring, or none for the zero polynomial. To compare in
    /// another order, change the ring with [`Self::with_ring`].
    pub fn leading_term(&self) -> Option<(&[u32], &Rational)> {
        let order = &self.ring.order;
        self.terms
            .iter()
            .max_by(|(a, _), (b, _)| order.compare(a, b))
            .map(|(e, c)| (e.as_slice(), c))
    }

    /// The exponents of [`Self::leading_term`].
    pub fn leading_monomial(&self) -> Option<&[u32]> {
        self.leading_term().map(|(e, _)| e)
    }

    /// The coefficient of [`Self::leading_term`].
    pub fn leading_coefficient(&self) -> Option<&Rational> {
        self.leading_term().map(|(_, c)| c)
    }
}

/// The exponent vector of the product of two monomials.
fn add_exponents(a: &[u32], b: &[u32]) -> Result<Vec<u32>, MathError> {
    a.iter()
        .zip(b)
        .map(|(p, q)| p.checked_add(*q).ok_or(MathError::ExponentOverflow))
        .collect()
}

/// Human-readable form for logs and display only; never used for comparisons.
impl fmt::Display for Polynomial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.terms.is_empty() {
            return f.write_str("0");
        }
        for (i, (exponents, coefficient)) in self.terms.iter().rev().enumerate() {
            let monomial: Vec<String> = exponents
                .iter()
                .zip(&self.ring.variables)
                .filter(|(d, _)| **d > 0)
                .map(|(d, v)| {
                    if *d == 1 {
                        v.clone()
                    } else {
                        format!("{v}^{d}")
                    }
                })
                .collect();
            let negative = coefficient.is_negative();
            let magnitude = Rational(num_traits::Signed::abs(&coefficient.0));
            if i > 0 {
                f.write_str(if negative { "-" } else { "+" })?;
            } else if negative {
                f.write_str("-")?;
            }
            match (monomial.is_empty(), magnitude == Rational::one()) {
                (true, _) => write!(f, "{magnitude}")?,
                (false, true) => write!(f, "{}", monomial.join("*"))?,
                (false, false) => write!(f, "{magnitude}*{}", monomial.join("*"))?,
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ideal {
    pub ring: RingSpec,
    pub generators: Vec<Polynomial>,
}

impl Ideal {
    pub fn new(ring: RingSpec, generators: Vec<Polynomial>) -> Result<Self, MathError> {
        if generators.iter().any(|g| g.ring() != &ring) {
            return Err(MathError::RingMismatch);
        }
        Ok(Self { ring, generators })
    }

    /// Reads an ideal, refusing an invalid ring and generators that do not
    /// belong to it. The order of the generators is kept, zero generators
    /// included: a transformation matrix refers to them by position.
    pub fn from_data(data: &IdealData) -> Result<Self, MathError> {
        data.ring.validate()?;
        let generators = data
            .generators
            .iter()
            .map(|g| Polynomial::from_data_in_valid_ring(data.ring.clone(), g))
            .collect::<Result<_, _>>()?;
        Self::new(data.ring.clone(), generators)
    }

    pub fn to_data(&self) -> IdealData {
        IdealData {
            kind: IdealKind::Ideal,
            ring: self.ring.clone(),
            generators: self.generators.iter().map(Polynomial::to_data).collect(),
        }
    }
}

/// The JSON form of an ideal given by generators:
/// `{"type": "ideal", "ring": {...}, "generators": [{"type": "polynomial", ...}]}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdealData {
    #[serde(rename = "type")]
    pub kind: IdealKind,
    pub ring: RingSpec,
    pub generators: Vec<PolynomialData>,
}

/// The `"type"` field of [`IdealData`], which must be `"ideal"`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum IdealKind {
    #[default]
    #[serde(rename = "ideal")]
    Ideal,
}

/// One irreducible factor with its multiplicity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Factor {
    pub factor: PolynomialData,
    pub multiplicity: u32,
    /// Display form for humans; not part of the mathematical value.
    pub display: String,
}

/// `constant * prod(factor_i ^ multiplicity_i)`, the result of `polynomial.factor`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename = "factorization")]
pub struct Factorization {
    pub ring: RingSpec,
    pub constant: Rational,
    pub factors: Vec<Factor>,
}

/// Reads the form that [`Factorization`] writes, and refuses an invalid ring,
/// a constant outside the coefficient domain and a factor that does not belong
/// to the ring. The display strings are kept as given. A multiplicity of 0 is
/// read, as [`Factorization::new`] accepts it; the check of the result refuses
/// it.
impl<'de> Deserialize<'de> for Factorization {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        // `tag` with `deny_unknown_fields` refuses the tag itself, so the tag
        // is read as a field.
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Repr {
            #[serde(rename = "type")]
            _kind: FactorizationKind,
            ring: RingSpec,
            constant: Rational,
            factors: Vec<Factor>,
        }
        #[derive(Deserialize)]
        enum FactorizationKind {
            #[serde(rename = "factorization")]
            Factorization,
        }
        let repr = Repr::deserialize(deserializer)?;
        repr.ring.validate().map_err(D::Error::custom)?;
        repr.ring
            .domain
            .check(&repr.constant)
            .map_err(D::Error::custom)?;
        for f in &repr.factors {
            if !f.factor.terms.is_empty() {
                Polynomial::from_data_in_valid_ring(repr.ring.clone(), &f.factor)
                    .map_err(D::Error::custom)?;
            }
        }
        Ok(Self {
            ring: repr.ring,
            constant: repr.constant,
            factors: repr.factors,
        })
    }
}

impl Factorization {
    pub fn new(
        ring: RingSpec,
        constant: Rational,
        factors: Vec<(Polynomial, u32)>,
    ) -> Result<Self, MathError> {
        ring.domain.check(&constant)?;
        let factors = factors
            .into_iter()
            .map(|(p, m)| {
                if p.ring() != &ring {
                    return Err(MathError::RingMismatch);
                }
                Ok(Factor {
                    display: p.to_string(),
                    factor: p.to_data(),
                    multiplicity: m,
                })
            })
            .collect::<Result<_, _>>()?;
        Ok(Self {
            ring,
            constant,
            factors,
        })
    }

    /// Multiplies the factorization out.
    pub fn expand(&self) -> Result<Polynomial, MathError> {
        self.expand_with(|| Ok::<(), MathError>(()))
    }

    /// Multiplies the factorization out, calling `checkpoint` before each factor
    /// and inside each product (Issue #49); an error from it ends the
    /// expansion. A caller under a deadline uses it to stop a long expansion
    /// early.
    pub fn expand_with<E: From<MathError>>(
        &self,
        mut checkpoint: impl FnMut() -> Result<(), E>,
    ) -> Result<Polynomial, E> {
        let mut product = Polynomial::constant(self.ring.clone(), self.constant.clone())?;
        for f in &self.factors {
            checkpoint()?;
            let p = Polynomial::from_data(self.ring.clone(), &f.factor)?;
            let power = p.pow_with(f.multiplicity, &mut checkpoint)?;
            checkpoint()?;
            product = product.mul_with(&power, &mut checkpoint)?;
        }
        Ok(product)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ring::{CoefficientDomain, MonomialOrder};

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

    /// A product of two polynomials of 2025 terms each, about 4 million
    /// products of terms, stops within 100 ms of the interruption.
    #[test]
    fn an_interrupted_product_stops_within_100_ms() {
        let dense = || {
            Polynomial::from_terms(
                ring(CoefficientDomain::Integers),
                (0..45u32).flat_map(|i| {
                    (0..45u32)
                        .map(move |j| (vec![i, j], Rational::integer(i64::from(i * 7 + j + 1))))
                }),
            )
            .unwrap()
        };
        let (a, b) = (dense(), dense());
        let (stopped, after) = stops_after_raise(move |checkpoint| {
            let mut checkpoint = || checkpoint();
            a.mul_with(&b, &mut checkpoint).map(|_| ())
        });
        assert!(stopped, "the product ended before the interruption");
        assert!(after < std::time::Duration::from_millis(100), "{after:?}");
    }

    #[test]
    fn a_product_without_interruption_is_the_product() {
        let p = poly(CoefficientDomain::Integers, &[("1", [1, 0]), ("1", [0, 1])]);
        let mut calls = 0;
        let mut count = || {
            calls += 1;
            Ok::<(), MathError>(())
        };
        let cube = p.pow_with(3, &mut count).unwrap();
        assert_eq!(cube, p.pow(3).unwrap());
        assert_eq!(p.mul_with(&p, &mut count).unwrap(), p.mul(&p).unwrap());
    }

    fn ring(domain: CoefficientDomain) -> RingSpec {
        RingSpec::new(domain, vec!["x".into(), "y".into()], MonomialOrder::Grevlex).unwrap()
    }

    fn poly(domain: CoefficientDomain, terms: &[(&str, [u32; 2])]) -> Polynomial {
        Polynomial::from_terms(
            ring(domain),
            terms
                .iter()
                .map(|(c, e)| (e.to_vec(), c.parse::<Rational>().unwrap())),
        )
        .unwrap()
    }

    #[test]
    fn design_example_parses() {
        let data: PolynomialData = serde_json::from_str(
            r#"{"type": "polynomial", "terms": [
                {"coefficient": "1", "exponents": [4, 0]},
                {"coefficient": "-1", "exponents": [0, 4]}]}"#,
        )
        .unwrap();
        let p = Polynomial::from_data(ring(CoefficientDomain::Rationals), &data).unwrap();
        assert_eq!(p.to_string(), "x^4-y^4");
    }

    #[test]
    fn x4_minus_y4_factors_multiply_back() {
        use CoefficientDomain::Integers as Z;
        let f = poly(Z, &[("1", [4, 0]), ("-1", [0, 4])]);
        let a = poly(Z, &[("1", [1, 0]), ("-1", [0, 1])]);
        let b = poly(Z, &[("1", [1, 0]), ("1", [0, 1])]);
        let c = poly(Z, &[("1", [2, 0]), ("1", [0, 2])]);
        let fact =
            Factorization::new(ring(Z), Rational::one(), vec![(a, 1), (b, 1), (c, 1)]).unwrap();
        assert_eq!(fact.expand().unwrap(), f);
        assert_eq!(fact.factors[2].display, "x^2+y^2");
    }

    #[test]
    fn pow_matches_repeated_multiplication() {
        use CoefficientDomain::Rationals as Q;
        let p = poly(Q, &[("1/2", [1, 0]), ("-3", [0, 1]), ("1", [0, 0])]);
        let mut expected = Polynomial::constant(ring(Q), Rational::one()).unwrap();
        for k in 0..=7 {
            assert_eq!(p.pow(k).unwrap(), expected, "k = {k}");
            expected = expected.mul(&p).unwrap();
        }
    }

    #[test]
    fn exponent_overflow_is_an_error() {
        use CoefficientDomain::{Integers as Z, Rationals as Q};
        for (domain, c) in [(Z, "2"), (Q, "1/2")] {
            let top = poly(domain, &[(c, [u32::MAX, 0])]);
            let x = poly(domain, &[("1", [1, 0])]);
            let y = poly(domain, &[("1", [0, 1])]);
            // x^(2^32 - 1) * x has degree 2^32, which does not fit.
            assert_eq!(top.mul(&x), Err(MathError::ExponentOverflow));
            assert_eq!(x.mul(&top), Err(MathError::ExponentOverflow));
            // The other variable is unaffected, and so is the zero polynomial.
            assert!(top.mul(&y).is_ok());
            assert!(top.mul(&Polynomial::zero(ring(domain))).unwrap().is_zero());
            // Powers.
            assert_eq!(top.pow(2), Err(MathError::ExponentOverflow));
            assert_eq!(top.pow(1).unwrap(), top);
            assert!(top.pow(0).unwrap().is_constant());
            let half = poly(domain, &[("1", [1 << 31, 0])]);
            assert_eq!(half.pow(2), Err(MathError::ExponentOverflow));
            let (e, _) = x
                .pow(u32::MAX)
                .unwrap()
                .terms()
                .next()
                .map(|(e, c)| (e.to_vec(), c.clone()))
                .unwrap();
            assert_eq!(e, [u32::MAX, 0]);
        }
        // A factorization whose product does not fit is refused, not expanded to a
        // wrong polynomial.
        let x = poly(Z, &[("1", [1, 0])]);
        let fact = Factorization::new(
            ring(Z),
            Rational::one(),
            vec![(x.clone(), u32::MAX), (x, 1)],
        )
        .unwrap();
        assert_eq!(fact.expand(), Err(MathError::ExponentOverflow));
    }

    #[test]
    fn validation() {
        let r = ring(CoefficientDomain::Integers);
        assert_eq!(
            Polynomial::from_terms(r.clone(), [(vec![1], Rational::one())]),
            Err(MathError::ExponentLength {
                expected: 2,
                got: 1
            })
        );
        assert!(matches!(
            Polynomial::from_terms(r.clone(), [(vec![1, 0], "1/2".parse().unwrap())]),
            Err(MathError::CoefficientNotInDomain { .. })
        ));
        assert!(matches!(
            Polynomial::from_terms(
                r.clone(),
                [(vec![1, 0], Rational::one()), (vec![1, 0], Rational::one())]
            ),
            Err(MathError::DuplicateExponent(_))
        ));
        let zero = Polynomial::from_terms(r, [(vec![1, 0], Rational::zero())]).unwrap();
        assert!(zero.is_zero());
        assert_eq!(zero.to_string(), "0");
    }

    #[test]
    fn rings_must_match() {
        let a = poly(CoefficientDomain::Integers, &[("1", [1, 0])]);
        let b = poly(CoefficientDomain::Rationals, &[("1", [1, 0])]);
        assert_eq!(a.add(&b), Err(MathError::RingMismatch));
        assert_eq!(a.with_ring(ring(CoefficientDomain::Rationals)).unwrap(), b);
    }

    #[test]
    fn display_signs_and_fractions() {
        let p = poly(
            CoefficientDomain::Rationals,
            &[("-1/2", [1, 0]), ("3", [0, 0]), ("-1", [0, 1])],
        );
        assert_eq!(p.to_string(), "-1/2*x-y+3");
    }
}

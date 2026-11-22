//! Matrices over a polynomial ring, sparse by columns (A3, Issue #65).
//!
//! The columns are the generators: a transformation matrix `C` with
//! `G = F C` has a column for each element of `G`, holding its coefficients
//! with respect to `F`. The checks of a result multiply such matrices and
//! compare them; no division or reduction is done here (ADR 0019).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use num_bigint::BigInt;
use num_integer::Integer;
use num_rational::BigRational;
use num_traits::{One, Zero};
use serde::{Deserialize, Serialize};

use crate::error::MathError;
use crate::polynomial::{CHECK_EVERY, Polynomial, PolynomialData};
use crate::ring::RingSpec;
use crate::scalar::Rational;

/// The JSON form of a polynomial matrix without its ring, as
/// [`PolynomialData`]: `{"type": "polynomial_matrix", "rows": 2, "columns":
/// [[{"row": 0, "entry": {"type": "polynomial", ...}}], []]}`. Each column
/// lists entries by row. [`PolynomialMatrix::from_data`] accepts zero entries
/// and drops them; [`PolynomialMatrix::to_data`] writes the nonzero entries
/// only, in increasing rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolynomialMatrixData {
    #[serde(rename = "type")]
    pub kind: PolynomialMatrixKind,
    pub rows: usize,
    pub columns: Vec<Vec<EntryData>>,
}

/// The `"type"` field of [`PolynomialMatrixData`], which must be
/// `"polynomial_matrix"`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PolynomialMatrixKind {
    #[default]
    #[serde(rename = "polynomial_matrix")]
    PolynomialMatrix,
}

/// One entry of a column of [`PolynomialMatrixData`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntryData {
    pub row: usize,
    pub entry: PolynomialData,
}

/// An `nrows x ncols` matrix over the polynomial ring of a [`RingSpec`],
/// holding the nonzero entries of each column by row. Structural equality is
/// equality of matrices over the same ring.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolynomialMatrix {
    ring: RingSpec,
    nrows: usize,
    columns: Vec<BTreeMap<usize, Polynomial>>,
}

impl PolynomialMatrix {
    /// The `nrows x ncols` zero matrix.
    pub fn zero(ring: RingSpec, nrows: usize, ncols: usize) -> Self {
        Self {
            ring,
            nrows,
            columns: vec![BTreeMap::new(); ncols],
        }
    }

    /// Reads a matrix over `ring`, refusing an invalid ring, a row out of
    /// range, two entries at one place and entries that do not belong to the
    /// ring. Zero entries are dropped. Whether the shape fits is for the
    /// caller to check. The ring is checked once, so the time is linear in
    /// the size of `data` and of the ring.
    pub fn from_data(ring: RingSpec, data: &PolynomialMatrixData) -> Result<Self, MathError> {
        ring.validate()?;
        let mut columns = Vec::with_capacity(data.columns.len());
        for (j, column) in data.columns.iter().enumerate() {
            let mut entries = BTreeMap::new();
            let mut seen = BTreeSet::new();
            for e in column {
                if e.row >= data.rows {
                    return Err(MathError::Invalid(format!(
                        "an entry in row {} of column {j}, of a matrix of {} rows",
                        e.row, data.rows
                    )));
                }
                if !seen.insert(e.row) {
                    return Err(MathError::Invalid(format!(
                        "two entries at row {} of column {j}",
                        e.row
                    )));
                }
                if e.entry.terms.is_empty() {
                    continue;
                }
                let p = Polynomial::from_data_in_valid_ring(ring.clone(), &e.entry)?;
                if !p.is_zero() {
                    entries.insert(e.row, p);
                }
            }
            columns.push(entries);
        }
        Ok(Self {
            ring,
            nrows: data.rows,
            columns,
        })
    }

    pub fn to_data(&self) -> PolynomialMatrixData {
        PolynomialMatrixData {
            kind: PolynomialMatrixKind::PolynomialMatrix,
            rows: self.nrows,
            columns: self
                .columns
                .iter()
                .map(|c| {
                    c.iter()
                        .map(|(&row, p)| EntryData {
                            row,
                            entry: p.to_data(),
                        })
                        .collect()
                })
                .collect(),
        }
    }

    /// The `1 x n` matrix whose entries are `polynomials`.
    pub fn from_row(ring: RingSpec, polynomials: &[Polynomial]) -> Result<Self, MathError> {
        let columns = polynomials
            .iter()
            .map(|p| Self::entries(&ring, [(0, p)]))
            .collect::<Result<_, _>>()?;
        Ok(Self {
            ring,
            nrows: 1,
            columns,
        })
    }

    /// The `n x 1` matrix whose entries are `polynomials`.
    pub fn from_column(ring: RingSpec, polynomials: &[Polynomial]) -> Result<Self, MathError> {
        let column = Self::entries(&ring, polynomials.iter().enumerate())?;
        Ok(Self {
            ring,
            nrows: polynomials.len(),
            columns: vec![column],
        })
    }

    fn entries<'a>(
        ring: &RingSpec,
        entries: impl IntoIterator<Item = (usize, &'a Polynomial)>,
    ) -> Result<BTreeMap<usize, Polynomial>, MathError> {
        let mut column = BTreeMap::new();
        for (row, p) in entries {
            if p.ring() != ring {
                return Err(MathError::RingMismatch);
            }
            if !p.is_zero() {
                column.insert(row, p.clone());
            }
        }
        Ok(column)
    }

    pub fn ring(&self) -> &RingSpec {
        &self.ring
    }

    pub fn nrows(&self) -> usize {
        self.nrows
    }

    pub fn ncols(&self) -> usize {
        self.columns.len()
    }

    /// The entry at row `i` and column `j`, or none when it is zero or out of
    /// range.
    pub fn get(&self, i: usize, j: usize) -> Option<&Polynomial> {
        self.columns.get(j).and_then(|c| c.get(&i))
    }

    /// The nonzero entries of column `j` by row, or none when `j` is out of
    /// range.
    pub fn column(&self, j: usize) -> Option<impl Iterator<Item = (usize, &Polynomial)>> {
        self.columns.get(j).map(|c| c.iter().map(|(&i, p)| (i, p)))
    }

    pub fn is_zero(&self) -> bool {
        self.columns.iter().all(BTreeMap::is_empty)
    }

    /// The product `self * other`.
    pub fn mul(&self, other: &Self) -> Result<Self, MathError> {
        self.mul_with(other, &mut || Ok::<(), MathError>(()))
    }

    /// [`Self::mul`], calling `checkpoint` before each product of two entries
    /// and after every [`CHECK_EVERY`] products of terms inside it (Issue
    /// #49), twice for each term while it clears denominators and after every
    /// [`CHECK_EVERY`] terms of the product while it reduces them; an error
    /// from it ends the product. Fails when the rings differ,
    /// when the number of columns of `self` is not the number of rows of
    /// `other`, and with [`MathError::ExponentOverflow`] when an exponent
    /// would exceed `u32::MAX`.
    ///
    /// Each entry of the product is summed in one map of its terms, with no
    /// gcd per product of terms (Issue #66). When the denominators of a
    /// column of `other`, and of the columns of `self` it takes, have a
    /// common denominator not much longer than the longest of them, and no
    /// term scaled by it grows longer than twice its coefficient and 64
    /// bits, as the results of a CAS have, they are cleared: the entries of column `k` of
    /// `self` are scaled by the lcm `d_k` of their denominators, those of
    /// column `j` of `other` divided by `d_k` and scaled by the lcm `L_j`
    /// that gives, the products are added in integers and divided by `L_j`
    /// once at the end. Otherwise (unrelated denominators, whose lcm would
    /// be long, or one long denominator among short ones) each sum keeps a fraction that it reduces only when its
    /// denominator grows long. Either way each term of the product takes
    /// one gcd at the end.
    pub fn mul_with<E: From<MathError>>(
        &self,
        other: &Self,
        checkpoint: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<Self, E> {
        if self.ring != other.ring {
            return Err(MathError::RingMismatch.into());
        }
        if self.ncols() != other.nrows {
            return Err(MathError::Invalid(format!(
                "a {} x {} matrix times a {} x {} matrix",
                self.nrows,
                self.ncols(),
                other.nrows,
                other.ncols()
            ))
            .into());
        }
        let scaled = self
            .columns
            .iter()
            .map(|c| IntegralColumn::new(c, checkpoint))
            .collect::<Result<Vec<_>, E>>()?;
        let mut columns = Vec::with_capacity(other.ncols());
        let mut exponents = vec![0u32; self.ring.nvars()];
        for b_column in &other.columns {
            let column = match integral_quotients(&scaled, b_column, checkpoint)? {
                Some((lcm, quotients)) => {
                    let mut sums: BTreeMap<usize, HashMap<Vec<u32>, BigInt>> = BTreeMap::new();
                    for (k, b) in &quotients {
                        let b: Vec<(&[u32], &BigInt)> = b.iter().map(|(e, c)| (*e, c)).collect();
                        let Some(a) = &scaled[*k] else {
                            unreachable!("only columns with a common denominator are scaled")
                        };
                        for (i, a) in &a.entries {
                            checkpoint()?;
                            let a: Vec<(&[u32], &BigInt)> =
                                a.iter().map(|(e, c)| (e.as_slice(), c)).collect();
                            let sum = sums.entry(*i).or_default();
                            add_products(&a, &b, sum, &mut exponents, checkpoint)?;
                        }
                    }
                    self.summed_entries(
                        sums,
                        |c| Rational(BigRational::new(c, lcm.clone())),
                        checkpoint,
                    )?
                }
                None => {
                    let mut sums: BTreeMap<usize, HashMap<Vec<u32>, Fraction>> = BTreeMap::new();
                    for (&k, b) in b_column {
                        let b: Vec<(&[u32], &Rational)> = b.terms().collect();
                        for (&i, a) in &self.columns[k] {
                            checkpoint()?;
                            let a: Vec<(&[u32], &Rational)> = a.terms().collect();
                            let sum = sums.entry(i).or_default();
                            add_products(&a, &b, sum, &mut exponents, checkpoint)?;
                        }
                    }
                    self.summed_entries(
                        sums,
                        |f| Rational(BigRational::new(f.numer, f.denom)),
                        checkpoint,
                    )?
                }
            };
            columns.push(column);
        }
        Ok(Self {
            ring: self.ring.clone(),
            nrows: self.nrows,
            columns,
        })
    }

    /// The nonzero entries of a column of a product, from the sums of their
    /// terms, calling `checkpoint` after every [`CHECK_EVERY`] terms: each
    /// takes a gcd in `value`.
    fn summed_entries<S: Sum, E: From<MathError>>(
        &self,
        sums: BTreeMap<usize, HashMap<Vec<u32>, S>>,
        value: impl Fn(S) -> Rational,
        checkpoint: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<BTreeMap<usize, Polynomial>, E> {
        let mut column = BTreeMap::new();
        let mut count = 0usize;
        for (i, sum) in sums {
            let mut terms = Vec::with_capacity(sum.len());
            for (e, s) in sum {
                if s.is_zero() {
                    continue;
                }
                count += 1;
                if count.is_multiple_of(CHECK_EVERY) {
                    checkpoint()?;
                }
                terms.push((e, value(s)));
            }
            if !terms.is_empty() {
                column.insert(i, Polynomial::from_terms(self.ring.clone(), terms)?);
            }
        }
        Ok(column)
    }
}

/// Whether denominators whose lcm is `lcm`, the longest of them of
/// `longest` bits, are worth clearing: the lcm is at most 64 bits longer than
/// the longest one. The denominators of a CAS result share their factors and
/// pass; unrelated ones, whose lcm grows with their number, do not.
fn worth_clearing(lcm: &BigInt, longest: u64) -> bool {
    lcm.bits() <= longest.saturating_add(64)
}

/// Whether `n / d`, scaled by `lcm` to the integer `n (lcm / d)`, is at most
/// twice as long as the coefficient `original` it comes from, and 64 bits
/// more. One long denominator would otherwise make every other term of its
/// column as long, and each product with them (Issue #66, review).
fn short_once_scaled(n: &BigInt, d: &BigInt, lcm: &BigInt, original: &BigRational) -> bool {
    let scaled = n.bits() + (lcm.bits() - d.bits());
    let length = original.numer().bits() + original.denom().bits();
    scaled <= length.saturating_mul(2).saturating_add(64)
}

/// The lcm of `lcm` and `denominator`, with no gcd when `denominator`
/// divides `lcm` already: a gcd with a long `lcm` takes a step for each of
/// its bits (Issue #91), a division by a short `denominator` a step for each
/// of its words.
fn lcm_with(lcm: BigInt, denominator: &BigInt) -> BigInt {
    if lcm.is_multiple_of(denominator) {
        lcm
    } else {
        lcm.lcm(denominator)
    }
}

/// The terms of a polynomial with integer coefficients.
type IntegerTerms = Vec<(Vec<u32>, BigInt)>;

/// A column of a matrix times the lcm of the denominators of its entries:
/// the entries by row, each as its terms with integer coefficients.
struct IntegralColumn {
    denominator: BigInt,
    entries: Vec<(usize, IntegerTerms)>,
}

impl IntegralColumn {
    /// The column scaled, or none when its denominators are not worth
    /// clearing or one of its terms would grow much longer, calling
    /// `checkpoint` for each term while it takes the lcm and while it
    /// scales.
    fn new<E>(
        column: &BTreeMap<usize, Polynomial>,
        checkpoint: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<Option<Self>, E> {
        let mut denominator = BigInt::one();
        let mut longest = 0;
        for p in column.values() {
            for (_, c) in p.terms() {
                checkpoint()?;
                denominator = lcm_with(denominator, c.denom());
                longest = longest.max(c.denom().bits());
                if !worth_clearing(&denominator, longest) {
                    return Ok(None);
                }
            }
        }
        let all_short = column
            .values()
            .flat_map(Polynomial::terms)
            .all(|(_, c)| short_once_scaled(c.numer(), c.denom(), &denominator, &c.0));
        if !all_short {
            return Ok(None);
        }
        let mut entries = Vec::with_capacity(column.len());
        for (&i, p) in column {
            let mut terms = Vec::new();
            for (e, c) in p.terms() {
                checkpoint()?;
                terms.push((e.to_vec(), c.numer() * (&denominator / c.denom())));
            }
            entries.push((i, terms));
        }
        Ok(Some(Self {
            denominator,
            entries,
        }))
    }
}

/// The entries `b_kj` of a column of the right factor, each divided by the
/// denominator `d_k` of column `k` of the left factor, as terms scaled to
/// integers by the lcm `L` of their denominators; with `L`. None when a
/// column of the left factor that they take, or these denominators, are not
/// worth clearing, or when a term would grow much longer than `b_kj`'s.
type IntegralQuotients<'a> = (BigInt, Vec<(usize, Vec<(&'a [u32], BigInt)>)>);

/// Calls `checkpoint` for each term while it divides (a gcd with `d_k`) and
/// while it scales.
fn integral_quotients<'a, E>(
    scaled: &[Option<IntegralColumn>],
    b_column: &'a BTreeMap<usize, Polynomial>,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<IntegralQuotients<'a>>, E> {
    let mut lcm = BigInt::one();
    let mut longest = 0;
    let mut quotients = Vec::with_capacity(b_column.len());
    for (&k, b) in b_column {
        let Some(a) = &scaled[k] else {
            return Ok(None);
        };
        let d = BigRational::from_integer(a.denominator.clone());
        let mut terms: Vec<(&[u32], &Rational, BigRational)> = Vec::new();
        for (e, c) in b.terms() {
            checkpoint()?;
            let q = &c.0 / &d;
            lcm = lcm_with(lcm, q.denom());
            longest = longest.max(q.denom().bits());
            if !worth_clearing(&lcm, longest) {
                return Ok(None);
            }
            terms.push((e, c, q));
        }
        quotients.push((k, terms));
    }
    let all_short = quotients
        .iter()
        .flat_map(|(_, terms)| terms)
        .all(|(_, c, q)| short_once_scaled(q.numer(), q.denom(), &lcm, &c.0));
    if !all_short {
        return Ok(None);
    }
    let mut integral = Vec::with_capacity(quotients.len());
    for (k, terms) in quotients {
        let mut scaled_terms = Vec::with_capacity(terms.len());
        for (e, _, q) in terms {
            checkpoint()?;
            scaled_terms.push((e, q.numer() * (&lcm / q.denom())));
        }
        integral.push((k, scaled_terms));
    }
    Ok(Some((lcm, integral)))
}

/// A sum of products of coefficients, for [`add_products`].
trait Sum {
    type Coefficient;
    fn product(a: &Self::Coefficient, b: &Self::Coefficient) -> Self;
    fn add_product(&mut self, a: &Self::Coefficient, b: &Self::Coefficient);
    fn is_zero(&self) -> bool;
}

impl Sum for BigInt {
    type Coefficient = BigInt;

    fn product(a: &BigInt, b: &BigInt) -> Self {
        a * b
    }

    fn add_product(&mut self, a: &BigInt, b: &BigInt) {
        *self += a * b;
    }

    fn is_zero(&self) -> bool {
        Zero::is_zero(self)
    }
}

/// A sum of products of rationals as a fraction, reduced only when its
/// denominator grows long: products, and sums over one denominator, take no
/// gcd.
struct Fraction {
    numer: BigInt,
    denom: BigInt,
}

impl Sum for Fraction {
    type Coefficient = Rational;

    fn product(a: &Rational, b: &Rational) -> Self {
        Self {
            numer: a.numer() * b.numer(),
            denom: a.denom() * b.denom(),
        }
    }

    fn add_product(&mut self, a: &Rational, b: &Rational) {
        let numer = a.numer() * b.numer();
        let denom = a.denom() * b.denom();
        if denom == self.denom {
            self.numer += numer;
            return;
        }
        self.numer = &self.numer * &denom + numer * &self.denom;
        self.denom *= &denom;
        // Unreduced, the denominator would grow with each product.
        if self.denom.bits() > denom.bits().saturating_mul(2).saturating_add(64) {
            let g = self.numer.gcd(&self.denom);
            if !g.is_one() {
                self.numer /= &g;
                self.denom /= &g;
            }
        }
    }

    fn is_zero(&self) -> bool {
        Zero::is_zero(&self.numer)
    }
}

/// `sum += a * b` on terms, calling `checkpoint` after every [`CHECK_EVERY`]
/// products of terms. `exponents` is room for one exponent vector.
fn add_products<S: Sum, E: From<MathError>>(
    a: &[(&[u32], &S::Coefficient)],
    b: &[(&[u32], &S::Coefficient)],
    sum: &mut HashMap<Vec<u32>, S>,
    exponents: &mut [u32],
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<(), E> {
    let mut count = 0usize;
    for (e1, c1) in a {
        for (e2, c2) in b {
            count += 1;
            if count.is_multiple_of(CHECK_EVERY) {
                checkpoint()?;
            }
            for ((x, p), q) in exponents.iter_mut().zip(*e1).zip(*e2) {
                *x = p.checked_add(*q).ok_or(MathError::ExponentOverflow)?;
            }
            match sum.get_mut(&*exponents) {
                Some(s) => s.add_product(c1, c2),
                None => {
                    sum.insert(exponents.to_vec(), S::product(c1, c2));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use proptest::prelude::*;

    use super::*;
    use crate::ring::{CoefficientDomain, MonomialOrder};

    fn ring() -> RingSpec {
        RingSpec::new(
            CoefficientDomain::Rationals,
            vec!["x".into(), "y".into()],
            MonomialOrder::Grevlex,
        )
        .unwrap()
    }

    /// The Mersenne number 2^p - 1, prime for p = 61, 89, 107, 127. The lcm
    /// of 2^89 - 1 and 2^107 - 1 is more than 64 bits longer than either;
    /// that of 2^61 - 1 and 2^89 - 1 is not.
    fn mersenne(p: u32) -> BigInt {
        (BigInt::one() << p) - 1
    }

    fn rational(n: i64, d: BigInt) -> Rational {
        Rational(BigRational::new(BigInt::from(n), d))
    }

    fn matrix(nrows: usize, entries: Vec<Vec<(usize, Polynomial)>>) -> PolynomialMatrix {
        PolynomialMatrix {
            ring: ring(),
            nrows,
            columns: entries
                .into_iter()
                .map(|c| c.into_iter().filter(|(_, p)| !p.is_zero()).collect())
                .collect(),
        }
    }

    /// The product by its definition, from products and sums of polynomials.
    fn by_definition(a: &PolynomialMatrix, b: &PolynomialMatrix, i: usize, j: usize) -> Polynomial {
        let mut sum = Polynomial::zero(ring());
        for k in 0..a.ncols() {
            if let (Some(x), Some(y)) = (a.get(i, k), b.get(k, j)) {
                sum = sum.add(&x.mul(y).unwrap()).unwrap();
            }
        }
        sum
    }

    fn agrees_with_the_definition(a: &PolynomialMatrix, b: &PolynomialMatrix) -> bool {
        let ab = a.mul(b).unwrap();
        (0..a.nrows()).all(|i| {
            (0..b.ncols()).all(|j| {
                let entry = ab
                    .get(i, j)
                    .cloned()
                    .unwrap_or_else(|| Polynomial::zero(ring()));
                entry == by_definition(a, b, i, j)
            })
        })
    }

    fn no_checkpoint() -> Result<(), MathError> {
        Ok(())
    }

    fn scaled_columns(a: &PolynomialMatrix) -> Vec<Option<IntegralColumn>> {
        a.columns
            .iter()
            .map(|c| IntegralColumn::new(c, &mut no_checkpoint).unwrap())
            .collect()
    }

    /// Whether the products with a column of the right factor are summed in
    /// integers.
    fn cleared(scaled: &[Option<IntegralColumn>], column: &BTreeMap<usize, Polynomial>) -> bool {
        integral_quotients(scaled, column, &mut no_checkpoint)
            .unwrap()
            .is_some()
    }

    /// Integer terms x^t, t < count, and y over 2^bits.
    fn one_long_denominator(count: u32, bits: u32) -> Polynomial {
        let mut terms: Vec<(Vec<u32>, Rational)> = (0..count)
            .map(|t| (vec![t, 0], rational(1, BigInt::one())))
            .collect();
        terms.push((vec![0, 1], rational(1, BigInt::one() << bits)));
        Polynomial::from_terms(ring(), terms).unwrap()
    }

    /// Issue #66 (review): one long denominator in a column is not cleared,
    /// since every other term of the column would grow as long, on either
    /// side of the product; the same column over that denominator
    /// throughout is.
    #[test]
    fn one_long_denominator_does_not_lengthen_the_other_terms() {
        let long = one_long_denominator(64, 100_000);
        let one =
            Polynomial::from_terms(ring(), [(vec![0, 0], rational(1, BigInt::one()))]).unwrap();
        let a = matrix(1, vec![vec![(0, long.clone())]]);
        let b = matrix(1, vec![vec![(0, one.clone())]]);
        assert!(scaled_columns(&a)[0].is_none());
        let unit = matrix(1, vec![vec![(0, one)]]);
        let scaled = scaled_columns(&unit);
        assert!(scaled[0].is_some());
        let long_column = matrix(1, vec![vec![(0, long.clone())]]);
        assert!(!cleared(&scaled, &long_column.columns[0]));
        assert!(agrees_with_the_definition(&a, &b));
        assert!(agrees_with_the_definition(&unit, &long_column));
        // Over 2^100000 throughout, the terms keep their length.
        let shared = Polynomial::from_terms(
            ring(),
            long.terms()
                .map(|(e, _)| (e.to_vec(), rational(1, BigInt::one() << 100_000u32))),
        )
        .unwrap();
        let shared = matrix(1, vec![vec![(0, shared)]]);
        assert!(scaled_columns(&shared)[0].is_some());
        assert!(cleared(&scaled, &shared.columns[0]));
        // One long denominator in a column of the left factor: the quotients
        // by it are long, and would let the lcm lengthen the quotients by
        // the other columns, though their products never meet it.
        let integers = |exponents: Vec<[u32; 2]>| {
            Polynomial::from_terms(
                ring(),
                exponents
                    .into_iter()
                    .map(|e| (e.to_vec(), rational(1, BigInt::one()))),
            )
            .unwrap()
        };
        let over = Polynomial::from_terms(
            ring(),
            [(vec![0, 1], rational(1, BigInt::one() << 1000u32))],
        )
        .unwrap();
        let a = matrix(
            1,
            vec![
                vec![(0, over)],
                vec![(0, integers((0..10).map(|t| [t, 0]).collect()))],
            ],
        );
        let b = matrix(
            2,
            vec![vec![
                (0, integers((0..5).map(|s| [10 + s, 0]).collect())),
                (1, integers((0..10).map(|s| [0, s + 2]).collect())),
            ]],
        );
        let scaled = scaled_columns(&a);
        assert!(scaled[0].is_some() && scaled[1].is_some());
        assert!(!cleared(&scaled, &b.columns[0]));
        assert!(agrees_with_the_definition(&a, &b));
    }

    /// Issue #66 (review): the lcms and the scaling before the products, and
    /// the gcds after them, call the checkpoint: twice for each term before,
    /// after every CHECK_EVERY terms after.
    #[test]
    fn the_work_before_and_after_the_products_calls_the_checkpoint() {
        let long = one_long_denominator(10, 64);
        let column = matrix(1, vec![vec![(0, long)]]);
        let mut calls = 0usize;
        let mut count = || -> Result<(), MathError> {
            calls += 1;
            Ok(())
        };
        IntegralColumn::new(&column.columns[0], &mut count)
            .unwrap()
            .unwrap();
        assert_eq!(calls, 22);
        let stop = || -> Result<(), MathError> { Err(MathError::Invalid("stopped".into())) };
        assert!(IntegralColumn::new(&column.columns[0], &mut { stop }).is_err());
        let scaled = scaled_columns(&column);
        assert!(integral_quotients(&scaled, &column.columns[0], &mut { stop }).is_err());
        let terms: HashMap<Vec<u32>, BigInt> = (0..2 * CHECK_EVERY as u32)
            .map(|t| (vec![t, 0], BigInt::one()))
            .collect();
        let sums = BTreeMap::from([(0usize, terms)]);
        let mut calls = 0usize;
        let mut count = || -> Result<(), MathError> {
            calls += 1;
            Ok(())
        };
        column
            .summed_entries(sums, |c| Rational(BigRational::from_integer(c)), &mut count)
            .unwrap();
        assert_eq!(calls, 2);
    }

    /// Issue #66: denominators that share their factors are cleared; those
    /// of unrelated long primes are summed as fractions. Both give the
    /// product.
    #[test]
    fn both_ways_of_summing_give_the_product() {
        let x = |c: Rational| Polynomial::from_terms(ring(), [(vec![1, 0], c)]).unwrap();
        let y = |c: Rational| Polynomial::from_terms(ring(), [(vec![0, 1], c)]).unwrap();
        let small = |n: i64, d: i64| rational(n, BigInt::from(d));
        // Column 0 of a over 6 and 4, column 1 over 2^89 - 1 and 2^107 - 1.
        let a = matrix(
            1,
            vec![
                vec![(0, x(small(1, 6)).add(&y(small(3, 4))).unwrap())],
                vec![(
                    0,
                    x(rational(5, mersenne(89)))
                        .add(&y(rational(-7, mersenne(107))))
                        .unwrap(),
                )],
            ],
        );
        // b column 0 takes column 0 of a only; column 1 takes both.
        let b = matrix(
            2,
            vec![
                vec![(0, y(small(2, 9)))],
                vec![(0, x(small(-1, 10))), (1, y(rational(3, mersenne(107))))],
            ],
        );
        let scaled = scaled_columns(&a);
        assert!(scaled[0].is_some() && scaled[1].is_none());
        assert!(cleared(&scaled, &b.columns[0]));
        assert!(!cleared(&scaled, &b.columns[1]));
        assert!(agrees_with_the_definition(&a, &b));
        // x y / p - x y / p + x^2 / q = x^2 / q: terms that cancel in the
        // sum leave no zero term, on either way. The lcm of 2^89 - 1 and
        // 2^127 - 1 is too long to clear.
        for (p, q) in [
            (BigInt::from(3), BigInt::from(5)),
            (mersenne(89), mersenne(127)),
        ] {
            let c = matrix(
                1,
                vec![
                    vec![(0, x(small(1, 1)))],
                    vec![(0, x(small(-1, 1)))],
                    vec![(0, x(small(1, 1)))],
                ],
            );
            let column = vec![
                (0, y(rational(1, p.clone()))),
                (1, y(rational(1, p.clone()))),
                (2, x(rational(1, q.clone()))),
            ];
            let d = matrix(3, vec![column]);
            let scaled = scaled_columns(&c);
            assert_eq!(cleared(&scaled, &d.columns[0]), p.bits() < 64);
            let expected = Polynomial::from_terms(ring(), [(vec![2, 0], rational(1, q))]).unwrap();
            assert_eq!(c.mul(&d).unwrap().get(0, 0), Some(&expected));
        }
    }

    /// A coefficient over a small denominator, or over one of three long
    /// primes.
    fn coefficient() -> impl Strategy<Value = Rational> {
        (
            -4i64..=4,
            prop_oneof![
                (1i64..=12).prop_map(BigInt::from),
                prop_oneof![Just(61u32), Just(89), Just(107)].prop_map(mersenne)
            ],
        )
            .prop_map(|(n, d)| rational(n, d))
    }

    fn polynomial() -> impl Strategy<Value = Polynomial> {
        proptest::collection::btree_map(proptest::collection::vec(0u32..3, 2), coefficient(), 0..4)
            .prop_map(|t| Polynomial::from_terms(ring(), t).unwrap())
    }

    fn dense(nrows: usize, ncols: usize) -> impl Strategy<Value = PolynomialMatrix> {
        proptest::collection::vec(proptest::collection::vec(polynomial(), nrows), ncols).prop_map(
            move |columns| {
                matrix(
                    nrows,
                    columns
                        .into_iter()
                        .map(|c| c.into_iter().enumerate().collect())
                        .collect(),
                )
            },
        )
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 200, ..ProptestConfig::default() })]

        #[test]
        fn the_product_of_rational_matrices_is_the_matrix_product(
            (a, b) in (1usize..3, 1usize..4, 1usize..3)
                .prop_flat_map(|(m, k, n)| (dense(m, k), dense(k, n))),
        ) {
            prop_assert!(agrees_with_the_definition(&a, &b));
        }
    }
}

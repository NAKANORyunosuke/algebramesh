//! Checks of exponents and truncated series returned by a CAS (design §9.3).
//!
//! A series is a finite sum of terms `c x^(v+u) l^(k)`, given by its exponent `v`,
//! integer shifts `u` in `ker_ZZ A` and, for logarithmic series, the exponents `k`
//! of the divided powers `l^(k) = prod_i (log x_i)^(k_i) / k_i!`.
//!
//! Normally ordered operators act on it term by term. Since
//! `d_i (x^e l^(k)) = x^(e - e_i) (e_i l^(k) + l^(k - e_i))`, where the last term is
//! present only if `k_i > 0`, the operator `x^a d^b` maps `x^e l^(k)` to
//! `x^(e-b+a)` times a combination of `l^(k')` with `k' <= k`. Without logarithms
//! this is `[e]_b x^(e-b+a)`, with the falling factorial
//! `[e]_b = prod_j e_j (e_j - 1) ... (e_j - b_j + 1)`. Only exact rational arithmetic
//! is used.

use std::collections::{BTreeMap, BTreeSet};

use algebramesh_math_types::{DifferentialOperator, Polynomial, Rational};
use num_bigint::BigInt;
use num_rational::BigRational;
use num_traits::{One, Zero};
use serde::{Deserialize, Serialize};

use crate::GkzInput;

/// One term `coefficient * x^(v + shift) * l^(log_powers)` of a series with
/// exponent `v`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeriesTerm {
    pub shift: Vec<i64>,
    /// `k` of the divided power `l^(k)`; empty for a term without logarithm.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub log_powers: Vec<u32>,
    pub coefficient: Rational,
}

impl SeriesTerm {
    /// `log_powers` with `n` entries.
    pub fn logs(&self, n: usize) -> Vec<u32> {
        if self.log_powers.is_empty() {
            vec![0; n]
        } else {
            self.log_powers.clone()
        }
    }
}

/// A monomial `x^(v + shift) l^(k)` as `(shift, k)`.
pub type Monomial = (Vec<i64>, Vec<u32>);

/// An operator applied to a series: monomials with non-zero coefficients.
pub type SeriesImage = BTreeMap<Monomial, BigRational>;

/// Why a returned series was not accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeriesDefect {
    /// The data contradicts how it was requested: it is not a series of the stated
    /// form, or Euler operators do not annihilate it. This indicates a fault in the
    /// CAS or the driver.
    Malformed(String),
    /// The series is well formed but an operator of the system does not annihilate
    /// it within the computed order, so it is not a solution.
    NotASolution(String),
}

/// `w . u`, exactly (Issue #11): each product fits in `i128`, but their sum
/// need not.
pub fn weight_of(weight: &[i64], u: &[i64]) -> BigInt {
    weight
        .iter()
        .zip(u)
        .map(|(&w, &x)| BigInt::from(i128::from(w) * i128::from(x)))
        .sum()
}

/// Total degree of `l^(k)`, exactly: the sum of `u32` entries is taken in `u64`.
pub fn log_degree(k: &[u32]) -> u64 {
    k.iter().map(|&d| u64::from(d)).sum()
}

/// `u + d` for shifts, or an error if an entry leaves `i64`. Shifts are stored as
/// `i64`, so such a term cannot be represented: the series is refused rather than
/// checked with a wrapped shift.
fn add_shifts(u: &[i64], d: &[i64]) -> Result<Vec<i64>, String> {
    u.iter()
        .zip(d)
        .map(|(&a, &b)| a.checked_add(b))
        .collect::<Option<Vec<i64>>>()
        .ok_or_else(|| format!("the shift {u:?} + {d:?} leaves the range of 64-bit integers"))
}

/// `d^b (x^e l^(k))` as `x^(e-b)` times `sum_k' c_k' l^(k')`; returns the `c_k'`.
fn differentiate(
    exponent: &[BigRational],
    b: &[u32],
    k: &[u32],
) -> BTreeMap<Vec<u32>, BigRational> {
    let mut c = BTreeMap::from([(k.to_vec(), BigRational::one())]);
    for (i, &times) in b.iter().enumerate() {
        let mut e = exponent[i].clone();
        for _ in 0..times {
            let mut next: BTreeMap<Vec<u32>, BigRational> = BTreeMap::new();
            for (k, coefficient) in c {
                if !e.is_zero() {
                    *next.entry(k.clone()).or_insert_with(BigRational::zero) += &e * &coefficient;
                }
                if k[i] > 0 {
                    let mut lower = k;
                    lower[i] -= 1;
                    *next.entry(lower).or_insert_with(BigRational::zero) += coefficient;
                }
            }
            next.retain(|_, v| !v.is_zero());
            c = next;
            e -= BigRational::one();
        }
    }
    c
}

/// `op` applied to the series. Fails if a shift of the image leaves `i64`.
pub fn apply(
    op: &DifferentialOperator,
    point: &[Rational],
    terms: &[SeriesTerm],
) -> Result<SeriesImage, String> {
    let n = point.len();
    let mut out = SeriesImage::new();
    let op_terms = op.to_data();
    for term in terms {
        let exponent: Vec<BigRational> = point
            .iter()
            .zip(&term.shift)
            .map(|(v, &u)| &v.0 + BigRational::from_integer(u.into()))
            .collect();
        let logs = term.logs(n);
        for t in &op_terms {
            // x^a d^b moves the shift by a - b; both fit in i64.
            let step: Vec<i64> = t
                .d_exponents
                .iter()
                .zip(&t.x_exponents)
                .map(|(&b, &a)| i64::from(a) - i64::from(b))
                .collect();
            let s = add_shifts(&term.shift, &step)?;
            for (k, factor) in differentiate(&exponent, &t.d_exponents, &logs) {
                *out.entry((s.clone(), k)).or_insert_with(BigRational::zero) +=
                    factor * &term.coefficient.0 * &t.coefficient.0;
            }
        }
    }
    out.retain(|_, c| !c.is_zero());
    Ok(out)
}

/// A solution with a verified series: the index of its exponent, its index among
/// the solutions of that exponent, the log degree of its leading terms and its
/// terms.
#[derive(Debug, Clone)]
pub struct Solution {
    pub exponent: usize,
    pub index: usize,
    pub log_degree: u32,
    pub terms: Vec<SeriesTerm>,
}

/// One step of [`GkzInput::normalize`]: `factor` times solution `solution` of
/// exponent `exponent`, multiplied by `x^(v_exponent - v)`, was subtracted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Subtraction {
    pub exponent: usize,
    pub solution: usize,
    pub factor: Rational,
}

/// Solves `sum_t x_t columns[t] = rhs` over QQ, where vectors are sparse maps.
fn solve(
    columns: &[BTreeMap<Vec<u32>, BigRational>],
    rhs: &BTreeMap<Vec<u32>, BigRational>,
) -> Option<Vec<BigRational>> {
    let keys: BTreeSet<&Vec<u32>> = columns
        .iter()
        .flat_map(|c| c.keys())
        .chain(rhs.keys())
        .collect();
    let width = columns.len();
    let mut rows: Vec<Vec<BigRational>> = keys
        .iter()
        .map(|k| {
            columns
                .iter()
                .map(|c| c.get(*k).cloned().unwrap_or_else(BigRational::zero))
                .chain(std::iter::once(
                    rhs.get(*k).cloned().unwrap_or_else(BigRational::zero),
                ))
                .collect()
        })
        .collect();
    let mut pivots = Vec::new();
    let mut r = 0;
    for col in 0..width {
        let Some(p) = (r..rows.len()).find(|&i| !rows[i][col].is_zero()) else {
            continue;
        };
        rows.swap(r, p);
        let inv = rows[r][col].recip();
        for x in rows[r].iter_mut() {
            *x *= &inv;
        }
        for i in 0..rows.len() {
            if i != r && !rows[i][col].is_zero() {
                let f = rows[i][col].clone();
                let pivot_row = rows[r].clone();
                for (x, y) in rows[i].iter_mut().zip(&pivot_row) {
                    *x -= &f * y;
                }
            }
        }
        pivots.push(col);
        r += 1;
    }
    if rows[r..].iter().any(|row| !row[width].is_zero()) {
        return None;
    }
    let mut x = vec![BigRational::zero(); width];
    for (i, &col) in pivots.iter().enumerate() {
        x[col] = rows[i][width].clone();
    }
    Some(x)
}

fn evaluate(p: &Polynomial, point: &[Rational]) -> BigRational {
    p.terms()
        .map(|(e, c)| {
            point.iter().zip(e).fold(c.0.clone(), |acc, (v, &k)| {
                (0..k).fold(acc, |acc, _| acc * &v.0)
            })
        })
        .sum()
}

impl GkzInput {
    fn in_lattice(&self, u: &[i64]) -> bool {
        u.len() == self.n()
            && self.a().rows().iter().all(|row| {
                row.iter()
                    .zip(u)
                    .map(|(a, &x)| a * BigInt::from(x))
                    .sum::<BigInt>()
                    .is_zero()
            })
    }

    /// Checks that `point` satisfies `A v = beta` and is a common root of `indicial`
    /// (polynomials in `theta_1..theta_n`).
    pub fn check_exponent(
        &self,
        point: &[Rational],
        indicial: &[Polynomial],
    ) -> Result<(), String> {
        if point.len() != self.n() {
            return Err(format!("exponent of length {}", point.len()));
        }
        for (i, row) in self.a().rows().iter().enumerate() {
            let value: BigRational = row
                .iter()
                .zip(point)
                .map(|(a, v)| BigRational::from_integer(a.clone()) * &v.0)
                .sum();
            if value != self.beta()[i].0 {
                return Err(format!("A v differs from beta in row {}", i + 1));
            }
        }
        if let Some(p) = indicial.iter().find(|p| !evaluate(p, point).is_zero()) {
            return Err(format!("the indicial generator {p} does not vanish"));
        }
        Ok(())
    }

    /// The canonical form of `solutions[target]` up to the order (SST, Section
    /// 2.5): no monomial `x^(v_j) l^(k)` of another exponent `v_j` occurs in it.
    ///
    /// Other exponents `v_j` with `v_j - v` in `ker_ZZ A` and weight in `(0, order]`
    /// are taken in order of increasing weight. The part of the series at
    /// `x^(v_j)`, a polynomial in the `l^(k)`, must be a combination of the leading
    /// parts (the terms at `x^(v_j)` itself) of the verified solutions of `v_j`; that
    /// combination of those solutions, times `x^(v_j - v)`, is subtracted. Their
    /// other terms have larger weight, so later steps do not bring back an earlier
    /// part. Fails if the part is not such a combination.
    ///
    /// The result is a combination of solutions, so it is again a solution; callers
    /// still check it with [`Self::check_series`].
    pub fn normalize(
        &self,
        points: &[Vec<Rational>],
        solutions: &[Solution],
        target: usize,
        weight: &[i64],
        order: u32,
    ) -> Result<(Vec<SeriesTerm>, Vec<Subtraction>), String> {
        let n = self.n();
        let s = &solutions[target];
        let v = &points[s.exponent];
        let mut candidates = Vec::new();
        for (j, p) in points.iter().enumerate() {
            if j == s.exponent {
                continue;
            }
            let Some(delta) = p
                .iter()
                .zip(v)
                .map(|(a, b)| {
                    let d = &a.0 - &b.0;
                    d.is_integer()
                        .then(|| d.to_integer())
                        .and_then(|d| i64::try_from(d).ok())
                })
                .collect::<Option<Vec<i64>>>()
            else {
                continue;
            };
            let w = weight_of(weight, &delta);
            if self.in_lattice(&delta) && w > BigInt::zero() && w <= BigInt::from(order) {
                candidates.push((w, j, delta));
            }
        }
        candidates.sort();
        let mut terms: SeriesImage = s
            .terms
            .iter()
            .map(|t| ((t.shift.clone(), t.logs(n)), t.coefficient.0.clone()))
            .collect();
        let mut steps = Vec::new();
        for (_, j, delta) in candidates {
            let part: BTreeMap<Vec<u32>, BigRational> = terms
                .iter()
                .filter(|((u, _), c)| *u == delta && !c.is_zero())
                .map(|((_, k), c)| (k.clone(), c.clone()))
                .collect();
            if part.is_empty() {
                continue;
            }
            let basis: Vec<&Solution> = solutions.iter().filter(|t| t.exponent == j).collect();
            let leading: Vec<BTreeMap<Vec<u32>, BigRational>> = basis
                .iter()
                .map(|t| {
                    t.terms
                        .iter()
                        .filter(|x| x.shift.iter().all(|&u| u == 0))
                        .map(|x| (x.logs(n), x.coefficient.0.clone()))
                        .collect()
                })
                .collect();
            let factors = solve(&leading, &part).ok_or_else(|| {
                format!(
                    "the terms at x^(v + {delta:?}), the exponent {j}, are not a combination \
                     of the leading terms of its {} verified solutions",
                    basis.len()
                )
            })?;
            for (t, factor) in basis.iter().zip(factors) {
                if factor.is_zero() {
                    continue;
                }
                for x in &t.terms {
                    let shift = add_shifts(&delta, &x.shift)?;
                    if weight_of(weight, &shift) > BigInt::from(order) {
                        continue;
                    }
                    *terms
                        .entry((shift, x.logs(n)))
                        .or_insert_with(BigRational::zero) -= &factor * &x.coefficient.0;
                }
                steps.push(Subtraction {
                    exponent: j,
                    solution: t.index,
                    factor: Rational(factor),
                });
            }
        }
        let logarithmic = terms.keys().any(|(_, k)| log_degree(k) > 0);
        let terms = terms
            .into_iter()
            .filter(|(_, c)| !c.is_zero())
            .map(|((shift, k), c)| SeriesTerm {
                shift,
                log_powers: if logarithmic { k } else { vec![] },
                coefficient: Rational(c),
            })
            .collect();
        Ok((terms, steps))
    }

    /// Checks a series truncated to `w . u <= order` against the system: its shape,
    /// the Euler operators exactly, and each operator in `toric` up to the order.
    ///
    /// The leading terms are those at `x^v`. Without logarithms (`log_degree = 0`)
    /// the leading term is `x^v` with coefficient 1. Otherwise every term has log
    /// degree at most `log_degree`, and some leading term has exactly that degree.
    ///
    /// For an operator `g = sum_t c_t x^(a_t) d^(b_t)`, the coefficient of
    /// `x^(v+s) l^(k)` in `g` applied to the full series involves the terms with
    /// shifts `s + b_t - a_t`. If all of those in `ker_ZZ A` have weight at most
    /// `order`, the truncated series has every term involved, so the coefficient
    /// must vanish. Non-zero coefficients are allowed only where a term beyond the
    /// order is involved.
    pub fn check_series(
        &self,
        point: &[Rational],
        terms: &[SeriesTerm],
        degree: u32,
        weight: &[i64],
        order: u32,
        toric: &[DifferentialOperator],
    ) -> Result<(), SeriesDefect> {
        let malformed = |m: String| Err(SeriesDefect::Malformed(m));
        let n = self.n();
        let mut monomials = BTreeSet::new();
        let mut leading = false;
        for t in terms {
            if t.shift.len() != n || !self.in_lattice(&t.shift) {
                return malformed(format!("shift {:?} is not in ker A", t.shift));
            }
            if !t.log_powers.is_empty() && t.log_powers.len() != n {
                return malformed(format!("log powers {:?} of the wrong length", t.log_powers));
            }
            let k = t.logs(n);
            if log_degree(&k) > u64::from(degree) {
                return malformed(format!(
                    "shift {:?} has log powers {k:?} above the degree {degree}",
                    t.shift
                ));
            }
            if !monomials.insert((t.shift.clone(), k.clone())) {
                return malformed(format!(
                    "shift {:?} with log powers {k:?} appears twice",
                    t.shift
                ));
            }
            if t.coefficient.is_zero() {
                return malformed(format!("shift {:?} has coefficient 0", t.shift));
            }
            let w = weight_of(weight, &t.shift);
            if w > BigInt::from(order) {
                return malformed(format!(
                    "shift {:?} has weight {w} above the order",
                    t.shift
                ));
            }
            if t.shift.iter().all(|&x| x == 0) {
                if log_degree(&k) == u64::from(degree) {
                    leading |= degree > 0 || t.coefficient == Rational::one();
                }
            } else if w <= BigInt::zero() {
                return malformed(format!(
                    "shift {:?} has weight {w}, so x^v is not the leading term",
                    t.shift
                ));
            }
        }
        if !leading {
            return malformed(if degree == 0 {
                "the term x^v with coefficient 1 is missing".into()
            } else {
                format!("no term at x^v has log degree {degree}")
            });
        }
        let euler = self
            .euler_operators()
            .map_err(|e| SeriesDefect::Malformed(e.to_string()))?;
        for e in &euler {
            if let Some(((s, _), _)) = apply(e, point, terms)
                .map_err(SeriesDefect::Malformed)?
                .into_iter()
                .next()
            {
                return malformed(format!("{e} does not annihilate the series (shift {s:?})"));
            }
        }
        for g in toric {
            let steps: Vec<Vec<i64>> = g
                .to_data()
                .iter()
                .map(|t| {
                    t.d_exponents
                        .iter()
                        .zip(&t.x_exponents)
                        .map(|(&b, &a)| i64::from(b) - i64::from(a))
                        .collect()
                })
                .collect();
            for ((s, k), c) in apply(g, point, terms).map_err(SeriesDefect::Malformed)? {
                let mut beyond = false;
                for step in &steps {
                    let u = add_shifts(&s, step).map_err(SeriesDefect::Malformed)?;
                    beyond |= self.in_lattice(&u) && weight_of(weight, &u) > BigInt::from(order);
                }
                if !beyond {
                    return Err(SeriesDefect::NotASolution(format!(
                        "{g} applied to the series has the coefficient {c} at shift {s:?} \
                         (log powers {k:?}), which involves only computed terms"
                    )));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use algebramesh_math_types::{CoefficientDomain, IntegerMatrix, MonomialOrder, RingSpec};

    fn q(s: &str) -> Rational {
        s.parse().unwrap()
    }

    fn rnc(beta: [&str; 2]) -> GkzInput {
        GkzInput::new(
            IntegerMatrix::from_i64(&[&[1, 1, 1], &[0, 1, 2]]).unwrap(),
            beta.iter().map(|b| q(b)).collect(),
        )
        .unwrap()
    }

    fn series(terms: &[([i64; 3], &str)]) -> Vec<SeriesTerm> {
        terms
            .iter()
            .map(|(u, c)| SeriesTerm {
                shift: u.to_vec(),
                log_powers: vec![],
                coefficient: q(c),
            })
            .collect()
    }

    fn log_series(terms: &[([i64; 3], [u32; 3], &str)]) -> Vec<SeriesTerm> {
        terms
            .iter()
            .map(|(u, k, c)| SeriesTerm {
                shift: u.to_vec(),
                log_powers: k.to_vec(),
                coefficient: q(c),
            })
            .collect()
    }

    /// The Gamma-series of A = [[1,1,1],[0,1,2]], beta = (1/2, 1/3) with exponent
    /// (1/6, 1/3, 0), truncated to (1,0,1).u <= 6, as computed by the driver.
    fn gamma() -> (GkzInput, Vec<Rational>, Vec<SeriesTerm>) {
        (
            rnc(["1/2", "1/3"]),
            vec![q("1/6"), q("1/3"), q("0")],
            series(&[
                ([0, 0, 0], "1"),
                ([1, -2, 1], "-4/21"),
                ([2, -4, 2], "-160/819"),
                ([3, -6, 3], "-7040/20007"),
            ]),
        )
    }

    #[test]
    fn accepts_the_truncated_gamma_series() {
        let (input, v, terms) = gamma();
        let toric = vec![input.toric_binomial(&[1, 0, 1], &[0, 2, 0]).unwrap()];
        assert_eq!(
            input.check_series(&v, &terms, 0, &[1, 0, 1], 6, &toric),
            Ok(())
        );
        // Applying d1 d3 - d2^2 leaves only terms that involve the shift (4,-8,4).
        let rest = apply(&toric[0], &v, &terms).unwrap();
        assert_eq!(rest.len(), 1);
    }

    #[test]
    fn rejects_a_wrong_coefficient_and_missing_terms() {
        let (input, v, mut terms) = gamma();
        let toric = vec![input.toric_binomial(&[1, 0, 1], &[0, 2, 0]).unwrap()];
        terms[2].coefficient = q("-1/5");
        assert!(matches!(
            input.check_series(&v, &terms, 0, &[1, 0, 1], 6, &toric),
            Err(SeriesDefect::NotASolution(_))
        ));
        let (_, _, terms) = gamma();
        // Claiming a higher order than computed exposes the missing terms.
        assert!(matches!(
            input.check_series(&v, &terms, 0, &[1, 0, 1], 8, &toric),
            Err(SeriesDefect::NotASolution(_))
        ));
    }

    #[test]
    fn rejects_malformed_series() {
        let (input, v, terms) = gamma();
        let toric = vec![input.toric_binomial(&[1, 0, 1], &[0, 2, 0]).unwrap()];
        let not_in_lattice = series(&[([0, 0, 0], "1"), ([1, -1, 0], "2")]);
        let no_leading = series(&[([1, -2, 1], "-4/21")]);
        for bad in [not_in_lattice, no_leading] {
            assert!(matches!(
                input.check_series(&v, &bad, 0, &[1, 0, 1], 6, &toric),
                Err(SeriesDefect::Malformed(_))
            ));
        }
        // A wrong exponent breaks the Euler operators.
        let wrong = [q("1/6"), q("1/2"), q("0")];
        assert!(matches!(
            input.check_series(&wrong, &terms, 0, &[1, 0, 1], 6, &toric),
            Err(SeriesDefect::Malformed(_))
        ));
    }

    #[test]
    fn integer_parameters_give_a_polynomial_solution_and_a_non_solution() {
        let input = rnc(["3", "4"]);
        let toric = vec![input.toric_binomial(&[1, 0, 1], &[0, 2, 0]).unwrap()];
        // x2^2 x3 + x1 x3^2 is a solution.
        let v = [q("0"), q("2"), q("1")];
        let terms = series(&[([0, 0, 0], "1"), ([1, -2, 1], "1")]);
        assert_eq!(
            input.check_series(&v, &terms, 0, &[1, 0, 1], 6, &toric),
            Ok(())
        );
        // x1^-1 x2^4 alone is not: d2^2 does not annihilate it.
        let v = [q("-1"), q("4"), q("0")];
        let terms = series(&[([0, 0, 0], "1")]);
        assert!(matches!(
            input.check_series(&v, &terms, 0, &[1, 0, 1], 6, &toric),
            Err(SeriesDefect::NotASolution(_))
        ));
    }

    /// beta = 0: the exponent 0 has multiplicity 2. With L = log x1 - 2 log x2 +
    /// log x3 and t = x1 x3 / x2^2, the second solution is L + 2t + 3t^2 + 20/3 t^3
    /// + ..., from (k+1)^2 a_(k+1) = (4k^2 + 2k) a_k.
    #[test]
    fn accepts_the_logarithmic_solution() {
        let input = rnc(["0", "0"]);
        let toric = vec![input.toric_binomial(&[1, 0, 1], &[0, 2, 0]).unwrap()];
        let v = [q("0"), q("0"), q("0")];
        let terms = log_series(&[
            ([0, 0, 0], [1, 0, 0], "1"),
            ([0, 0, 0], [0, 1, 0], "-2"),
            ([0, 0, 0], [0, 0, 1], "1"),
            ([1, -2, 1], [0, 0, 0], "2"),
            ([2, -4, 2], [0, 0, 0], "3"),
            ([3, -6, 3], [0, 0, 0], "20/3"),
        ]);
        assert_eq!(
            input.check_series(&v, &terms, 1, &[1, 0, 1], 6, &toric),
            Ok(())
        );
        // A wrong coefficient, or the logarithm alone, is not a solution.
        let mut wrong = terms.clone();
        wrong[4].coefficient = q("4");
        let alone = terms[..3].to_vec();
        for bad in [wrong, alone] {
            assert!(matches!(
                input.check_series(&v, &bad, 1, &[1, 0, 1], 6, &toric),
                Err(SeriesDefect::NotASolution(_))
            ));
        }
        // Log powers above the degree, and log x1 alone (not killed by the Euler
        // operators), are malformed.
        assert!(matches!(
            input.check_series(&v, &terms, 0, &[1, 0, 1], 6, &toric),
            Err(SeriesDefect::Malformed(_))
        ));
        let log_x1 = log_series(&[([0, 0, 0], [1, 0, 0], "1")]);
        assert!(matches!(
            input.check_series(&v, &log_x1, 1, &[1, 0, 1], 6, &toric),
            Err(SeriesDefect::Malformed(_))
        ));
    }

    /// With beta = (1/2, 1/3) and weight (1,0,1) the two exponents (1/6, 1/3, 0)
    /// and (0, 2/3, -1/6) differ by a non-integer vector, so both Gamma-series are
    /// already canonical. A made-up second exponent at v + (1,-2,1) is removed.
    #[test]
    fn normalizes_against_other_exponents() {
        let (input, v, terms) = gamma();
        let points = vec![v.clone(), vec![q("0"), q("2/3"), q("-1/6")]];
        let first = Solution {
            exponent: 0,
            index: 0,
            log_degree: 0,
            terms: terms.clone(),
        };
        let (same, steps) = input
            .normalize(&points, std::slice::from_ref(&first), 0, &[1, 0, 1], 6)
            .unwrap();
        assert_eq!(same, terms);
        assert!(steps.is_empty());

        // Pretend that v + (1,-2,1) were an exponent whose solution starts with 1
        // and continues with 1 at the next shift.
        let tail = Solution {
            exponent: 1,
            index: 0,
            log_degree: 0,
            terms: series(&[([0, 0, 0], "1"), ([1, -2, 1], "1")]),
        };
        let points = vec![v.clone(), vec![q("7/6"), q("-5/3"), q("1")]];
        let (normal, steps) = input
            .normalize(&points, &[first.clone(), tail], 0, &[1, 0, 1], 6)
            .unwrap();
        assert_eq!(
            steps,
            vec![Subtraction {
                exponent: 1,
                solution: 0,
                factor: q("-4/21"),
            }]
        );
        // -160/819 + 4/21 = -4/819 at (2,-4,2); (1,-2,1) is gone.
        assert_eq!(
            normal,
            series(&[
                ([0, 0, 0], "1"),
                ([2, -4, 2], "-4/819"),
                ([3, -6, 3], "-7040/20007"),
            ])
        );
        // Without a solution of that exponent, normalization fails.
        assert!(
            input
                .normalize(&points, &[first], 0, &[1, 0, 1], 6)
                .is_err()
        );
    }

    #[test]
    fn solves_small_systems() {
        let r = |s: &str| s.parse::<Rational>().unwrap().0;
        let col = |entries: &[(&[u32], &str)]| -> BTreeMap<Vec<u32>, BigRational> {
            entries.iter().map(|(k, c)| (k.to_vec(), r(c))).collect()
        };
        let a = col(&[(&[0], "1")]);
        let b = col(&[(&[1], "2"), (&[0], "1")]);
        let rhs = col(&[(&[1], "4"), (&[0], "5")]);
        assert_eq!(
            solve(&[a.clone(), b.clone()], &rhs),
            Some(vec![r("3"), r("2")])
        );
        assert_eq!(solve(&[a], &col(&[(&[1], "1")])), None);
    }

    #[test]
    fn checks_exponents() {
        let input = rnc(["1/2", "1/3"]);
        let ring = RingSpec::new(
            CoefficientDomain::Rationals,
            vec!["theta1".into(), "theta2".into(), "theta3".into()],
            MonomialOrder::Grevlex,
        )
        .unwrap();
        // theta1 * theta3 and 6 theta3^2 + theta3, from the driver for weight (1,0,1).
        let indicial = vec![
            Polynomial::from_terms(ring.clone(), [(vec![1, 0, 1], q("1"))]).unwrap(),
            Polynomial::from_terms(ring, [(vec![0, 0, 2], q("6")), (vec![0, 0, 1], q("1"))])
                .unwrap(),
        ];
        assert_eq!(
            input.check_exponent(&[q("1/6"), q("1/3"), q("0")], &indicial),
            Ok(())
        );
        assert_eq!(
            input.check_exponent(&[q("0"), q("2/3"), q("-1/6")], &indicial),
            Ok(())
        );
        assert!(
            input
                .check_exponent(&[q("1/2"), q("0"), q("0")], &indicial)
                .is_err()
        );
        assert!(
            input
                .check_exponent(&[q("0"), q("1/2"), q("0")], &indicial)
                .is_err()
        );
    }

    /// Issue #11: `A = [[1,1,1,1],[0,1,0,1]]`, `M = i64::MAX`, `w = u =
    /// (M,-M,-M,M)`. `A u = 0`, and `w . u = 4 M^2` exceeds `i128`.
    fn extreme() -> (GkzInput, Vec<i64>) {
        let input = GkzInput::new(
            IntegerMatrix::from_i64(&[&[1, 1, 1, 1], &[0, 1, 0, 1]]).unwrap(),
            vec![q("0"), q("0")],
        )
        .unwrap();
        let m = i64::MAX;
        (input, vec![m, -m, -m, m])
    }

    #[test]
    fn weights_beyond_i128_are_exact() {
        let (input, u) = extreme();
        assert!(input.in_lattice(&u));
        let m = BigInt::from(i64::MAX);
        assert_eq!(weight_of(&u, &u), BigInt::from(4) * &m * &m);
        let minus: Vec<i64> = u.iter().map(|x| -x).collect();
        assert_eq!(weight_of(&u, &minus), BigInt::from(-4) * &m * &m);
        assert!(weight_of(&u, &u) > BigInt::from(i128::MAX));
    }

    #[test]
    fn a_term_of_huge_weight_is_refused_not_wrapped() {
        let (input, u) = extreme();
        let v = vec![q("0"); 4];
        let term = |shift: Vec<i64>| SeriesTerm {
            shift,
            log_powers: vec![],
            coefficient: q("1"),
        };
        // Weight 4 M^2: above any order.
        let above = vec![term(vec![0; 4]), term(u.clone())];
        match input.check_series(&v, &above, 0, &u, u32::MAX, &[]) {
            Err(SeriesDefect::Malformed(m)) => assert!(m.contains("above the order"), "{m}"),
            other => panic!("{other:?}"),
        }
        // Weight -4 M^2: x^v is not the leading term.
        let minus: Vec<i64> = u.iter().map(|x| -x).collect();
        let below = vec![term(vec![0; 4]), term(minus)];
        match input.check_series(&v, &below, 0, &u, u32::MAX, &[]) {
            Err(SeriesDefect::Malformed(m)) => assert!(m.contains("not the leading term"), "{m}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn log_degrees_are_summed_without_wrapping() {
        assert_eq!(log_degree(&[u32::MAX, 1]), 1 << 32);
        let input = rnc(["1/2", "1/3"]);
        let v = vec![q("1/6"), q("1/3"), q("0")];
        // A log degree of 2^32 is above any degree a u32 can state; it used to
        // wrap to 0.
        let terms = log_series(&[([0, 0, 0], [u32::MAX, 1, 0], "1")]);
        match input.check_series(&v, &terms, u32::MAX, &[1, 0, 1], 6, &[]) {
            Err(SeriesDefect::Malformed(m)) => assert!(m.contains("above the degree"), "{m}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn shifts_that_leave_i64_are_errors() {
        let input = rnc(["1/2", "1/3"]);
        let v = vec![q("1/6"), q("1/3"), q("0")];
        // d2^2 moves the shift by (0,-2,0); from i64::MIN + 1 that leaves i64.
        let toric = input.toric_binomial(&[1, 0, 1], &[0, 2, 0]).unwrap();
        let terms = series(&[([0, i64::MIN + 1, 0], "1")]);
        let err = apply(&toric, &v, &terms).unwrap_err();
        assert!(err.contains("range of 64-bit integers"), "{err}");
        // And at the top: x moves it up.
        let x1 = DifferentialOperator::from_terms(
            input.weyl_algebra(),
            [(vec![1, 0, 0], vec![0, 0, 0], Rational::one())],
        )
        .unwrap();
        let terms = series(&[([i64::MAX, 0, 0], "1")]);
        assert!(apply(&x1, &v, &terms).is_err());
        assert_eq!(add_shifts(&[i64::MAX - 1], &[1]), Ok(vec![i64::MAX]));
    }
}

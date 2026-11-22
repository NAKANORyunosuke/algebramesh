//! GKZ systems (design §9).
//!
//! For `A in ZZ^(d x n)` and `beta in QQ^d`, the GKZ system is the left ideal
//! `H_A(beta) = D_n I_A + sum_i D_n (sum_j a_ij x_j d_j - beta_i)` in the Weyl algebra
//! `D_n = QQ<x_1..x_n, d_1..d_n>`, with toric ideal `I_A = <d^u - d^v : Au = Av>`.
//!
//! This crate validates the input and builds the parts that need no Gröbner basis
//! computation: the Euler operators and individual toric binomials (with the check
//! `Au = Av`), and checks generators returned by a CAS backend (`gkz.generate`).
//! For `gkz.indicial` and `gkz.series` it checks exponents and truncated series
//! ([`series`]) and reports facets, resonance and homogeneity of the input ([`cone`]).
//!
//! It is the GKZ package (`docs/packages/gkz.md`, Issues #99, #100): a backend
//! builds the values of the three operations from its CAS's answers with
//! [`value`], and [`verify`] checks them after the backend.

pub mod cone;
pub mod series;
pub mod value;
pub mod verify;

pub use cone::{Facet, ParameterReport};
pub use series::{SeriesDefect, SeriesTerm, Solution, Subtraction};

use algebramesh_math_types::{
    CoefficientDomain, DifferentialOperator, IntegerMatrix, MathError, Rational, WeylAlgebraSpec,
};
use num_bigint::BigInt;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Reasons an input is outside the supported specification. Inputs are reported,
/// never silently normalized.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum GkzError {
    #[error("beta has {got} entries but A has {expected} rows")]
    BetaLength { expected: usize, got: usize },
    #[error("A has rank {rank} but {rows} rows; full row rank is required")]
    RankDeficient { rank: usize, rows: usize },
    #[error("u and v must have length {expected}")]
    ExponentLength { expected: usize },
    #[error("A*u != A*v, so d^u - d^v is not in the toric ideal")]
    NotToric,
    #[error("weight must have {expected} entries, not all zero")]
    Weight { expected: usize },
    #[error("input too large: {0}")]
    TooLarge(String),
    #[error(transparent)]
    Math(#[from] MathError),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, try_from = "GkzInputData")]
pub struct GkzInput {
    #[serde(rename = "A")]
    a: IntegerMatrix,
    beta: Vec<Rational>,
}

/// The JSON of a [`GkzInput`], read before its size is checked: even an input
/// that is not validated keeps `A` within the bounds of the rank.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GkzInputData {
    #[serde(rename = "A")]
    a: IntegerMatrix,
    beta: Vec<Rational>,
}

impl TryFrom<GkzInputData> for GkzInput {
    type Error = GkzError;

    fn try_from(data: GkzInputData) -> Result<Self, GkzError> {
        let input = Self {
            a: data.a,
            beta: data.beta,
        };
        input.check_size()?;
        Ok(input)
    }
}

impl GkzInput {
    pub fn new(a: IntegerMatrix, beta: Vec<Rational>) -> Result<Self, GkzError> {
        let input = Self { a, beta };
        input.validate()?;
        Ok(input)
    }

    /// Checks `len(beta) = d` and `rank(A) = d` (the matrix shape is checked by
    /// [`IntegerMatrix`]). `A` must fit the bounds of the rank with one row to
    /// spare, which [`Self::is_homogeneous`] adds (Issue #59); a larger one is
    /// [`GkzError::TooLarge`].
    pub fn validate(&self) -> Result<(), GkzError> {
        self.check_size()?;
        let d = self.a.nrows();
        if self.beta.len() != d {
            return Err(GkzError::BetaLength {
                expected: d,
                got: self.beta.len(),
            });
        }
        let rank = self.a.rank()?;
        if rank != d {
            return Err(GkzError::RankDeficient { rank, rows: d });
        }
        Ok(())
    }

    /// `A` fits the bounds of the rank with one row to spare.
    fn check_size(&self) -> Result<(), GkzError> {
        let (d, n) = (self.a.nrows(), self.a.ncols());
        if d >= algebramesh_math_types::MAX_RANK_ROWS || n > algebramesh_math_types::MAX_RANK_COLS {
            return Err(GkzError::TooLarge(format!(
                "A has {d} rows and {n} columns; at most {} rows and {} columns are accepted",
                algebramesh_math_types::MAX_RANK_ROWS - 1,
                algebramesh_math_types::MAX_RANK_COLS
            )));
        }
        Ok(())
    }

    pub fn a(&self) -> &IntegerMatrix {
        &self.a
    }

    pub fn beta(&self) -> &[Rational] {
        &self.beta
    }

    pub fn d(&self) -> usize {
        self.a.nrows()
    }

    pub fn n(&self) -> usize {
        self.a.ncols()
    }

    /// A weight vector `w` for `in_(-w,w)`: one integer per column, not all zero.
    /// Whether it is generic for the system is decided by the CAS.
    pub fn validate_weight(&self, weight: &[i64]) -> Result<(), GkzError> {
        if weight.len() != self.n() || weight.iter().all(|&w| w == 0) {
            return Err(GkzError::Weight { expected: self.n() });
        }
        Ok(())
    }

    pub fn weyl_algebra(&self) -> WeylAlgebraSpec {
        // Standard names are always valid.
        WeylAlgebraSpec::standard(CoefficientDomain::Rationals, self.n())
            .unwrap_or_else(|_| unreachable!("x1..xn, d1..dn are valid variable names"))
    }

    /// `E_i - beta_i = sum_j a_ij x_j d_j - beta_i` for `i = 1..d`.
    pub fn euler_operators(&self) -> Result<Vec<DifferentialOperator>, GkzError> {
        let n = self.n();
        (0..self.d())
            .map(|i| {
                let mut terms = Vec::new();
                for j in 0..n {
                    let mut e = vec![0; n];
                    e[j] = 1;
                    terms.push((e.clone(), e, Rational::integer(self.a.get(i, j).clone())));
                }
                terms.push((vec![0; n], vec![0; n], Rational(-self.beta[i].0.clone())));
                Ok(DifferentialOperator::from_terms(
                    self.weyl_algebra(),
                    terms,
                )?)
            })
            .collect()
    }

    /// `d^u - d^v`, after checking `Au = Av`.
    pub fn toric_binomial(&self, u: &[u32], v: &[u32]) -> Result<DifferentialOperator, GkzError> {
        let n = self.n();
        if u.len() != n || v.len() != n {
            return Err(GkzError::ExponentLength { expected: n });
        }
        for row in self.a.rows() {
            let dot = |w: &[u32]| -> BigInt { row.iter().zip(w).map(|(a, &k)| a * k).sum() };
            if dot(u) != dot(v) {
                return Err(GkzError::NotToric);
            }
        }
        Ok(DifferentialOperator::from_terms(
            self.weyl_algebra(),
            [
                (vec![0; n], u.to_vec(), Rational::one()),
                (vec![0; n], v.to_vec(), Rational::integer(-1)),
            ],
        )?)
    }
}

/// Whether `a = c * b` for some non-zero rational `c`. CAS output may scale or
/// negate generators, so comparisons of single generators are made up to units.
pub fn proportional(a: &DifferentialOperator, b: &DifferentialOperator) -> bool {
    let (da, db) = (a.to_data(), b.to_data());
    if da.len() != db.len() || da.is_empty() {
        return a.is_zero() && b.is_zero();
    }
    let ratio = |x: &algebramesh_math_types::WeylTermData,
                 y: &algebramesh_math_types::WeylTermData| {
        Rational(&x.coefficient.0 / &y.coefficient.0)
    };
    let r = ratio(&da[0], &db[0]);
    da.iter().zip(&db).all(|(x, y)| {
        x.x_exponents == y.x_exponents && x.d_exponents == y.d_exponents && ratio(x, y) == r
    })
}

impl GkzInput {
    /// Whether `operators` are, up to units and order, exactly the Euler operators
    /// `sum_j a_ij x_j d_j - beta_i`.
    pub fn euler_operators_match(
        &self,
        operators: &[DifferentialOperator],
    ) -> Result<bool, GkzError> {
        let ours = self.euler_operators()?;
        Ok(operators.len() == ours.len()
            && operators
                .iter()
                .all(|e| ours.iter().any(|o| proportional(e, o)))
            && ours
                .iter()
                .all(|o| operators.iter().any(|e| proportional(e, o))))
    }

    /// Whether every operator is, up to a unit, a binomial `d^u - d^v` with
    /// `A u = A v` and no `x`. This does not decide whether they generate `I_A`.
    pub fn toric_binomials_valid(&self, operators: &[DifferentialOperator]) -> bool {
        operators.iter().all(|t| {
            let data = t.to_data();
            data.len() == 2
                && data.iter().all(|d| d.x_exponents.iter().all(|&k| k == 0))
                && self
                    .toric_binomial(&data[0].d_exponents, &data[1].d_exponents)
                    .is_ok_and(|b| proportional(&b, t))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Design §9.2: A = [[1,1,1],[0,1,2]], beta = [3,4].
    fn example() -> GkzInput {
        serde_json::from_str(r#"{"A": [[1,1,1],[0,1,2]], "beta": ["3", "4"]}"#).unwrap()
    }

    #[test]
    fn design_example_operators() {
        let input = example();
        input.validate().unwrap();
        let euler: Vec<String> = input
            .euler_operators()
            .unwrap()
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(euler, ["x1*d1+x2*d2+x3*d3-3", "x2*d2+2*x3*d3-4"]);
        let toric = input.toric_binomial(&[1, 0, 1], &[0, 2, 0]).unwrap();
        assert_eq!(toric.to_string(), "d1*d3-d2^2");
    }

    #[test]
    fn checks_cas_output_up_to_units() {
        let input = example();
        let mut euler = input.euler_operators().unwrap();
        euler.reverse();
        let negated = euler[0].neg();
        assert!(
            input
                .euler_operators_match(&[negated, euler[1].clone()])
                .unwrap()
        );
        assert!(!input.euler_operators_match(&euler[..1]).unwrap());
        let toric = input.toric_binomial(&[0, 2, 0], &[1, 0, 1]).unwrap();
        assert!(input.toric_binomials_valid(&[toric.clone(), toric.neg()]));
        assert!(!input.toric_binomials_valid(&euler));
    }

    #[test]
    fn rejects_non_toric_binomial() {
        assert_eq!(
            example().toric_binomial(&[1, 0, 0], &[0, 1, 0]),
            Err(GkzError::NotToric)
        );
    }

    #[test]
    fn diagnoses_invalid_inputs() {
        let a = IntegerMatrix::from_i64(&[&[1, 1, 1], &[0, 1, 2]]).unwrap();
        assert_eq!(
            GkzInput::new(a, vec![Rational::one()]),
            Err(GkzError::BetaLength {
                expected: 2,
                got: 1
            })
        );
        let deficient = IntegerMatrix::from_i64(&[&[1, 2, 3], &[2, 4, 6]]).unwrap();
        assert_eq!(
            GkzInput::new(deficient, vec![Rational::one(), Rational::one()]),
            Err(GkzError::RankDeficient { rank: 1, rows: 2 })
        );
        assert!(serde_json::from_str::<GkzInput>(r#"{"A": [], "beta": []}"#).is_err());
    }
}

//! Typed mathematical data shared by all backends (design §6).
//!
//! Rules enforced here:
//! 1. Rationals are stored reduced with a positive denominator and are never
//!    converted to floating point.
//! 2. Coefficient domain, variables, their order and the monomial order are explicit.
//! 3. Weyl algebra elements are a separate type from commutative polynomials; there is
//!    no implicit conversion between them.
//! 4. Equality is structural on canonical forms, never on display strings.

mod error;
mod matrix;
mod polymatrix;
mod polynomial;
mod ring;
mod scalar;
mod weyl;

pub use error::MathError;
pub use matrix::{IntegerMatrix, MAX_RANK_COLS, MAX_RANK_ROWS, RationalMatrix};
pub use polymatrix::{EntryData, PolynomialMatrix, PolynomialMatrixData, PolynomialMatrixKind};
pub use polynomial::{
    Factor, Factorization, Ideal, IdealData, IdealKind, Polynomial, PolynomialData, PolynomialKind,
    TermData,
};
pub use ring::{BaseOrder, CoefficientDomain, MAX_WEIGHT, MonomialOrder, OrderBlock, RingSpec};
pub use scalar::{PowerBudget, Rational, parse_rational};
pub use weyl::{DModuleIdeal, DifferentialOperator, WeylAlgebraSpec, WeylTermData};

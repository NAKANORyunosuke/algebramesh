//! In-memory representation of the CMO objects supported so far (design §5.3, stage 1).
//!
//! Any other CMO tag is rejected by the decoder with [`crate::DecodeError::UnsupportedCmo`];
//! it is never converted to a string and never skipped.

use num_bigint::BigInt;

use crate::constants::cmo_tag;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cmo {
    Null,
    /// `CMO_INT32`. Asir sends unsigned values here (e.g. mathcap entries), so callers
    /// that expect an unsigned value should use [`Cmo::as_u32`].
    Int32(i32),
    Datum(Vec<u8>),
    String(String),
    Mathcap(Box<Cmo>),
    List(Vec<Cmo>),
    Zz(BigInt),
    /// `CMO_QQ`, kept exactly as received; the denominator is non-zero but not
    /// necessarily positive or coprime to the numerator.
    Qq {
        num: BigInt,
        den: BigInt,
    },
    Zero,
    Error2(Box<Cmo>),
    Indeterminate(Box<Cmo>),
    /// `CMO_RECURSIVE_POLYNOMIAL`: a variable list (sent as a `CMO_LIST`) and a body
    /// whose `CMO_UNIVARIATE_POLYNOMIAL` nodes index into that list.
    RecursivePolynomial {
        variables: Vec<Cmo>,
        body: Box<Cmo>,
    },
    /// `CMO_UNIVARIATE_POLYNOMIAL`: `(exponent, coefficient)` pairs in the variable
    /// with index `variable` of the enclosing recursive polynomial.
    UnivariatePolynomial {
        variable: u32,
        terms: Vec<(u32, Cmo)>,
    },
}

impl Cmo {
    pub fn tag(&self) -> u32 {
        match self {
            Self::Null => cmo_tag::NULL,
            Self::Int32(_) => cmo_tag::INT32,
            Self::Datum(_) => cmo_tag::DATUM,
            Self::String(_) => cmo_tag::STRING,
            Self::Mathcap(_) => cmo_tag::MATHCAP,
            Self::List(_) => cmo_tag::LIST,
            Self::Zz(_) => cmo_tag::ZZ,
            Self::Qq { .. } => cmo_tag::QQ,
            Self::Zero => cmo_tag::ZERO,
            Self::Error2(_) => cmo_tag::ERROR2,
            Self::Indeterminate(_) => cmo_tag::INDETERMINATE,
            Self::RecursivePolynomial { .. } => cmo_tag::RECURSIVE_POLYNOMIAL,
            Self::UnivariatePolynomial { .. } => cmo_tag::UNIVARIATE_POLYNOMIAL,
        }
    }

    pub fn string(s: impl Into<String>) -> Self {
        Self::String(s.into())
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_list(&self) -> Option<&[Cmo]> {
        match self {
            Self::List(items) => Some(items),
            _ => None,
        }
    }

    /// Reads `CMO_INT32` as the unsigned word it carries on the wire.
    pub fn as_u32(&self) -> Option<u32> {
        match self {
            Self::Int32(v) => Some(*v as u32),
            _ => None,
        }
    }

    pub fn is_error(&self) -> bool {
        matches!(self, Self::Error2(_))
    }
}

use thiserror::Error;

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum MathError {
    #[error("invalid rational number {0:?}")]
    InvalidRational(String),
    #[error("invalid variable name {0:?}")]
    InvalidVariable(String),
    #[error("duplicate variable {0:?}")]
    DuplicateVariable(String),
    #[error("exponent vector of length {got}, but the ring has {expected} variables")]
    ExponentLength { expected: usize, got: usize },
    #[error("duplicate exponent vector {0:?}")]
    DuplicateExponent(Vec<u32>),
    #[error("coefficient {coefficient} is not in {domain}")]
    CoefficientNotInDomain {
        coefficient: String,
        domain: &'static str,
    },
    #[error("operands belong to different rings")]
    RingMismatch,
    #[error("variable {0:?} does not belong to the ring")]
    UnknownVariable(String),
    #[error("matrix rows have different lengths")]
    RaggedMatrix,
    /// An exponent of a product or power would exceed `u32::MAX`.
    #[error("an exponent of the result exceeds {max} (exponents are 32-bit)", max = u32::MAX)]
    ExponentOverflow,
    #[error("{0}")]
    Invalid(String),
    /// Larger than a bounded computation accepts (ADR 0019).
    #[error("too large: {0}")]
    TooLarge(String),
}

//! What the operations with a certificate share (ADR 0018): a certificate in
//! a value, or the note that the checker left it out for its size.

use serde::{Deserialize, Serialize};

/// A certificate, or the note that it was left out because it was too large.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Certificate<T> {
    Omitted { omitted: String },
    Given(T),
}

/// The value written in place of a certificate too large for a result.
pub const CERTIFICATE_TOO_LARGE: &str = "certificate_too_large";

//! `gkz.generate`, `gkz.indicial` and `gkz.series` (ADR 0007, 0011, 0015).
//! Their results are given as JSON until the GKZ package (see the crate's
//! documentation).

use algebramesh_math_types::{IntegerMatrix, Rational, WeylTermData};
use serde::{Deserialize, Serialize};

pub const GKZ_GENERATE: &str = "gkz.generate";
pub const GKZ_INDICIAL: &str = "gkz.indicial";
pub const GKZ_SERIES: &str = "gkz.series";

/// The arguments of `gkz.generate`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GkzGenerateArguments {
    #[serde(rename = "A")]
    pub a: IntegerMatrix,
    pub beta: Vec<Rational>,
    /// Operators expected to lie in the GKZ ideal, checked by the CAS.
    #[serde(default)]
    pub expected: Vec<Vec<WeylTermData>>,
}

/// The arguments of `gkz.indicial`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GkzIndicialArguments {
    #[serde(rename = "A")]
    pub a: IntegerMatrix,
    pub beta: Vec<Rational>,
    pub weight: Vec<i64>,
}

/// The arguments of `gkz.series`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GkzSeriesArguments {
    #[serde(rename = "A")]
    pub a: IntegerMatrix,
    pub beta: Vec<Rational>,
    pub weight: Vec<i64>,
    /// Terms with `weight . u <= order` are computed.
    pub order: u32,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn the_order_belongs_to_the_series_only() {
        let a = serde_json::json!([[1, 1, 1], [0, 1, 2]]);
        let with_order =
            serde_json::json!({"A": a, "beta": ["1", "2"], "weight": [1, 0, 1], "order": 3});
        assert!(serde_json::from_value::<GkzIndicialArguments>(with_order.clone()).is_err());
        assert!(serde_json::from_value::<GkzSeriesArguments>(with_order).is_ok());
        let without = serde_json::json!({"A": a, "beta": ["1", "2"], "weight": [1, 0, 1]});
        assert!(serde_json::from_value::<GkzSeriesArguments>(without).is_err());
        let generate = serde_json::json!({"A": a, "beta": ["1", "2"], "x": 1});
        assert!(serde_json::from_value::<GkzGenerateArguments>(generate).is_err());
    }
}

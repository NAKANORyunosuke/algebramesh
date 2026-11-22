//! `simplicial.homology` (Issue #103, ADR 0030): the homology over ZZ of a
//! simplicial complex given by its facets. The topology package
//! (`algebramesh-topology`) checks the complex, builds its boundary matrices,
//! builds the value from the Smith normal forms a CAS returns and checks it;
//! its result is given here as JSON.

use algebramesh_math_types::CoefficientDomain;
use serde::{Deserialize, Serialize};

use crate::matrix::CertificateRequest;

pub const SIMPLICIAL_HOMOLOGY: &str = "simplicial.homology";

/// A simplicial complex by its facets, each a set of vertices numbered by
/// non-negative integers. Read as it is written; `algebramesh-topology`
/// checks it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SimplicialComplexData {
    #[serde(rename = "type")]
    pub kind: SimplicialComplexKind,
    pub facets: Vec<Vec<u32>>,
}

/// The `"type"` field of [`SimplicialComplexData`], which must be
/// `"simplicial_complex"`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SimplicialComplexKind {
    #[default]
    #[serde(rename = "simplicial_complex")]
    SimplicialComplex,
}

/// The arguments of `simplicial.homology`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SimplicialHomologyArguments {
    pub complex: SimplicialComplexData,
    /// The coefficients; ZZ only (GF(p) after A5).
    pub domain: CoefficientDomain,
    /// Whether the Smith normal forms come with their certificates.
    #[serde(default)]
    pub certificate: CertificateRequest,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn the_complex_is_typed() {
        let args: SimplicialHomologyArguments = serde_json::from_value(serde_json::json!({
            "complex": {"type": "simplicial_complex", "facets": [[0, 1], [1, 2]]},
            "domain": "ZZ",
        }))
        .unwrap();
        assert_eq!(args.certificate, CertificateRequest::Include);
        assert_eq!(args.complex.facets, [[0, 1], [1, 2]]);
        let untyped = serde_json::json!({"complex": {"facets": [[0]]}, "domain": "ZZ"});
        assert!(serde_json::from_value::<SimplicialHomologyArguments>(untyped).is_err());
        let other = serde_json::json!({"complex": {"type": "ideal", "facets": [[0]]},
                                       "domain": "ZZ"});
        assert!(serde_json::from_value::<SimplicialHomologyArguments>(other).is_err());
        let negative = serde_json::json!({
            "complex": {"type": "simplicial_complex", "facets": [[-1]]}, "domain": "ZZ"});
        assert!(serde_json::from_value::<SimplicialHomologyArguments>(negative).is_err());
    }
}

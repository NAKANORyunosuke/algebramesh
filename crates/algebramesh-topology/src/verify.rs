//! The checks of `simplicial.homology` (Issue #103, `docs/claims.md`): the
//! complex is read from the job, the boundary matrices are built again from
//! it, and the value is checked against them.
//!
//! | claim | statement |
//! |---|---|
//! | `homology.boundary` | the face counts are those of the complex, and ∂_k ∂_(k+1) = 0 |
//! | `homology.snf_form` | each diagonal has min(f_(k-1), f_k) entries d_i >= 0 with d_1 \| d_2 \| ... |
//! | `homology.snf_equivalent` | diag(d^(k)) = P_k ∂_k Q_k with P_k, Q_k unimodular |
//! | `homology.groups` | H_k is the group the diagonals give (a theorem on the three above) |
//! | `homology.euler_characteristic` | Σ (-1)^k f_k, equal to Σ (-1)^k rank H_k |
//!
//! [`register`] adds the checker to a [`Registry`]; an agent built with the
//! topology package does so.

use algebramesh_core::guarantee::{Claim, Method};
use algebramesh_core::{CasError, JobSpec};
use algebramesh_math_types::{CoefficientDomain, RationalMatrix};
use algebramesh_ops::certificate::Certificate;
use algebramesh_ops::matrix::CertificateRequest;
use algebramesh_ops::topology::{SIMPLICIAL_HOMOLOGY, SimplicialHomologyArguments};
use algebramesh_verify::{Context, OperationVerifier, Registry, certificate, matrix, missing};

use crate::value::{HomologyValue, groups};
use crate::{SimplicialComplex, alternating_sum};

/// Adds the check of `simplicial.homology` to `registry`.
pub fn register(registry: &mut Registry) {
    registry.register(Box::new(HomologyVerifier));
}

/// The standard checks and that of the topology package.
pub fn registry() -> Registry {
    let mut registry = Registry::standard();
    register(&mut registry);
    registry
}

const OP: &str = SIMPLICIAL_HOMOLOGY;

fn refuse(message: impl std::fmt::Display) -> CasError {
    CasError::UnsupportedResult(format!("{OP}: {message}"))
}

/// `a * b`, stopping at the checkpoints of the job.
fn product(
    a: &RationalMatrix,
    b: &RationalMatrix,
    context: &Context<'_>,
) -> Result<RationalMatrix, CasError> {
    enum Stop {
        Interrupted(CasError),
        Math(algebramesh_math_types::MathError),
    }
    impl From<algebramesh_math_types::MathError> for Stop {
        fn from(e: algebramesh_math_types::MathError) -> Self {
            Self::Math(e)
        }
    }
    a.mul_with(b, &mut || {
        context.interrupt.checkpoint().map_err(Stop::Interrupted)
    })
    .map_err(|e| match e {
        Stop::Interrupted(e) => e,
        Stop::Math(e) => refuse(e),
    })
}

/// The checks of `simplicial.homology`.
pub struct HomologyVerifier;

impl OperationVerifier for HomologyVerifier {
    fn operation(&self) -> &'static str {
        OP
    }

    fn verify(
        &self,
        spec: &JobSpec,
        value_json: &serde_json::Value,
        context: &Context<'_>,
    ) -> Result<Vec<Claim>, CasError> {
        let args: SimplicialHomologyArguments = serde_json::from_value(spec.arguments.clone())
            .map_err(|e| refuse(format!("the job's arguments: {e}")))?;
        if args.domain != CoefficientDomain::Integers {
            return Err(refuse("the homology is over ZZ only"));
        }
        let complex = SimplicialComplex::from_data(&args.complex)
            .map_err(|e| refuse(format!("the job's complex: {e}")))?;
        let value: HomologyValue = serde_json::from_value(value_json.clone())
            .map_err(|e| refuse(format!("the value cannot be read: {e}")))?;

        // homology.boundary: the matrices are built here from the spec.
        let counts = complex.face_counts();
        if value.face_counts != counts {
            return Err(refuse(format!(
                "the face counts {:?} are not those of the complex, {counts:?}",
                value.face_counts
            )));
        }
        let boundaries = complex.boundaries();
        for (k, pair) in boundaries.windows(2).enumerate() {
            if !product(&pair[0], &pair[1], context)?.is_zero() {
                return Err(refuse(format!("∂_{} ∂_{} is not zero", k + 1, k + 2)));
            }
        }
        let mut claims = vec![context.checked("homology.boundary", Method::Recomputation)];

        // homology.snf_form
        if value.smith_diagonals.len() != boundaries.len() {
            return Err(refuse(format!(
                "{} Smith diagonals for {} boundary matrices",
                value.smith_diagonals.len(),
                boundaries.len()
            )));
        }
        for (k, (d, b)) in value.smith_diagonals.iter().zip(&boundaries).enumerate() {
            if d.len() != b.nrows().min(b.ncols()) || !certificate::is_smith_diagonal(d) {
                return Err(refuse(format!(
                    "the diagonal of ∂_{} is not of min(f_{}, f_{}) non-negative integers each \
                     dividing the next",
                    k + 1,
                    k,
                    k + 1
                )));
            }
        }
        claims.push(context.checked("homology.snf_form", Method::SelfCertifying));

        // homology.snf_equivalent
        match missing(
            OP,
            &value.certificate,
            args.certificate == CertificateRequest::None,
            context,
        )? {
            Some(reason) => claims.push(context.asserted("homology.snf_equivalent", reason)),
            None => {
                let Some(Certificate::Given(c)) = &value.certificate else {
                    unreachable!("a missing certificate is handled above")
                };
                if c.boundaries.len() != boundaries.len() {
                    return Err(refuse(format!(
                        "{} certificates for {} boundary matrices",
                        c.boundaries.len(),
                        boundaries.len()
                    )));
                }
                for (k, ((b, d), t)) in boundaries
                    .iter()
                    .zip(&value.smith_diagonals)
                    .zip(&c.boundaries)
                    .enumerate()
                {
                    matrix::check_smith_equivalence(
                        &format!("{OP} (∂_{})", k + 1),
                        b,
                        d,
                        t,
                        context,
                    )?;
                }
                claims.push(context.checked("homology.snf_equivalent", Method::Certificate));
            }
        }

        // homology.groups, by the theorem on the three claims above.
        let expected = groups(&counts, &value.smith_diagonals)
            .ok_or_else(|| refuse("the ranks of the Smith normal forms exceed the faces"))?;
        if value.homology != expected {
            return Err(refuse(
                "the homology groups are not those the Smith normal forms give",
            ));
        }
        claims.push(context.theorem(
            "homology.groups",
            &[
                "homology.boundary",
                "homology.snf_form",
                "homology.snf_equivalent",
            ],
        ));

        // homology.euler_characteristic: recomputed from the faces, and
        // compared with the ranks of the groups (a consistency).
        let chi = alternating_sum(&counts);
        if value.euler_characteristic != chi {
            return Err(refuse(format!(
                "the Euler characteristic is {}, not Σ (-1)^k f_k = {chi}",
                value.euler_characteristic
            )));
        }
        let ranks: Vec<usize> = value.homology.iter().map(|h| h.rank).collect();
        if alternating_sum(&ranks) != chi {
            return Err(refuse(format!(
                "Σ (-1)^k rank H_k = {} differs from the Euler characteristic {chi}",
                alternating_sum(&ranks)
            )));
        }
        claims.push(context.checked("homology.euler_characteristic", Method::Recomputation));
        Ok(claims)
    }

    fn not_claimed(&self) -> Vec<String> {
        [
            "cycles that represent generators of the homology groups",
            "the uniqueness of the transforms P_k and Q_k",
            "anything about a space the complex is meant to triangulate",
        ]
        .map(String::from)
        .to_vec()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn the_checker_is_registered() {
        assert!(registry().checks(OP));
        assert!(!Registry::standard().checks(OP));
    }
}

//! The value of `simplicial.homology`, built from the Smith normal forms of
//! the boundary matrices that a CAS returns.
//!
//! With d^(k) the diagonal of the Smith normal form of `∂_k` and r_k the
//! number of its non-zero entries (r_0 = r_(d+1) = 0), H_k is
//! ZZ^(f_k - r_k - r_(k+1)) plus ZZ/t for each entry t > 1 of d^(k+1).
//! [`crate::verify`] checks the forms and derives the groups again.

use algebramesh_math_types::Rational;
use algebramesh_ops::certificate::Certificate;
use algebramesh_ops::matrix::{SmithCertificate, SmithResult};
use serde::{Deserialize, Serialize};

use crate::{SimplicialComplex, alternating_sum};

/// The value of `simplicial.homology`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HomologyValue {
    #[serde(rename = "type")]
    pub kind: HomologyKind,
    /// f_0, ..., f_d: the number of faces of each dimension.
    pub face_counts: Vec<usize>,
    /// The diagonal of the Smith normal form of `∂_k`, for k = 1, ..., d,
    /// each of min(f_(k-1), f_k) entries.
    pub smith_diagonals: Vec<Vec<Rational>>,
    /// H_0, ..., H_d.
    pub homology: Vec<HomologyGroup>,
    /// Σ_k (-1)^k f_k.
    pub euler_characteristic: i64,
    /// P_k and Q_k with diag(d^(k)) = P_k ∂_k Q_k, for k = 1, ..., d, unless
    /// the job asked for none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub certificate: Option<Certificate<BoundaryCertificates>>,
}

/// The certificates of the Smith normal forms of the boundary matrices.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoundaryCertificates {
    /// For `∂_1, ..., ∂_d`.
    pub boundaries: Vec<SmithCertificate>,
}

/// H_k = ZZ^rank plus ZZ/t for each t of `torsion`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HomologyGroup {
    pub rank: usize,
    /// The torsion coefficients, each > 1 and dividing the next.
    pub torsion: Vec<Rational>,
}

/// The `"type"` field, which must be `"simplicial_homology"`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum HomologyKind {
    #[default]
    #[serde(rename = "simplicial_homology")]
    Value,
}

/// H_0, ..., H_d from the face counts and the diagonals of `∂_1, ..., ∂_d`:
/// `None` when the diagonals have not one per boundary matrix, or their
/// non-zero entries are more than the faces allow (no chain complex has
/// such forms).
pub fn groups(face_counts: &[usize], diagonals: &[Vec<Rational>]) -> Option<Vec<HomologyGroup>> {
    if face_counts.is_empty() || diagonals.len() != face_counts.len() - 1 {
        return None;
    }
    // r[k] = rank of ∂_k, with r[0] = r[d + 1] = 0.
    let mut r = vec![0usize; face_counts.len() + 1];
    for (k, d) in diagonals.iter().enumerate() {
        r[k + 1] = d.iter().filter(|x| !x.is_zero()).count();
    }
    face_counts
        .iter()
        .enumerate()
        .map(|(k, &f)| {
            let rank = f.checked_sub(r[k])?.checked_sub(r[k + 1])?;
            let torsion = diagonals
                .get(k)
                .map(|d| {
                    d.iter()
                        .filter(|x| **x > Rational::one())
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            Some(HomologyGroup { rank, torsion })
        })
        .collect()
}

/// The value for `complex` from the Smith normal forms of its boundary
/// matrices `∂_1, ..., ∂_d`, in that order. Each form has a certificate
/// exactly when `certificate` is true. An error is a reply that does not
/// fit the complex; whether the forms are right is checked after the
/// backend ([`crate::verify`]).
pub fn homology_value(
    complex: &SimplicialComplex,
    forms: Vec<SmithResult>,
    certificate: bool,
) -> Result<HomologyValue, String> {
    let counts = complex.face_counts();
    if forms.len() != complex.dimension() {
        return Err(format!(
            "{} Smith normal forms for {} boundary matrices",
            forms.len(),
            complex.dimension()
        ));
    }
    let mut diagonals = Vec::with_capacity(forms.len());
    let mut certificates = Vec::with_capacity(forms.len());
    for (k, form) in forms.into_iter().enumerate() {
        let size = counts[k].min(counts[k + 1]);
        if form.diagonal.len() != size {
            return Err(format!(
                "the Smith normal form of ∂_{} has {} diagonal entries, not {size}",
                k + 1,
                form.diagonal.len()
            ));
        }
        diagonals.push(form.diagonal);
        match (form.certificate, certificate) {
            (Some(Certificate::Given(c)), true) => certificates.push(c),
            (None, false) => {}
            (Some(Certificate::Omitted { .. }), _) => {
                return Err("the driver left a certificate out; only the checker does".into());
            }
            (Some(_), false) => return Err("a certificate the job did not ask for".into()),
            (None, true) => {
                return Err(format!(
                    "the job asked for certificates and the form of ∂_{} has none",
                    k + 1
                ));
            }
        }
    }
    let homology = groups(&counts, &diagonals).ok_or_else(|| {
        "the ranks of the Smith normal forms exceed the numbers of faces".to_owned()
    })?;
    Ok(HomologyValue {
        kind: HomologyKind::Value,
        euler_characteristic: alternating_sum(&counts),
        face_counts: counts,
        smith_diagonals: diagonals,
        homology,
        certificate: certificate.then_some(Certificate::Given(BoundaryCertificates {
            boundaries: certificates,
        })),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn q(n: i64) -> Rational {
        Rational::integer(n)
    }

    /// The real projective plane with 6 vertices: f = (6, 15, 10), ∂_1 of
    /// rank 5, ∂_2 with diagonal (1, ..., 1, 2) of nine 1s.
    #[test]
    fn the_groups_of_the_projective_plane() {
        let mut d2 = vec![q(1); 9];
        d2.push(q(2));
        let mut d1 = vec![q(1); 5];
        d1.push(q(0));
        let h = groups(&[6, 15, 10], &[d1, d2]).unwrap();
        assert_eq!(
            h,
            [
                HomologyGroup {
                    rank: 1,
                    torsion: vec![]
                },
                HomologyGroup {
                    rank: 0,
                    torsion: vec![q(2)]
                },
                HomologyGroup {
                    rank: 0,
                    torsion: vec![]
                },
            ]
        );
    }

    #[test]
    fn ranks_beyond_the_faces_give_no_groups() {
        // ∂_1 of rank 2 and ∂_2 of rank 2 on 3 edges.
        assert_eq!(
            groups(&[3, 3, 2], &[vec![q(1), q(1), q(0)], vec![q(1), q(1)]]),
            None
        );
        assert_eq!(groups(&[3, 3], &[]), None);
        assert_eq!(groups(&[], &[]), None);
    }
}

//! Simplicial homology over ZZ (Issue #103, ADR 0030).
//!
//! A simplicial complex is given by its facets, each a set of vertices
//! numbered by non-negative integers. This crate checks the complex,
//! enumerates its faces under a bound on their number ([`MAX_FACES`],
//! [`MAX_FACE_WORK`]) and builds its signed boundary matrices; a CAS computes
//! the Smith normal form of each matrix with a certificate, [`value`] builds
//! the value of `simplicial.homology` from them, and [`verify`] checks it.
//!
//! The faces of dimension k are the subsets of k + 1 vertices of the facets,
//! each written with its vertices in increasing order, and ordered
//! lexicographically as such lists. The boundary matrix `∂_k` (k ≥ 1) has a
//! row for each face of dimension k - 1 and a column for each face of
//! dimension k, in that order; the column of `[v_0, ..., v_k]` has `(-1)^i`
//! in the row of `[v_0, ..., v_k]` without `v_i`, and 0 elsewhere.
//!
//! It is the topology package (`docs/packages/topology.md`): it does no IO.

pub mod value;
pub mod verify;

use std::collections::{BTreeMap, BTreeSet};

use algebramesh_math_types::{Rational, RationalMatrix};
use algebramesh_ops::topology::SimplicialComplexData;
use thiserror::Error;

/// The most faces, of all dimensions together, a complex may have.
pub const MAX_FACES: usize = 512;

/// The most subsets of facets the enumeration of the faces may visit: the
/// sum over the facets F of 2^|F| - 1. A facet of dimension k has
/// 2^(k+1) - 1 faces, and faces shared by facets are visited once for each.
pub const MAX_FACE_WORK: u64 = 16_384;

/// Why a complex is refused. Inputs are reported, never silently normalized.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum TopologyError {
    #[error("the complex has no facets")]
    NoFacets,
    #[error("facet {index} has no vertices")]
    EmptyFacet { index: usize },
    #[error("facet {index} has the vertex {vertex} twice")]
    RepeatedVertex { index: usize, vertex: u32 },
    #[error("facet {inner} is contained in facet {outer}; only the facets are given")]
    NotAFacet { inner: usize, outer: usize },
    #[error("input too large: {0}")]
    TooLarge(String),
}

/// A simplicial complex with its faces enumerated: `faces[k]` are the faces
/// of dimension k, in the order of the crate's documentation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimplicialComplex {
    faces: Vec<Vec<Vec<u32>>>,
}

impl SimplicialComplex {
    /// The complex with these facets. Every facet is a non-empty set of
    /// vertices (in any order, each once), none is contained in another, and
    /// the faces are within [`MAX_FACES`] and [`MAX_FACE_WORK`].
    pub fn new(facets: &[Vec<u32>]) -> Result<Self, TopologyError> {
        if facets.is_empty() {
            return Err(TopologyError::NoFacets);
        }
        if facets.len() > MAX_FACES {
            return Err(TopologyError::TooLarge(format!(
                "{} facets; at most {MAX_FACES} faces are accepted",
                facets.len()
            )));
        }
        let mut sorted = Vec::with_capacity(facets.len());
        let mut work: u64 = 0;
        for (index, facet) in facets.iter().enumerate() {
            if facet.is_empty() {
                return Err(TopologyError::EmptyFacet { index });
            }
            let mut f = facet.clone();
            f.sort_unstable();
            if let Some(w) = f.windows(2).find(|w| w[0] == w[1]) {
                return Err(TopologyError::RepeatedVertex {
                    index,
                    vertex: w[0],
                });
            }
            // 2^|F| - 1 subsets; a facet of 15 or more vertices alone is
            // over the bound, so the shift cannot overflow.
            if f.len() >= 15 {
                return Err(too_much_work());
            }
            work += (1u64 << f.len()) - 1;
            if work > MAX_FACE_WORK {
                return Err(too_much_work());
            }
            sorted.push(f);
        }
        for (inner, f) in sorted.iter().enumerate() {
            for (outer, g) in sorted.iter().enumerate() {
                if inner != outer && f.len() <= g.len() && is_subset(f, g) {
                    // Of two equal facets, the second is reported as inside
                    // the first.
                    if f.len() < g.len() || inner > outer {
                        return Err(TopologyError::NotAFacet { inner, outer });
                    }
                }
            }
        }
        let dimension = sorted.iter().map(Vec::len).max().unwrap_or(1) - 1;
        let mut faces = vec![BTreeSet::new(); dimension + 1];
        for f in &sorted {
            // Every non-empty subset, by the bits of a mask.
            for mask in 1u32..(1u32 << f.len()) {
                let face: Vec<u32> = f
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| mask & (1 << i) != 0)
                    .map(|(_, v)| *v)
                    .collect();
                faces[face.len() - 1].insert(face);
            }
        }
        let total: usize = faces.iter().map(BTreeSet::len).sum();
        if total > MAX_FACES {
            return Err(TopologyError::TooLarge(format!(
                "the complex has {total} faces; at most {MAX_FACES} are accepted"
            )));
        }
        Ok(Self {
            faces: faces.into_iter().map(|s| s.into_iter().collect()).collect(),
        })
    }

    /// The complex of the JSON of an argument.
    pub fn from_data(data: &SimplicialComplexData) -> Result<Self, TopologyError> {
        Self::new(&data.facets)
    }

    /// The largest dimension of a face.
    pub fn dimension(&self) -> usize {
        self.faces.len() - 1
    }

    /// The faces of dimension `k`, in order; none above the dimension.
    pub fn faces(&self, k: usize) -> &[Vec<u32>] {
        self.faces.get(k).map_or(&[], Vec::as_slice)
    }

    /// The number of faces of each dimension 0, ..., [`Self::dimension`].
    pub fn face_counts(&self) -> Vec<usize> {
        self.faces.iter().map(Vec::len).collect()
    }

    /// Σ_k (-1)^k f_k, with f_k the number of faces of dimension k.
    pub fn euler_characteristic(&self) -> i64 {
        alternating_sum(&self.face_counts())
    }

    /// The boundary matrix `∂_k` for `1 <= k <= dimension`: f_(k-1) rows and
    /// f_k columns, in the order of the faces.
    pub fn boundary(&self, k: usize) -> RationalMatrix {
        let (rows, columns) = (self.faces(k - 1), self.faces(k));
        let index: BTreeMap<&[u32], usize> = rows
            .iter()
            .enumerate()
            .map(|(i, f)| (f.as_slice(), i))
            .collect();
        let mut entries = vec![vec![Rational::zero(); columns.len()]; rows.len()];
        for (j, face) in columns.iter().enumerate() {
            for i in 0..face.len() {
                let mut side = face.clone();
                side.remove(i);
                let row = index[side.as_slice()];
                entries[row][j] = Rational::integer(if i % 2 == 0 { 1 } else { -1 });
            }
        }
        RationalMatrix::new(entries)
            .and_then(|m| m.with_shape(rows.len(), columns.len()))
            .unwrap_or_else(|_| unreachable!("the rows have one entry per column"))
    }

    /// `∂_1, ..., ∂_d` for the dimension d.
    pub fn boundaries(&self) -> Vec<RationalMatrix> {
        (1..=self.dimension()).map(|k| self.boundary(k)).collect()
    }
}

fn too_much_work() -> TopologyError {
    TopologyError::TooLarge(format!(
        "the facets have more than {MAX_FACE_WORK} subsets in all"
    ))
}

/// Whether the sorted `a` is a subset of the sorted `b`.
fn is_subset(a: &[u32], b: &[u32]) -> bool {
    let mut b = b.iter();
    a.iter().all(|x| b.any(|y| y == x))
}

/// Σ_k (-1)^k c_k; the counts are bounded by [`MAX_FACES`].
pub fn alternating_sum(counts: &[usize]) -> i64 {
    counts
        .iter()
        .enumerate()
        .map(|(k, &c)| {
            let c = i64::try_from(c).unwrap_or(i64::MAX);
            if k % 2 == 0 { c } else { -c }
        })
        .sum()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn entries(m: &RationalMatrix) -> Vec<Vec<i64>> {
        m.rows()
            .iter()
            .map(|r| {
                r.iter()
                    .map(|x| i64::try_from(x.numer()).unwrap())
                    .collect()
            })
            .collect()
    }

    /// The boundary of the triangle [0, 1, 2], written out by hand:
    /// ∂[0,1,2] = [1,2] - [0,2] + [0,1], ∂[a,b] = [b] - [a].
    #[test]
    fn the_boundary_matrices_of_a_triangle() {
        let c = SimplicialComplex::new(&[vec![2, 0, 1]]).unwrap();
        assert_eq!(c.dimension(), 2);
        assert_eq!(c.face_counts(), [3, 3, 1]);
        assert_eq!(c.faces(1), [vec![0, 1], vec![0, 2], vec![1, 2]]);
        assert_eq!(
            entries(&c.boundary(1)),
            [vec![-1, -1, 0], vec![1, 0, -1], vec![0, 1, 1]]
        );
        assert_eq!(entries(&c.boundary(2)), [vec![1], vec![-1], vec![1]]);
        assert_eq!(c.euler_characteristic(), 1);
    }

    #[test]
    fn faces_are_shared_between_facets() {
        // The boundary of the tetrahedron: 4 + 6 + 4 faces.
        let c =
            SimplicialComplex::new(&[vec![0, 1, 2], vec![0, 1, 3], vec![0, 2, 3], vec![1, 2, 3]])
                .unwrap();
        assert_eq!(c.face_counts(), [4, 6, 4]);
        assert_eq!(c.euler_characteristic(), 2);
        let points = SimplicialComplex::new(&[vec![5], vec![3]]).unwrap();
        assert_eq!(points.dimension(), 0);
        assert_eq!(points.faces(0), [vec![3], vec![5]]);
        assert!(points.boundaries().is_empty());
    }

    #[test]
    fn invalid_complexes_are_refused() {
        assert_eq!(SimplicialComplex::new(&[]), Err(TopologyError::NoFacets));
        assert_eq!(
            SimplicialComplex::new(&[vec![0, 1], vec![]]),
            Err(TopologyError::EmptyFacet { index: 1 })
        );
        assert_eq!(
            SimplicialComplex::new(&[vec![0, 1, 0]]),
            Err(TopologyError::RepeatedVertex {
                index: 0,
                vertex: 0
            })
        );
        assert_eq!(
            SimplicialComplex::new(&[vec![0, 1], vec![0, 1, 2]]),
            Err(TopologyError::NotAFacet { inner: 0, outer: 1 })
        );
        assert_eq!(
            SimplicialComplex::new(&[vec![0, 1], vec![1, 0]]),
            Err(TopologyError::NotAFacet { inner: 1, outer: 0 })
        );
    }

    /// The bounds hold before the faces are enumerated, and on the faces.
    #[test]
    fn a_complex_with_too_many_faces_is_refused() {
        // One simplex of 10 vertices: 2^10 - 1 = 1023 faces.
        let big: Vec<u32> = (0..10).collect();
        assert!(matches!(
            SimplicialComplex::new(&[big]),
            Err(TopologyError::TooLarge(_))
        ));
        // A simplex of 9 vertices has 511 faces: accepted.
        let nine: Vec<u32> = (0..9).collect();
        assert_eq!(
            SimplicialComplex::new(std::slice::from_ref(&nine))
                .unwrap()
                .face_counts()
                .iter()
                .sum::<usize>(),
            511
        );
        // A huge facet is refused without enumerating its subsets.
        let huge: Vec<u32> = (0..100_000).collect();
        assert!(matches!(
            SimplicialComplex::new(&[huge]),
            Err(TopologyError::TooLarge(_))
        ));
        // 300 disjoint edges: 600 + 300 faces, within the work.
        let edges: Vec<Vec<u32>> = (0..300).map(|i| vec![2 * i, 2 * i + 1]).collect();
        assert!(matches!(
            SimplicialComplex::new(&edges),
            Err(TopologyError::TooLarge(_))
        ));
        // More facets than faces may be.
        let points: Vec<Vec<u32>> = (0..=512).map(|i| vec![i]).collect();
        assert!(matches!(
            SimplicialComplex::new(&points),
            Err(TopologyError::TooLarge(_))
        ));
    }
}

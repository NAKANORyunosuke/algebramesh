//! The cone spanned by the columns of `A`: facets, resonance and homogeneity.
//!
//! These are computed here exactly, without a CAS, so that they can be reported
//! next to CAS results as independent information about the parameters.

use num_bigint::BigInt;
use num_integer::Integer;
use num_rational::BigRational;
use num_traits::{One, Signed, Zero};
use serde::Serialize;

use crate::{GkzError, GkzInput};

/// Upper bound on the number of `(d-1)`-subsets of columns examined for facets.
const MAX_FACET_CANDIDATES: u64 = 200_000;
/// Upper bound for `candidates * d^4`: each candidate costs `d` determinants of
/// size `d - 1`, about `d^4` operations, so the bound limits the work of the
/// enumeration, not only the number of subsets (Issue #24). It allows the
/// largest number of candidates up to `d = 4`.
const MAX_FACET_WORK: u128 = 200_000 * 4u128.pow(4);

/// A facet of `cone(A)` given by its primitive inner normal `h`: `h . a_j >= 0` for
/// every column, with equality on columns spanning a hyperplane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Facet {
    #[serde(serialize_with = "integers")]
    pub normal: Vec<BigInt>,
    /// `h(beta)`.
    pub value_at_beta: crate::Rational,
    /// `h(beta)` lies in `h(ZZ A)`, i.e. `beta` lies in `ZZ A + span(F)`.
    pub resonant: bool,
}

/// Facets and the derived properties of the parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ParameterReport {
    pub facets: Vec<Facet>,
    /// `beta` lies in `ZZ A + span(F)` for some facet `F` of `cone(A)`.
    pub resonant: bool,
    /// `(1, ..., 1)` lies in the row space of `A`; then `H_A(beta)` is regular
    /// holonomic (Hotta).
    pub homogeneous: bool,
}

/// Integers as JSON numbers when they fit in `i64`, as strings otherwise (the same
/// convention as [`algebramesh_math_types::IntegerMatrix`]).
fn integers<S: serde::Serializer>(v: &[BigInt], s: S) -> Result<S::Ok, S::Error> {
    use num_traits::ToPrimitive;
    use serde::ser::SerializeSeq;
    let mut seq = s.serialize_seq(Some(v.len()))?;
    for x in v {
        match x.to_i64() {
            Some(i) => seq.serialize_element(&i)?,
            None => seq.serialize_element(&x.to_string())?,
        }
    }
    seq.end()
}

fn determinant(mut m: Vec<Vec<BigRational>>) -> BigRational {
    let size = m.len();
    let mut det = BigRational::one();
    for col in 0..size {
        let Some(pivot) = (col..size).find(|&r| !m[r][col].is_zero()) else {
            return BigRational::zero();
        };
        if pivot != col {
            m.swap(pivot, col);
            det = -det;
        }
        let pivot_row = m[col].clone();
        det *= &pivot_row[col];
        for row in m.iter_mut().skip(col + 1) {
            if !row[col].is_zero() {
                let factor = &row[col] / &pivot_row[col];
                for (entry, p) in row[col..].iter_mut().zip(&pivot_row[col..]) {
                    *entry -= &factor * p;
                }
            }
        }
    }
    det
}

/// A non-zero integer vector orthogonal to the given `d - 1` vectors of length `d`,
/// by cofactor expansion; zero if they are linearly dependent.
fn orthogonal(vectors: &[&Vec<BigInt>], d: usize) -> Vec<BigInt> {
    (0..d)
        .map(|i| {
            let minor: Vec<Vec<BigRational>> = (0..d)
                .filter(|&r| r != i)
                .map(|r| {
                    vectors
                        .iter()
                        .map(|v| BigRational::from_integer(v[r].clone()))
                        .collect()
                })
                .collect();
            let det = determinant(minor).to_integer();
            if i % 2 == 0 { det } else { -det }
        })
        .collect()
}

fn primitive(v: Vec<BigInt>) -> Vec<BigInt> {
    let g = v.iter().fold(BigInt::zero(), |g, x| g.gcd(x));
    if g.is_zero() || g.is_one() {
        v
    } else {
        v.into_iter().map(|x| x / &g).collect()
    }
}

/// `C(n, k)` exactly, or `None` once it is known to exceed `cap` (Issue #24).
///
/// Uses `C(n, k) = C(n, n - k)` and the running products
/// `C(n - k + i, i) = C(n - k + i - 1, i - 1) * (n - k + i) / i`, each an
/// integer. An intermediate product is at most `cap * n`, which fits in `u128`
/// for every `cap` and `n` below `2^64`; it never saturates, so the result is
/// never smaller than the true value.
fn binomial_at_most(n: u64, k: u64, cap: u64) -> Option<u64> {
    if k > n {
        return Some(0);
    }
    let k = k.min(n - k);
    let mut c: u128 = 1;
    for i in 1..=u128::from(k) {
        c = c * (u128::from(n - k) + i) / i;
        if c > u128::from(cap) {
            return None;
        }
    }
    // Also for k = 0, where the loop does not run.
    (c <= u128::from(cap))
        .then(|| u64::try_from(c).ok())
        .flatten()
}

/// Calls `f` with every `k`-subset of `0..n` in lexicographic order.
fn for_each_subset(n: usize, k: usize, f: &mut impl FnMut(&[usize])) {
    let mut subset: Vec<usize> = (0..k).collect();
    loop {
        f(&subset);
        let Some(i) = (0..k).rev().find(|&i| subset[i] < n - k + i) else {
            return;
        };
        subset[i] += 1;
        for j in i + 1..k {
            subset[j] = subset[j - 1] + 1;
        }
    }
}

impl GkzInput {
    fn columns(&self) -> Vec<Vec<BigInt>> {
        (0..self.n())
            .map(|j| (0..self.d()).map(|i| self.a().get(i, j).clone()).collect())
            .collect()
    }

    /// Primitive inner normals of the facets of `cone(A)`. Every facet contains
    /// `d - 1` linearly independent columns, so examining all `(d-1)`-subsets finds
    /// them all.
    pub fn facet_normals(&self) -> Result<Vec<Vec<BigInt>>, GkzError> {
        let (d, n) = (self.d(), self.n());
        let too_large = || {
            GkzError::TooLarge(format!(
                "more than {MAX_FACET_CANDIDATES} column subsets of size {} out of {n}, or more \
                 than {MAX_FACET_WORK} operations, would be examined for facets",
                d - 1
            ))
        };
        let candidates = binomial_at_most(n as u64, (d - 1) as u64, MAX_FACET_CANDIDATES)
            .ok_or_else(too_large)?;
        if u128::from(candidates) * (d as u128).pow(4) > MAX_FACET_WORK {
            return Err(too_large());
        }
        let columns = self.columns();
        let dot =
            |h: &[BigInt], c: &[BigInt]| -> BigInt { h.iter().zip(c).map(|(a, b)| a * b).sum() };
        let mut normals: Vec<Vec<BigInt>> = Vec::new();
        for_each_subset(n, d - 1, &mut |subset| {
            let chosen: Vec<&Vec<BigInt>> = subset.iter().map(|&j| &columns[j]).collect();
            let h = orthogonal(&chosen, d);
            if h.iter().all(Zero::is_zero) {
                return;
            }
            let values: Vec<BigInt> = columns.iter().map(|c| dot(&h, c)).collect();
            let h = if values.iter().all(|v| !v.is_negative()) {
                h
            } else if values.iter().all(|v| !v.is_positive()) {
                h.into_iter().map(|x| -x).collect()
            } else {
                return;
            };
            let h = primitive(h);
            if !normals.contains(&h) {
                normals.push(h);
            }
        });
        normals.sort();
        Ok(normals)
    }

    /// Facets with the resonance of `beta` along each, and homogeneity of `A`.
    ///
    /// `beta` is resonant along a facet with primitive inner normal `h` when `h(beta)`
    /// lies in `h(ZZ A) = g ZZ`, `g = gcd_j h(a_j)`; equivalently
    /// `beta in ZZ A + span(F)`.
    pub fn parameter_report(&self) -> Result<ParameterReport, GkzError> {
        let columns = self.columns();
        let facets: Vec<Facet> = self
            .facet_normals()?
            .into_iter()
            .map(|h| {
                let value: BigRational = h
                    .iter()
                    .zip(self.beta())
                    .map(|(a, b)| BigRational::from_integer(a.clone()) * &b.0)
                    .sum();
                let g = columns.iter().fold(BigInt::zero(), |g, c| {
                    g.gcd(&h.iter().zip(c).map(|(a, b)| a * b).sum::<BigInt>())
                });
                let resonant = !g.is_zero() && (&value / BigRational::from_integer(g)).is_integer();
                Facet {
                    normal: h,
                    value_at_beta: crate::Rational(value),
                    resonant,
                }
            })
            .collect();
        Ok(ParameterReport {
            resonant: facets.iter().any(|f| f.resonant),
            facets,
            homogeneous: self.is_homogeneous(),
        })
    }

    /// Whether `(1, ..., 1)` lies in the row space of `A` over QQ.
    ///
    /// [`GkzInput::validate`] leaves a row to spare in the bounds of the rank,
    /// so the rank of `A` with the row of ones is always computed: a matrix
    /// too large for it is never taken for an inhomogeneous one (Issue #59).
    pub fn is_homogeneous(&self) -> bool {
        let mut rows = self.a().rows().to_vec();
        rows.push(vec![BigInt::one(); self.n()]);
        algebramesh_math_types::IntegerMatrix::new(rows)
            .is_ok_and(|m| m.rank().is_ok_and(|rank| rank == self.d()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Issue #59: an A beyond the bounds of the rank is too large, not
    /// rank-deficient, also when it is read from JSON, so that the rank in
    /// `is_homogeneous` is always computed.
    #[test]
    fn an_a_beyond_the_bounds_of_the_rank_is_too_large() {
        use crate::GkzError;
        use algebramesh_math_types::{IntegerMatrix, Rational};
        let d = algebramesh_math_types::MAX_RANK_ROWS;
        let rows: Vec<Vec<i64>> = (0..d)
            .map(|i| (0..d).map(|j| i64::from(i == j)).collect())
            .collect();
        let refs: Vec<&[i64]> = rows.iter().map(Vec::as_slice).collect();
        let a = IntegerMatrix::from_i64(&refs).unwrap();
        let beta = vec![Rational::one(); d];
        assert!(matches!(
            GkzInput::new(a.clone(), beta.clone()),
            Err(GkzError::TooLarge(_))
        ));
        let json = serde_json::json!({"A": a, "beta": beta});
        assert!(serde_json::from_value::<GkzInput>(json).is_err());
        let fits: Vec<&[i64]> = rows[..d - 1].iter().map(|r| &r[..d - 1]).collect();
        let input = GkzInput::new(
            IntegerMatrix::from_i64(&fits).unwrap(),
            vec![Rational::one(); d - 1],
        )
        .unwrap();
        // The identity spans every row, the row of ones among them.
        assert!(input.is_homogeneous());
    }

    /// `C(n, k)` by Pascal's rule, in `u128`.
    fn pascal(n: usize) -> Vec<Vec<u128>> {
        let mut rows = vec![vec![1u128]];
        for m in 1..=n {
            let prev = &rows[m - 1];
            let mut row = vec![1u128; m + 1];
            for k in 1..m {
                row[k] = prev[k - 1] + prev[k];
            }
            rows.push(row);
        }
        rows
    }

    #[test]
    fn capped_binomials_are_exact_or_refused() {
        for (n, row) in pascal(70).iter().enumerate() {
            for k in 0..=n + 1 {
                let exact = row.get(k).copied().unwrap_or(0);
                for cap in [0u64, 1, 1_000, 200_000, u64::MAX] {
                    let got = binomial_at_most(n as u64, k as u64, cap);
                    if exact <= u128::from(cap) {
                        assert_eq!(got.map(u128::from), Some(exact), "C({n},{k}) cap {cap}");
                    } else {
                        assert_eq!(got, None, "C({n},{k}) cap {cap}");
                    }
                }
            }
        }
        // The counterexamples of Issue #24: the saturating fold gave 199,860,
        // 104,844 and 0.
        for (n, k, exact) in [
            (64, 60, 635_376u64),
            (65, 61, 677_040),
            (100, 95, 75_287_520),
        ] {
            assert_eq!(binomial_at_most(n, k, u64::MAX), Some(exact));
            assert_eq!(binomial_at_most(n, k, MAX_FACET_CANDIDATES), None);
        }
        assert_eq!(binomial_at_most(200, 100, u64::MAX), None);
    }

    #[test]
    fn a_large_full_rank_matrix_is_refused_before_enumerating() {
        // 96 x 100: the identity and four more columns, of full row rank.
        // C(100, 95) = 75,287,520 subsets would be examined.
        let rows: Vec<Vec<i64>> = (0..96)
            .map(|i| {
                let mut row = vec![0i64; 100];
                row[i] = 1;
                for (j, x) in row[96..].iter_mut().enumerate() {
                    *x = ((i + j) % 3) as i64;
                }
                row
            })
            .collect();
        let refs: Vec<&[i64]> = rows.iter().map(Vec::as_slice).collect();
        let a = algebramesh_math_types::IntegerMatrix::from_i64(&refs).unwrap();
        let input = GkzInput::new(a, vec!["0".parse().unwrap(); 96]).unwrap();
        let started = std::time::Instant::now();
        assert!(matches!(input.facet_normals(), Err(GkzError::TooLarge(_))));
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        // Few subsets, but large ones: the work bound refuses them too.
        // d = 41, n = 41: C(41, 40) = 41 subsets, 41 * 41^4 operations.
        let square: Vec<Vec<i64>> = (0..41)
            .map(|i| (0..41).map(|j| i64::from(i == j)).collect())
            .collect();
        let refs: Vec<&[i64]> = square.iter().map(Vec::as_slice).collect();
        let a = algebramesh_math_types::IntegerMatrix::from_i64(&refs).unwrap();
        let input = GkzInput::new(a, vec!["0".parse().unwrap(); 41]).unwrap();
        assert!(matches!(input.facet_normals(), Err(GkzError::TooLarge(_))));
    }

    fn input(a: &[&[i64]], beta: &[&str]) -> GkzInput {
        GkzInput::new(
            algebramesh_math_types::IntegerMatrix::from_i64(a).unwrap(),
            beta.iter().map(|b| b.parse().unwrap()).collect(),
        )
        .unwrap()
    }

    fn ints(v: &[i64]) -> Vec<BigInt> {
        v.iter().map(|&x| BigInt::from(x)).collect()
    }

    #[test]
    fn facets_of_the_rational_normal_curve() {
        let a = input(&[&[1, 1, 1], &[0, 1, 2]], &["1/2", "1/3"]);
        assert_eq!(
            a.facet_normals().unwrap(),
            vec![ints(&[0, 1]), ints(&[2, -1])]
        );
        let report = a.parameter_report().unwrap();
        assert!(!report.resonant);
        assert!(report.homogeneous);
        // Integer parameters are always resonant.
        assert!(
            input(&[&[1, 1, 1], &[0, 1, 2]], &["3", "4"])
                .parameter_report()
                .unwrap()
                .resonant
        );
        // h(ZZ A) = ZZ for both facets. beta = (1/3, 2/3) gives h(beta) = 2/3 for
        // h = (0,1) and 0 for h = (2,-1): resonant along the second facet only.
        let report = input(&[&[1, 1, 1], &[0, 1, 2]], &["1/3", "2/3"])
            .parameter_report()
            .unwrap();
        assert_eq!(
            report.facets.iter().map(|f| f.resonant).collect::<Vec<_>>(),
            [false, true]
        );
    }

    #[test]
    fn facets_in_dimension_one_and_three() {
        let line = input(&[&[1, 2]], &["1/2"]);
        assert_eq!(line.facet_normals().unwrap(), vec![ints(&[1])]);
        // h(ZZ A) = ZZ, so only non-integer h(beta) is non-resonant.
        assert!(!line.parameter_report().unwrap().resonant);
        assert!(!line.is_homogeneous());
        // The cone over a square has four facets.
        let square = input(
            &[&[1, 1, 1, 1], &[0, 1, 0, 1], &[0, 0, 1, 1]],
            &["1/2", "1/3", "1/5"],
        );
        assert_eq!(
            square.facet_normals().unwrap(),
            vec![
                ints(&[0, 0, 1]),
                ints(&[0, 1, 0]),
                ints(&[1, -1, 0]),
                ints(&[1, 0, -1])
            ]
        );
    }

    /// Issue #81: when h(ZZ A) = g ZZ with g > 1, beta is resonant along the
    /// facet exactly when g divides h(beta), not when h(beta) is an integer.
    /// The tests above all have g = 1, so they cannot tell the two apart.
    #[test]
    fn resonance_divides_by_the_index_of_the_lattice() {
        // The index g = gcd_j h(a_j), worked out here by hand.
        fn index(h: &[i64], columns: &[&[i64]]) -> i64 {
            fn gcd(a: i64, b: i64) -> i64 {
                if b == 0 { a.abs() } else { gcd(b, a % b) }
            }
            columns
                .iter()
                .map(|c| h.iter().zip(*c).map(|(x, y)| x * y).sum::<i64>())
                .fold(0, gcd)
        }
        // A = (2 4): the cone is the ray, its facet is {0} with h = (1), and
        // h(ZZ A) = 2 ZZ. beta = 1 is not in 2 ZZ: not resonant, although
        // h(beta) = 1 is an integer. beta = 2 is resonant.
        assert_eq!(index(&[1], &[&[2], &[4]]), 2);
        let line = input(&[&[2, 4]], &["1"]);
        assert_eq!(line.facet_normals().unwrap(), vec![ints(&[1])]);
        let report = line.parameter_report().unwrap();
        assert_eq!(
            report.facets[0].value_at_beta.0,
            BigRational::from_integer(1.into())
        );
        assert!(!report.resonant);
        assert!(
            input(&[&[2, 4]], &["2"])
                .parameter_report()
                .unwrap()
                .resonant
        );
        // A = diag(2, 2): facets with h = (0, 1) and (1, 0), each with g = 2.
        // beta = (1, 1) is resonant along neither; beta = (2, 1) along the
        // facet h = (1, 0) alone, where h(beta) = 2.
        let square = input(&[&[2, 0], &[0, 2]], &["1", "1"]);
        assert_eq!(
            square.facet_normals().unwrap(),
            vec![ints(&[0, 1]), ints(&[1, 0])]
        );
        assert_eq!(index(&[0, 1], &[&[2, 0], &[0, 2]]), 2);
        assert_eq!(index(&[1, 0], &[&[2, 0], &[0, 2]]), 2);
        assert!(!square.parameter_report().unwrap().resonant);
        let report = input(&[&[2, 0], &[0, 2]], &["2", "1"])
            .parameter_report()
            .unwrap();
        assert_eq!(
            report.facets.iter().map(|f| f.resonant).collect::<Vec<_>>(),
            [false, true]
        );
    }

    #[test]
    fn subsets_are_enumerated() {
        let mut seen = Vec::new();
        for_each_subset(4, 2, &mut |s| seen.push(s.to_vec()));
        assert_eq!(seen.len(), 6);
        assert_eq!(seen.last().unwrap(), &vec![2, 3]);
        let mut empty = 0;
        for_each_subset(3, 0, &mut |s| {
            assert!(s.is_empty());
            empty += 1;
        });
        assert_eq!(empty, 1);
    }
}

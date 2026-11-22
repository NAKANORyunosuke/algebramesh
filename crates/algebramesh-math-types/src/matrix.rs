use num_bigint::BigInt;
use num_rational::BigRational;
use num_traits::Zero;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::MathError;
use crate::scalar::Rational;

/// The most rows [`IntegerMatrix::rank`] eliminates (ADR 0019, class (b)).
pub const MAX_RANK_ROWS: usize = 128;

/// The most columns [`IntegerMatrix::rank`] eliminates (ADR 0019, class (b)).
pub const MAX_RANK_COLS: usize = 1024;

/// A rectangular integer matrix with at least one row and one column.
///
/// In JSON each entry is a number, or a decimal string when it does not fit in 64 bits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegerMatrix {
    rows: Vec<Vec<BigInt>>,
}

impl IntegerMatrix {
    pub fn new(rows: Vec<Vec<BigInt>>) -> Result<Self, MathError> {
        let Some(first) = rows.first() else {
            return Err(MathError::Invalid("matrix has no rows".into()));
        };
        if first.is_empty() {
            return Err(MathError::Invalid("matrix has no columns".into()));
        }
        if rows.iter().any(|r| r.len() != first.len()) {
            return Err(MathError::RaggedMatrix);
        }
        Ok(Self { rows })
    }

    pub fn from_i64(rows: &[&[i64]]) -> Result<Self, MathError> {
        Self::new(
            rows.iter()
                .map(|r| r.iter().map(|&v| BigInt::from(v)).collect())
                .collect(),
        )
    }

    pub fn nrows(&self) -> usize {
        self.rows.len()
    }

    pub fn ncols(&self) -> usize {
        self.rows[0].len()
    }

    pub fn rows(&self) -> &[Vec<BigInt>] {
        &self.rows
    }

    pub fn get(&self, i: usize, j: usize) -> &BigInt {
        &self.rows[i][j]
    }

    /// The `n x n` identity matrix (`n >= 1`).
    pub fn identity(n: usize) -> Result<Self, MathError> {
        Self::new(
            (0..n)
                .map(|i| (0..n).map(|j| BigInt::from(u8::from(i == j))).collect())
                .collect(),
        )
    }

    pub fn transpose(&self) -> Self {
        Self {
            rows: (0..self.ncols())
                .map(|j| self.rows.iter().map(|r| r[j].clone()).collect())
                .collect(),
        }
    }

    /// The product `self * other`.
    pub fn mul(&self, other: &Self) -> Result<Self, MathError> {
        self.mul_with(other, &mut || Ok::<(), MathError>(()))
    }

    /// [`Self::mul`], calling `checkpoint` after every few hundred products of
    /// entries; an error from it ends the product (Issue #49).
    pub fn mul_with<E: From<MathError>>(
        &self,
        other: &Self,
        checkpoint: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<Self, E> {
        if self.ncols() != other.nrows() {
            return Err(MathError::Invalid(format!(
                "a {} x {} matrix times a {} x {} matrix",
                self.nrows(),
                self.ncols(),
                other.nrows(),
                other.ncols()
            ))
            .into());
        }
        let mut count = 0usize;
        let mut rows = Vec::with_capacity(self.nrows());
        for row in &self.rows {
            let mut out = vec![BigInt::zero(); other.ncols()];
            for (k, a) in row.iter().enumerate() {
                if a.is_zero() {
                    continue;
                }
                for (j, b) in other.rows[k].iter().enumerate() {
                    count += 1;
                    if count.is_multiple_of(crate::polynomial::CHECK_EVERY) {
                        checkpoint()?;
                    }
                    out[j] += a * b;
                }
            }
            rows.push(out);
        }
        Ok(Self { rows })
    }

    /// Whether the matrix is square with ones on the diagonal and zeros
    /// elsewhere.
    pub fn is_identity(&self) -> bool {
        self.nrows() == self.ncols()
            && self.rows.iter().enumerate().all(|(i, r)| {
                r.iter()
                    .enumerate()
                    .all(|(j, v)| *v == BigInt::from(u8::from(i == j)))
            })
    }

    /// The same matrix over QQ.
    pub fn to_rational(&self) -> RationalMatrix {
        RationalMatrix {
            nrows: self.nrows(),
            ncols: self.ncols(),
            rows: self
                .rows
                .iter()
                .map(|r| r.iter().map(|v| Rational::integer(v.clone())).collect())
                .collect(),
        }
    }

    /// Rank over QQ by exact Gaussian elimination, for a matrix of at most
    /// [`MAX_RANK_ROWS`] rows and [`MAX_RANK_COLS`] columns; a larger one is
    /// refused before any elimination (Issue #59).
    pub fn rank(&self) -> Result<usize, MathError> {
        if self.nrows() > MAX_RANK_ROWS || self.ncols() > MAX_RANK_COLS {
            return Err(MathError::TooLarge(format!(
                "the rank of a {} x {} matrix; at most {MAX_RANK_ROWS} rows and \
                 {MAX_RANK_COLS} columns are eliminated",
                self.nrows(),
                self.ncols()
            )));
        }
        let mut m: Vec<Vec<BigRational>> = self
            .rows
            .iter()
            .map(|r| r.iter().cloned().map(BigRational::from_integer).collect())
            .collect();
        let (rows, cols) = (self.nrows(), self.ncols());
        let mut rank = 0;
        for col in 0..cols {
            let Some(pivot) = (rank..rows).find(|&r| !m[r][col].is_zero()) else {
                continue;
            };
            m.swap(rank, pivot);
            let pivot_row = m[rank].clone();
            for (r, row) in m.iter_mut().enumerate() {
                if r != rank && !row[col].is_zero() {
                    let factor = &row[col] / &pivot_row[col];
                    for (entry, p) in row[col..].iter_mut().zip(&pivot_row[col..]) {
                        *entry -= &factor * p;
                    }
                }
            }
            rank += 1;
            if rank == rows {
                break;
            }
        }
        Ok(rank)
    }
}

/// A dense matrix of rational numbers, of any shape, `0 x 0` and `n x 0`
/// included (a basis of the zero space has no vectors).
///
/// In JSON it is the list of its rows, each entry a rational number as a
/// string (or an integer of 64 bits as a number); the number of columns of a
/// matrix without rows comes from the context ([`Self::with_shape`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RationalMatrix {
    nrows: usize,
    ncols: usize,
    rows: Vec<Vec<Rational>>,
}

impl RationalMatrix {
    /// The matrix with these rows, which must have the same length.
    pub fn new(rows: Vec<Vec<Rational>>) -> Result<Self, MathError> {
        let ncols = rows.first().map_or(0, Vec::len);
        if rows.iter().any(|r| r.len() != ncols) {
            return Err(MathError::RaggedMatrix);
        }
        Ok(Self {
            nrows: rows.len(),
            ncols,
            rows,
        })
    }

    /// `self` read as a matrix of `nrows x ncols`, which it must be.
    pub fn with_shape(self, nrows: usize, ncols: usize) -> Result<Self, MathError> {
        if self.nrows != nrows || (nrows > 0 && self.ncols != ncols) {
            return Err(MathError::Invalid(format!(
                "a {} x {} matrix where a {nrows} x {ncols} one is expected",
                self.nrows, self.ncols
            )));
        }
        Ok(Self {
            nrows,
            ncols,
            rows: self.rows,
        })
    }

    /// The `nrows x ncols` zero matrix.
    pub fn zeros(nrows: usize, ncols: usize) -> Self {
        Self {
            nrows,
            ncols,
            rows: vec![vec![Rational::zero(); ncols]; nrows],
        }
    }

    /// The `n x n` identity matrix.
    pub fn identity(n: usize) -> Self {
        let mut m = Self::zeros(n, n);
        for (i, row) in m.rows.iter_mut().enumerate() {
            row[i] = Rational::one();
        }
        m
    }

    pub fn nrows(&self) -> usize {
        self.nrows
    }

    pub fn ncols(&self) -> usize {
        self.ncols
    }

    pub fn rows(&self) -> &[Vec<Rational>] {
        &self.rows
    }

    pub fn get(&self, i: usize, j: usize) -> &Rational {
        &self.rows[i][j]
    }

    pub fn transpose(&self) -> Self {
        Self {
            nrows: self.ncols,
            ncols: self.nrows,
            rows: (0..self.ncols)
                .map(|j| self.rows.iter().map(|r| r[j].clone()).collect())
                .collect(),
        }
    }

    /// The product `self * other`.
    pub fn mul(&self, other: &Self) -> Result<Self, MathError> {
        self.mul_with(other, &mut || Ok::<(), MathError>(()))
    }

    /// [`Self::mul`], calling `checkpoint` after every few hundred products of
    /// entries; an error from it ends the product (Issue #49).
    pub fn mul_with<E: From<MathError>>(
        &self,
        other: &Self,
        checkpoint: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<Self, E> {
        if self.ncols != other.nrows {
            return Err(MathError::Invalid(format!(
                "a {} x {} matrix times a {} x {} matrix",
                self.nrows, self.ncols, other.nrows, other.ncols
            ))
            .into());
        }
        let mut count = 0usize;
        let mut rows = Vec::with_capacity(self.nrows);
        for row in &self.rows {
            let mut out = vec![BigRational::zero(); other.ncols];
            for (k, a) in row.iter().enumerate() {
                if a.is_zero() {
                    continue;
                }
                for (j, b) in other.rows[k].iter().enumerate() {
                    count += 1;
                    if count.is_multiple_of(crate::polynomial::CHECK_EVERY) {
                        checkpoint()?;
                    }
                    out[j] += &a.0 * &b.0;
                }
            }
            rows.push(out.into_iter().map(Rational).collect());
        }
        Ok(Self {
            nrows: self.nrows,
            ncols: other.ncols,
            rows,
        })
    }

    /// Whether the matrix is square with ones on the diagonal and zeros
    /// elsewhere.
    pub fn is_identity(&self) -> bool {
        self.nrows == self.ncols
            && self.rows.iter().enumerate().all(|(i, r)| {
                r.iter().enumerate().all(|(j, v)| {
                    if i == j {
                        *v == Rational::one()
                    } else {
                        v.is_zero()
                    }
                })
            })
    }

    pub fn is_zero(&self) -> bool {
        self.rows.iter().flatten().all(Rational::is_zero)
    }

    /// Whether every entry is an integer.
    pub fn is_integral(&self) -> bool {
        self.rows.iter().flatten().all(Rational::is_integer)
    }
}

impl Serialize for RationalMatrix {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.rows.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for RationalMatrix {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(Vec::<Vec<Rational>>::deserialize(deserializer)?)
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum IntegerRepr {
    Int(i64),
    Text(String),
}

impl Serialize for IntegerMatrix {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let rows: Vec<Vec<IntegerRepr>> = self
            .rows
            .iter()
            .map(|r| {
                r.iter()
                    .map(|v| match i64::try_from(v) {
                        Ok(i) => IntegerRepr::Int(i),
                        Err(_) => IntegerRepr::Text(v.to_string()),
                    })
                    .collect()
            })
            .collect();
        rows.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for IntegerMatrix {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let rows = Vec::<Vec<IntegerRepr>>::deserialize(deserializer)?
            .into_iter()
            .map(|r| {
                r.into_iter()
                    .map(|v| match v {
                        IntegerRepr::Int(i) => Ok(BigInt::from(i)),
                        IntegerRepr::Text(s) => crate::scalar::parse_rational(&s)
                            .ok()
                            .filter(|q| q.is_integer())
                            .map(|q| q.numer().clone())
                            .ok_or_else(|| {
                                serde::de::Error::custom(format!("invalid integer {s:?}"))
                            }),
                    })
                    .collect::<Result<Vec<_>, D::Error>>()
            })
            .collect::<Result<Vec<_>, _>>()?;
        Self::new(rows).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn int(rows: &[&[i64]]) -> IntegerMatrix {
        IntegerMatrix::from_i64(rows).unwrap()
    }

    #[test]
    fn products_transposes_and_identities() {
        let a = int(&[&[1, 2, 3], &[4, 5, 6]]);
        assert_eq!(a.transpose(), int(&[&[1, 4], &[2, 5], &[3, 6]]));
        assert_eq!(a.mul(&a.transpose()).unwrap(), int(&[&[14, 32], &[32, 77]]));
        assert!(a.mul(&a).is_err(), "2 x 3 times 2 x 3");
        assert!(IntegerMatrix::identity(3).unwrap().is_identity());
        assert!(!a.is_identity());
        let i = IntegerMatrix::identity(3).unwrap();
        assert_eq!(a.mul(&i).unwrap(), a);

        let q = a.to_rational();
        assert_eq!(q.mul(&RationalMatrix::identity(3)).unwrap(), q);
        let half = RationalMatrix::new(vec![vec!["1/2".parse().unwrap()]]).unwrap();
        assert!(!half.is_integral());
        assert!(
            half.mul(&RationalMatrix::new(vec![vec![Rational::integer(2)]]).unwrap())
                .unwrap()
                .is_identity()
        );
    }

    /// A matrix without columns keeps its shape, which JSON gives only for its
    /// rows.
    #[test]
    fn empty_shapes_are_matrices() {
        let k = RationalMatrix::zeros(3, 0);
        assert_eq!((k.nrows(), k.ncols()), (3, 0));
        let a = int(&[&[1, 0, 0]]).to_rational();
        assert_eq!(a.mul(&k).unwrap(), RationalMatrix::zeros(1, 0));
        let json = serde_json::to_value(&k).unwrap();
        assert_eq!(json, serde_json::json!([[], [], []]));
        let none: RationalMatrix = serde_json::from_value(serde_json::json!([])).unwrap();
        assert_eq!(none.with_shape(0, 4).unwrap().ncols(), 4);
        let back: RationalMatrix = serde_json::from_value(json).unwrap();
        assert!(back.with_shape(3, 2).is_err());
        assert!(serde_json::from_value::<RationalMatrix>(serde_json::json!([["1"], []])).is_err());
    }

    /// Issue #59: a matrix beyond the bounds is refused at once; one within
    /// them gets its rank as before.
    #[test]
    fn the_rank_is_bounded() {
        let tall = IntegerMatrix::new(vec![vec![BigInt::from(1)]; MAX_RANK_ROWS + 1]).unwrap();
        assert!(matches!(tall.rank(), Err(MathError::TooLarge(_))));
        let wide = IntegerMatrix::new(vec![vec![BigInt::from(1); MAX_RANK_COLS + 1]]).unwrap();
        assert!(matches!(wide.rank(), Err(MathError::TooLarge(_))));
        let largest =
            IntegerMatrix::new(vec![vec![BigInt::from(0); MAX_RANK_COLS]; MAX_RANK_ROWS]).unwrap();
        assert_eq!(largest.rank().unwrap(), 0);
    }

    /// ADR 0019 states the bounds the implementation enforces.
    #[test]
    fn the_bounds_are_those_of_adr_0019() {
        let adr = include_str!(
            "../../../docs/adr/0019-rust-checks-certificates-and-bounded-recomputation.md"
        );
        assert!(
            adr.contains(&format!("`MAX_RANK_ROWS` = {MAX_RANK_ROWS} ")),
            "rows"
        );
        assert!(
            adr.contains(&format!("`MAX_RANK_COLS` = {MAX_RANK_COLS} ")),
            "columns"
        );
    }

    #[test]
    fn rank_examples() {
        let rank = |rows: &[&[i64]]| IntegerMatrix::from_i64(rows).unwrap().rank().unwrap();
        assert_eq!(rank(&[&[1, 1, 1], &[0, 1, 2]]), 2);
        assert_eq!(rank(&[&[1, 2], &[2, 4]]), 1);
        assert_eq!(rank(&[&[0, 0]]), 0);
    }

    #[test]
    fn shape_validation() {
        assert!(IntegerMatrix::new(vec![]).is_err());
        assert!(IntegerMatrix::new(vec![vec![]]).is_err());
        assert_eq!(
            IntegerMatrix::from_i64(&[&[1, 2], &[3]]),
            Err(MathError::RaggedMatrix)
        );
        assert!(serde_json::from_str::<IntegerMatrix>("[[1,2],[3]]").is_err());
        let m: IntegerMatrix = serde_json::from_str("[[1,1,1],[0,1,2]]").unwrap();
        assert_eq!((m.nrows(), m.ncols()), (2, 3));
        assert_eq!(serde_json::to_string(&m).unwrap(), "[[1,1,1],[0,1,2]]");
        let big: IntegerMatrix = serde_json::from_str(r#"[["100000000000000000000"]]"#).unwrap();
        assert_eq!(
            serde_json::to_string(&big).unwrap(),
            r#"[["100000000000000000000"]]"#
        );
        assert!(serde_json::from_str::<IntegerMatrix>(r#"[["1/2"]]"#).is_err());
    }
}

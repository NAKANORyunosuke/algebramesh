//! `matrix.*` (Issue #57): rank, kernel and solutions over ZZ and QQ, and the
//! Hermite and Smith normal forms over ZZ. A CAS computes the value and a
//! certificate; `algebramesh-verify` checks the certificate (ADR 0019).
//!
//! Every value holds the mathematical value and, unless the job asked for
//! none, a `certificate`. A certificate too large for a result (before #36)
//! is replaced by `{"omitted": "certificate_too_large"}`.

use algebramesh_math_types::{CoefficientDomain, Rational, RationalMatrix};
use serde::{Deserialize, Serialize};

pub const MATRIX_RANK: &str = "matrix.rank";
pub const MATRIX_KERNEL: &str = "matrix.kernel";
pub const MATRIX_SOLVE: &str = "matrix.solve";
pub const MATRIX_HERMITE_NORMAL_FORM: &str = "matrix.hermite_normal_form";
pub const MATRIX_SMITH_NORMAL_FORM: &str = "matrix.smith_normal_form";

/// Whether the backend returns a certificate with the value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CertificateRequest {
    #[default]
    Include,
    None,
}

/// The arguments of `matrix.rank`, `matrix.kernel`,
/// `matrix.hermite_normal_form` and `matrix.smith_normal_form`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MatrixArguments {
    /// ZZ or QQ; the normal forms are over ZZ only.
    pub domain: CoefficientDomain,
    /// The matrix A, by rows; over ZZ every entry is an integer.
    pub matrix: RationalMatrix,
    #[serde(default)]
    pub certificate: CertificateRequest,
}

/// The arguments of `matrix.solve`: A x = b.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SolveArguments {
    pub domain: CoefficientDomain,
    pub matrix: RationalMatrix,
    /// The right-hand side b, one entry per row of A.
    pub rhs: Vec<Rational>,
    #[serde(default)]
    pub certificate: CertificateRequest,
}

pub use crate::certificate::{CERTIFICATE_TOO_LARGE, Certificate};

/// A nonsingular square submatrix of A and its inverse: the rank is at least
/// its size.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Minor {
    /// The rows of A it takes, from 0.
    pub rows: Vec<usize>,
    /// The columns of A it takes, from 0.
    pub columns: Vec<usize>,
    /// Its inverse.
    pub inverse: RationalMatrix,
}

/// `matrix.rank`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RankResult {
    #[serde(rename = "type")]
    pub kind: RankKind,
    pub rank: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub certificate: Option<Certificate<RankCertificate>>,
}

/// A minor of the size of the rank (rank >= r), and n - r independent vectors
/// of the kernel in column echelon form (rank <= r).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RankCertificate {
    pub minor: Minor,
    /// n x (n - r), by rows.
    pub kernel: RationalMatrix,
}

/// `matrix.kernel`: a basis of {x : A x = 0}, over ZZ a basis of the lattice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KernelResult {
    #[serde(rename = "type")]
    pub kind: KernelKind,
    /// n x k, by rows: the basis vectors are its columns.
    pub basis: RationalMatrix,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub certificate: Option<Certificate<KernelCertificate>>,
}

/// Over QQ, a minor of size n - k (the kernel has dimension at most k); over
/// ZZ, a unimodular U with U A^T in row echelon form, whose rows at the zero
/// rows of U A^T are the basis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum KernelCertificate {
    Rationals { minor: Minor },
    Integers(Transform),
}

/// A square U with its inverse V (U V = I).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Transform {
    pub transform: RationalMatrix,
    pub inverse: RationalMatrix,
}

/// `matrix.solve`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SolveResult {
    #[serde(rename = "type")]
    pub kind: SolveKind,
    /// A solution x, none if there is none.
    pub solution: Option<Vec<Rational>>,
    /// Without a solution: y with y A = 0 and y b != 0 over QQ, or y A
    /// integral and y b not over ZZ.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub certificate: Option<Certificate<NoSolution>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoSolution {
    pub y: Vec<Rational>,
}

/// `matrix.hermite_normal_form`: H = U A in row Hermite normal form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HermiteResult {
    #[serde(rename = "type")]
    pub kind: HermiteKind,
    pub form: RationalMatrix,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub certificate: Option<Certificate<Transform>>,
}

/// `matrix.smith_normal_form`: D = P A Q, with the diagonal of D.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SmithResult {
    #[serde(rename = "type")]
    pub kind: SmithKind,
    /// d_1, ..., d_min(m, n).
    pub diagonal: Vec<Rational>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub certificate: Option<Certificate<SmithCertificate>>,
}

/// Unimodular P and Q with their inverses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SmithCertificate {
    pub left: Transform,
    pub right: Transform,
}

/// The five operations.
pub const OPERATIONS: [&str; 5] = [
    MATRIX_RANK,
    MATRIX_KERNEL,
    MATRIX_SOLVE,
    MATRIX_HERMITE_NORMAL_FORM,
    MATRIX_SMITH_NORMAL_FORM,
];

/// A matrix job as the drivers take it, checked before any CAS runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatrixRequest {
    pub operation: &'static str,
    pub domain: CoefficientDomain,
    pub matrix: RationalMatrix,
    /// For `matrix.solve`.
    pub rhs: Option<Vec<Rational>>,
    pub certificate: bool,
}

impl MatrixRequest {
    /// Reads and checks the arguments of a job of `operation`: A has rows and
    /// columns, over ZZ its entries (and those of b) are integers, the normal
    /// forms are over ZZ, and b has one entry per row of A.
    pub fn parse(operation: &str, arguments: &serde_json::Value) -> Result<Self, String> {
        let Some(&operation) = OPERATIONS.iter().find(|op| **op == operation) else {
            return Err(format!("{operation} is not a matrix operation"));
        };
        let (domain, matrix, rhs, certificate) = if operation == MATRIX_SOLVE {
            let args: SolveArguments =
                serde_json::from_value(arguments.clone()).map_err(|e| e.to_string())?;
            (args.domain, args.matrix, Some(args.rhs), args.certificate)
        } else {
            let args: MatrixArguments =
                serde_json::from_value(arguments.clone()).map_err(|e| e.to_string())?;
            (args.domain, args.matrix, None, args.certificate)
        };
        if matrix.nrows() == 0 || matrix.ncols() == 0 {
            return Err("the matrix has no rows or no columns".into());
        }
        let integers = domain == CoefficientDomain::Integers;
        if integers && !matrix.is_integral() {
            return Err("over ZZ the entries of the matrix are integers".into());
        }
        if !integers
            && (operation == MATRIX_HERMITE_NORMAL_FORM || operation == MATRIX_SMITH_NORMAL_FORM)
        {
            return Err(format!("{operation} is over ZZ only"));
        }
        if let Some(b) = &rhs {
            if b.len() != matrix.nrows() {
                return Err(format!(
                    "the right-hand side has {} entries for {} rows",
                    b.len(),
                    matrix.nrows()
                ));
            }
            if integers && !b.iter().all(Rational::is_integer) {
                return Err("over ZZ the entries of the right-hand side are integers".into());
            }
        }
        Ok(Self {
            operation,
            domain,
            matrix,
            rhs,
            certificate: certificate == CertificateRequest::Include,
        })
    }

    /// The request for a driver: entries as decimal strings.
    pub fn driver_json(&self) -> serde_json::Value {
        let mut request = serde_json::json!({
            "operation": self.operation,
            "domain": self.domain,
            "matrix": self.matrix,
            "certificate": self.certificate,
        });
        if let Some(b) = &self.rhs {
            request["rhs"] = serde_json::json!(b);
        }
        request
    }

    /// The value of the job from the driver's reply, which must have the
    /// shape of the operation's result; it is written again by its type, so
    /// that every value has one form whatever the driver printed.
    pub fn value(&self, reply: &serde_json::Value) -> Result<serde_json::Value, String> {
        fn typed<T: Serialize + serde::de::DeserializeOwned>(
            reply: &serde_json::Value,
        ) -> Result<serde_json::Value, String> {
            let value: T = serde_json::from_value(reply.clone()).map_err(|e| e.to_string())?;
            serde_json::to_value(value).map_err(|e| e.to_string())
        }
        let value = match self.operation {
            MATRIX_RANK => typed::<RankResult>(reply),
            MATRIX_KERNEL => typed::<KernelResult>(reply),
            MATRIX_SOLVE => typed::<SolveResult>(reply),
            MATRIX_HERMITE_NORMAL_FORM => typed::<HermiteResult>(reply),
            _ => typed::<SmithResult>(reply),
        }?;
        // A solution is its own certificate; anything else needs one when the
        // job asked for it. Only the checker leaves a certificate out.
        let solved = self.operation == MATRIX_SOLVE && !value["solution"].is_null();
        let needed = self.certificate && !solved;
        match value.get("certificate") {
            Some(_) if !self.certificate => Err("a certificate the job did not ask for".into()),
            Some(c) if c.get("omitted").is_some() => {
                Err("the driver left the certificate out; only the checker does".into())
            }
            None if needed => Err("the job asked for a certificate and the reply has none".into()),
            _ => Ok(value),
        }
    }
}

macro_rules! kind {
    ($name:ident, $value:literal) => {
        #[doc = concat!("The `\"type\"` field, which must be `\"", $value, "\"`.")]
        #[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
        pub enum $name {
            #[default]
            #[serde(rename = $value)]
            Value,
        }
    };
}

kind!(RankKind, "matrix_rank");
kind!(KernelKind, "matrix_kernel");
kind!(SolveKind, "matrix_solution");
kind!(HermiteKind, "hermite_normal_form");
kind!(SmithKind, "smith_normal_form");

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn values_read_and_write_their_json() {
        let snf: SmithResult = serde_json::from_value(serde_json::json!({
            "type": "smith_normal_form", "diagonal": ["2", "4"],
            "certificate": {
                "left": {"transform": [["1", "0"], ["-3", "1"]], "inverse": [["1", "0"], ["3", "1"]]},
                "right": {"transform": [["1", "-2"], ["0", "1"]], "inverse": [["1", "2"], ["0", "1"]]},
            },
        }))
        .unwrap();
        assert!(matches!(snf.certificate, Some(Certificate::Given(_))));
        let omitted: SmithResult = serde_json::from_value(serde_json::json!({
            "type": "smith_normal_form", "diagonal": ["2", "4"],
            "certificate": {"omitted": "certificate_too_large"},
        }))
        .unwrap();
        assert!(matches!(
            omitted.certificate,
            Some(Certificate::Omitted { .. })
        ));
        let none: SolveResult = serde_json::from_value(serde_json::json!({
            "type": "matrix_solution", "solution": null, "certificate": {"y": ["1/2"]},
        }))
        .unwrap();
        assert_eq!(none.solution, None);
        assert!(
            serde_json::from_value::<SmithResult>(serde_json::json!({
                "type": "hermite_normal_form", "diagonal": [],
            }))
            .is_err()
        );
        let kernel: KernelResult = serde_json::from_value(serde_json::json!({
            "type": "matrix_kernel", "basis": [["1"], ["-1"]],
            "certificate": {"transform": [["1", "0"], ["1", "1"]], "inverse": [["1", "0"], ["-1", "1"]]},
        }))
        .unwrap();
        assert!(matches!(
            kernel.certificate,
            Some(Certificate::Given(KernelCertificate::Integers(_)))
        ));
    }

    #[test]
    fn requests_are_checked_before_any_cas_runs() {
        let parse = |op: &str, args: serde_json::Value| MatrixRequest::parse(op, &args);
        let snf = parse(
            MATRIX_SMITH_NORMAL_FORM,
            serde_json::json!({"domain": "ZZ", "matrix": [[2, 4], [6, 8]]}),
        )
        .unwrap();
        assert_eq!(
            snf.driver_json(),
            serde_json::json!({"operation": "matrix.smith_normal_form", "domain": "ZZ",
                               "matrix": [["2", "4"], ["6", "8"]], "certificate": true})
        );
        assert!(
            parse(
                MATRIX_SMITH_NORMAL_FORM,
                serde_json::json!({"domain": "QQ", "matrix": [[1]]})
            )
            .is_err()
        );
        assert!(
            parse(
                MATRIX_RANK,
                serde_json::json!({"domain": "ZZ", "matrix": [["1/2"]]})
            )
            .is_err()
        );
        assert!(
            parse(
                MATRIX_RANK,
                serde_json::json!({"domain": "QQ", "matrix": []})
            )
            .is_err()
        );
        assert!(
            parse(
                MATRIX_RANK,
                serde_json::json!({"domain": "QQ", "matrix": [[]]})
            )
            .is_err()
        );
        assert!(
            parse(
                MATRIX_SOLVE,
                serde_json::json!({"domain": "QQ", "matrix": [[1]], "rhs": [1, 2]})
            )
            .is_err()
        );
        assert!(
            parse(
                MATRIX_SOLVE,
                serde_json::json!({"domain": "ZZ", "matrix": [[1]], "rhs": ["1/2"]})
            )
            .is_err()
        );
        assert!(parse("matrix.inverse", serde_json::json!({})).is_err());
        let solve = parse(
            MATRIX_SOLVE,
            serde_json::json!({"domain": "QQ", "matrix": [[1]], "rhs": ["1/2"],
                                                           "certificate": "none"}),
        )
        .unwrap();
        assert_eq!(solve.driver_json()["rhs"], serde_json::json!(["1/2"]));
        assert!(!solve.certificate);
        // A reply is read by the type of its operation, and a certificate the job
        // did not ask for is refused.
        let reply = serde_json::json!({"type": "matrix_solution", "solution": [1]});
        assert_eq!(
            solve.value(&reply).unwrap()["solution"],
            serde_json::json!(["1"])
        );
        let extra = serde_json::json!({"type": "matrix_solution", "solution": null, "certificate": {"y": [1]}});
        assert!(solve.value(&extra).is_err());
        // A certificate the job asked for must come, unless a solution stands
        // for it; and no driver leaves one out.
        let wanted = parse(
            MATRIX_SOLVE,
            serde_json::json!({"domain": "QQ", "matrix": [[1]], "rhs": ["1/2"]}),
        )
        .unwrap();
        assert!(
            wanted
                .value(&serde_json::json!({"type": "matrix_solution", "solution": null}))
                .is_err()
        );
        assert!(
            wanted
                .value(&serde_json::json!({"type": "matrix_solution", "solution": ["1/2"]}))
                .is_ok()
        );
        assert!(
            snf.value(&serde_json::json!({"type": "smith_normal_form", "diagonal": ["2", "4"]}))
                .is_err()
        );
        let omitted = serde_json::json!({"type": "smith_normal_form", "diagonal": ["2", "4"],
                                          "certificate": {"omitted": "certificate_too_large"}});
        assert!(snf.value(&omitted).is_err());
        // A job that asked for none takes a reply without one.
        let bare = parse(
            MATRIX_SMITH_NORMAL_FORM,
            serde_json::json!({"domain": "ZZ", "matrix": [[2, 4], [6, 8]], "certificate": "none"}),
        )
        .unwrap();
        assert!(
            bare.value(&serde_json::json!({"type": "smith_normal_form", "diagonal": ["2", "4"]}))
                .is_ok()
        );
        assert!(
            solve
                .value(&serde_json::json!({"type": "matrix_solution", "solution": null}))
                .is_ok()
        );
        assert!(
            snf.value(&serde_json::json!({"type": "matrix_rank", "rank": 1}))
                .is_err()
        );
    }

    #[test]
    fn arguments_default_to_a_certificate() {
        let args: MatrixArguments =
            serde_json::from_value(serde_json::json!({"domain": "ZZ", "matrix": [[2, 4], [6, 8]]}))
                .unwrap();
        assert_eq!(args.certificate, CertificateRequest::Include);
        assert!(
            serde_json::from_value::<MatrixArguments>(
                serde_json::json!({"domain": "ZZ", "matrix": [[1]], "script": "x"})
            )
            .is_err()
        );
    }
}

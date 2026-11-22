//! `matrix.*` (Issue #58): the certificates of rank, kernel, solutions and
//! the Hermite and Smith normal forms are checked against the matrix of the
//! job's specification, with products and comparisons only (ADR 0019, class
//! (a)): no elimination, no division but by the entries of the certificate.
//!
//! | claim | statement |
//! |---|---|
//! | `rank.lower` | a minor of size r is invertible: rank A >= r |
//! | `rank.upper` | A K = 0 for n - r independent columns K: rank A <= r |
//! | `kernel.contained` | A K = 0 |
//! | `kernel.independent` | the columns of K are independent |
//! | `kernel.spanning` | the columns of K span the kernel (over ZZ, the lattice) |
//! | `solve.solution` | A x = b, over ZZ with x integral |
//! | `solve.no_solution` | no x: y A = 0 and y b != 0 (QQ); y A integral, y b not (ZZ) |
//! | `hnf.form` | H is in row Hermite normal form |
//! | `hnf.equivalent` | H = U A with U unimodular |
//! | `snf.form` | d_i >= 0 and d_1 \| d_2 \| ... |
//! | `snf.equivalent` | diag(d) = P A Q with P, Q unimodular |
//!
//! A claim whose evidence is the certificate is asserted when the job asked
//! for none or the certificate was left out for its size.

use algebramesh_core::guarantee::{Claim, Method};
use algebramesh_core::{CasError, JobSpec};
use algebramesh_math_types::{CoefficientDomain, MathError, Rational, RationalMatrix};
use algebramesh_ops::matrix::{
    Certificate, CertificateRequest, KernelCertificate, KernelResult, MATRIX_HERMITE_NORMAL_FORM,
    MATRIX_KERNEL, MATRIX_RANK, MATRIX_SMITH_NORMAL_FORM, MATRIX_SOLVE, MatrixArguments, Minor,
    RankResult, SolveArguments, SolveResult, Transform,
};
use algebramesh_ops::matrix::{HermiteResult, SmithCertificate, SmithResult};

use crate::{Context, OperationVerifier, certificate, missing};

fn refuse(operation: &str, message: impl std::fmt::Display) -> CasError {
    CasError::UnsupportedResult(format!("{operation}: {message}"))
}

/// A product stopped at a checkpoint, or failed on the data.
enum Stop {
    Interrupted(CasError),
    Math(MathError),
}

impl From<MathError> for Stop {
    fn from(e: MathError) -> Self {
        Self::Math(e)
    }
}

/// `a * b`, stopping at the checkpoints of the job.
fn product(
    operation: &str,
    a: &RationalMatrix,
    b: &RationalMatrix,
    context: &Context<'_>,
) -> Result<RationalMatrix, CasError> {
    a.mul_with(b, &mut || {
        context.interrupt.checkpoint().map_err(Stop::Interrupted)
    })
    .map_err(|e| match e {
        Stop::Interrupted(e) => e,
        Stop::Math(e) => refuse(operation, e),
    })
}

/// The arguments of a job of `operation`, read from its specification. Over
/// ZZ every entry of A is an integer.
fn arguments(spec: &JobSpec, operation: &str) -> Result<MatrixArguments, CasError> {
    let args: MatrixArguments = serde_json::from_value(spec.arguments.clone())
        .map_err(|e| refuse(operation, format!("the arguments of the job: {e}")))?;
    if args.domain == CoefficientDomain::Integers && !args.matrix.is_integral() {
        return Err(refuse(operation, "the matrix of the job is not integral"));
    }
    Ok(args)
}

fn value<T: serde::de::DeserializeOwned>(
    operation: &str,
    value: &serde_json::Value,
) -> Result<T, CasError> {
    serde_json::from_value(value.clone())
        .map_err(|e| refuse(operation, format!("the value cannot be read: {e}")))
}

/// `a` read in the shape `nrows x ncols`.
fn shaped(
    operation: &str,
    what: &str,
    a: &RationalMatrix,
    nrows: usize,
    ncols: usize,
) -> Result<RationalMatrix, CasError> {
    a.clone()
        .with_shape(nrows, ncols)
        .map_err(|e| refuse(operation, format!("{what}: {e}")))
}

/// Checks that `minor` is an invertible submatrix of `a`; returns its size.
fn check_minor(
    operation: &str,
    a: &RationalMatrix,
    minor: &Minor,
    context: &Context<'_>,
) -> Result<usize, CasError> {
    let r = minor.rows.len();
    if minor.columns.len() != r {
        return Err(refuse(operation, "the minor takes as many rows as columns"));
    }
    let distinct = |v: &[usize], bound: usize| {
        v.iter().all(|&i| i < bound)
            && v.iter().collect::<std::collections::BTreeSet<_>>().len() == v.len()
    };
    if !distinct(&minor.rows, a.nrows()) || !distinct(&minor.columns, a.ncols()) {
        return Err(refuse(
            operation,
            "the minor takes rows or columns twice or out of A",
        ));
    }
    let b = RationalMatrix::new(
        minor
            .rows
            .iter()
            .map(|&i| minor.columns.iter().map(|&j| a.get(i, j).clone()).collect())
            .collect(),
    )
    .map_err(|e| refuse(operation, e))?
    .with_shape(r, r)
    .map_err(|e| refuse(operation, e))?;
    let inverse = shaped(operation, "the inverse of the minor", &minor.inverse, r, r)?;
    if !product(operation, &b, &inverse, context)?.is_identity() {
        return Err(refuse(
            operation,
            "the inverse of the minor is not its inverse",
        ));
    }
    Ok(r)
}

/// Checks that `t` is unimodular of size n: integral, with an integral inverse.
fn check_unimodular(
    operation: &str,
    what: &str,
    t: &Transform,
    n: usize,
    context: &Context<'_>,
) -> Result<(RationalMatrix, RationalMatrix), CasError> {
    let u = shaped(operation, what, &t.transform, n, n)?;
    let v = shaped(
        operation,
        &format!("the inverse of {what}"),
        &t.inverse,
        n,
        n,
    )?;
    if !u.is_integral() || !v.is_integral() {
        return Err(refuse(
            operation,
            format!("{what} or its inverse is not integral"),
        ));
    }
    if !product(operation, &u, &v, context)?.is_identity() {
        return Err(refuse(
            operation,
            format!("the inverse of {what} is not its inverse"),
        ));
    }
    Ok((u, v))
}

/// `rank.upper`: the n x (n - r) matrix `k` is in column echelon form and
/// A k = 0.
fn check_kernel_vectors(
    operation: &str,
    a: &RationalMatrix,
    k: &RationalMatrix,
    context: &Context<'_>,
) -> Result<(), CasError> {
    if !certificate::is_column_echelon(k) {
        return Err(refuse(
            operation,
            "the kernel vectors are not in column echelon form",
        ));
    }
    if !product(operation, a, k, context)?.is_zero() {
        return Err(refuse(operation, "A times the kernel vectors is not zero"));
    }
    Ok(())
}

/// The checks of `matrix.rank`.
pub struct RankVerifier;

impl OperationVerifier for RankVerifier {
    fn operation(&self) -> &'static str {
        MATRIX_RANK
    }

    fn verify(
        &self,
        spec: &JobSpec,
        value_json: &serde_json::Value,
        context: &Context<'_>,
    ) -> Result<Vec<Claim>, CasError> {
        let op = MATRIX_RANK;
        let args = arguments(spec, op)?;
        let requested = args.certificate;
        let a = args.matrix;
        let result: RankResult = value(op, value_json)?;
        let (m, n) = (a.nrows(), a.ncols());
        if result.rank > m.min(n) {
            return Err(refuse(
                op,
                format!("a rank of {} for a {m} x {n} matrix", result.rank),
            ));
        }
        if let Some(reason) = missing(
            op,
            &result.certificate,
            requested == CertificateRequest::None,
            context,
        )? {
            return Ok(vec![
                context.asserted("rank.lower", reason),
                context.asserted("rank.upper", reason),
            ]);
        }
        let Some(Certificate::Given(c)) = &result.certificate else {
            unreachable!("a missing certificate is handled above")
        };
        if check_minor(op, &a, &c.minor, context)? != result.rank {
            return Err(refuse(op, "the minor is not of the size of the rank"));
        }
        let k = shaped(op, "the kernel vectors", &c.kernel, n, n - result.rank)?;
        check_kernel_vectors(op, &a, &k, context)?;
        Ok(vec![
            context.checked("rank.lower", Method::Certificate),
            context.checked("rank.upper", Method::Certificate),
        ])
    }
}

/// The checks of `matrix.kernel`.
pub struct KernelVerifier;

impl OperationVerifier for KernelVerifier {
    fn operation(&self) -> &'static str {
        MATRIX_KERNEL
    }

    fn verify(
        &self,
        spec: &JobSpec,
        value_json: &serde_json::Value,
        context: &Context<'_>,
    ) -> Result<Vec<Claim>, CasError> {
        let op = MATRIX_KERNEL;
        let args = arguments(spec, op)?;
        let requested = args.certificate;
        let a = args.matrix;
        let result: KernelResult = value(op, value_json)?;
        let n = a.ncols();
        let k = result.basis.ncols();
        let basis = shaped(op, "the basis", &result.basis, n, k)?;
        if !product(op, &a, &basis, context)?.is_zero() {
            return Err(refuse(op, "A times the basis is not zero"));
        }
        let mut claims = vec![context.checked("kernel.contained", Method::SelfCertifying)];
        let integers = args.domain == CoefficientDomain::Integers;
        if integers && !basis.is_integral() {
            return Err(refuse(op, "over ZZ the basis is not integral"));
        }
        if !integers {
            // Over QQ the basis comes in column echelon form, independent by
            // its shape.
            if !certificate::is_column_echelon(&basis) {
                return Err(refuse(op, "the basis is not in column echelon form"));
            }
            claims.push(context.checked("kernel.independent", Method::SelfCertifying));
        }
        if let Some(reason) = missing(
            op,
            &result.certificate,
            requested == CertificateRequest::None,
            context,
        )? {
            if integers {
                claims.push(context.asserted("kernel.independent", reason));
            }
            claims.push(context.asserted("kernel.spanning", reason));
            return Ok(claims);
        }
        let Some(Certificate::Given(c)) = &result.certificate else {
            unreachable!("a missing certificate is handled above")
        };
        match (integers, c) {
            (false, KernelCertificate::Rationals { minor }) => {
                // rank A >= n - k, so the kernel has dimension at most k.
                if check_minor(op, &a, minor, context)? != n - k {
                    return Err(refuse(op, "the minor is not of size n - k"));
                }
            }
            (true, KernelCertificate::Integers(t)) => {
                // U A^T = H in row echelon form with U unimodular: the rows of
                // U at the zero rows of H are a basis of the lattice.
                let (u, _) = check_unimodular(op, "U", t, n, context)?;
                let h = product(op, &u, &a.transpose(), context)?;
                let Some(nonzero) = certificate::row_echelon_rank(&h) else {
                    return Err(refuse(op, "U A^T is not in row echelon form"));
                };
                let expected = RationalMatrix::new(u.rows()[nonzero..].to_vec())
                    .map_err(|e| refuse(op, e))?
                    .with_shape(n - nonzero, n)
                    .map_err(|e| refuse(op, e))?
                    .transpose();
                if expected != basis {
                    return Err(refuse(
                        op,
                        "the basis is not the rows of U at the zero rows of U A^T",
                    ));
                }
                claims.push(context.checked("kernel.independent", Method::Certificate));
            }
            _ => return Err(refuse(op, "the certificate is not one for this domain")),
        }
        let mut spanning = context.checked("kernel.spanning", Method::Certificate);
        if !integers {
            // Over QQ the certificate bounds the dimension of the kernel; the
            // columns span it as they lie in it and are independent.
            spanning.depends_on = vec!["kernel.contained".into(), "kernel.independent".into()];
        }
        claims.push(spanning);
        Ok(claims)
    }
}

/// The checks of `matrix.solve`.
pub struct SolveVerifier;

impl OperationVerifier for SolveVerifier {
    fn operation(&self) -> &'static str {
        MATRIX_SOLVE
    }

    fn verify(
        &self,
        spec: &JobSpec,
        value_json: &serde_json::Value,
        context: &Context<'_>,
    ) -> Result<Vec<Claim>, CasError> {
        let op = MATRIX_SOLVE;
        let args: SolveArguments = serde_json::from_value(spec.arguments.clone())
            .map_err(|e| refuse(op, format!("the arguments of the job: {e}")))?;
        let integers = args.domain == CoefficientDomain::Integers;
        let a = args.matrix;
        let (m, n) = (a.nrows(), a.ncols());
        if args.rhs.len() != m {
            return Err(refuse(
                op,
                "the right-hand side of the job has not one entry per row",
            ));
        }
        let b = column(&args.rhs);
        if integers && (!a.is_integral() || !b.is_integral()) {
            return Err(refuse(op, "the system of the job is not integral"));
        }
        let result: SolveResult = value(op, value_json)?;
        if let Some(x) = &result.solution {
            if x.len() != n {
                return Err(refuse(op, "the solution has not one entry per column"));
            }
            let x = column(x);
            if integers && !x.is_integral() {
                return Err(refuse(op, "over ZZ the solution is not integral"));
            }
            if product(op, &a, &x, context)? != b {
                return Err(refuse(op, "A x is not b"));
            }
            return Ok(vec![
                context.checked("solve.solution", Method::SelfCertifying),
            ]);
        }
        if let Some(reason) = missing(
            op,
            &result.certificate,
            args.certificate == CertificateRequest::None,
            context,
        )? {
            return Ok(vec![context.asserted("solve.no_solution", reason)]);
        }
        let Some(Certificate::Given(c)) = &result.certificate else {
            unreachable!("a missing certificate is handled above")
        };
        if c.y.len() != m {
            return Err(refuse(op, "y has not one entry per row"));
        }
        let y = RationalMatrix::new(vec![c.y.clone()]).map_err(|e| refuse(op, e))?;
        let ya = product(op, &y, &a, context)?;
        let yb = product(op, &y, &b, context)?.get(0, 0).clone();
        let refutes = if integers {
            // An integral x would give y b = (y A) x, an integer.
            ya.is_integral() && !yb.is_integer()
        } else {
            ya.is_zero() && !yb.is_zero()
        };
        if !refutes {
            return Err(refuse(op, "y does not show that there is no solution"));
        }
        Ok(vec![
            context.checked("solve.no_solution", Method::Certificate),
        ])
    }
}

/// `v` as a column.
fn column(v: &[Rational]) -> RationalMatrix {
    RationalMatrix::new(v.iter().map(|x| vec![x.clone()]).collect())
        .and_then(|m| m.with_shape(v.len(), 1))
        .unwrap_or_else(|_| RationalMatrix::zeros(v.len(), 1))
}

/// The checks of `matrix.hermite_normal_form`.
pub struct HermiteVerifier;

impl OperationVerifier for HermiteVerifier {
    fn operation(&self) -> &'static str {
        MATRIX_HERMITE_NORMAL_FORM
    }

    fn verify(
        &self,
        spec: &JobSpec,
        value_json: &serde_json::Value,
        context: &Context<'_>,
    ) -> Result<Vec<Claim>, CasError> {
        let op = MATRIX_HERMITE_NORMAL_FORM;
        let args = arguments(spec, op)?;
        if args.domain != CoefficientDomain::Integers {
            return Err(refuse(op, "the Hermite normal form is over ZZ only"));
        }
        let requested = args.certificate;
        let a = args.matrix;
        let result: HermiteResult = value(op, value_json)?;
        let h = shaped(op, "H", &result.form, a.nrows(), a.ncols())?;
        if !certificate::is_hermite_form(&h) {
            return Err(refuse(op, "H is not in row Hermite normal form"));
        }
        let mut claims = vec![context.checked("hnf.form", Method::SelfCertifying)];
        if let Some(reason) = missing(
            op,
            &result.certificate,
            requested == CertificateRequest::None,
            context,
        )? {
            claims.push(context.asserted("hnf.equivalent", reason));
            return Ok(claims);
        }
        let Some(Certificate::Given(t)) = &result.certificate else {
            unreachable!("a missing certificate is handled above")
        };
        let (u, _) = check_unimodular(op, "U", t, a.nrows(), context)?;
        if product(op, &u, &a, context)? != h {
            return Err(refuse(op, "U A is not H"));
        }
        claims.push(context.checked("hnf.equivalent", Method::Certificate));
        Ok(claims)
    }
}

/// The checks of `matrix.smith_normal_form`.
pub struct SmithVerifier;

impl OperationVerifier for SmithVerifier {
    fn operation(&self) -> &'static str {
        MATRIX_SMITH_NORMAL_FORM
    }

    fn verify(
        &self,
        spec: &JobSpec,
        value_json: &serde_json::Value,
        context: &Context<'_>,
    ) -> Result<Vec<Claim>, CasError> {
        let op = MATRIX_SMITH_NORMAL_FORM;
        let args = arguments(spec, op)?;
        if args.domain != CoefficientDomain::Integers {
            return Err(refuse(op, "the Smith normal form is over ZZ only"));
        }
        let requested = args.certificate;
        let a = args.matrix;
        let (m, n) = (a.nrows(), a.ncols());
        let result: SmithResult = value(op, value_json)?;
        if result.diagonal.len() != m.min(n) || !certificate::is_smith_diagonal(&result.diagonal) {
            return Err(refuse(
                op,
                "the diagonal is not of non-negative integers each dividing the next",
            ));
        }
        let mut claims = vec![context.checked("snf.form", Method::SelfCertifying)];
        if let Some(reason) = missing(
            op,
            &result.certificate,
            requested == CertificateRequest::None,
            context,
        )? {
            claims.push(context.asserted("snf.equivalent", reason));
            return Ok(claims);
        }
        let Some(Certificate::Given(c)) = &result.certificate else {
            unreachable!("a missing certificate is handled above")
        };
        check_smith_equivalence(op, &a, &result.diagonal, c, context)?;
        claims.push(context.checked("snf.equivalent", Method::Certificate));
        Ok(claims)
    }
}

/// Checks the certificate of a Smith normal form of `a` (`snf.equivalent`):
/// P and Q are unimodular, of the sizes of the rows and columns of `a`, and
/// P a Q = diag(`diagonal`). The diagonal has min(m, n) entries; whether
/// they divide one another is checked apart (`snf.form`). An error refuses
/// the result as one of `operation`; the products stop at the checkpoints of
/// `context`. The topology package checks each boundary matrix with it.
pub fn check_smith_equivalence(
    operation: &str,
    a: &RationalMatrix,
    diagonal: &[Rational],
    c: &SmithCertificate,
    context: &Context<'_>,
) -> Result<(), CasError> {
    let (m, n) = (a.nrows(), a.ncols());
    if diagonal.len() != m.min(n) {
        return Err(refuse(
            operation,
            format!("{} diagonal entries for a {m} x {n} matrix", diagonal.len()),
        ));
    }
    let (p, _) = check_unimodular(operation, "P", &c.left, m, context)?;
    let (q, _) = check_unimodular(operation, "Q", &c.right, n, context)?;
    let paq = product(operation, &product(operation, &p, a, context)?, &q, context)?;
    let mut d = vec![vec![Rational::zero(); n]; m];
    for (i, di) in diagonal.iter().enumerate() {
        d[i][i] = di.clone();
    }
    let d = RationalMatrix::new(d)
        .and_then(|d| d.with_shape(m, n))
        .map_err(|e| refuse(operation, e))?;
    if paq != d {
        return Err(refuse(operation, "P A Q is not the diagonal matrix"));
    }
    Ok(())
}

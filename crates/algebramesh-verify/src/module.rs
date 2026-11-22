//! `module.syzygies`, `complex.free_resolution` (Issue #97) and
//! `ideal.hilbert_series` (Issue #98): the values are checked against the
//! generators of the job's specification with products of polynomial
//! matrices, degrees of terms and integer arithmetic only (ADR 0019, class
//! (a); ADR 0027).
//!
//! | claim | statement | level |
//! |---|---|---|
//! | `syz.annihilates` | F S = 0 | checked |
//! | `syz.generates` | the columns of S generate the syzygies of F | asserted |
//! | `res.complex` | d_i d_{i+1} = 0 | checked |
//! | `res.image` | the image of d_1 is I: d_1 = F C and F = d_1 N | checked |
//! | `res.input_homogeneous` | every generator of I is homogeneous | checked |
//! | `res.differentials_homogeneous` | each d_i is homogeneous of degree 0 for the degrees of the value | checked |
//! | `res.entries_in_maximal_ideal` | no d_i has a nonzero constant entry | checked |
//! | `res.exact` | the complex is exact at F_1, ..., F_k | asserted |
//! | `res.minimal` | the resolution is minimal | checked by a theorem; effective level asserted |
//! | `res.betti` | the Betti numbers of the value are those of R/I | checked by a theorem; effective level asserted |
//! | `hilbert.series` | P(t) / (1 - t)^d is the Hilbert series of R/I | asserted (compared with the Betti numbers on request) |

use algebramesh_core::guarantee::{Claim, EvidenceLevel, Method, Scope};
use algebramesh_core::{CasError, JobSpec};
use algebramesh_math_types::{
    MathError, PolynomialMatrix, PolynomialMatrixData, Rational, RingSpec,
};
use algebramesh_ops::certificate::Certificate;
use algebramesh_ops::matrix::CertificateRequest;
use algebramesh_ops::module::{
    COMPLEX_FREE_RESOLUTION, FreeResolutionResult, HilbertSeriesResult, IDEAL_HILBERT_SERIES,
    MODULE_SYZYGIES, ModuleRequest, SyzygiesResult, betti_from_degrees, check_degree_shape,
    homogeneous_degree,
};
use serde::de::DeserializeOwned;

use crate::{Context, OperationVerifier, missing};

/// The premises of `res.minimal`: a graded free resolution of a module
/// generated in a homogeneous way, with differentials homogeneous of degree
/// 0 whose entries lie in the homogeneous maximal ideal, is minimal.
const MINIMAL_PREMISES: [&str; 4] = [
    "res.input_homogeneous",
    "res.differentials_homogeneous",
    "res.entries_in_maximal_ideal",
    "res.exact",
];

/// What a free resolution does not claim when it is not shown minimal.
const NOT_MINIMAL: [&str; 2] = ["res.minimal", "res.betti"];

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

fn request(operation: &str, spec: &JobSpec) -> Result<ModuleRequest, CasError> {
    ModuleRequest::parse(operation, &spec.arguments)
        .map_err(|e| refuse(operation, format!("the arguments of the job: {e}")))
}

fn read_value<T: DeserializeOwned>(
    operation: &str,
    value: &serde_json::Value,
) -> Result<T, CasError> {
    serde_json::from_value(value.clone())
        .map_err(|e| refuse(operation, format!("the value cannot be read: {e}")))
}

fn read_matrix(
    operation: &str,
    ring: &RingSpec,
    name: &str,
    data: &PolynomialMatrixData,
) -> Result<PolynomialMatrix, CasError> {
    PolynomialMatrix::from_data(ring.clone(), data)
        .map_err(|e| refuse(operation, format!("{name}: {e}")))
}

/// `a b`, stopping at the checkpoints of the job.
fn product(
    operation: &str,
    a: &PolynomialMatrix,
    b: &PolynomialMatrix,
    context: &Context<'_>,
) -> Result<PolynomialMatrix, CasError> {
    a.mul_with(b, &mut || {
        context.interrupt.checkpoint().map_err(Stop::Interrupted)
    })
    .map_err(|e| match e {
        Stop::Interrupted(e) => e,
        Stop::Math(e) => refuse(operation, e),
    })
}

/// The checks of `module.syzygies`.
pub struct SyzygiesVerifier;

impl OperationVerifier for SyzygiesVerifier {
    fn operation(&self) -> &'static str {
        MODULE_SYZYGIES
    }

    fn verify(
        &self,
        spec: &JobSpec,
        value: &serde_json::Value,
        context: &Context<'_>,
    ) -> Result<Vec<Claim>, CasError> {
        const OP: &str = MODULE_SYZYGIES;
        let ModuleRequest::Syzygies { ring, generators } = request(OP, spec)? else {
            return Err(refuse(OP, "the job is not one of syzygies"));
        };
        let result: SyzygiesResult = read_value(OP, value)?;
        let s = read_matrix(OP, &ring, "S", &result.syzygies)?;
        if s.nrows() != generators.ncols() {
            return Err(refuse(
                OP,
                format!(
                    "S has {} rows, and F has {} columns",
                    s.nrows(),
                    generators.ncols()
                ),
            ));
        }
        let fs = product(OP, &generators, &s, context)?;
        if let Some(j) =
            (0..fs.ncols()).find(|&j| fs.column(j).into_iter().flatten().next().is_some())
        {
            return Err(refuse(OP, format!("F times column {j} of S is not 0")));
        }
        Ok(vec![
            context.checked("syz.annihilates", Method::SelfCertifying),
            context.asserted(
                "syz.generates",
                "that the columns of S generate every syzygy is the backend's answer",
            ),
        ])
    }

    fn not_claimed(&self) -> Vec<String> {
        vec!["that the columns of S are a minimal set of generators".into()]
    }
}

/// The checks of `complex.free_resolution`.
pub struct FreeResolutionVerifier;

/// The differentials of a value, read in `ring`, with their shapes checked:
/// d_1 has one row, and d_{i+1} as many rows as d_i has columns. Without
/// differentials, d_1 is the 1 x 0 matrix.
fn differentials(
    operation: &str,
    ring: &RingSpec,
    data: &[PolynomialMatrixData],
) -> Result<Vec<PolynomialMatrix>, CasError> {
    let mut read = Vec::with_capacity(data.len());
    let mut rows = 1;
    for (i, d) in data.iter().enumerate() {
        let m = read_matrix(operation, ring, &format!("d_{}", i + 1), d)?;
        if m.nrows() != rows {
            return Err(refuse(
                operation,
                format!("d_{} has {} rows, not {rows}", i + 1, m.nrows()),
            ));
        }
        rows = m.ncols();
        read.push(m);
    }
    Ok(read)
}

/// `res.differentials_homogeneous`: each nonzero entry (r, c) of d_i is
/// homogeneous of degree `degrees[i][c] - degrees[i-1][r]`, and F_0 has its
/// one basis element in degree 0.
fn check_homogeneous_differentials(
    operation: &str,
    d: &[PolynomialMatrix],
    degrees: &[Vec<u64>],
    context: &Context<'_>,
) -> Result<(), CasError> {
    if degrees[0] != [0] {
        return Err(refuse(
            operation,
            format!(
                "F_0 = R has its basis element in degree 0, not {:?}",
                degrees[0]
            ),
        ));
    }
    for (i, m) in d.iter().enumerate() {
        let (rows, columns) = (&degrees[i], &degrees[i + 1]);
        for (c, &column_degree) in columns.iter().enumerate() {
            context.interrupt.checkpoint()?;
            for (r, p) in m.column(c).into_iter().flatten() {
                let expected = column_degree.checked_sub(rows[r]);
                if expected.is_none() || homogeneous_degree(p) != expected {
                    return Err(refuse(
                        operation,
                        format!(
                            "the entry ({r}, {c}) of d_{} is not homogeneous of degree {} - {}",
                            i + 1,
                            column_degree,
                            rows[r]
                        ),
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Whether some differential has a nonzero constant entry.
fn has_constant_entry(d: &[PolynomialMatrix]) -> bool {
    d.iter().any(|m| {
        (0..m.ncols()).any(|c| {
            m.column(c)
                .into_iter()
                .flatten()
                .any(|(_, p)| p.is_constant() && !p.is_zero())
        })
    })
}

/// Whether the generators are homogeneous and every differential is free of
/// nonzero constants: the cases in which the value claims minimality.
fn shown_minimal(spec: &JobSpec, value: &serde_json::Value) -> Option<bool> {
    let ModuleRequest::FreeResolution {
        ideal, homogeneous, ..
    } = ModuleRequest::parse(COMPLEX_FREE_RESOLUTION, &spec.arguments).ok()?
    else {
        return None;
    };
    if !homogeneous {
        return Some(false);
    }
    let result: FreeResolutionResult = serde_json::from_value(value.clone()).ok()?;
    let d = result
        .differentials
        .iter()
        .map(|m| PolynomialMatrix::from_data(ideal.ring.clone(), m).ok())
        .collect::<Option<Vec<_>>>()?;
    Some(!has_constant_entry(&d))
}

impl OperationVerifier for FreeResolutionVerifier {
    fn operation(&self) -> &'static str {
        COMPLEX_FREE_RESOLUTION
    }

    fn verify(
        &self,
        spec: &JobSpec,
        value: &serde_json::Value,
        context: &Context<'_>,
    ) -> Result<Vec<Claim>, CasError> {
        const OP: &str = COMPLEX_FREE_RESOLUTION;
        let ModuleRequest::FreeResolution {
            ideal,
            certificate: requested,
            homogeneous,
        } = request(OP, spec)?
        else {
            return Err(refuse(OP, "the job is not one of a free resolution"));
        };
        let ring = ideal.ring.clone();
        let result: FreeResolutionResult = read_value(OP, value)?;
        let d = differentials(OP, &ring, &result.differentials)?;

        // res.complex, from the value alone.
        for (i, pair) in d.windows(2).enumerate() {
            let p = product(OP, &pair[0], &pair[1], context)?;
            if !p.is_zero() {
                return Err(refuse(OP, format!("d_{} d_{} is not 0", i + 1, i + 2)));
            }
        }
        let mut claims = vec![context.checked("res.complex", Method::SelfCertifying)];

        // res.image, from the certificate.
        let none_requested = requested == CertificateRequest::None;
        match missing(OP, &result.certificate, none_requested, context)? {
            Some(reason) => claims.push(context.asserted("res.image", reason)),
            None => {
                let Some(Certificate::Given(c)) = &result.certificate else {
                    unreachable!("a missing certificate is handled above")
                };
                let f = PolynomialMatrix::from_row(ring.clone(), &ideal.generators)
                    .map_err(|e| refuse(OP, e))?;
                let d1 = d
                    .first()
                    .cloned()
                    .unwrap_or_else(|| PolynomialMatrix::zero(ring.clone(), 1, 0));
                let (s, n1) = (f.ncols(), d1.ncols());
                let cm = read_matrix(OP, &ring, "C", &c.differential_in_generators)?;
                let nm = read_matrix(OP, &ring, "N", &c.generators_in_differential)?;
                if (cm.nrows(), cm.ncols()) != (s, n1) || (nm.nrows(), nm.ncols()) != (n1, s) {
                    return Err(refuse(
                        OP,
                        format!(
                            "C is {} x {} and N is {} x {}, not {s} x {n1} and {n1} x {s}",
                            cm.nrows(),
                            cm.ncols(),
                            nm.nrows(),
                            nm.ncols()
                        ),
                    ));
                }
                if product(OP, &f, &cm, context)? != d1 {
                    return Err(refuse(OP, "d_1 is not F C"));
                }
                if product(OP, &d1, &nm, context)? != f {
                    return Err(refuse(OP, "F is not d_1 N"));
                }
                claims.push(context.checked("res.image", Method::Certificate));
            }
        }

        let exact = context.asserted("res.exact", "exactness is the backend's answer");
        if !homogeneous {
            if result.degrees.is_some() || result.betti.is_some() {
                return Err(refuse(
                    OP,
                    "degrees or Betti numbers for generators that are not all homogeneous",
                ));
            }
            claims.push(exact);
            return Ok(claims);
        }

        // The grading.
        let Some(degrees) = &result.degrees else {
            return Err(refuse(
                OP,
                "the generators are homogeneous and the value has no degrees",
            ));
        };
        check_degree_shape(degrees, &result.differentials).map_err(|e| refuse(OP, e))?;
        claims.push(context.checked("res.input_homogeneous", Method::Recomputation));
        check_homogeneous_differentials(OP, &d, degrees, context)?;
        claims.push(context.checked("res.differentials_homogeneous", Method::SelfCertifying));
        claims.push(exact);
        if has_constant_entry(&d) {
            if result.betti.is_some() {
                return Err(refuse(
                    OP,
                    "Betti numbers for a resolution with a nonzero constant entry",
                ));
            }
            return Ok(claims);
        }
        claims.push(context.checked("res.entries_in_maximal_ideal", Method::SelfCertifying));
        claims.push(context.theorem("res.minimal", &MINIMAL_PREMISES));
        match &result.betti {
            Some(betti) if *betti == betti_from_degrees(degrees) => {
                claims.push(context.theorem("res.betti", &["res.minimal"]));
            }
            Some(_) => {
                return Err(refuse(
                    OP,
                    "the Betti numbers are not those the degrees of the free modules give",
                ));
            }
            None => return Err(refuse(OP, "a minimal resolution and no Betti numbers")),
        }
        Ok(claims)
    }

    fn not_claimed_for(&self, spec: &JobSpec, value: &serde_json::Value) -> Vec<String> {
        let mut not_claimed = Vec::new();
        match shown_minimal(spec, value) {
            Some(true) | None => {}
            Some(false) => {
                if !matches!(
                    ModuleRequest::parse(COMPLEX_FREE_RESOLUTION, &spec.arguments),
                    Ok(ModuleRequest::FreeResolution {
                        homogeneous: true,
                        ..
                    })
                ) {
                    not_claimed.extend(
                        ["res.input_homogeneous", "res.differentials_homogeneous"]
                            .map(String::from),
                    );
                }
                not_claimed.push("res.entries_in_maximal_ideal".into());
                not_claimed.extend(NOT_MINIMAL.map(String::from));
            }
        }
        not_claimed
    }
}

/// The checks of `ideal.hilbert_series`.
pub struct HilbertSeriesVerifier;

/// `p q` for polynomials in t given by their coefficients from degree 0.
fn multiply(p: &[Rational], q: &[Rational]) -> Vec<Rational> {
    if p.is_empty() || q.is_empty() {
        return Vec::new();
    }
    let mut r = vec![Rational::zero(); p.len() + q.len() - 1];
    for (i, a) in p.iter().enumerate() {
        for (j, b) in q.iter().enumerate() {
            r[i + j] = Rational(&r[i + j].0 + &(&a.0 * &b.0));
        }
    }
    while r.last().is_some_and(Rational::is_zero) {
        r.pop();
    }
    r
}

/// (1 - t)^e, stopping at the checkpoints of the job.
fn one_minus_t_to(e: u64, context: &Context<'_>) -> Result<Vec<Rational>, CasError> {
    let factor = [Rational::one(), Rational::integer(-1)];
    let mut r = vec![Rational::one()];
    for _ in 0..e {
        context.interrupt.checkpoint()?;
        r = multiply(&r, &factor);
    }
    Ok(r)
}

impl OperationVerifier for HilbertSeriesVerifier {
    fn operation(&self) -> &'static str {
        IDEAL_HILBERT_SERIES
    }

    fn verify(
        &self,
        spec: &JobSpec,
        value: &serde_json::Value,
        context: &Context<'_>,
    ) -> Result<Vec<Claim>, CasError> {
        const OP: &str = IDEAL_HILBERT_SERIES;
        let ModuleRequest::HilbertSeries { ideal, betti_table } = request(OP, spec)? else {
            return Err(refuse(OP, "the job is not one of a Hilbert series"));
        };
        let result: HilbertSeriesResult = read_value(OP, value)?;
        let numerator = result
            .numerator
            .iter()
            .enumerate()
            .map(|(k, c)| match algebramesh_math_types::parse_rational(c) {
                Ok(r) if r.is_integer() => Ok(r),
                _ => Err(refuse(
                    OP,
                    format!("the coefficient of t^{k} is not an integer: {c}"),
                )),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let betti = match (&result.betti, betti_table) {
            (Some(b), true) => b,
            (None, false) => {
                return Ok(vec![context.asserted(
                    "hilbert.series",
                    "the Hilbert series is the backend's answer",
                )]);
            }
            (Some(_), false) => return Err(refuse(OP, "Betti numbers the job did not ask for")),
            (None, true) => {
                return Err(refuse(
                    OP,
                    "the job asked for the Betti numbers and the value has none",
                ));
            }
        };
        let n = ideal.ring.nvars() as u64;
        // sum_{i,j} (-1)^i beta_{ij} t^j, the numerator over (1 - t)^n.
        let top = betti.iter().map(|b| b.j).max().unwrap_or(0);
        let top = usize::try_from(top).map_err(|e| refuse(OP, e))?;
        if top > 1 << 20 {
            return Err(refuse(OP, format!("a Betti number in degree {top}")));
        }
        let mut alternating = vec![Rational::zero(); top + 1];
        for b in betti {
            let sign = if b.i % 2 == 0 { 1i64 } else { -1 };
            let term = Rational::integer(sign).0 * Rational::integer(b.beta).0;
            let j = usize::try_from(b.j).map_err(|e| refuse(OP, e))?;
            alternating[j] = Rational(&alternating[j].0 + &term);
        }
        while alternating.last().is_some_and(Rational::is_zero) {
            alternating.pop();
        }
        if result.denominator_exponent > n {
            return Err(refuse(
                OP,
                format!(
                    "the exponent {} of 1 - t is above the {n} variables",
                    result.denominator_exponent
                ),
            ));
        }
        // P / (1 - t)^d = A / (1 - t)^n  <=>  P (1 - t)^(n - d) = A.
        let left = multiply(
            &numerator,
            &one_minus_t_to(n - result.denominator_exponent, context)?,
        );
        if left != alternating {
            return Err(refuse(
                OP,
                "the Hilbert series is not the one the Betti numbers give",
            ));
        }
        Ok(vec![Claim {
            id: "hilbert.series".into(),
            level: EvidenceLevel::Asserted,
            method: Some(Method::Consistency),
            by: context.producer.clone(),
            scope: Scope::Exact,
            depends_on: Vec::new(),
            reason: Some(
                "the Hilbert series and the Betti numbers are the backend's answers, and they \
                 agree"
                    .into(),
            ),
        }])
    }
}

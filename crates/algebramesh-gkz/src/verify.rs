//! The checks of `gkz.generate`, `gkz.indicial` and `gkz.series` (Issue #99,
//! `docs/claims.md`): each reads the input from the job and the rest from the
//! value, decides the claims it can itself, and takes the others from the
//! backend as `asserted`. A value whose claims do not hold is refused
//! (`UNSUPPORTED_RESULT`).
//!
//! [`register`] adds them to a [`Registry`]; an agent built with the GKZ
//! package does so.
//!
//! A record without a guarantee (V4, ADR 0031) gets the same claims from the
//! fields of its value that record the checks of its writer (`checks.*`, the
//! `status` of each series, `parameters`, `canonical`); a field that writers
//! of the time did not write (`evidence_terms`, `certificate`) leaves the
//! claims it supports asserted.

use std::collections::BTreeSet;

use algebramesh_core::guarantee::{Claim, Method, Scope};
use algebramesh_core::{CasError, JobSpec};
use algebramesh_math_types::{
    DifferentialOperator, Polynomial, PolynomialData, Rational, WeylTermData,
};
use algebramesh_ops::{
    GKZ_GENERATE, GKZ_INDICIAL, GKZ_SERIES, GkzGenerateArguments, GkzIndicialArguments,
    GkzSeriesArguments,
};
use algebramesh_verify::{Context, OperationVerifier, Registry};

use crate::value::{canonical_fields, multiplicity_sum, read_terms, render_terms, theta_ring};
use crate::{GkzInput, SeriesDefect, Solution};

/// Adds the checks of the three GKZ operations to `registry`.
pub fn register(registry: &mut Registry) {
    registry.register(Box::new(GenerateVerifier));
    registry.register(Box::new(IndicialVerifier));
    registry.register(Box::new(SeriesVerifier));
}

/// The standard checks and those of the GKZ package.
pub fn registry() -> Registry {
    let mut registry = Registry::standard();
    register(&mut registry);
    registry
}

fn refuse(operation: &str, message: impl std::fmt::Display) -> CasError {
    CasError::UnsupportedResult(format!("{operation}: {message}"))
}

fn arguments<T: serde::de::DeserializeOwned>(
    operation: &str,
    spec: &JobSpec,
) -> Result<T, CasError> {
    serde_json::from_value(spec.arguments.clone())
        .map_err(|e| refuse(operation, format!("the job's arguments: {e}")))
}

fn input(
    operation: &str,
    a: algebramesh_math_types::IntegerMatrix,
    beta: Vec<Rational>,
) -> Result<GkzInput, CasError> {
    GkzInput::new(a, beta).map_err(|e| refuse(operation, format!("the job's input: {e}")))
}

/// A claim taken from the backend that rests on `premises`.
fn asserted_on(context: &Context<'_>, id: &str, reason: &str, premises: &[&str]) -> Claim {
    Claim {
        depends_on: premises.iter().map(|p| (*p).to_owned()).collect(),
        ..context.asserted(id, reason)
    }
}

/// A claim decided here by `method` that rests on `premises`.
fn checked_on(context: &Context<'_>, id: &str, method: Method, premises: &[&str]) -> Claim {
    Claim {
        depends_on: premises.iter().map(|p| (*p).to_owned()).collect(),
        ..context.checked(id, method)
    }
}

fn truncated(claim: Claim, weight: &[i64], order: u32) -> Claim {
    Claim {
        scope: Scope::Truncated {
            weight: weight.to_vec(),
            order: u64::from(order),
        },
        ..claim
    }
}

/// The operators of a list of the value (`euler_operators`,
/// `toric_generators`), each `{"display", "terms"}`.
fn operators(
    operation: &str,
    input: &GkzInput,
    value: &serde_json::Value,
    key: &str,
) -> Result<Vec<DifferentialOperator>, CasError> {
    let list = value[key]
        .as_array()
        .ok_or_else(|| refuse(operation, format!("the value has no list {key}")))?;
    list.iter()
        .map(|g| {
            let terms: Vec<WeylTermData> = serde_json::from_value(g["terms"].clone())
                .map_err(|e| refuse(operation, format!("{key}: {e}")))?;
            DifferentialOperator::from_terms(
                input.weyl_algebra(),
                terms
                    .into_iter()
                    .map(|t| (t.x_exponents, t.d_exponents, t.coefficient)),
            )
            .map_err(|e| refuse(operation, format!("{key}: {e}")))
        })
        .collect()
}

/// That the field `checks.<name>` of the value is `true`: the value records
/// as checked what was checked here.
fn check_field(operation: &str, value: &serde_json::Value, name: &str) -> Result<(), CasError> {
    if value["checks"][name] == true {
        Ok(())
    } else {
        Err(refuse(
            operation,
            format!("the field checks.{name} of the value is not true"),
        ))
    }
}

/// The toric generators of the value: each must be `c (d^u - d^v)` with
/// `A u = A v` (`gkz.toric_in_IA`).
fn toric(
    operation: &str,
    input: &GkzInput,
    value: &serde_json::Value,
) -> Result<Vec<DifferentialOperator>, CasError> {
    let toric = operators(operation, input, value, "toric_generators")?;
    if !input.toric_binomials_valid(&toric) {
        return Err(refuse(
            operation,
            "a toric generator is not c (d^u - d^v) with A u = A v",
        ));
    }
    check_field(operation, value, "toric_binomials_valid")?;
    Ok(toric)
}

/// The answer of the CAS that its GKZ ideal is generated by the toric
/// generators and the Euler operators; a value whose CAS said no is refused.
fn generated(operation: &str, value: &serde_json::Value) -> Result<(), CasError> {
    if value["checks"]["generated_by_toric_and_euler"] == true {
        Ok(())
    } else {
        Err(refuse(
            operation,
            "the CAS did not find its GKZ ideal generated by the toric generators and the \
             Euler operators",
        ))
    }
}

/// For each operator P_j of the job, whether the value proves P_j in H_A(beta)
/// by its certificate (Issue #102): `certificate.membership[j]` holds left
/// coefficients q_k over the toric generators and the Euler operators, which
/// lie in H_A(beta) by `gkz.toric_in_IA` and `gkz.euler`, with
/// P_j = sum_k q_k g_k. A certificate that does not give P_j, or one for an
/// operator the CAS did not find in the ideal, is refused.
#[allow(clippy::too_many_arguments)]
fn membership(
    op: &str,
    input: &GkzInput,
    value: &serde_json::Value,
    toric: &[DifferentialOperator],
    euler: &[DifferentialOperator],
    expected: &[DifferentialOperator],
    answers: &[serde_json::Value],
    context: &Context<'_>,
) -> Result<Vec<bool>, CasError> {
    let Some(certificate) = value.get("certificate") else {
        return Ok(vec![false; expected.len()]);
    };
    let list = certificate["membership"]
        .as_array()
        .filter(|l| l.len() == expected.len())
        .ok_or_else(|| {
            refuse(
                op,
                format!(
                    "certificate.membership is not a list of {} entries",
                    expected.len()
                ),
            )
        })?;
    let mut certified = Vec::with_capacity(expected.len());
    for (j, (entry, p)) in list.iter().zip(expected).enumerate() {
        if entry.is_null() {
            certified.push(false);
            continue;
        }
        if answers[j]["in_ideal"] != true {
            return Err(refuse(
                op,
                format!(
                    "certificate.membership[{j}] is given for an operator the CAS did not find in the ideal"
                ),
            ));
        }
        let q_toric = operators(op, input, entry, "toric")?;
        let q_euler = operators(op, input, entry, "euler")?;
        if q_toric.len() != toric.len() || q_euler.len() != euler.len() {
            return Err(refuse(
                op,
                format!(
                    "certificate.membership[{j}] has {} and {} coefficients for {} toric generators and {} Euler operators",
                    q_toric.len(),
                    q_euler.len(),
                    toric.len(),
                    euler.len()
                ),
            ));
        }
        let mut sum = DifferentialOperator::from_terms(input.weyl_algebra(), std::iter::empty())
            .map_err(|e| refuse(op, e))?;
        for (q, g) in q_toric.iter().zip(toric).chain(q_euler.iter().zip(euler)) {
            let mut checkpoint = || context.interrupt.checkpoint().map_err(Stop::Interrupted);
            let product = q.mul_with(g, &mut checkpoint).map_err(|e| match e {
                Stop::Interrupted(e) => e,
                Stop::Math(e) => refuse(op, e),
            })?;
            sum = sum.add(&product).map_err(|e| refuse(op, e))?;
        }
        if sum != *p {
            return Err(refuse(
                op,
                format!("certificate.membership[{j}] gives {sum}, not the operator {p} of the job"),
            ));
        }
        certified.push(true);
    }
    Ok(certified)
}

/// The certificate `certificate.independence` of an exponent with
/// logarithmic solutions (Issue #102): with B the matrix of the coefficients
/// of the terms x^v l^(k) (shift 0) of the solutions, a row for each k and a
/// column for each solution, the rows `log_powers` give a square B_R with
/// B_R * `inverse` = 1. Then B has rank s, the shift-0 parts of the s
/// solutions are linearly independent, and so are the solutions, as a
/// relation among them would hold among their shift-0 parts. The terms of a
/// solution not verified are its `evidence_terms`. A certificate that does
/// not check is refused.
fn independence(
    op: &str,
    entry: &serde_json::Value,
    n: usize,
    context: &Context<'_>,
) -> Result<(), CasError> {
    let at = format!(
        "the certificate of independence of exponent {}",
        entry["point"]
    );
    let certificate: crate::value::Independence =
        serde_json::from_value(entry["certificate"]["independence"].clone())
            .map_err(|e| refuse(op, format!("{at}: {e}")))?;
    let solutions = entry["solutions"]
        .as_array()
        .ok_or_else(|| refuse(op, format!("{at}: no solutions")))?;
    let s = solutions.len();
    if certificate.log_powers.len() != s
        || certificate.log_powers.iter().any(|k| k.len() != n)
        || certificate.inverse.len() != s
        || certificate.inverse.iter().any(|row| row.len() != s)
    {
        return Err(refuse(
            op,
            format!("{at} is not of {s} log monomials of length {n} and an inverse of size {s}"),
        ));
    }
    // B_R: row r, column c.
    use num_traits::{One, Zero};
    let mut b = vec![vec![num_rational::BigRational::zero(); s]; s];
    for (c, item) in solutions.iter().enumerate() {
        context.interrupt.checkpoint()?;
        let terms = item
            .get("terms")
            .or_else(|| item.get("evidence_terms"))
            .ok_or_else(|| refuse(op, format!("{at}: solution {c} has no terms")))?;
        let terms = read_terms(terms).map_err(|e| refuse(op, format!("{at}: {e}")))?;
        for t in terms.iter().filter(|t| t.shift.iter().all(|&u| u == 0)) {
            let logs = t.logs(n);
            for (r, k) in certificate.log_powers.iter().enumerate() {
                if *k == logs {
                    b[r][c] += &t.coefficient.0;
                }
            }
        }
    }
    for (r, row) in b.iter().enumerate() {
        context.interrupt.checkpoint()?;
        for c in 0..s {
            let mut sum = num_rational::BigRational::zero();
            for (k, x) in row.iter().enumerate() {
                sum += x * &certificate.inverse[k][c].0;
            }
            let identity = if r == c {
                num_rational::BigRational::one()
            } else {
                num_rational::BigRational::zero()
            };
            if sum != identity {
                return Err(refuse(
                    op,
                    format!("{at}: the product with the inverse is not 1 at ({r}, {c})"),
                ));
            }
        }
    }
    Ok(())
}

/// A product stopped at a checkpoint, or failed on the data.
enum Stop {
    Interrupted(CasError),
    Math(algebramesh_math_types::MathError),
}

impl From<algebramesh_math_types::MathError> for Stop {
    fn from(e: algebramesh_math_types::MathError) -> Self {
        Self::Math(e)
    }
}

const TORIC_GENERATES: &str = "4ti2 (markov) through Macaulay2's toricIdealPartials";
const GENERATED: &str = "Macaulay2's == on the ideal gkz(A, beta) and the one the toric \
                         generators and the Euler operators generate";

// ------------------------------------------------------------ gkz.generate

/// `gkz.generate`: the Euler operators are recomputed from A and beta, the
/// toric generators are checked to lie in I_A; that they generate it, that
/// the GKZ ideal is theirs and the memberships are the CAS's answers.
pub struct GenerateVerifier;

impl OperationVerifier for GenerateVerifier {
    fn operation(&self) -> &'static str {
        GKZ_GENERATE
    }

    fn verify(
        &self,
        spec: &JobSpec,
        value: &serde_json::Value,
        context: &Context<'_>,
    ) -> Result<Vec<Claim>, CasError> {
        let op = GKZ_GENERATE;
        let args: GkzGenerateArguments = arguments(op, spec)?;
        let input = input(op, args.a, args.beta)?;
        let euler = operators(op, &input, value, "euler_operators")?;
        let matches = input
            .euler_operators_match(&euler)
            .map_err(|e| refuse(op, e))?;
        if !matches {
            return Err(refuse(
                op,
                "the Euler operators are not E_i - beta_i, up to units and order",
            ));
        }
        check_field(op, value, "euler_operators_match")?;
        let toric_list = toric(op, &input, value)?;
        generated(op, value)?;
        let answers = value["checks"]["expected_in_ideal"]
            .as_array()
            .ok_or_else(|| refuse(op, "the value has no list checks.expected_in_ideal"))?;
        if answers.len() != args.expected.len()
            || answers.iter().any(|a| !a["in_ideal"].is_boolean())
        {
            return Err(refuse(
                op,
                format!(
                    "{} membership answers for {} operators",
                    answers.len(),
                    args.expected.len()
                ),
            ));
        }
        let expected = args
            .expected
            .iter()
            .map(|terms| {
                DifferentialOperator::from_terms(
                    input.weyl_algebra(),
                    terms
                        .iter()
                        .cloned()
                        .map(|t| (t.x_exponents, t.d_exponents, t.coefficient)),
                )
                .map_err(|e| refuse(op, format!("the job's expected operator: {e}")))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let certified = membership(
            op,
            &input,
            value,
            &toric_list,
            &euler,
            &expected,
            answers,
            context,
        )?;
        let mut claims = vec![
            context.checked("gkz.euler", Method::Recomputation),
            context.checked("gkz.toric_in_IA", Method::SelfCertifying),
            asserted_on(
                context,
                "gkz.toric_generates_IA",
                TORIC_GENERATES,
                &["gkz.toric_in_IA"],
            ),
            context.asserted("gkz.generated_by_toric_and_euler", GENERATED),
        ];
        for (j, certified) in certified.into_iter().enumerate() {
            let id = format!("gkz.membership[{j}]");
            claims.push(if certified {
                checked_on(
                    context,
                    &id,
                    Method::Certificate,
                    &["gkz.euler", "gkz.toric_in_IA"],
                )
            } else {
                asserted_on(
                    context,
                    &id,
                    "Macaulay2's isSubset",
                    &[
                        "gkz.generated_by_toric_and_euler",
                        "gkz.euler",
                        "gkz.toric_in_IA",
                        "gkz.toric_generates_IA",
                    ],
                )
            });
        }
        Ok(claims)
    }

    /// The Euler operators and the toric generators from their fields
    /// `checks.*`; a membership with a certificate is asserted, since its
    /// operator is in the job's specification.
    fn legacy_claims(
        &self,
        value: &serde_json::Value,
        context: &Context<'_>,
    ) -> Option<Result<Vec<Claim>, CasError>> {
        Some(legacy_generate(value, context))
    }

    fn not_claimed(&self) -> Vec<String> {
        [
            "that the three lists are Groebner bases, reduced or minimal, or in a fixed order or scaling",
            "that the toric generators are a minimal or unique set of generators",
            "that H_A(beta) is holonomic, its holonomic rank, and its solutions",
            "that beta is generic or nonresonant",
            "membership in the two-sided or the right ideal",
        ]
        .map(String::from)
        .to_vec()
    }
}

/// The claims of a `gkz.generate` value recorded without a guarantee.
fn legacy_generate(
    value: &serde_json::Value,
    context: &Context<'_>,
) -> Result<Vec<Claim>, CasError> {
    let op = GKZ_GENERATE;
    check_field(op, value, "euler_operators_match")?;
    check_field(op, value, "toric_binomials_valid")?;
    generated(op, value)?;
    let answers = value["checks"]["expected_in_ideal"]
        .as_array()
        .ok_or_else(|| refuse(op, "the value has no list checks.expected_in_ideal"))?;
    if answers.iter().any(|a| !a["in_ideal"].is_boolean()) {
        return Err(refuse(op, "a membership answer is not true or false"));
    }
    let mut claims = vec![
        context.recorded(
            "gkz.euler",
            Method::Recomputation,
            "checks.euler_operators_match",
        ),
        context.recorded(
            "gkz.toric_in_IA",
            Method::SelfCertifying,
            "checks.toric_binomials_valid",
        ),
        asserted_on(
            context,
            "gkz.toric_generates_IA",
            TORIC_GENERATES,
            &["gkz.toric_in_IA"],
        ),
        context.asserted("gkz.generated_by_toric_and_euler", GENERATED),
    ];
    let certified = value["certificate"]["membership"].as_array();
    for j in 0..answers.len() {
        let reason = if certified.is_some_and(|c| c.get(j).is_some_and(|e| !e.is_null())) {
            "Macaulay2's isSubset; its certificate needs the operator of the job's \
             specification, and algebramesh verify checks it"
        } else {
            "Macaulay2's isSubset"
        };
        claims.push(asserted_on(
            context,
            &format!("gkz.membership[{j}]"),
            reason,
            &[
                "gkz.generated_by_toric_and_euler",
                "gkz.euler",
                "gkz.toric_in_IA",
                "gkz.toric_generates_IA",
            ],
        ));
    }
    Ok(claims)
}

// ------------------------------------------------------ the local data

/// What the checks of the local data found, for those of the series.
struct Local {
    input: GkzInput,
    exponents: Vec<(Vec<Rational>, u32)>,
}

const LOCAL_NOT_CLAIMED: [&str; 5] = [
    "that the weight is generic for H_A(beta)",
    "that the holonomic rank and the indicial ideal were computed independently",
    "that the formal solutions converge",
    "that the exponents and their multiplicities give a basis of the solutions",
    "anything about other weights and parameters",
];

/// The claims of the local data of `gkz.indicial` and `gkz.series`.
fn local(
    op: &str,
    a: algebramesh_math_types::IntegerMatrix,
    beta: Vec<Rational>,
    weight: &[i64],
    value: &serde_json::Value,
    context: &Context<'_>,
) -> Result<(Vec<Claim>, Local), CasError> {
    let input = input(op, a, beta)?;
    input
        .validate_weight(weight)
        .map_err(|e| refuse(op, format!("the job's weight: {e}")))?;

    // local.parameters: computed again from A and beta.
    let report = input.parameter_report().map_err(|e| refuse(op, e))?;
    if serde_json::to_value(&report).ok().as_ref() != Some(&value["parameters"]) {
        return Err(refuse(op, "the parameters are not those of A and beta"));
    }
    context.interrupt.checkpoint()?;

    let theta = theta_ring(input.n())?;
    let indicial = value["indicial_ideal"]
        .as_array()
        .ok_or_else(|| refuse(op, "the value has no list indicial_ideal"))?
        .iter()
        .map(|p| {
            let data: PolynomialData = serde_json::from_value(p["terms"].clone())
                .map_err(|e| refuse(op, format!("indicial_ideal: {e}")))?;
            Polynomial::from_data(theta.clone(), &data)
                .map_err(|e| refuse(op, format!("indicial_ideal: {e}")))
        })
        .collect::<Result<Vec<_>, _>>()?;

    let rank: u32 = serde_json::from_value(value["holonomic_rank"].clone())
        .map_err(|e| refuse(op, format!("holonomic_rank: {e}")))?;
    let mut exponents = Vec::new();
    let mut seen = BTreeSet::new();
    for e in value["exponents"]
        .as_array()
        .ok_or_else(|| refuse(op, "the value has no list exponents"))?
    {
        context.interrupt.checkpoint()?;
        let point: Vec<Rational> = serde_json::from_value(e["point"].clone())
            .map_err(|e| refuse(op, format!("an exponent: {e}")))?;
        let multiplicity: u32 = serde_json::from_value(e["multiplicity"].clone())
            .map_err(|e| refuse(op, format!("a multiplicity: {e}")))?;
        input
            .check_exponent(&point, &indicial)
            .map_err(|m| refuse(op, format!("exponent {point:?}: {m}")))?;
        if multiplicity == 0 {
            return Err(refuse(op, "exponent of multiplicity 0"));
        }
        // A root listed twice would count its multiplicity twice in the sum.
        if !seen.insert(point.clone()) {
            return Err(refuse(op, format!("exponent {point:?} appears twice")));
        }
        exponents.push((point, multiplicity));
    }
    check_field(op, value, "exponents_are_roots")?;

    // local.rank_sum, as a consistency of the CAS's answers (ADR 0015): the
    // exponents describe the whole solution space only if their
    // multiplicities account for the holonomic rank. For a homogeneous A, a
    // weight that is not generic is a cause. For an inhomogeneous A, H_A(beta)
    // is irregular, and the sum can be smaller than the rank even for a
    // generic weight: for A = (1 2), beta = 1/3 and w = (1,3), there is one
    // root, of multiplicity 1, and the rank is 2.
    let sum = multiplicity_sum(exponents.iter().map(|(_, m)| *m)).ok_or_else(|| {
        refuse(
            op,
            "the sum of the multiplicities leaves the range of 64-bit integers",
        )
    })?;
    if sum != u64::from(rank) {
        return Err(refuse(
            op,
            format!(
                "the multiplicities of the {} exponents sum to {sum}, but the holonomic rank \
                 is {rank}: the local data for weight {weight:?} is incomplete. For a \
                 homogeneous A, the weight may not be generic for this A and beta; for an \
                 inhomogeneous A, the sum can be smaller than the rank even for a generic \
                 weight",
                exponents.len()
            ),
        ));
    }
    check_field(op, value, "multiplicities_sum_to_rank")?;

    let claims = vec![
        context.checked("local.exponent_equations", Method::SelfCertifying),
        checked_on(
            context,
            "local.exponent_roots",
            Method::SelfCertifying,
            &["local.indicial_ideal"],
        ),
        context.asserted(
            "local.indicial_ideal",
            "Macaulay2's gkz, inw, isTorusFixed and distraction",
        ),
        asserted_on(
            context,
            "local.roots_complete",
            "Macaulay2's primaryDecomposition, radical and degree",
            &["local.indicial_ideal"],
        ),
        context.asserted("local.rank", "Macaulay2's holonomicRank"),
        asserted_on(
            context,
            "local.rank_sum",
            "the CAS's multiplicities and rank, whose sums were compared",
            &["local.rank", "local.roots_complete"],
        ),
        context.checked("local.parameters", Method::Recomputation),
    ];
    Ok((claims, Local { input, exponents }))
}

/// The claims of the local data of a value recorded without a guarantee.
fn legacy_local(
    op: &str,
    value: &serde_json::Value,
    context: &Context<'_>,
) -> Result<Vec<Claim>, CasError> {
    check_field(op, value, "exponents_are_roots")?;
    check_field(op, value, "multiplicities_sum_to_rank")?;
    let mut claims = vec![
        context.recorded(
            "local.exponent_equations",
            Method::SelfCertifying,
            "checks.exponents_are_roots",
        ),
        Claim {
            depends_on: vec!["local.indicial_ideal".into()],
            ..context.recorded(
                "local.exponent_roots",
                Method::SelfCertifying,
                "checks.exponents_are_roots",
            )
        },
        context.asserted(
            "local.indicial_ideal",
            "Macaulay2's gkz, inw, isTorusFixed and distraction",
        ),
        asserted_on(
            context,
            "local.roots_complete",
            "Macaulay2's primaryDecomposition, radical and degree",
            &["local.indicial_ideal"],
        ),
        context.asserted("local.rank", "Macaulay2's holonomicRank"),
        asserted_on(
            context,
            "local.rank_sum",
            "the CAS's multiplicities and rank, whose sums were compared",
            &["local.rank", "local.roots_complete"],
        ),
    ];
    // The parameters are computed by Rust from A and beta when the value is
    // written (ADR 0015).
    if !value["parameters"].is_object() {
        return Err(refuse(op, "the value has no parameters"));
    }
    claims.push(context.recorded("local.parameters", Method::Recomputation, "parameters"));
    Ok(claims)
}

/// `gkz.indicial`: the local data.
pub struct IndicialVerifier;

impl OperationVerifier for IndicialVerifier {
    fn operation(&self) -> &'static str {
        GKZ_INDICIAL
    }

    fn verify(
        &self,
        spec: &JobSpec,
        value: &serde_json::Value,
        context: &Context<'_>,
    ) -> Result<Vec<Claim>, CasError> {
        let args: GkzIndicialArguments = arguments(GKZ_INDICIAL, spec)?;
        let (claims, _) = local(
            GKZ_INDICIAL,
            args.a,
            args.beta,
            &args.weight,
            value,
            context,
        )?;
        Ok(claims)
    }

    fn legacy_claims(
        &self,
        value: &serde_json::Value,
        context: &Context<'_>,
    ) -> Option<Result<Vec<Claim>, CasError>> {
        Some(legacy_local(GKZ_INDICIAL, value, context))
    }

    fn not_claimed(&self) -> Vec<String> {
        LOCAL_NOT_CLAIMED.map(String::from).to_vec()
    }
}

// ------------------------------------------------------------ gkz.series

/// `gkz.series`: the local data, the toric generators, and each series.
pub struct SeriesVerifier;

/// One series of the value with its status.
enum Item<'a> {
    Verified(&'a serde_json::Value, u32),
    NotASolution(&'a serde_json::Value, u32),
}

impl OperationVerifier for SeriesVerifier {
    fn operation(&self) -> &'static str {
        GKZ_SERIES
    }

    fn verify(
        &self,
        spec: &JobSpec,
        value: &serde_json::Value,
        context: &Context<'_>,
    ) -> Result<Vec<Claim>, CasError> {
        let op = GKZ_SERIES;
        let args: GkzSeriesArguments = arguments(op, spec)?;
        let (weight, order) = (args.weight.clone(), args.order);
        let (mut claims, local) = local(op, args.a, args.beta, &weight, value, context)?;
        let input = &local.input;
        let toric = toric(op, input, value)?;
        generated(op, value)?;
        if value["order"] != order {
            return Err(refuse(op, "the order of the value is not that of the job"));
        }
        let series = value["series"]
            .as_array()
            .ok_or_else(|| refuse(op, "the value has no list series"))?;
        if series.len() != local.exponents.len() {
            return Err(refuse(
                op,
                format!(
                    "{} series for {} exponents",
                    series.len(),
                    local.exponents.len()
                ),
            ));
        }

        // Each series against the system. `owners` locates each verified
        // solution in the value, as the backend listed them.
        let mut solutions: Vec<Solution> = Vec::new();
        let mut owners: Vec<&serde_json::Value> = Vec::new();
        // A series not_a_solution recorded before its evidence was (ADR
        // 0028), checked again.
        let mut without_evidence = false;
        for (index, (entry, (point, multiplicity))) in
            series.iter().zip(&local.exponents).enumerate()
        {
            context.interrupt.checkpoint()?;
            if entry["point"] != serde_json::json!(point) || entry["multiplicity"] != *multiplicity
            {
                return Err(refuse(
                    op,
                    format!("series {index} is not that of exponent {point:?}"),
                ));
            }
            let mut items = Vec::new();
            match (entry["status"].as_str(), *multiplicity) {
                (Some("verified_to_order"), 1) => items.push((None, Item::Verified(entry, 0))),
                (Some("not_a_solution"), 1) => items.push((None, Item::NotASolution(entry, 0))),
                (Some("unsupported"), _) => {}
                (Some("logarithmic"), m) if m > 1 => {
                    let found = entry["solutions"].as_array().ok_or_else(|| {
                        refuse(op, format!("exponent {point:?} has no solutions"))
                    })?;
                    let missing: u32 = serde_json::from_value(entry["missing"].clone())
                        .map_err(|e| refuse(op, format!("missing: {e}")))?;
                    // series.missing, as a consistency: counted in u64.
                    let counted = u64::try_from(found.len())
                        .ok()
                        .and_then(|f| f.checked_add(u64::from(missing)));
                    if counted != Some(u64::from(m)) {
                        return Err(refuse(
                            op,
                            format!(
                                "{} solutions and {missing} missing for exponent {point:?} of \
                                 multiplicity {m}",
                                found.len()
                            ),
                        ));
                    }
                    for (b, item) in found.iter().enumerate() {
                        let degree: u32 = serde_json::from_value(item["log_degree"].clone())
                            .map_err(|e| refuse(op, format!("log_degree: {e}")))?;
                        match item["status"].as_str() {
                            Some("verified_to_order") => {
                                items.push((Some(b), Item::Verified(item, degree)));
                            }
                            Some("not_a_solution") => {
                                items.push((Some(b), Item::NotASolution(item, degree)));
                            }
                            other => {
                                return Err(refuse(
                                    op,
                                    format!(
                                        "solution {b} of exponent {point:?} has the status {other:?}"
                                    ),
                                ));
                            }
                        }
                    }
                }
                (other, m) => {
                    return Err(refuse(
                        op,
                        format!("exponent {point:?} of multiplicity {m} has the status {other:?}"),
                    ));
                }
            }
            for (slot, item) in items {
                context.interrupt.checkpoint()?;
                let at = match slot {
                    None => format!("the series of exponent {point:?}"),
                    Some(b) => format!("solution {b} of exponent {point:?}"),
                };
                match item {
                    Item::Verified(item, degree) => {
                        let terms = read_terms(&item["terms"])
                            .map_err(|e| refuse(op, format!("{at}: {e}")))?;
                        // series.shape, series.euler_exact, series.toric_to_order.
                        match input.check_series(point, &terms, degree, &weight, order, &toric) {
                            Ok(()) => {}
                            Err(SeriesDefect::Malformed(reason)) => {
                                return Err(refuse(op, format!("{at}: {reason}")));
                            }
                            Err(SeriesDefect::NotASolution(reason)) => {
                                return Err(refuse(
                                    op,
                                    format!("{at} is verified_to_order, but {reason}"),
                                ));
                            }
                        }
                        if item["terms"] != render_terms(point, &weight, &terms) {
                            return Err(refuse(
                                op,
                                format!("{at}: the terms are not written as their shifts give"),
                            ));
                        }
                        solutions.push(Solution {
                            exponent: index,
                            index: slot.unwrap_or(0),
                            log_degree: degree,
                            terms,
                        });
                        owners.push(item);
                    }
                    Item::NotASolution(item, _)
                        if context.recheck && item.get("evidence_terms").is_none() =>
                    {
                        if item.get("terms").is_some() {
                            return Err(refuse(
                                op,
                                format!("{at} is not_a_solution and has terms"),
                            ));
                        }
                        without_evidence = true;
                    }
                    Item::NotASolution(item, degree) => {
                        // series.not_a_solution: the evidence is again not a
                        // solution, by a coefficient found here.
                        let terms = read_terms(&item["evidence_terms"])
                            .map_err(|e| refuse(op, format!("{at}, the evidence: {e}")))?;
                        match input.check_series(point, &terms, degree, &weight, order, &toric) {
                            Err(SeriesDefect::NotASolution(_)) => {}
                            Err(SeriesDefect::Malformed(reason)) => {
                                return Err(refuse(op, format!("{at}, the evidence: {reason}")));
                            }
                            Ok(()) => {
                                return Err(refuse(
                                    op,
                                    format!(
                                        "{at} is not_a_solution, but its evidence solves the system to the order"
                                    ),
                                ));
                            }
                        }
                        if item.get("terms").is_some() {
                            return Err(refuse(
                                op,
                                format!("{at} is not_a_solution and has terms"),
                            ));
                        }
                    }
                }
            }
        }

        // series.canonical: computed again.
        let points: Vec<Vec<Rational>> = local.exponents.iter().map(|(p, _)| p.clone()).collect();
        for (i, item) in owners.iter().enumerate() {
            context.interrupt.checkpoint()?;
            let mut expected = serde_json::json!({});
            canonical_fields(
                input,
                &points,
                &solutions,
                i,
                &weight,
                order,
                &toric,
                &mut expected,
            )
            .map_err(|e| refuse(op, e))?;
            for key in [
                "canonical",
                "normalization",
                "canonical_terms",
                "canonical_reason",
            ] {
                if item.get(key) != expected.get(key) {
                    return Err(refuse(
                        op,
                        format!(
                            "the field {key} of solution {i} is not the one normalization gives"
                        ),
                    ));
                }
            }
        }

        // series.log_independent (Issue #102): checked when every exponent
        // with logarithmic solutions carries a certificate that checks.
        let mut logarithmic = 0usize;
        let mut certified = 0usize;
        for entry in series {
            if entry["status"] != "logarithmic" {
                continue;
            }
            logarithmic += 1;
            if entry.get("certificate").is_some() {
                independence(op, entry, input.n(), context)?;
                certified += 1;
            }
        }
        let log_independent = if logarithmic > 0 && certified == logarithmic {
            context.checked("series.log_independent", Method::Certificate)
        } else {
            context.asserted(
                "series.log_independent",
                "Macaulay2, by the rank of the matrix of the leading coefficients",
            )
        };

        claims.extend([
            context.checked("gkz.toric_in_IA", Method::SelfCertifying),
            asserted_on(
                context,
                "gkz.toric_generates_IA",
                TORIC_GENERATES,
                &["gkz.toric_in_IA"],
            ),
            context.asserted("gkz.generated_by_toric_and_euler", GENERATED),
            context.checked("series.shape", Method::SelfCertifying),
            context.checked("series.euler_exact", Method::SelfCertifying),
            truncated(
                checked_on(
                    context,
                    "series.toric_to_order",
                    Method::SelfCertifying,
                    &[
                        "series.shape",
                        "series.euler_exact",
                        "gkz.toric_generates_IA",
                    ],
                ),
                &weight,
                order,
            ),
            if without_evidence {
                asserted_on(
                    context,
                    "series.not_a_solution",
                    WITHOUT_EVIDENCE,
                    &["gkz.toric_in_IA"],
                )
            } else {
                checked_on(
                    context,
                    "series.not_a_solution",
                    Method::SelfCertifying,
                    &["gkz.toric_in_IA"],
                )
            },
            truncated(
                context.checked("series.canonical", Method::Recomputation),
                &weight,
                order,
            ),
            log_independent,
            context.asserted(
                "series.missing",
                "the driver's account of the solutions it did not find",
            ),
        ]);
        Ok(claims)
    }

    /// The local data and the toric generators from their fields
    /// `checks.*`, the series from their `status` and `canonical` fields. A
    /// series not_a_solution without `evidence_terms` makes
    /// `series.not_a_solution` asserted; `series.log_independent` is
    /// asserted, since its certificate is not checked here.
    fn legacy_claims(
        &self,
        value: &serde_json::Value,
        context: &Context<'_>,
    ) -> Option<Result<Vec<Claim>, CasError>> {
        Some(legacy_series(value, context))
    }

    fn not_claimed(&self) -> Vec<String> {
        let mut out: Vec<String> = LOCAL_NOT_CLAIMED.map(String::from).to_vec();
        out.extend(
            [
                "that the series form a basis of the solutions of H_A(beta)",
                "that a series verified_to_order extends to a solution beyond the order",
                "that the terms are the truncation of the Gamma-series or of its perturbation",
                "that not_a_solution means that the Gamma-series is not a solution",
                "that the check to the order holds for every element of H_A(beta)",
                "that the solutions of an exponent span a space of the dimension of its multiplicity",
                "that canonical false means that there is no canonical form",
                "that a canonical series is the truncation of a standard series of SST, Section 2.5",
            ]
            .map(String::from),
        );
        out
    }
}

/// Why `series.not_a_solution` of a recorded value is asserted.
const WITHOUT_EVIDENCE: &str = "a series not_a_solution has no evidence_terms: the value was \
                                recorded before they were (ADR 0028), and only the status of the \
                                driver's series remains";

/// The series of a value and their items, each with its status.
fn statuses(value: &serde_json::Value) -> Result<Vec<&serde_json::Value>, CasError> {
    let op = GKZ_SERIES;
    let mut items = Vec::new();
    for entry in value["series"]
        .as_array()
        .ok_or_else(|| refuse(op, "the value has no list series"))?
    {
        match entry["status"].as_str() {
            Some("verified_to_order" | "not_a_solution") => items.push(entry),
            Some("unsupported") => {}
            Some("logarithmic") => {
                for item in entry["solutions"]
                    .as_array()
                    .ok_or_else(|| refuse(op, "a logarithmic exponent has no solutions"))?
                {
                    match item["status"].as_str() {
                        Some("verified_to_order" | "not_a_solution") => items.push(item),
                        other => {
                            return Err(refuse(
                                op,
                                format!("a logarithmic solution has the status {other:?}"),
                            ));
                        }
                    }
                }
            }
            other => {
                return Err(refuse(op, format!("a series has the status {other:?}")));
            }
        }
    }
    Ok(items)
}

/// The claims of a `gkz.series` value recorded without a guarantee.
fn legacy_series(value: &serde_json::Value, context: &Context<'_>) -> Result<Vec<Claim>, CasError> {
    let op = GKZ_SERIES;
    let mut claims = legacy_local(op, value, context)?;
    check_field(op, value, "toric_binomials_valid")?;
    generated(op, value)?;
    let weight: Vec<i64> = serde_json::from_value(value["weight"].clone())
        .map_err(|e| refuse(op, format!("weight: {e}")))?;
    let order: u32 = serde_json::from_value(value["order"].clone())
        .map_err(|e| refuse(op, format!("order: {e}")))?;
    let without_evidence = statuses(value)?
        .iter()
        .any(|item| item["status"] == "not_a_solution" && item.get("evidence_terms").is_none());
    let status = "series[].status";
    claims.extend([
        context.recorded(
            "gkz.toric_in_IA",
            Method::SelfCertifying,
            "checks.toric_binomials_valid",
        ),
        asserted_on(
            context,
            "gkz.toric_generates_IA",
            TORIC_GENERATES,
            &["gkz.toric_in_IA"],
        ),
        context.asserted("gkz.generated_by_toric_and_euler", GENERATED),
        context.recorded("series.shape", Method::SelfCertifying, status),
        context.recorded("series.euler_exact", Method::SelfCertifying, status),
        truncated(
            Claim {
                depends_on: [
                    "series.shape",
                    "series.euler_exact",
                    "gkz.toric_generates_IA",
                ]
                .map(String::from)
                .to_vec(),
                ..context.recorded("series.toric_to_order", Method::SelfCertifying, status)
            },
            &weight,
            order,
        ),
        if without_evidence {
            asserted_on(
                context,
                "series.not_a_solution",
                WITHOUT_EVIDENCE,
                &["gkz.toric_in_IA"],
            )
        } else {
            Claim {
                depends_on: vec!["gkz.toric_in_IA".into()],
                ..context.recorded("series.not_a_solution", Method::SelfCertifying, status)
            }
        },
        truncated(
            context.recorded(
                "series.canonical",
                Method::Recomputation,
                "series[].canonical",
            ),
            &weight,
            order,
        ),
        context.asserted(
            "series.log_independent",
            "Macaulay2, by the rank of the matrix of the leading coefficients",
        ),
        context.asserted(
            "series.missing",
            "the driver's account of the solutions it did not find",
        ),
    ]);
    Ok(claims)
}

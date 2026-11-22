//! The values of `gkz.generate`, `gkz.indicial` and `gkz.series`, built from
//! what a CAS answered (Issues #99, #100).
//!
//! A backend reads its CAS's output into the answers of this module
//! ([`LocalAnswer`] and the operators of [`generate_value`]) and gets the value
//! of the job from them. Building the value decides what only the value can
//! record: the status of each series (`verified_to_order` or
//! `not_a_solution`, by [`GkzInput::check_series`]) and its canonical form
//! ([`GkzInput::normalize`]). Nothing here refuses a result for a claim the
//! value makes: [`crate::verify`] checks those after the backend, from the
//! value and the job alone. What is refused here is an answer that does not
//! have the form of the request: a series that is not of the stated form
//! ([`SeriesDefect::Malformed`]) or series data that does not fit the
//! multiplicity of its exponent.

use algebramesh_core::CasError;
use algebramesh_core::check::Interrupt;
use algebramesh_math_types::{
    CoefficientDomain, DifferentialOperator, MonomialOrder, Polynomial, Rational, RingSpec,
};
use num_bigint::BigInt;
use num_rational::BigRational;

use crate::series::{log_degree, weight_of};
use crate::{GkzInput, ParameterReport, SeriesDefect, SeriesTerm, Solution};

fn bad(e: impl std::fmt::Display) -> CasError {
    CasError::UnsupportedResult(e.to_string())
}

/// The ring `QQ[theta1, ..., thetan]` of the indicial ideal, with grevlex.
pub fn theta_ring(n: usize) -> Result<RingSpec, CasError> {
    RingSpec::new(
        CoefficientDomain::Rationals,
        (1..=n).map(|i| format!("theta{i}")).collect(),
        MonomialOrder::Grevlex,
    )
    .map_err(bad)
}

/// Operators as the value lists them: each with its display and its terms.
pub fn operators_json(list: &[DifferentialOperator]) -> Vec<serde_json::Value> {
    list.iter()
        .map(|g| serde_json::json!({"display": g.to_string(), "terms": g.to_data()}))
        .collect()
}

/// `x1^(1/6)*x2^(1/3)`: a monomial with rational exponents, for display only.
fn monomial_display(exponent: &[Rational]) -> String {
    let factors: Vec<String> = exponent
        .iter()
        .enumerate()
        .filter(|(_, e)| !e.is_zero())
        .map(|(j, e)| {
            if *e == Rational::one() {
                format!("x{}", j + 1)
            } else if e.is_integer() && !e.is_negative() {
                format!("x{}^{e}", j + 1)
            } else {
                format!("x{}^({e})", j + 1)
            }
        })
        .collect();
    if factors.is_empty() {
        "1".into()
    } else {
        factors.join("*")
    }
}

fn term_display(coefficient: &Rational, exponent: &[Rational]) -> String {
    let m = monomial_display(exponent);
    if m == "1" {
        coefficient.to_string()
    } else if *coefficient == Rational::one() {
        m
    } else if *coefficient == Rational::integer(-1) {
        format!("-{m}")
    } else {
        format!("{coefficient}*{m}")
    }
}

/// `term_display` times `prod_i log(x_i)^(k_i)`, for the divided power `l^(k)` with
/// coefficient `c`, that is `c / prod_i k_i!` times that product.
fn log_term_display(coefficient: &Rational, exponent: &[Rational], k: &[u32]) -> String {
    let factorial: BigInt = k.iter().flat_map(|&ki| 1..=ki).map(BigInt::from).product();
    let c = Rational(&coefficient.0 / BigRational::from_integer(factorial));
    let logs: Vec<String> = k
        .iter()
        .enumerate()
        .filter(|(_, ki)| **ki > 0)
        .map(|(i, &ki)| {
            if ki == 1 {
                format!("log(x{})", i + 1)
            } else {
                format!("log(x{})^{ki}", i + 1)
            }
        })
        .collect();
    let base = term_display(&c, exponent);
    if logs.is_empty() {
        return base;
    }
    let logs = logs.join("*");
    match base.as_str() {
        "1" => logs,
        "-1" => format!("-{logs}"),
        _ => format!("{base}*{logs}"),
    }
}

/// `d_1 log(x1) + ...` for the direction `d`.
pub fn log_display(direction: &[i64]) -> String {
    let mut out = String::new();
    for (j, &d) in direction.iter().enumerate().filter(|(_, d)| **d != 0) {
        let magnitude = d.unsigned_abs();
        let factor = if magnitude == 1 {
            String::new()
        } else {
            format!("{magnitude}*")
        };
        let sign = match (out.is_empty(), d < 0) {
            (true, false) => "",
            (true, true) => "-",
            (false, false) => " + ",
            (false, true) => " - ",
        };
        out.push_str(&format!("{sign}{factor}log(x{})", j + 1));
    }
    out
}

/// Terms of a series about `point`, sorted by weight, as JSON.
pub fn render_terms(point: &[Rational], weight: &[i64], terms: &[SeriesTerm]) -> serde_json::Value {
    let mut terms = terms.to_vec();
    terms.sort_by_key(|t| {
        (
            weight_of(weight, &t.shift),
            t.shift.clone(),
            std::cmp::Reverse(log_degree(&t.log_powers)),
            t.log_powers.clone(),
        )
    });
    terms
        .iter()
        .map(|t| {
            let exponent: Vec<Rational> = point
                .iter()
                .zip(&t.shift)
                .map(|(v, &u)| Rational(&v.0 + BigRational::from_integer(u.into())))
                .collect();
            let mut term = serde_json::json!({
                "shift": t.shift,
                "exponent": exponent,
                "weight": weight_of(weight, &t.shift).to_string(),
                "coefficient": t.coefficient,
                "display": log_term_display(&t.coefficient, &exponent, &t.log_powers),
            });
            if !t.log_powers.is_empty() {
                term["log_powers"] = t.log_powers.clone().into();
            }
            term
        })
        .collect::<Vec<_>>()
        .into()
}

/// The value of `gkz.generate`: the generators of the GKZ ideal the CAS built,
/// its toric generators and Euler operators, and whether it found each
/// operator of `expected` in the ideal. The checks of the value are those
/// [`crate::verify`] repeats; they are recorded here as the fields of the
/// value have always been.
///
/// `coefficients[j]`, for an operator P_j the CAS found in the ideal, are its
/// left coefficients over the toric generators and then the Euler operators
/// (Issue #102): P_j = sum_k q_k g_k. They go to `certificate.membership[j]`
/// as `{"toric": [...], "euler": [...]}`, `null` where there are none. An
/// empty list (a CAS that gives none) leaves the certificate out.
#[allow(clippy::too_many_arguments)]
pub fn generate_value(
    input: &GkzInput,
    generators: &[DifferentialOperator],
    toric: &[DifferentialOperator],
    euler: &[DifferentialOperator],
    generated_by_toric_and_euler: bool,
    expected: &[DifferentialOperator],
    in_ideal: &[bool],
    coefficients: &[Option<Vec<DifferentialOperator>>],
    holonomic_systems_version: &str,
) -> Result<serde_json::Value, CasError> {
    if in_ideal.len() != expected.len() {
        return Err(bad(format!(
            "{} membership answers for {} operators",
            in_ideal.len(),
            expected.len()
        )));
    }
    if !coefficients.is_empty() && coefficients.len() != expected.len() {
        return Err(bad(format!(
            "{} lists of coefficients for {} operators",
            coefficients.len(),
            expected.len()
        )));
    }
    let membership = coefficients
        .iter()
        .map(|q| match q {
            None => Ok(serde_json::Value::Null),
            Some(q) if q.len() == toric.len() + euler.len() => {
                let (t, e) = q.split_at(toric.len());
                Ok(serde_json::json!({"toric": operators_json(t), "euler": operators_json(e)}))
            }
            Some(q) => Err(bad(format!(
                "{} coefficients for {} generators",
                q.len(),
                toric.len() + euler.len()
            ))),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let euler_match = input.euler_operators_match(euler).map_err(bad)?;
    let toric_ok = input.toric_binomials_valid(toric);
    let mut value = serde_json::json!({
        "type": "gkz_system",
        "algebra": input.weyl_algebra(),
        "A": input.a(),
        "beta": input.beta(),
        "generators": operators_json(generators),
        "toric_generators": operators_json(toric),
        "euler_operators": operators_json(euler),
        "checks": {
            "euler_operators_match": euler_match,
            "toric_binomials_valid": toric_ok,
            "generated_by_toric_and_euler": generated_by_toric_and_euler,
            "expected_in_ideal": expected.iter().zip(in_ideal)
                .map(|(g, m)| serde_json::json!({"operator": g.to_string(), "in_ideal": m}))
                .collect::<Vec<_>>(),
        },
        "holonomic_systems_version": holonomic_systems_version,
    });
    if !coefficients.is_empty() {
        value["certificate"] = serde_json::json!({"membership": membership});
    }
    Ok(value)
}

/// One solution of an exponent of multiplicity above 1, as the CAS gave it.
#[derive(Debug, Clone)]
pub struct LogSolutionAnswer {
    pub log_degree: u32,
    /// The perturbation direction `d`; `None` for the Gamma-series itself.
    pub direction: Option<Vec<i64>>,
    pub terms: Vec<SeriesTerm>,
}

/// The series of one exponent, as the CAS gave it.
#[derive(Debug, Clone)]
pub enum SeriesAnswer {
    /// The Gamma-series of an exponent of multiplicity 1.
    Computed { terms: Vec<SeriesTerm> },
    /// The solutions of an exponent of multiplicity above 1, and how many it
    /// did not find.
    Logarithmic {
        solutions: Vec<LogSolutionAnswer>,
        missing: u32,
        reason: String,
        /// That the solutions are linearly independent (Issue #102), if the
        /// CAS gave it.
        independence: Option<Independence>,
    },
    /// No series, and why.
    Unsupported { reason: String },
}

/// A certificate that the solutions of one exponent are linearly independent
/// (Issue #102). B has a row for each log monomial and a column for each
/// solution, in the order of the solutions, with the coefficient of the term
/// x^v l^(k) (shift 0) of that solution; `log_powers` names s rows of B, for s
/// solutions, and `inverse` is the inverse of the s x s matrix B_R they give.
/// It goes to the value as `certificate.independence` of the exponent.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Independence {
    pub log_powers: Vec<Vec<u32>>,
    pub inverse: Vec<Vec<Rational>>,
}

/// An exponent with its multiplicity and, for `gkz.series`, its series.
#[derive(Debug, Clone)]
pub struct ExponentAnswer {
    pub point: Vec<Rational>,
    pub multiplicity: u32,
    pub series: Option<SeriesAnswer>,
}

/// The answer of a CAS to `gkz.indicial` or `gkz.series`.
#[derive(Debug, Clone)]
pub struct LocalAnswer {
    pub holonomic_rank: u32,
    /// Generators of the indicial ideal, in [`theta_ring`].
    pub indicial: Vec<Polynomial>,
    /// For `gkz.series`: the toric generators.
    pub toric: Vec<DifferentialOperator>,
    /// For `gkz.series`: whether the GKZ ideal is generated by the toric
    /// generators and the Euler operators.
    pub generated_by_toric_and_euler: Option<bool>,
    pub exponents: Vec<ExponentAnswer>,
}

/// A local request: the input, the weight, the order of `gkz.series` (none
/// for `gkz.indicial`) and the report on the parameters.
pub struct LocalJob<'a> {
    pub input: &'a GkzInput,
    pub weight: &'a [i64],
    pub order: Option<u32>,
    pub parameters: &'a ParameterReport,
}

/// The sum of the multiplicities, in u64 with checked additions: a u32 sum
/// wraps in release builds, and (2^32 - 1) + 3 would then match a rank of 2.
pub fn multiplicity_sum(multiplicities: impl IntoIterator<Item = u32>) -> Option<u64> {
    multiplicities
        .into_iter()
        .try_fold(0u64, |sum, m| sum.checked_add(u64::from(m)))
}

/// Where a verified solution is in the value: its exponent, and its index
/// among the logarithmic solutions of that exponent.
type Owner = (usize, Option<usize>);

/// The value of `gkz.indicial` (no order) or `gkz.series`. Each series is
/// checked against the system to get its status; a series that solves it
/// gets its canonical form, checked again as a solution.
pub fn local_value(
    job: &LocalJob<'_>,
    answer: LocalAnswer,
    interrupt: &Interrupt,
) -> Result<serde_json::Value, CasError> {
    let input = job.input;
    let theta = theta_ring(input.n())?;
    let indicial = &answer.indicial;
    let exponents: Vec<Vec<Rational>> = answer.exponents.iter().map(|e| e.point.clone()).collect();
    let mut roots = true;
    for point in &exponents {
        interrupt.checkpoint()?;
        roots &= input.check_exponent(point, indicial).is_ok();
    }
    let sum_matches = multiplicity_sum(answer.exponents.iter().map(|e| e.multiplicity))
        == Some(u64::from(answer.holonomic_rank));
    let mut value = serde_json::json!({
        "type": if job.order.is_some() { "gkz_series" } else { "gkz_indicial" },
        "algebra": input.weyl_algebra(),
        "A": input.a(),
        "beta": input.beta(),
        "weight": job.weight,
        "theta_ring": theta,
        "indicial_ideal": indicial.iter()
            .map(|p| serde_json::json!({"display": p.to_string(), "terms": p.to_data()}))
            .collect::<Vec<_>>(),
        "holonomic_rank": answer.holonomic_rank,
        "exponents": answer.exponents.iter()
            .map(|e| serde_json::json!({"point": e.point, "multiplicity": e.multiplicity}))
            .collect::<Vec<_>>(),
        "parameters": job.parameters,
        "checks": {
            "exponents_are_roots": roots,
            "multiplicities_sum_to_rank": sum_matches,
        },
    });
    let Some(order) = job.order else {
        return Ok(value);
    };

    let toric = &answer.toric;
    let toric_ok = input.toric_binomials_valid(toric);
    let generated = answer.generated_by_toric_and_euler == Some(true);
    let weight = job.weight;
    // First pass: check every series. `owners` locates each verified solution in
    // the output: the exponent, and the index among its logarithmic solutions.
    let mut series = Vec::new();
    let mut solutions: Vec<Solution> = Vec::new();
    let mut owners: Vec<Owner> = Vec::new();
    for (index, e) in answer.exponents.iter().enumerate() {
        interrupt.checkpoint()?;
        let point = &e.point;
        let mut entry = serde_json::json!({"point": point, "multiplicity": e.multiplicity});
        match &e.series {
            Some(SeriesAnswer::Computed { terms }) if e.multiplicity == 1 => {
                match input.check_series(point, terms, 0, weight, order, toric) {
                    Ok(()) => {
                        entry["status"] = "verified_to_order".into();
                        entry["terms"] = render_terms(point, weight, terms);
                        solutions.push(Solution {
                            exponent: index,
                            index: 0,
                            log_degree: 0,
                            terms: terms.clone(),
                        });
                        owners.push((index, None));
                    }
                    Err(SeriesDefect::NotASolution(reason)) => {
                        entry["status"] = "not_a_solution".into();
                        entry["reason"] = reason.into();
                        entry["evidence_terms"] = render_terms(point, weight, terms);
                    }
                    Err(SeriesDefect::Malformed(reason)) => {
                        return Err(bad(format!("series for exponent {point:?}: {reason}")));
                    }
                }
            }
            Some(SeriesAnswer::Logarithmic {
                solutions: found,
                missing,
                reason,
                independence,
            }) if e.multiplicity > 1 => {
                let mut entries = Vec::new();
                for (b, sol) in found.iter().enumerate() {
                    interrupt.checkpoint()?;
                    let mut item = serde_json::json!({
                        "log_degree": sol.log_degree,
                        "direction": sol.direction,
                    });
                    if let Some(d) = &sol.direction {
                        item["log"] = log_display(d).into();
                    }
                    let terms = &sol.terms;
                    match input.check_series(point, terms, sol.log_degree, weight, order, toric) {
                        Ok(()) => {
                            item["status"] = "verified_to_order".into();
                            item["terms"] = render_terms(point, weight, terms);
                            solutions.push(Solution {
                                exponent: index,
                                index: b,
                                log_degree: sol.log_degree,
                                terms: terms.clone(),
                            });
                            owners.push((index, Some(b)));
                        }
                        Err(SeriesDefect::NotASolution(reason)) => {
                            item["status"] = "not_a_solution".into();
                            item["reason"] = reason.into();
                            item["evidence_terms"] = render_terms(point, weight, terms);
                        }
                        Err(SeriesDefect::Malformed(reason)) => {
                            return Err(bad(format!(
                                "solution {b} for exponent {point:?}: {reason}"
                            )));
                        }
                    }
                    entries.push(item);
                }
                entry["status"] = "logarithmic".into();
                entry["solutions"] = entries.into();
                entry["missing"] = (*missing).into();
                if *missing > 0 {
                    entry["reason"] = reason.clone().into();
                }
                if let Some(independence) = independence {
                    entry["certificate"] = serde_json::json!({"independence": independence});
                }
            }
            Some(SeriesAnswer::Unsupported { reason }) => {
                entry["status"] = "unsupported".into();
                entry["reason"] = reason.clone().into();
            }
            _ => {
                return Err(bad(format!(
                    "unexpected series data for exponent {point:?}"
                )));
            }
        }
        series.push(entry);
    }

    // Second pass: canonical forms, checked again as solutions.
    for (i, &(index, slot)) in owners.iter().enumerate() {
        interrupt.checkpoint()?;
        let target = match slot {
            None => &mut series[index],
            Some(b) => &mut series[index]["solutions"][b],
        };
        canonical_fields(
            input, &exponents, &solutions, i, weight, order, toric, target,
        )?;
    }
    value["order"] = order.into();
    value["toric_generators"] = operators_json(toric).into();
    value["series"] = series.into();
    value["checks"]["toric_binomials_valid"] = toric_ok.into();
    value["checks"]["generated_by_toric_and_euler"] = generated.into();
    Ok(value)
}

/// The canonical form of `solutions[i]` written into `target`, the item of
/// the value that holds it: `canonical`, `normalization` and, when
/// normalization changed the series, `canonical_terms`; or `canonical`
/// false with `canonical_reason`. A changed series must again pass
/// [`GkzInput::check_series`].
#[allow(clippy::too_many_arguments)]
pub fn canonical_fields(
    input: &GkzInput,
    exponents: &[Vec<Rational>],
    solutions: &[Solution],
    i: usize,
    weight: &[i64],
    order: u32,
    toric: &[DifferentialOperator],
    target: &mut serde_json::Value,
) -> Result<(), CasError> {
    let solution = &solutions[i];
    let point = &exponents[solution.exponent];
    match input.normalize(exponents, solutions, i, weight, order) {
        Ok((terms, steps)) => {
            if !steps.is_empty() {
                input
                    .check_series(point, &terms, solution.log_degree, weight, order, toric)
                    .map_err(|e| bad(format!("canonical form for exponent {point:?}: {e:?}")))?;
                target["canonical_terms"] = render_terms(point, weight, &terms);
            }
            target["canonical"] = true.into();
            target["normalization"] = serde_json::to_value(&steps).map_err(bad)?;
        }
        Err(reason) => {
            target["canonical"] = false.into();
            target["canonical_reason"] = reason.into();
        }
    }
    Ok(())
}

/// The terms of a series as the value writes them ([`render_terms`]), read
/// back: shift, log powers and coefficient.
pub fn read_terms(terms: &serde_json::Value) -> Result<Vec<SeriesTerm>, String> {
    let list = terms
        .as_array()
        .ok_or_else(|| "the terms are not a list".to_owned())?;
    list.iter()
        .map(|t| {
            Ok(SeriesTerm {
                shift: serde_json::from_value(t["shift"].clone())
                    .map_err(|e| format!("a shift: {e}"))?,
                log_powers: match t.get("log_powers") {
                    Some(k) => {
                        serde_json::from_value(k.clone()).map_err(|e| format!("log powers: {e}"))?
                    }
                    None => Vec::new(),
                },
                coefficient: serde_json::from_value(t["coefficient"].clone())
                    .map_err(|e| format!("a coefficient: {e}"))?,
            })
        })
        .collect()
}

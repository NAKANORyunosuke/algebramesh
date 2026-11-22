//! The GKZ package on Macaulay2 (Issues #99, #100, `docs/packages/gkz.md`):
//! `gkz.generate`, `gkz.indicial` and `gkz.series`, with the driver
//! `drivers/macaulay2/gkz.m2`.
//!
//! This glue reads the job's arguments, writes the request to the driver and
//! reads its reply into the answers of `algebramesh-gkz`, which builds the
//! value. The claims of the value are checked after the backend, by
//! `algebramesh_gkz::verify`.

use algebramesh_core::CasError;
use algebramesh_core::check::Interrupt;
use algebramesh_gkz::value::{
    self, ExponentAnswer, Independence, LocalAnswer, LocalJob, LogSolutionAnswer, SeriesAnswer,
};
use algebramesh_gkz::{GkzInput, ParameterReport, SeriesTerm};
use algebramesh_math_types::{DifferentialOperator, IntegerMatrix, Rational};
use algebramesh_ops::{
    GKZ_GENERATE, GKZ_INDICIAL, GKZ_SERIES, GkzGenerateArguments, GkzIndicialArguments,
    GkzSeriesArguments,
};
use serde::Deserialize;

use crate::{DriverInfo, DriverTerm, WireTerm, bad, invalid, polynomial_from};

/// The operations of the package.
pub(crate) const OPERATIONS: [&str; 3] = [GKZ_GENERATE, GKZ_INDICIAL, GKZ_SERIES];

/// The types of their arguments and values.
pub(crate) const TYPES: [&str; 5] = [
    "integer_matrix",
    "weyl_algebra[QQ]",
    "gkz_system",
    "gkz_indicial",
    "gkz_series",
];

/// Largest truncation order accepted for `gkz.series`.
const MAX_ORDER: u32 = 1000;
/// Largest number of terms the driver computes for one series.
const MAX_TERMS: u32 = 10_000;

/// `gkz.indicial` (no order) and `gkz.series` (with an order).
pub(crate) struct LocalRequest {
    input: GkzInput,
    weight: Vec<i64>,
    order: Option<u32>,
    parameters: ParameterReport,
}

/// A job of the package.
pub(crate) enum GkzOperation {
    Generate(GkzInput, Vec<DifferentialOperator>),
    Local(LocalRequest),
}

fn local_request(
    a: IntegerMatrix,
    beta: Vec<Rational>,
    weight: Vec<i64>,
    order: Option<u32>,
) -> Result<GkzOperation, CasError> {
    let input = GkzInput::new(a, beta).map_err(invalid)?;
    input.validate_weight(&weight).map_err(invalid)?;
    if order.is_some_and(|k| k > MAX_ORDER) {
        return Err(invalid(format!("order must be at most {MAX_ORDER}")));
    }
    let parameters = input.parameter_report().map_err(invalid)?;
    Ok(GkzOperation::Local(LocalRequest {
        input,
        weight,
        order,
        parameters,
    }))
}

/// The job, if its operation is of the package. Reading it computes the
/// facets of cone(A) (bounded, Issue #24).
pub(crate) fn parse(
    operation: &str,
    arguments: &serde_json::Value,
) -> Result<Option<GkzOperation>, CasError> {
    match operation {
        GKZ_GENERATE => {
            let args: GkzGenerateArguments =
                serde_json::from_value(arguments.clone()).map_err(invalid)?;
            let input = GkzInput::new(args.a, args.beta).map_err(invalid)?;
            let algebra = input.weyl_algebra();
            let expected = args
                .expected
                .iter()
                .map(|terms| {
                    DifferentialOperator::from_terms(
                        algebra.clone(),
                        terms.iter().map(|t| {
                            (
                                t.x_exponents.clone(),
                                t.d_exponents.clone(),
                                t.coefficient.clone(),
                            )
                        }),
                    )
                    .map_err(invalid)
                })
                .collect::<Result<_, _>>()?;
            Ok(Some(GkzOperation::Generate(input, expected)))
        }
        GKZ_INDICIAL => {
            let args: GkzIndicialArguments =
                serde_json::from_value(arguments.clone()).map_err(invalid)?;
            local_request(args.a, args.beta, args.weight, None).map(Some)
        }
        GKZ_SERIES => {
            let args: GkzSeriesArguments =
                serde_json::from_value(arguments.clone()).map_err(invalid)?;
            local_request(args.a, args.beta, args.weight, Some(args.order)).map(Some)
        }
        _ => Ok(None),
    }
}

fn weyl_wire(op: &DifferentialOperator) -> Vec<WireTerm> {
    op.to_data()
        .into_iter()
        .map(|t| WireTerm {
            coefficient: t.coefficient.to_string(),
            exponents: t.x_exponents.into_iter().chain(t.d_exponents).collect(),
        })
        .collect()
}

/// The request to the driver.
pub(crate) fn request(op: &GkzOperation) -> serde_json::Value {
    match op {
        GkzOperation::Generate(input, expected) => serde_json::json!({
            "operation": GKZ_GENERATE,
            "A": input.a(),
            "beta": input.beta().iter().map(ToString::to_string).collect::<Vec<_>>(),
            "expected": expected.iter().map(weyl_wire).collect::<Vec<_>>(),
        }),
        GkzOperation::Local(local) => {
            let mut req = serde_json::json!({
                "operation": if local.order.is_some() { GKZ_SERIES } else { GKZ_INDICIAL },
                "A": local.input.a(),
                "beta": local.input.beta().iter().map(ToString::to_string).collect::<Vec<_>>(),
                "weight": local.weight,
            });
            if let Some(order) = local.order {
                req["order"] = order.into();
                req["max_terms"] = MAX_TERMS.into();
            }
            req
        }
    }
}

#[derive(Debug, Deserialize)]
struct GkzReply {
    n: usize,
    variables: Vec<String>,
    generated_by_toric_and_euler: bool,
    expected_in_ideal: Vec<bool>,
    /// The left coefficients of each operator found in the ideal (Issue
    /// #102); empty from a driver before it.
    #[serde(default)]
    membership_coefficients: Vec<Option<Vec<Vec<DriverTerm>>>>,
    generators: Vec<Vec<DriverTerm>>,
    toric: Vec<Vec<DriverTerm>>,
    euler: Vec<Vec<DriverTerm>>,
    info: DriverInfo,
}

#[derive(Debug, Deserialize)]
struct DriverSeriesTerm {
    shift: Vec<i64>,
    #[serde(default)]
    log_powers: Vec<u32>,
    coefficient: String,
}

#[derive(Debug, Deserialize)]
struct DriverLogSolution {
    log_degree: u32,
    /// The perturbation direction `d`; `None` for the Gamma-series itself.
    direction: Option<Vec<i64>>,
    terms: Vec<DriverSeriesTerm>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum DriverSeries {
    Computed {
        terms: Vec<DriverSeriesTerm>,
    },
    Logarithmic {
        solutions: Vec<DriverLogSolution>,
        missing: u32,
        #[serde(default)]
        reason: String,
        /// Issue #102; absent from a driver before it.
        #[serde(default)]
        independence: Option<DriverIndependence>,
    },
    Unsupported {
        reason: String,
    },
}

#[derive(Debug, Deserialize)]
struct DriverIndependence {
    log_powers: Vec<Vec<u32>>,
    inverse: Vec<Vec<String>>,
}

impl DriverIndependence {
    fn answer(&self) -> Result<Independence, CasError> {
        Ok(Independence {
            log_powers: self.log_powers.clone(),
            inverse: self
                .inverse
                .iter()
                .map(|row| {
                    row.iter()
                        .map(|c| c.parse::<Rational>().map_err(bad))
                        .collect::<Result<Vec<_>, _>>()
                })
                .collect::<Result<Vec<_>, _>>()?,
        })
    }
}

fn series_terms(terms: &[DriverSeriesTerm]) -> Result<Vec<SeriesTerm>, CasError> {
    terms
        .iter()
        .map(|t| {
            Ok(SeriesTerm {
                shift: t.shift.clone(),
                log_powers: t.log_powers.clone(),
                coefficient: t.coefficient.parse().map_err(bad)?,
            })
        })
        .collect()
}

impl DriverSeries {
    fn answer(&self) -> Result<SeriesAnswer, CasError> {
        Ok(match self {
            Self::Computed { terms } => SeriesAnswer::Computed {
                terms: series_terms(terms)?,
            },
            Self::Logarithmic {
                solutions,
                missing,
                reason,
                independence,
            } => SeriesAnswer::Logarithmic {
                solutions: solutions
                    .iter()
                    .map(|s| {
                        Ok(LogSolutionAnswer {
                            log_degree: s.log_degree,
                            direction: s.direction.clone(),
                            terms: series_terms(&s.terms)?,
                        })
                    })
                    .collect::<Result<_, CasError>>()?,
                missing: *missing,
                reason: reason.clone(),
                independence: independence
                    .as_ref()
                    .map(DriverIndependence::answer)
                    .transpose()?,
            },
            Self::Unsupported { reason } => SeriesAnswer::Unsupported {
                reason: reason.clone(),
            },
        })
    }
}

#[derive(Debug, Deserialize)]
struct DriverExponent {
    multiplicity: u32,
    point: Vec<String>,
    #[serde(default)]
    series: Option<DriverSeries>,
}

#[derive(Debug, Deserialize)]
struct LocalReply {
    n: usize,
    variables: Vec<String>,
    holonomic_rank: u32,
    indicial: Vec<Vec<DriverTerm>>,
    #[serde(default)]
    toric: Vec<Vec<DriverTerm>>,
    #[serde(default)]
    generated_by_toric_and_euler: Option<bool>,
    exponents: Vec<DriverExponent>,
    info: DriverInfo,
}

fn operator_from(input: &GkzInput, terms: &[DriverTerm]) -> Result<DifferentialOperator, CasError> {
    let n = input.n();
    let terms = terms
        .iter()
        .map(|t| {
            if t.exponents.len() != 2 * n {
                return Err(bad("exponent vector of the wrong length"));
            }
            Ok((
                t.exponents[..n].to_vec(),
                t.exponents[n..].to_vec(),
                t.coefficient.parse::<Rational>().map_err(bad)?,
            ))
        })
        .collect::<Result<Vec<_>, CasError>>()?;
    DifferentialOperator::from_terms(input.weyl_algebra(), terms).map_err(bad)
}

fn check_variables(input: &GkzInput, n: usize, variables: &[String]) -> Result<(), CasError> {
    let expected: Vec<String> = (1..=input.n())
        .map(|i| format!("x_{i}"))
        .chain((1..=input.n()).map(|i| format!("D_{i}")))
        .collect();
    if n != input.n() || variables != expected {
        return Err(bad(format!(
            "unexpected Weyl algebra variables {variables:?}"
        )));
    }
    Ok(())
}

/// The value of the reply, and the versions of the CAS.
pub(crate) fn interpret(
    op: &GkzOperation,
    reply: serde_json::Value,
    interrupt: &Interrupt,
) -> Result<(serde_json::Value, String), CasError> {
    match op {
        GkzOperation::Generate(input, expected) => {
            let reply: GkzReply = serde_json::from_value(reply).map_err(bad)?;
            check_variables(input, reply.n, &reply.variables)?;
            let convert = |list: &[Vec<DriverTerm>]| {
                list.iter()
                    .map(|t| operator_from(input, t))
                    .collect::<Result<Vec<_>, _>>()
            };
            let value = value::generate_value(
                input,
                &convert(&reply.generators)?,
                &convert(&reply.toric)?,
                &convert(&reply.euler)?,
                reply.generated_by_toric_and_euler,
                expected,
                &reply.expected_in_ideal,
                &reply
                    .membership_coefficients
                    .iter()
                    .map(|q| q.as_deref().map(convert).transpose())
                    .collect::<Result<Vec<_>, _>>()?,
                &reply.info.holonomic_systems_version,
            )?;
            Ok((value, reply.info.describe()))
        }
        GkzOperation::Local(local) => {
            let reply: LocalReply = serde_json::from_value(reply).map_err(bad)?;
            let input = &local.input;
            check_variables(input, reply.n, &reply.variables)?;
            let theta = value::theta_ring(input.n())?;
            let answer = LocalAnswer {
                holonomic_rank: reply.holonomic_rank,
                indicial: reply
                    .indicial
                    .iter()
                    .map(|t| polynomial_from(&theta, t))
                    .collect::<Result<_, _>>()?,
                toric: reply
                    .toric
                    .iter()
                    .map(|t| operator_from(input, t))
                    .collect::<Result<_, _>>()?,
                generated_by_toric_and_euler: reply.generated_by_toric_and_euler,
                exponents: reply
                    .exponents
                    .iter()
                    .map(|e| {
                        Ok(ExponentAnswer {
                            point: e
                                .point
                                .iter()
                                .map(|s| s.parse::<Rational>().map_err(bad))
                                .collect::<Result<_, _>>()?,
                            multiplicity: e.multiplicity,
                            series: e.series.as_ref().map(DriverSeries::answer).transpose()?,
                        })
                    })
                    .collect::<Result<_, CasError>>()?,
            };
            let job = LocalJob {
                input,
                weight: &local.weight,
                order: local.order,
                parameters: &local.parameters,
            };
            let value = value::local_value(&job, answer, interrupt)?;
            Ok((value, reply.info.describe()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CasRequest, Operation};
    use algebramesh_core::check::Interrupt;
    use algebramesh_core::guarantee::ResultVerifier;
    use algebramesh_core::{BackendInfo, CasResult, JobSpec};
    use algebramesh_ops::POLYNOMIAL_FACTOR;
    use num_bigint::BigInt;
    use num_rational::BigRational;
    use std::collections::BTreeMap;

    fn job(operation: &str, arguments: serde_json::Value) -> CasRequest {
        CasRequest {
            job_id: algebramesh_core::JobId::new(),
            operation: operation.into(),
            arguments,
            limits: Default::default(),
        }
    }

    fn parse(job: &CasRequest) -> Result<Operation, CasError> {
        crate::parse(job)
    }

    fn request(op: &Operation) -> serde_json::Value {
        crate::request(op)
    }

    /// The arguments of a GKZ job, written back from what was read.
    fn arguments(op: &Operation) -> serde_json::Value {
        let Operation::Gkz(op) = op else {
            panic!("not a GKZ job")
        };
        match op {
            GkzOperation::Generate(input, expected) => serde_json::json!({
                "A": input.a(), "beta": input.beta(),
                "expected": expected.iter().map(DifferentialOperator::to_data).collect::<Vec<_>>(),
            }),
            GkzOperation::Local(local) => {
                let mut arguments = serde_json::json!({
                    "A": local.input.a(), "beta": local.input.beta(), "weight": local.weight,
                });
                if let Some(order) = local.order {
                    arguments["order"] = order.into();
                }
                arguments
            }
        }
    }

    fn operation(op: &Operation) -> &'static str {
        match op {
            Operation::Gkz(GkzOperation::Generate(..)) => GKZ_GENERATE,
            Operation::Gkz(GkzOperation::Local(l)) if l.order.is_some() => GKZ_SERIES,
            Operation::Gkz(GkzOperation::Local(_)) => GKZ_INDICIAL,
            _ => POLYNOMIAL_FACTOR,
        }
    }

    /// The reply read by the backend and its value checked as the runner
    /// checks it, by the standard checks and those of the package: what a
    /// job with this reply gives (Issues #47, #99).
    fn interpret(
        op: &Operation,
        reply: serde_json::Value,
    ) -> Result<(serde_json::Value, String), CasError> {
        let (value, version) = crate::interpret(op, reply, &Interrupt::none())?;
        let spec = JobSpec {
            schema_version: 1,
            backend: crate::NAME.into(),
            operation: operation(op).into(),
            arguments: if matches!(op, Operation::Gkz(_)) {
                arguments(op)
            } else {
                serde_json::Value::Null
            },
            limits: Default::default(),
            retry_policy: Default::default(),
            environment: Default::default(),
        };
        if !matches!(op, Operation::Gkz(_)) {
            return Ok((value, version));
        }
        let result = CasResult {
            value,
            backend: BackendInfo::default(),
            execution_host: None,
            log: vec![],
            guarantee: None,
        };
        let checked =
            algebramesh_gkz::verify::registry().verify(&spec, result, &Interrupt::none())?;
        Ok((checked.value, version))
    }

    #[test]
    fn parses_gkz_arguments_with_expected_operators() {
        let op = parse(&job(
            GKZ_GENERATE,
            serde_json::json!({"A": [[1, 1, 1], [0, 1, 2]], "beta": ["3", "4"], "expected": [[
                {"coefficient": "1", "x_exponents": [0, 0, 0], "d_exponents": [1, 0, 1]},
                {"coefficient": "-1", "x_exponents": [0, 0, 0], "d_exponents": [0, 2, 0]}]]}),
        ))
        .unwrap();
        let req = request(&op);
        assert_eq!(
            req["expected"][0][0]["exponents"],
            serde_json::json!([0, 0, 0, 1, 0, 1])
        );
        assert!(matches!(
            parse(&job(
                GKZ_GENERATE,
                serde_json::json!({"A": [[1]], "beta": ["1"], "x": 1})
            )),
            Err(CasError::InvalidArguments(_))
        ));
    }

    /// `gkz.generate` for A = [[1,1,1],[0,1,2]], beta = (3,4), with the expected
    /// operators d1 d3 - d2^2 and d2.
    fn generate_job() -> Operation {
        let d = |e: [u32; 3], c: &str| {
            let x = [0, 0, 0];
            serde_json::json!({"coefficient": c, "x_exponents": x, "d_exponents": e})
        };
        parse(&job(
            GKZ_GENERATE,
            serde_json::json!({"A": [[1, 1, 1], [0, 1, 2]], "beta": ["3", "4"],
                "expected": [[d([1, 0, 1], "1"), d([0, 2, 0], "-1")], [d([0, 1, 0], "1")]]}),
        ))
        .unwrap()
    }

    /// The reply of Macaulay2 1.24.11 (HolonomicSystems 1.0) to the request of
    /// [`generate_job`], recorded in the image `algebramesh-agent-macaulay2:dev`
    /// with `M2 --script /opt/algebramesh/drivers/macaulay2/algebramesh.m2`.
    fn recorded_generate() -> serde_json::Value {
        serde_json::from_str(include_str!("../../testdata/gkz_generate.json")).unwrap()
    }

    #[test]
    fn interprets_a_recorded_gkz_generate_reply() {
        let (value, version) = interpret(&generate_job(), recorded_generate()).unwrap();
        assert_eq!(version, "Macaulay2 1.24.11 (HolonomicSystems 1.0)");
        assert_eq!(value["type"], "gkz_system");
        let checks = &value["checks"];
        assert_eq!(checks["euler_operators_match"], true);
        assert_eq!(checks["toric_binomials_valid"], true);
        assert_eq!(checks["generated_by_toric_and_euler"], true);
        let in_ideal: Vec<&serde_json::Value> = checks["expected_in_ideal"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| &m["in_ideal"])
            .collect();
        assert_eq!(in_ideal, [true, false]);
        assert_eq!(value["toric_generators"][0]["display"], "d1*d3-d2^2");
    }

    /// Whether driver terms over x1..xn, d1..dn, with integer coefficients, are
    /// c (sum_j a_j x_j d_j - b) for some c != 0, compared term by term.
    fn is_euler_multiple(terms: &serde_json::Value, a: &[i64], b: i64) -> bool {
        let n = a.len();
        let mut ours: Vec<(Vec<u64>, i64)> = (0..n)
            .filter(|&j| a[j] != 0)
            .map(|j| {
                let mut e = vec![0; 2 * n];
                e[j] = 1;
                e[n + j] = 1;
                (e, a[j])
            })
            .chain((b != 0).then(|| (vec![0; 2 * n], -b)))
            .collect();
        let mut theirs: Vec<(Vec<u64>, i64)> = terms
            .as_array()
            .unwrap()
            .iter()
            .map(|t| {
                let e = t["exponents"].as_array().unwrap();
                let c = t["coefficient"].as_str().unwrap().parse().unwrap();
                (e.iter().map(|k| k.as_u64().unwrap()).collect(), c)
            })
            .filter(|(_, c)| *c != 0)
            .collect();
        ours.sort();
        theirs.sort();
        ours.len() == theirs.len()
            && ours
                .iter()
                .zip(&theirs)
                .all(|((e, x), (f, y))| e == f && x * theirs[0].1 == y * ours[0].1)
    }

    /// gkz.euler: a changed, a dropped or a repeated Euler operator is refused,
    /// with the toric binomials kept. Multiples of E_i - beta_i are read from the
    /// terms.
    #[test]
    fn a_changed_or_dropped_euler_operator_is_refused() {
        let (a, beta) = ([[1, 1, 1], [0, 1, 2]], [3, 4]);
        let multiple = |g: &serde_json::Value, i: usize| is_euler_multiple(g, &a[i], beta[i]);
        let reply = recorded_generate();
        assert!((0..2).all(|i| multiple(&reply["euler"][i], i)));
        // x2 d2 + 2 x3 d3 - 5 is a multiple of neither.
        let mut changed = reply.clone();
        assert_eq!(changed["euler"][1][2]["coefficient"], "-4");
        changed["euler"][1][2]["coefficient"] = "-5".into();
        assert!((0..2).all(|i| !multiple(&changed["euler"][1], i)));
        // Without the second operator, no operator is a multiple of E_2 - 4.
        let mut dropped = reply.clone();
        dropped["euler"].as_array_mut().unwrap().pop();
        // and its coefficient in the certificate of the first operator.
        dropped["membership_coefficients"][0]
            .as_array_mut()
            .unwrap()
            .pop();
        let no_e2 = |reply: &serde_json::Value| {
            reply["euler"]
                .as_array()
                .unwrap()
                .iter()
                .all(|g| !multiple(g, 1))
        };
        assert!(no_e2(&dropped));
        // With E_1 - 3 in place of E_2 - 4, there are still d = 2 operators, each
        // a multiple of E_1 - 3, but none is a multiple of E_2 - 4.
        let mut repeated = reply;
        repeated["euler"][1] = repeated["euler"][0].clone();
        assert_eq!(repeated["euler"].as_array().unwrap().len(), 2);
        assert!((0..2).all(|k| multiple(&repeated["euler"][k], 0)));
        assert!(no_e2(&repeated));
        for reply in [changed, dropped, repeated] {
            match interpret(&generate_job(), reply) {
                Err(CasError::UnsupportedResult(m)) => {
                    assert!(m.contains("the Euler operators are not"), "{m}");
                }
                other => panic!("{other:?}"),
            }
        }
    }

    /// `A u` for A = [[1,1,1],[0,1,2]] and the d-exponents u of a driver term
    /// over x1..x3, d1..d3.
    fn a_times_d(term: &serde_json::Value) -> [i64; 2] {
        let e: Vec<i64> = term["exponents"].as_array().unwrap()[3..]
            .iter()
            .map(|k| k.as_i64().unwrap())
            .collect();
        [e[0] + e[1] + e[2], e[1] + 2 * e[2]]
    }

    /// gkz.toric_in_IA: d1 d3 - d2 in place of d1 d3 - d2^2 is refused, in a
    /// gkz.generate and in a gkz.series reply: A (1,0,1) = (2,2), A (0,1,0) = (1,1).
    /// So is d1 d3 - 2 d2^2, which keeps A u = A v but is not c (d^u - d^v).
    #[test]
    fn a_toric_binomial_with_a_u_unequal_to_a_v_is_refused() {
        let unequal = |mut reply: serde_json::Value| {
            let toric = &mut reply["toric"][0];
            assert_eq!(toric[0]["exponents"], serde_json::json!([0, 0, 0, 0, 2, 0]));
            toric[0]["exponents"] = serde_json::json!([0, 0, 0, 0, 1, 0]);
            assert_ne!(a_times_d(&toric[0]), a_times_d(&toric[1]));
            reply
        };
        let not_binomial = |mut reply: serde_json::Value| {
            let toric = &mut reply["toric"][0];
            assert_eq!(toric[0]["coefficient"], "-1");
            toric[0]["coefficient"] = "-2".into();
            assert_eq!(a_times_d(&toric[0]), a_times_d(&toric[1]));
            let c: Vec<i64> = (0..2)
                .map(|i| toric[i]["coefficient"].as_str().unwrap().parse().unwrap())
                .collect();
            assert_ne!(c[0], -c[1]);
            reply
        };
        let mutations: [fn(serde_json::Value) -> serde_json::Value; 2] = [unequal, not_binomial];
        for mutate in mutations {
            // The Euler operators are kept.
            match interpret(&generate_job(), mutate(recorded_generate())) {
                Err(CasError::UnsupportedResult(m)) => {
                    assert!(m.contains("a toric generator is not"), "{m}");
                }
                other => panic!("{other:?}"),
            }
            match interpret(&series_job(["1/2", "1/3"], 6), mutate(recorded("generic"))) {
                Err(CasError::UnsupportedResult(m)) => {
                    assert!(m.contains("a toric generator is not"), "{m}")
                }
                other => panic!("{other:?}"),
            }
        }
    }

    fn series_job(beta: [&str; 2], order: u32) -> Operation {
        parse(&job(
            GKZ_SERIES,
            serde_json::json!({"A": [[1, 1, 1], [0, 1, 2]], "beta": beta, "weight": [1, 0, 1], "order": order}),
        ))
        .unwrap()
    }

    /// Driver replies recorded from Macaulay2 1.24.11 for A = [[1,1,1],[0,1,2]],
    /// weight (1,0,1), order 6.
    fn recorded(name: &str) -> serde_json::Value {
        let text = match name {
            "generic" => include_str!("../../testdata/series_generic.json"),
            "logarithmic" => include_str!("../../testdata/series_logarithmic.json"),
            _ => include_str!("../../testdata/series_integer.json"),
        };
        serde_json::from_str(text).unwrap()
    }

    /// The recorded reply with the holonomic rank replaced, as a driver whose
    /// exponents do not account for the rank would answer.
    fn with_rank(mut reply: serde_json::Value, rank: u32) -> serde_json::Value {
        reply["holonomic_rank"] = serde_json::json!(rank);
        reply
    }

    fn indicial_job(beta: [&str; 2]) -> Operation {
        parse(&job(
            GKZ_INDICIAL,
            serde_json::json!({"A": [[1, 1, 1], [0, 1, 2]], "beta": beta, "weight": [1, 0, 1]}),
        ))
        .unwrap()
    }

    /// The multiplicities of a reply, summed in the test.
    fn multiplicity_sum(reply: &serde_json::Value) -> u64 {
        reply["exponents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["multiplicity"].as_u64().unwrap())
            .sum()
    }

    /// local.rank_sum.
    #[test]
    fn multiplicities_that_miss_the_rank_are_not_a_success() {
        // Two simple exponents, whose multiplicities sum to 2, against rank 3,
        // and against rank 1.
        let sum = multiplicity_sum(&recorded("generic"));
        assert_eq!(sum, 2);
        for rank in [3, 1] {
            assert_ne!(sum, u64::from(rank));
            for op in [indicial_job(["1/2", "1/3"]), series_job(["1/2", "1/3"], 6)] {
                let result = interpret(&op, with_rank(recorded("generic"), rank));
                match result {
                    Err(CasError::UnsupportedResult(m)) => {
                        assert!(m.contains("sum to 2"), "{m}");
                        assert!(m.contains(&format!("rank is {rank}")), "{m}");
                    }
                    other => panic!("rank {rank}: {other:?}"),
                }
            }
        }
        // The recorded reply itself (rank 2) is a success, for both operations.
        let (value, _) = interpret(&indicial_job(["1/2", "1/3"]), recorded("generic")).unwrap();
        assert_eq!(value["type"], "gkz_indicial");
        assert_eq!(value["checks"]["multiplicities_sum_to_rank"], true);
    }

    /// local.rank_sum: the multiplicities 2^32 - 1 and 3 sum to 2^32 + 2, not
    /// to the rank 2. A sum in u32 wraps to 2 in a release build, where this
    /// reply was accepted.
    #[test]
    fn multiplicities_are_summed_without_wrapping() {
        let mut reply = recorded("generic");
        reply["exponents"][0]["multiplicity"] = u32::MAX.into();
        reply["exponents"][1]["multiplicity"] = 3.into();
        let sum = multiplicity_sum(&reply);
        assert_eq!(sum, (1 << 32) + 2);
        assert_eq!(u32::MAX.wrapping_add(3), 2);
        match interpret(&indicial_job(["1/2", "1/3"]), reply) {
            Err(CasError::UnsupportedResult(m)) => {
                assert!(m.contains(&format!("sum to {sum}")), "{m}")
            }
            other => panic!("{other:?}"),
        }
    }

    fn q(s: &str) -> BigRational {
        s.parse().unwrap()
    }

    fn is_zero(x: &BigRational) -> bool {
        x.numer() == &BigInt::from(0)
    }

    /// A point of a reply as rationals.
    fn point_of(point: &serde_json::Value) -> Vec<BigRational> {
        point
            .as_array()
            .unwrap()
            .iter()
            .map(|s| q(s.as_str().unwrap()))
            .collect()
    }

    /// `A v` for A = [[1,1,1],[0,1,2]].
    fn a_times(v: &[BigRational]) -> Vec<BigRational> {
        [[1, 1, 1], [0, 1, 2]]
            .iter()
            .map(|row| {
                row.iter()
                    .zip(v)
                    .map(|(&a, x)| BigRational::from_integer(a.into()) * x)
                    .sum()
            })
            .collect()
    }

    /// The value at `v` of a polynomial given by driver terms.
    fn evaluate(terms: &serde_json::Value, v: &[BigRational]) -> BigRational {
        terms
            .as_array()
            .unwrap()
            .iter()
            .map(|t| {
                let c = q(t["coefficient"].as_str().unwrap());
                let e = t["exponents"].as_array().unwrap();
                e.iter().zip(v).fold(c, |acc, (k, x)| {
                    (0..k.as_u64().unwrap()).fold(acc, |acc, _| acc * x)
                })
            })
            .sum()
    }

    /// local.roots_complete: the exponents are distinct, which Rust checks. The
    /// second point is replaced by (2/12, 1/3, 0), the first point written
    /// otherwise. The multiplicities 1 + 1 still match the rank 2, and the point
    /// is a root with A v = beta, but the same exponent is listed twice: the
    /// reply is refused.
    #[test]
    fn a_repeated_exponent_is_refused() {
        let mut reply = recorded("generic");
        reply["exponents"][1]["point"] = serde_json::json!(["2/12", "1/3", "0"]);
        let first = point_of(&reply["exponents"][0]["point"]);
        assert_eq!(first, point_of(&reply["exponents"][1]["point"]));
        assert_eq!(
            multiplicity_sum(&reply),
            reply["holonomic_rank"].as_u64().unwrap()
        );
        assert_eq!(a_times(&first), [q("1/2"), q("1/3")]);
        let indicial = reply["indicial"].as_array().unwrap();
        assert!(indicial.iter().all(|g| is_zero(&evaluate(g, &first))));
        for op in [indicial_job(["1/2", "1/3"]), series_job(["1/2", "1/3"], 6)] {
            match interpret(&op, reply.clone()) {
                Err(CasError::UnsupportedResult(m)) => assert!(m.contains("appears twice"), "{m}"),
                other => panic!("{other:?}"),
            }
        }
    }

    /// local.exponent_equations: (1/2, 0, 0) is refused, as A v = (1/2, 0) is not
    /// beta = (1/2, 1/3). The reply keeps only the indicial generators
    /// theta1 theta3 and 6 theta3^2 + theta3, and both points are roots of them,
    /// so only A v = beta fails.
    #[test]
    fn an_exponent_with_a_v_unequal_to_beta_is_refused() {
        let mut reply = recorded("generic");
        reply["indicial"].as_array_mut().unwrap().drain(..2);
        reply["exponents"][0]["point"] = serde_json::json!(["1/2", "0", "0"]);
        let point = point_of(&reply["exponents"][0]["point"]);
        assert_ne!(a_times(&point), [q("1/2"), q("1/3")]);
        for e in reply["exponents"].as_array().unwrap() {
            let v = point_of(&e["point"]);
            for g in reply["indicial"].as_array().unwrap() {
                assert!(is_zero(&evaluate(g, &v)), "{g}");
            }
        }
        // The series of the point is not of the form of a series of an
        // exponent with A v = beta either: reading the value refuses it first.
        for (op, why) in [
            (indicial_job(["1/2", "1/3"]), "A v differs from beta"),
            (
                series_job(["1/2", "1/3"], 6),
                "does not annihilate the series",
            ),
        ] {
            match interpret(&op, reply.clone()) {
                Err(CasError::UnsupportedResult(m)) => assert!(m.contains(why), "{m}"),
                other => panic!("{other:?}"),
            }
        }
    }

    /// local.exponent_roots: (7/6, -5/3, 1) = (1/6, 1/3, 0) + (1,-2,1) satisfies
    /// A v = beta but is not a root of the indicial generators returned
    /// (theta1 theta3 is 7/6 there); it is refused.
    #[test]
    fn an_exponent_that_is_not_a_root_is_refused() {
        let mut reply = recorded("generic");
        reply["exponents"][0]["point"] = serde_json::json!(["7/6", "-5/3", "1"]);
        let point = point_of(&reply["exponents"][0]["point"]);
        assert_eq!(a_times(&point), [q("1/2"), q("1/3")]);
        let values: Vec<BigRational> = reply["indicial"]
            .as_array()
            .unwrap()
            .iter()
            .map(|g| evaluate(g, &point))
            .collect();
        assert!(values.iter().any(|x| !is_zero(x)), "{values:?}");
        for op in [indicial_job(["1/2", "1/3"]), series_job(["1/2", "1/3"], 6)] {
            match interpret(&op, reply.clone()) {
                Err(CasError::UnsupportedResult(m)) => {
                    assert!(m.contains("does not vanish"), "{m}")
                }
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn interprets_and_checks_series() {
        let (value, _) = interpret(&series_job(["1/2", "1/3"], 6), recorded("generic")).unwrap();
        assert_eq!(value["type"], "gkz_series");
        assert_eq!(value["parameters"]["resonant"], false);
        assert_eq!(value["parameters"]["homogeneous"], true);
        assert_eq!(value["checks"]["multiplicities_sum_to_rank"], true);
        let series = value["series"].as_array().unwrap();
        assert_eq!(series.len(), 2);
        for s in series {
            assert_eq!(s["status"], "verified_to_order");
            assert_eq!(s["terms"].as_array().unwrap().len(), 4);
        }
        assert_eq!(series[0]["terms"][0]["display"], "x1^(1/6)*x2^(1/3)");
        assert_eq!(
            series[0]["terms"][1]["display"],
            "-4/21*x1^(7/6)*x2^(-5/3)*x3"
        );

        // Integer parameters: one exponent gives a polynomial solution, the other a
        // Gamma-series that is not a solution, which is reported, not hidden.
        let (value, _) = interpret(&series_job(["3", "4"], 6), recorded("integer")).unwrap();
        assert_eq!(value["parameters"]["resonant"], true);
        let status: Vec<&str> = value["series"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["status"].as_str().unwrap())
            .collect();
        assert_eq!(status, ["not_a_solution", "verified_to_order"]);
    }

    #[test]
    fn interprets_and_checks_logarithmic_solutions() {
        let job = series_job(["0", "0"], 6);
        let (value, _) = interpret(&job, recorded("logarithmic")).unwrap();
        let exponent = &value["series"][0];
        assert_eq!(exponent["status"], "logarithmic");
        assert_eq!(exponent["missing"], 0);
        let solutions = exponent["solutions"].as_array().unwrap();
        assert_eq!(solutions.len(), 2);
        assert_eq!(solutions[0]["status"], "verified_to_order");
        assert_eq!(solutions[0]["terms"][0]["display"], "1");
        assert_eq!(solutions[1]["status"], "verified_to_order");
        assert_eq!(solutions[1]["direction"], serde_json::json!([1, -2, 1]));
        assert_eq!(solutions[1]["log"], "log(x1) - 2*log(x2) + log(x3)");
        let displays: Vec<&str> = solutions[1]["terms"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["display"].as_str().unwrap())
            .collect();
        assert_eq!(
            displays,
            [
                "log(x3)",
                "-2*log(x2)",
                "log(x1)",
                "2*x1*x2^(-2)*x3",
                "3*x1^2*x2^(-4)*x3^2",
                "20/3*x1^3*x2^(-6)*x3^3"
            ]
        );
        for s in solutions {
            assert_eq!(s["canonical"], true);
        }

        // A wrong coefficient makes that solution fail, and only that one.
        let mut wrong = recorded("logarithmic");
        wrong["exponents"][0]["series"]["solutions"][1]["terms"][4]["coefficient"] = "4".into();
        let (value, _) = interpret(&job, wrong).unwrap();
        let solutions = &value["series"][0]["solutions"];
        assert_eq!(solutions[0]["status"], "verified_to_order");
        assert_eq!(solutions[1]["status"], "not_a_solution");
        // A solution reported as missing is shown with the reason.
        let mut partial = recorded("logarithmic");
        partial["exponents"][0]["series"]["solutions"]
            .as_array_mut()
            .unwrap()
            .pop();
        partial["exponents"][0]["series"]["missing"] = 1.into();
        partial["exponents"][0]["series"]["reason"] = "not bounded".into();
        // The certificate of independence was for both solutions.
        partial["exponents"][0]["series"]["independence"] = serde_json::Value::Null;
        let (value, _) = interpret(&job, partial).unwrap();
        assert_eq!(value["series"][0]["missing"], 1);
        assert_eq!(value["series"][0]["reason"], "not bounded");
        // 3 solutions and 2^32 - 1 missing ones make 2^32 + 2, not the
        // multiplicity 2; a 32-bit sum would wrap to 2.
        let mut overcounted = recorded("logarithmic");
        let series = &mut overcounted["exponents"][0]["series"];
        let first = series["solutions"][0].clone();
        series["solutions"].as_array_mut().unwrap().push(first);
        series["missing"] = u32::MAX.into();
        assert_eq!(3 + u64::from(u32::MAX), (1 << 32) + 2);
        assert_eq!(3u32.wrapping_add(u32::MAX), 2);
        match interpret(&job, overcounted) {
            Err(CasError::UnsupportedResult(m)) => {
                assert!(m.contains("3 solutions and 4294967295 missing"), "{m}")
            }
            other => panic!("{other:?}"),
        }
        // Solutions that do not add up to the multiplicity, or a logarithm that the
        // Euler operators do not kill, are driver faults.
        let mut uncounted = recorded("logarithmic");
        uncounted["exponents"][0]["series"]["solutions"]
            .as_array_mut()
            .unwrap()
            .pop();
        let mut not_euler = recorded("logarithmic");
        not_euler["exponents"][0]["series"]["solutions"][1]["terms"][1]["coefficient"] =
            "-1".into();
        for reply in [uncounted, not_euler] {
            assert!(matches!(
                interpret(&job, reply),
                Err(CasError::UnsupportedResult(_))
            ));
        }
    }

    /// The twisted cubic with beta = 0 and weight (1,0,0,1): the exponent 0 has
    /// multiplicity 3, and its solutions need two directions of logarithms.
    #[test]
    fn logarithmic_solutions_in_two_directions() {
        let job = parse(&job(
            GKZ_SERIES,
            serde_json::json!({"A": [[1, 1, 1, 1], [0, 1, 2, 3]], "beta": ["0", "0"],
                               "weight": [1, 0, 0, 1], "order": 6}),
        ))
        .unwrap();
        let reply: serde_json::Value =
            serde_json::from_str(include_str!("../../testdata/series_twisted_cubic_log.json"))
                .unwrap();
        let (value, _) = interpret(&job, reply).unwrap();
        let exponent = &value["series"][0];
        assert_eq!(exponent["multiplicity"], 3);
        assert_eq!(exponent["missing"], 0);
        let solutions = exponent["solutions"].as_array().unwrap();
        let directions: Vec<&serde_json::Value> =
            solutions.iter().map(|s| &s["direction"]).collect();
        assert_eq!(
            directions,
            [
                &serde_json::Value::Null,
                &serde_json::json!([1, -2, 1, 0]),
                &serde_json::json!([2, -3, 0, 1])
            ]
        );
        for s in solutions {
            assert_eq!(s["status"], "verified_to_order", "{s}");
            assert_eq!(s["canonical"], true);
        }
    }

    /// Issue #88: A = [[1,1,1,1],[2,1,0,1],[1,2,0,1]], beta = (-1,0,0), weight
    /// (1,1,1,0), order 3. ker_ZZ A is spanned by l = (1,1,1,-3), the toric
    /// operator is D1 D2 D3 - D4^3, and the indicial polynomial along
    /// theta = (t,t,t-1,-3t) is t^2 (t-1): v = (0,0,-1,0) has multiplicity 2. The
    /// perturbed coefficient at u = l is
    ///   (-3e)(-3e-1)(-3e-2) / (e (1+e)^2) = -6 - 15e + O(e^2),
    /// whose numerator and denominator share the zero e = 0, so the second
    /// solution is x3^(-1) (log z + z (-15 - 6 log z)) with z = x^l. The reply is
    /// written by hand (the driver failed on this input before the fix), with the
    /// Euler equations and the distraction t1 t2 t3 as indicial generators.
    #[test]
    fn a_removable_zero_of_the_perturbed_coefficient_gives_a_solution() {
        let job = parse(&job(
            GKZ_SERIES,
            serde_json::json!({"A": [[1, 1, 1, 1], [2, 1, 0, 1], [1, 2, 0, 1]],
                               "beta": ["-1", "0", "0"], "weight": [1, 1, 1, 0], "order": 3}),
        ))
        .unwrap();
        fn term(e: &[u32], c: &str) -> serde_json::Value {
            serde_json::json!({"coefficient": c, "exponents": e})
        }
        let log = |u: [i64; 4], k: [u32; 4], c: &str| serde_json::json!({"shift": u, "log_powers": k, "coefficient": c});
        let l = [1, 1, 1, -3];
        let unit = |i: usize| {
            let mut k = [0; 4];
            k[i] = 1;
            k
        };
        // phi_1 = sum_u sum_(a+j=1) [e^a] c_u(e) x^(v+u) (l . log x)^j / j!, with
        // c_0 = 1 and c_l = -6 - 15e.
        let mut phi1: Vec<serde_json::Value> = (0..4)
            .map(|i| log([0; 4], unit(i), &l[i].to_string()))
            .collect();
        phi1.push(log(l, [0; 4], "-15"));
        phi1.extend((0..4).map(|i| log(l, unit(i), &(-6 * l[i]).to_string())));
        let reply = serde_json::json!({
            "n": 4,
            "variables": ["x_1", "x_2", "x_3", "x_4", "D_1", "D_2", "D_3", "D_4"],
            "holonomic_rank": 3,
            "indicial": [
                [term(&[1, 0, 0, 0], "1"), term(&[0, 1, 0, 0], "-1")],
                [term(&[1, 0, 0, 0], "1"), term(&[0, 0, 1, 0], "-1"), term(&[0, 0, 0, 0], "-1")],
                [term(&[1, 0, 0, 0], "3"), term(&[0, 0, 0, 1], "1")],
                [term(&[1, 1, 1, 0], "1")]
            ],
            "toric": [[term(&[0, 0, 0, 0, 1, 1, 1, 0], "1"), term(&[0, 0, 0, 0, 0, 0, 0, 3], "-1")]],
            "generated_by_toric_and_euler": true,
            "exponents": [
                {"multiplicity": 2, "point": ["0", "0", "-1", "0"], "series": {
                    "status": "logarithmic",
                    "solutions": [
                        {"log_degree": 0, "direction": null, "terms": [log([0; 4], [0; 4], "1")]},
                        {"log_degree": 1, "direction": l, "terms": phi1}
                    ],
                    "missing": 0, "reason": ""}},
                // The Gamma-series: [v]_3 / ([2]_1 [2]_1 [1]_1) = (-3)(-4)(-5) / 4 = -15
                // at u = l.
                {"multiplicity": 1, "point": ["1", "1", "0", "-3"], "series": {
                    "status": "computed",
                    "terms": [{"shift": [0, 0, 0, 0], "coefficient": "1"},
                              {"shift": l, "coefficient": "-15"}]}}
            ],
            "info": {"macaulay2_version": "1.24.11", "holonomic_systems_version": "1.0"}
        });

        let (value, _) = interpret(&job, reply.clone()).unwrap();
        let solutions = value["series"][0]["solutions"].as_array().unwrap();
        assert_eq!(value["series"][0]["missing"], 0);
        assert_eq!(solutions.len(), 2);
        for s in solutions {
            assert_eq!(s["status"], "verified_to_order", "{s}");
        }
        assert_eq!(value["series"][1]["status"], "verified_to_order");
        let coefficients: Vec<&str> = solutions[1]["terms"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["coefficient"].as_str().unwrap())
            .collect();
        assert_eq!(
            coefficients,
            ["-3", "1", "1", "1", "18", "-6", "-6", "-6", "-15"]
        );
        // The part -15 - 6 log z at x^(1,1,0,-3), the other exponent, is not a
        // multiple of the leading part 1 of its Gamma-series (Issue #75).
        assert_eq!(solutions[1]["canonical"], false);

        // Dropping the terms at u = l, as skipping the removable zero would, leaves
        // x3^(-1) log z. With D = z d/dz, D^2 (D-1) log z = 0 but
        // (-3D)(-3D-1)(-3D-2) log z = -6, so the toric operator leaves -6 z x3^(-1)
        // times a monomial at weight 3 = N: it is not a solution.
        let mut dropped = reply;
        dropped["exponents"][0]["series"]["solutions"][1]["terms"]
            .as_array_mut()
            .unwrap()
            .truncate(4);
        let (value, _) = interpret(&job, dropped).unwrap();
        assert_eq!(
            value["series"][0]["solutions"][1]["status"],
            "not_a_solution"
        );
    }

    #[test]
    fn rejects_inconsistent_series_replies() {
        let op = series_job(["1/2", "1/3"], 6);
        let mut wrong_root = recorded("generic");
        wrong_root["exponents"][0]["point"][0] = "1/5".into();
        let mut wrong_term = recorded("generic");
        wrong_term["exponents"][0]["series"]["terms"][1]["shift"] = serde_json::json!([1, -1, 0]);
        let mut not_generated = recorded("generic");
        not_generated["generated_by_toric_and_euler"] = false.into();
        let mut log_for_simple_root = recorded("generic");
        log_for_simple_root["exponents"][0]["series"] =
            serde_json::json!({"status": "logarithmic"});
        for reply in [wrong_root, wrong_term, not_generated, log_for_simple_root] {
            assert!(matches!(
                interpret(&op, reply),
                Err(CasError::UnsupportedResult(_))
            ));
        }
        // A wrong coefficient is a series that does not solve the system.
        let mut wrong_coefficient = recorded("generic");
        wrong_coefficient["exponents"][0]["series"]["terms"][2]["coefficient"] = "1".into();
        let (value, _) = interpret(&op, wrong_coefficient).unwrap();
        assert_eq!(value["series"][0]["status"], "not_a_solution");
    }

    /// series.shape: a term of weight above the order is refused. The shift
    /// (4,-8,4) is in ker A, so the Euler operators still annihilate the series
    /// and the toric operator sees only terms up to the order; its weight
    /// (1,0,1).(4,-8,4) = 8 exceeds the order 6.
    #[test]
    fn a_term_above_the_order_is_refused() {
        let u = [4i64, -8, 4];
        assert_eq!([u[0] + u[1] + u[2], u[1] + 2 * u[2]], [0, 0]);
        assert!(u[0] + u[2] > 6);
        let mut reply = recorded("generic");
        reply["exponents"][0]["series"]["terms"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"shift": u, "coefficient": "1"}));
        match interpret(&series_job(["1/2", "1/3"], 6), reply) {
            Err(CasError::UnsupportedResult(m)) => assert!(m.contains("above the order"), "{m}"),
            other => panic!("{other:?}"),
        }
    }

    /// series.euler_exact: with -log x2 in place of -2 log x2, the second solution
    /// for beta = 0 has the logarithmic part c . log x, c = (1,-1,1), at x^0. E_i
    /// maps it to (A c)_i, and A c = (1, 1); the other terms x^u, A u = 0, are
    /// annihilated. The series is refused, where one that fails only the toric
    /// operator is labelled not_a_solution.
    #[test]
    fn a_logarithm_the_euler_operators_do_not_annihilate_is_refused() {
        let mut reply = recorded("logarithmic");
        let terms = &mut reply["exponents"][0]["series"]["solutions"][1]["terms"];
        assert_eq!(terms[1]["log_powers"], serde_json::json!([0, 1, 0]));
        terms[1]["coefficient"] = "-1".into();
        let c: Vec<i64> = (0..3)
            .map(|j| {
                terms
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|t| t["shift"] == serde_json::json!([0, 0, 0]))
                    .filter(|t| t["log_powers"][j] == 1)
                    .map(|t| t["coefficient"].as_str().unwrap().parse::<i64>().unwrap())
                    .sum()
            })
            .collect();
        assert_eq!(c, [1, -1, 1]);
        assert_ne!([c[0] + c[1] + c[2], c[1] + 2 * c[2]], [0, 0]);
        match interpret(&series_job(["0", "0"], 6), reply) {
            Err(CasError::UnsupportedResult(m)) => {
                assert!(m.contains("does not annihilate"), "{m}")
            }
            other => panic!("{other:?}"),
        }
    }

    /// series.toric_to_order: with 1 in place of -160/819 at the shift (2,-4,2),
    /// the first series is not verified to the order, and the second still is.
    /// The coefficient of x^(v + (1,-4,1)) in (d1 d3 - d2^2) applied to the series
    /// comes only from the terms at (2,-4,2), by d1 d3, and at (1,-2,1), by d2^2,
    /// of weights 4 and 2: it involves only computed terms, and it is not 0.
    #[test]
    fn a_series_the_toric_operator_does_not_annihilate_is_not_verified() {
        let reply = recorded("generic");
        let v = point_of(&reply["exponents"][0]["point"]);
        assert_eq!(v, [q("1/6"), q("1/3"), q("0")]);
        let at = |u: [i64; 3]| -> Vec<BigRational> {
            v.iter()
                .zip(u)
                .map(|(x, k)| x + BigRational::from_integer(k.into()))
                .collect()
        };
        let (e2, e1) = (at([2, -4, 2]), at([1, -2, 1]));
        let one = BigRational::from_integer(1.into());
        // d1 d3 x^e = e_1 e_3 x^(e - (1,0,1)), d2^2 x^e = e_2 (e_2 - 1) x^(e - (0,2,0)).
        let witness = |reply: &serde_json::Value| {
            let terms = &reply["exponents"][0]["series"]["terms"];
            assert_eq!(terms[1]["shift"], serde_json::json!([1, -2, 1]));
            assert_eq!(terms[2]["shift"], serde_json::json!([2, -4, 2]));
            let c = |i: usize| q(terms[i]["coefficient"].as_str().unwrap());
            c(2) * &e2[0] * &e2[2] - c(1) * &e1[1] * (&e1[1] - &one)
        };
        assert!(is_zero(&witness(&reply)));
        let mut wrong = reply;
        wrong["exponents"][0]["series"]["terms"][2]["coefficient"] = "1".into();
        assert!(!is_zero(&witness(&wrong)));
        let (value, _) = interpret(&series_job(["1/2", "1/3"], 6), wrong).unwrap();
        assert_ne!(value["series"][0]["status"], "verified_to_order");
        assert_eq!(value["series"][1]["status"], "verified_to_order");
    }

    /// The terms c x^e of a series with integer exponent and coefficients.
    fn integer_series(entry: &serde_json::Value) -> Vec<([i64; 3], i64)> {
        let int = |s: &serde_json::Value| s.as_str().unwrap().parse::<i64>().unwrap();
        let v: Vec<i64> = entry["point"].as_array().unwrap().iter().map(int).collect();
        entry["series"]["terms"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| {
                let u = &t["shift"];
                let e = std::array::from_fn(|j| v[j] + u[j].as_i64().unwrap());
                (e, int(&t["coefficient"]))
            })
            .collect()
    }

    /// (d1 d3 - d2^2) applied to sum c x^e, with integer exponents.
    fn toric_image(terms: &[([i64; 3], i64)]) -> BTreeMap<[i64; 3], i64> {
        let mut image = BTreeMap::new();
        for &(e, c) in terms {
            *image.entry([e[0] - 1, e[1], e[2] - 1]).or_insert(0) += c * e[0] * e[2];
            *image.entry([e[0], e[1] - 2, e[2]]).or_insert(0) -= c * e[1] * (e[1] - 1);
        }
        image.retain(|_, c| *c != 0);
        image
    }

    /// series.not_a_solution: for beta = (3,4), the series x2^2 x3 + 2 x1 x3^2 of
    /// the exponent (0,2,1) is labelled not_a_solution; d1 d3 - d2^2 maps it to
    /// 2 x3. The mutation is the recorded x2^2 x3 + x1 x3^2, which d1 d3 - d2^2 and
    /// the Euler operators annihilate (each monomial has A e = beta); it loses the
    /// label. The other exponent, (-1,4,0), is not used: its label is true for
    /// every series without logarithms with that exponent, as d2^2 maps
    /// x1^-1 x2^4 to 12 x1^-1 x2^2 and no other term contributes to that monomial.
    ///
    /// A truncated solution is not labelled either: for beta = (1/2, 1/3) and the
    /// order 6, (d1 d3 - d2^2) T is not 0, but each of its nonzero coefficients
    /// involves a term of weight above 6, which is not computed.
    #[test]
    fn a_series_that_solves_the_system_is_not_labelled_not_a_solution() {
        let op = series_job(["3", "4"], 6);
        let mut labelled = recorded("integer");
        labelled["exponents"][1]["series"]["terms"][1]["coefficient"] = "2".into();
        assert_eq!(
            toric_image(&integer_series(&labelled["exponents"][1])),
            BTreeMap::from([([0, 0, 1], 2)])
        );
        let (value, _) = interpret(&op, labelled).unwrap();
        assert_eq!(value["series"][1]["status"], "not_a_solution");

        let solution = recorded("integer");
        let terms = integer_series(&solution["exponents"][1]);
        assert!(toric_image(&terms).is_empty());
        assert!(
            terms
                .iter()
                .all(|(e, _)| [e[0] + e[1] + e[2], e[1] + 2 * e[2]] == [3, 4])
        );
        let (value, _) = interpret(&op, solution).unwrap();
        assert_ne!(value["series"][1]["status"], "not_a_solution");
        assert_eq!(value["series"][1]["status"], "verified_to_order");

        // The truncated solutions for beta = (1/2, 1/3), of the order 6 and the
        // weight (1,0,1). For a term c x^a, a = v + u, d1 d3 gives c a1 a3 at the
        // shift u - (1,0,1), and d2^2 gives c a2 (a2 - 1) at the shift u - (0,2,0).
        // The coefficient at the shift s combines the terms at s + (1,0,1) and
        // s + (0,2,0); it involves only computed terms when both have weight at
        // most 6.
        let (order, step1, step2) = (6, [1, 0, 1], [0, 2, 0]);
        let weight = |u: [i64; 3]| u[0] + u[2];
        let add = |s: [i64; 3], t: [i64; 3]| -> [i64; 3] { std::array::from_fn(|j| s[j] + t[j]) };
        let truncated = recorded("generic");
        for entry in truncated["exponents"].as_array().unwrap() {
            let v = point_of(&entry["point"]);
            let mut image: BTreeMap<[i64; 3], BigRational> = BTreeMap::new();
            for t in entry["series"]["terms"].as_array().unwrap() {
                assert!(t.get("log_powers").is_none(), "{t}");
                let u: [i64; 3] = std::array::from_fn(|j| t["shift"][j].as_i64().unwrap());
                let c = q(t["coefficient"].as_str().unwrap());
                let a: Vec<BigRational> = v
                    .iter()
                    .zip(u)
                    .map(|(x, k)| x + BigRational::from_integer(k.into()))
                    .collect();
                let one = BigRational::from_integer(1.into());
                let down = |step: [i64; 3]| -> [i64; 3] { std::array::from_fn(|j| u[j] - step[j]) };
                *image.entry(down(step1)).or_default() += &c * &a[0] * &a[2];
                *image.entry(down(step2)).or_default() -= &c * &a[1] * (&a[1] - one);
            }
            image.retain(|_, c| !is_zero(c));
            assert!(!image.is_empty());
            for s in image.keys() {
                assert!(
                    weight(add(*s, step1)) > order || weight(add(*s, step2)) > order,
                    "{s:?}"
                );
            }
        }
        let (value, _) = interpret(&series_job(["1/2", "1/3"], 6), truncated).unwrap();
        for entry in value["series"].as_array().unwrap() {
            assert_ne!(entry["status"], "not_a_solution");
        }
    }

    #[test]
    fn validates_local_arguments() {
        let reject = |op: &str, args: serde_json::Value| {
            assert!(
                matches!(
                    parse(&job(op, args.clone())),
                    Err(CasError::InvalidArguments(_))
                ),
                "{args}"
            );
        };
        let a = serde_json::json!([[1, 1, 1], [0, 1, 2]]);
        reject(
            GKZ_INDICIAL,
            serde_json::json!({"A": a, "beta": ["1", "1"], "weight": [1, 0]}),
        );
        reject(
            GKZ_INDICIAL,
            serde_json::json!({"A": a, "beta": ["1", "1"], "weight": [0, 0, 0]}),
        );
        reject(
            GKZ_INDICIAL,
            serde_json::json!({"A": a, "beta": ["1", "1"], "weight": [1, 0, 1], "order": 3}),
        );
        reject(
            GKZ_SERIES,
            serde_json::json!({"A": a, "beta": ["1", "1"], "weight": [1, 0, 1]}),
        );
        reject(
            GKZ_SERIES,
            serde_json::json!({"A": a, "beta": ["1", "1"], "weight": [1, 0, 1], "order": 5000}),
        );
        let op = parse(&job(
            GKZ_INDICIAL,
            serde_json::json!({"A": a, "beta": ["1/2", "1/3"], "weight": [1, 0, 1]}),
        ))
        .unwrap();
        let req = request(&op);
        assert_eq!(req["operation"], GKZ_INDICIAL);
        assert!(req.get("order").is_none());
        let req = request(&series_job(["1/2", "1/3"], 6));
        assert_eq!(
            (req["order"].as_u64(), req["max_terms"].as_u64()),
            (Some(6), Some(10_000))
        );
    }

    /// A recorded reply of Macaulay2 to a matrix or ideal job, read as the
    #[test]
    fn rejects_bad_gkz_input_before_running() {
        assert!(matches!(
            parse(&job(
                GKZ_GENERATE,
                serde_json::json!({"A": [[1, 2], [2, 4]], "beta": ["1", "1"]})
            )),
            Err(CasError::InvalidArguments(_))
        ));
    }

    /// The fields of `value` that record a check, as `docs/claims.md` names
    /// them: `verified_product`, `checks.<key>`, and the `status`, `reason`,
    /// `missing`, `normalization`, `canonical*` and `evidence_terms` (Issue
    /// #99) of each series and solution.
    fn check_fields(value: &serde_json::Value) -> std::collections::BTreeSet<String> {
        let mut fields = std::collections::BTreeSet::new();
        if value.get("verified_product").is_some() {
            fields.insert("verified_product".to_owned());
        }
        if let Some(checks) = value["checks"].as_object() {
            fields.extend(checks.keys().map(|k| format!("checks.{k}")));
        }
        let records = |key: &str| {
            matches!(
                key,
                "status" | "reason" | "missing" | "normalization" | "evidence_terms"
            ) || key.starts_with("canonical")
        };
        for entry in value["series"].as_array().into_iter().flatten() {
            let entry = entry.as_object().unwrap();
            fields.extend(
                entry
                    .keys()
                    .filter(|k| records(k))
                    .map(|k| format!("series[].{k}")),
            );
            for solution in entry
                .get("solutions")
                .and_then(|s| s.as_array())
                .into_iter()
                .flatten()
            {
                let keys = solution.as_object().unwrap().keys();
                fields.extend(
                    keys.filter(|k| records(k))
                        .map(|k| format!("series[].solutions[].{k}")),
                );
            }
        }
        fields
    }

    /// Issue #61: every field of a value of the four older operations that
    /// records a check is mapped to a claim of the catalogue
    /// (`docs/claims.md`, item `既存の欄`).
    #[test]
    fn every_check_field_is_in_the_catalogue() {
        let catalogue =
            algebramesh_verify::catalogue::parse(include_str!("../../../../docs/claims.md"));
        let mapped: std::collections::BTreeSet<String> = catalogue
            .claims()
            .flat_map(|c| c.names("既存の欄"))
            .collect();
        let factor = parse(&job(
            POLYNOMIAL_FACTOR,
            serde_json::json!({
                "ring": {"domain": "ZZ", "variables": ["x"], "order": "lex"},
                "expression": {"type": "polynomial", "terms": [
                    {"coefficient": "1", "exponents": [2]},
                    {"coefficient": "-1", "exponents": [0]}]}
            }),
        ))
        .unwrap();
        let factor_reply = serde_json::json!({"factors": [
            {"terms": [{"coefficient": "1", "exponents": [1]}, {"coefficient": "-1", "exponents": [0]}], "multiplicity": 1},
            {"terms": [{"coefficient": "1", "exponents": [1]}, {"coefficient": "1", "exponents": [0]}], "multiplicity": 1}],
            "info": {"macaulay2_version": "1.24.11", "holonomic_systems_version": "1.0"}});
        // The logarithmic solution with 4 in place of 3 is not a solution, and
        // gets a reason.
        let mut wrong_logarithmic = recorded("logarithmic");
        wrong_logarithmic["exponents"][0]["series"]["solutions"][1]["terms"][4]["coefficient"] =
            "4".into();
        let replies = [
            (factor, factor_reply),
            (generate_job(), recorded_generate()),
            (indicial_job(["1/2", "1/3"]), recorded("generic")),
            (series_job(["1/2", "1/3"], 6), recorded("generic")),
            (series_job(["3", "4"], 6), recorded("integer")),
            (series_job(["0", "0"], 6), recorded("logarithmic")),
            (series_job(["0", "0"], 6), wrong_logarithmic),
        ];
        let mut seen = std::collections::BTreeSet::new();
        for (op, reply) in replies {
            let (value, _) = interpret(&op, reply).unwrap();
            seen.extend(check_fields(&value));
        }
        // The fields the recorded replies have, all of them.
        for field in [
            "verified_product",
            "checks.euler_operators_match",
            "checks.expected_in_ideal",
            "checks.exponents_are_roots",
            "checks.multiplicities_sum_to_rank",
            "series[].status",
            "series[].solutions[].status",
            "series[].solutions[].reason",
            "series[].solutions[].canonical",
            "series[].solutions[].evidence_terms",
            "series[].evidence_terms",
        ] {
            assert!(seen.contains(field), "{field} is not in {seen:?}");
        }
        // Every field that `interpret` and `interpret_local` can write, whether or
        // not a reply above produces it.
        const WRITTEN: [&str; 22] = [
            "verified_product",
            "checks.euler_operators_match",
            "checks.toric_binomials_valid",
            "checks.generated_by_toric_and_euler",
            "checks.expected_in_ideal",
            "checks.exponents_are_roots",
            "checks.multiplicities_sum_to_rank",
            "series[].status",
            "series[].reason",
            "series[].missing",
            "series[].canonical",
            "series[].canonical_terms",
            "series[].canonical_reason",
            "series[].normalization",
            "series[].evidence_terms",
            "series[].solutions[].status",
            "series[].solutions[].reason",
            "series[].solutions[].canonical",
            "series[].solutions[].canonical_terms",
            "series[].solutions[].canonical_reason",
            "series[].solutions[].normalization",
            "series[].solutions[].evidence_terms",
        ];
        let unlisted: Vec<&String> = seen
            .iter()
            .filter(|f| !WRITTEN.contains(&f.as_str()))
            .collect();
        assert!(unlisted.is_empty(), "fields not in the list: {unlisted:?}");
        let unmapped: Vec<&&str> = WRITTEN.iter().filter(|f| !mapped.contains(**f)).collect();
        assert!(unmapped.is_empty(), "fields without a claim: {unmapped:?}");
    }

    /// The jobs and the recorded replies of `algebramesh-gkz/testdata/values.json`.
    fn value_cases() -> Vec<(
        &'static str,
        &'static str,
        serde_json::Value,
        serde_json::Value,
    )> {
        let a3 = serde_json::json!([[1, 1, 1], [0, 1, 2]]);
        let d = |e: [u32; 3], c: &str| serde_json::json!({"coefficient": c, "x_exponents": [0, 0, 0], "d_exponents": e});
        let file = |text: &str| -> serde_json::Value { serde_json::from_str(text).unwrap() };
        vec![
            (
                "generate",
                GKZ_GENERATE,
                serde_json::json!({"A": a3, "beta": ["3", "4"],
                "expected": [[d([1, 0, 1], "1"), d([0, 2, 0], "-1")], [d([0, 1, 0], "1")]]}),
                recorded_generate(),
            ),
            (
                "indicial_generic",
                GKZ_INDICIAL,
                serde_json::json!({"A": a3, "beta": ["1/2", "1/3"], "weight": [1, 0, 1]}),
                recorded("generic"),
            ),
            (
                "series_generic",
                GKZ_SERIES,
                serde_json::json!({"A": a3, "beta": ["1/2", "1/3"], "weight": [1, 0, 1], "order": 6}),
                recorded("generic"),
            ),
            (
                "series_integer",
                GKZ_SERIES,
                serde_json::json!({"A": a3, "beta": ["3", "4"], "weight": [1, 0, 1], "order": 6}),
                recorded("integer"),
            ),
            (
                "series_logarithmic",
                GKZ_SERIES,
                serde_json::json!({"A": a3, "beta": ["0", "0"], "weight": [1, 0, 1], "order": 6}),
                recorded("logarithmic"),
            ),
            (
                "series_twisted_cubic_logarithmic",
                GKZ_SERIES,
                serde_json::json!({"A": [[1, 1, 1, 1], [0, 1, 2, 3]], "beta": ["0", "0"],
                                "weight": [1, 0, 0, 1], "order": 6}),
                file(include_str!("../../testdata/series_twisted_cubic_log.json")),
            ),
            (
                "series_removable_zero",
                GKZ_SERIES,
                serde_json::json!({"A": [[1, 1, 1, 1], [2, 1, 0, 1], [1, 2, 0, 1]],
                                "beta": ["-1", "0", "0"], "weight": [1, 1, 1, 0], "order": 3}),
                file(include_str!("../../testdata/series_removable_zero.json")),
            ),
        ]
    }

    /// Issues #99, #100: the values the package's tests check
    /// (`algebramesh-gkz/testdata/values.json`) are those this backend gives
    /// for its recorded replies, and pass the checks. With
    /// `ALGEBRAMESH_WRITE_GKZ_VALUES=1` the file is written again.
    #[test]
    fn the_values_of_the_package_tests_are_those_of_the_recorded_replies() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../algebramesh-gkz/testdata/values.json"
        );
        let mut out = Vec::new();
        for (name, operation, arguments, reply) in value_cases() {
            let op = parse(&job(operation, arguments.clone())).unwrap();
            assert_eq!(self::operation(&op), operation);
            let (value, _) = interpret(&op, reply).unwrap_or_else(|e| panic!("{name}: {e}"));
            out.push(serde_json::json!({"name": name, "operation": operation,
                "arguments": arguments, "value": value}));
        }
        if std::env::var_os("ALGEBRAMESH_WRITE_GKZ_VALUES").is_some() {
            std::fs::write(path, serde_json::to_string_pretty(&out).unwrap() + "\n").unwrap();
        }
        let recorded: serde_json::Value = serde_json::from_str(include_str!(
            "../../../algebramesh-gkz/testdata/values.json"
        ))
        .unwrap();
        assert_eq!(serde_json::Value::from(out), recorded);
    }
}

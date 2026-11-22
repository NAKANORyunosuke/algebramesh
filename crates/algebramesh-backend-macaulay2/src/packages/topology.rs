//! The topology package on Macaulay2 (Issue #103, `docs/packages/topology.md`):
//! `simplicial.homology`, with the driver `drivers/macaulay2/topology.m2`.
//!
//! This glue reads the complex of the job, which `algebramesh-topology`
//! checks and bounds, and sends its boundary matrices to the driver, which
//! computes the Smith normal form of each with the procedure of
//! `matrix.smith_normal_form` in one run. `algebramesh-topology` builds the
//! value from the forms; its claims are checked after the backend, by
//! `algebramesh_topology::verify`.

use algebramesh_core::CasError;
use algebramesh_math_types::{CoefficientDomain, RationalMatrix};
use algebramesh_ops::matrix::{CertificateRequest, SmithResult};
use algebramesh_ops::topology::{SIMPLICIAL_HOMOLOGY, SimplicialHomologyArguments};
use algebramesh_topology::SimplicialComplex;
use algebramesh_topology::value::homology_value;
use serde::Deserialize;

use crate::{DriverInfo, bad, invalid};

/// The operations of the package.
pub(crate) const OPERATIONS: [&str; 1] = [SIMPLICIAL_HOMOLOGY];

/// The types of their arguments and values.
pub(crate) const TYPES: [&str; 2] = ["simplicial_complex", "simplicial_homology"];

/// A job of the package: the complex, its boundary matrices `∂_1, ..., ∂_d`
/// and whether the forms come with certificates.
pub(crate) struct TopologyOperation {
    complex: SimplicialComplex,
    boundaries: Vec<RationalMatrix>,
    certificate: bool,
}

/// The job of `operation`, if it is one of the package; the complex is
/// checked here, before the CAS runs.
pub(crate) fn parse(
    operation: &str,
    arguments: &serde_json::Value,
) -> Result<Option<TopologyOperation>, CasError> {
    if operation != SIMPLICIAL_HOMOLOGY {
        return Ok(None);
    }
    let args: SimplicialHomologyArguments =
        serde_json::from_value(arguments.clone()).map_err(invalid)?;
    if args.domain != CoefficientDomain::Integers {
        return Err(invalid(format!("{SIMPLICIAL_HOMOLOGY} is over ZZ only")));
    }
    let complex = SimplicialComplex::from_data(&args.complex).map_err(invalid)?;
    let boundaries = complex.boundaries();
    Ok(Some(TopologyOperation {
        complex,
        boundaries,
        certificate: args.certificate == CertificateRequest::Include,
    }))
}

/// The request for the driver: the boundary matrices, entries as decimal
/// strings, and whether the forms come with certificates.
pub(crate) fn request(op: &TopologyOperation) -> serde_json::Value {
    serde_json::json!({
        "operation": SIMPLICIAL_HOMOLOGY,
        "matrices": op.boundaries,
        "certificate": op.certificate,
    })
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FormsReply {
    smith_forms: Vec<SmithResult>,
}

#[derive(Debug, Deserialize)]
struct TopologyReply {
    value: FormsReply,
    info: DriverInfo,
}

/// The value of the job from the driver's reply: one Smith normal form for
/// each boundary matrix, each with a certificate exactly when the job asked
/// for them.
pub(crate) fn interpret(
    op: &TopologyOperation,
    reply: serde_json::Value,
) -> Result<(serde_json::Value, String), CasError> {
    let reply: TopologyReply = serde_json::from_value(reply).map_err(bad)?;
    let value =
        homology_value(&op.complex, reply.value.smith_forms, op.certificate).map_err(bad)?;
    Ok((
        serde_json::to_value(value).map_err(bad)?,
        reply.info.describe(),
    ))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::{CasRequest, Operation};
    use algebramesh_core::check::Interrupt;
    use algebramesh_core::guarantee::ResultVerifier;
    use algebramesh_core::{BackendInfo, CasResult, JobSpec};

    fn job(arguments: serde_json::Value) -> CasRequest {
        CasRequest {
            job_id: algebramesh_core::JobId::new(),
            operation: SIMPLICIAL_HOMOLOGY.into(),
            arguments,
            limits: Default::default(),
        }
    }

    fn topology(op: &Operation) -> &TopologyOperation {
        match op {
            Operation::Topology(t) => t,
            _ => panic!("not a topology job"),
        }
    }

    /// The recorded cases: name, arguments and the driver's reply.
    fn cases() -> Vec<(String, serde_json::Value, serde_json::Value)> {
        let cases: Vec<serde_json::Value> =
            serde_json::from_str(include_str!("../../testdata/topology.json")).unwrap();
        cases
            .into_iter()
            .map(|c| {
                (
                    c["name"].as_str().unwrap().to_owned(),
                    c["arguments"].clone(),
                    c["reply"].clone(),
                )
            })
            .collect()
    }

    fn verified(arguments: &serde_json::Value, value: serde_json::Value) -> CasResult {
        let spec = JobSpec {
            schema_version: 1,
            backend: crate::NAME.into(),
            operation: SIMPLICIAL_HOMOLOGY.into(),
            arguments: arguments.clone(),
            limits: Default::default(),
            retry_policy: Default::default(),
            environment: Default::default(),
        };
        let result = CasResult {
            value,
            backend: BackendInfo::default(),
            execution_host: None,
            log: vec![],
            guarantee: None,
        };
        algebramesh_topology::verify::registry()
            .verify(&spec, result, &Interrupt::none())
            .unwrap()
    }

    /// The driver receives the boundary matrices of the complex, in the
    /// order of the faces, and nothing else of the job.
    #[test]
    fn a_homology_request_carries_the_boundary_matrices() {
        let op = crate::parse(&job(serde_json::json!({
            "complex": {"type": "simplicial_complex", "facets": [[1, 0], [2, 1]]},
            "domain": "ZZ",
        })))
        .unwrap();
        assert_eq!(
            crate::request(&op),
            serde_json::json!({
                "operation": "simplicial.homology",
                // vertices 0, 1, 2; edges [0, 1], [1, 2]
                "matrices": [[["-1", "0"], ["1", "-1"], ["0", "1"]]],
                "certificate": true,
            })
        );
        assert_eq!(topology(&op).boundaries.len(), 1);
    }

    /// Issue #103: a complex over the bound on its faces, an invalid
    /// complex, and coefficients other than ZZ are refused before the CAS
    /// runs.
    #[test]
    fn homology_jobs_the_driver_cannot_take_are_refused_before_running() {
        let refused = |arguments: serde_json::Value, why: &str| match crate::parse(&job(arguments))
        {
            Err(CasError::InvalidArguments(m)) => assert!(m.contains(why), "{m}"),
            Err(e) => panic!("expected INVALID_ARGUMENTS ({why}), got {e:?}"),
            Ok(_) => panic!("expected INVALID_ARGUMENTS ({why})"),
        };
        let simplex: Vec<u32> = (0..10).collect();
        refused(
            serde_json::json!({"complex": {"type": "simplicial_complex", "facets": [simplex]},
                               "domain": "ZZ"}),
            "1023 faces",
        );
        refused(
            serde_json::json!({"complex": {"type": "simplicial_complex",
                                           "facets": [[0, 1], [0, 1, 2]]},
                               "domain": "ZZ"}),
            "contained in facet",
        );
        refused(
            serde_json::json!({"complex": {"type": "simplicial_complex", "facets": [[0, 1]]},
                               "domain": "QQ"}),
            "over ZZ only",
        );
    }

    /// The recorded replies give values that pass the checks with every
    /// claim checked, and the groups of the acceptance criteria of #103.
    #[test]
    fn recorded_homology_replies_are_checked() {
        let mut seen = Vec::new();
        for (name, arguments, reply) in cases() {
            let op = crate::parse(&job(arguments.clone())).unwrap();
            let (value, version) = crate::interpret(&op, reply, &Interrupt::none())
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(version.starts_with("Macaulay2 "), "{version}");
            let checked = verified(&arguments, value);
            let g = checked.guarantee.unwrap();
            let expected = arguments["certificate"] != "none";
            for c in &g.claims {
                assert_eq!(
                    g.effective_level(&c.id) == algebramesh_core::guarantee::EvidenceLevel::Checked,
                    expected || !(c.id == "homology.snf_equivalent" || c.id == "homology.groups"),
                    "{name}: {}",
                    c.id
                );
            }
            let groups: Vec<(u64, Vec<String>)> = checked.value["homology"]
                .as_array()
                .unwrap()
                .iter()
                .map(|h| {
                    (
                        h["rank"].as_u64().unwrap(),
                        h["torsion"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|t| t.as_str().unwrap().to_owned())
                            .collect(),
                    )
                })
                .collect();
            let none = Vec::<String>::new;
            let want: Vec<(u64, Vec<String>)> = match name.as_str() {
                "tetrahedron_boundary" => vec![(1, none()), (0, none()), (1, none())],
                "torus" | "torus_without_certificate" => {
                    vec![(1, none()), (2, none()), (1, none())]
                }
                "projective_plane" => vec![(1, none()), (0, vec!["2".to_owned()]), (0, none())],
                "two_points" => vec![(2, none())],
                other => panic!("unknown case {other}"),
            };
            assert_eq!(groups, want, "{name}");
            seen.push(name);
        }
        assert_eq!(seen.len(), 5);
    }

    /// Issue #103: the values the package's tests check
    /// (`algebramesh-topology/testdata/values.json`) are those this backend
    /// gives for its recorded replies. With
    /// `ALGEBRAMESH_WRITE_TOPOLOGY_VALUES=1` the file is written again.
    #[test]
    fn the_topology_values_are_those_of_the_recorded_replies() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../algebramesh-topology/testdata/values.json"
        );
        let mut out = Vec::new();
        for (name, arguments, reply) in cases() {
            let op = crate::parse(&job(arguments.clone())).unwrap();
            let (value, _) = crate::interpret(&op, reply, &Interrupt::none()).unwrap();
            out.push(
                serde_json::json!({"name": name, "operation": SIMPLICIAL_HOMOLOGY,
                "arguments": arguments, "value": value}),
            );
        }
        if std::env::var_os("ALGEBRAMESH_WRITE_TOPOLOGY_VALUES").is_some() {
            std::fs::write(path, serde_json::to_string_pretty(&out).unwrap() + "\n").unwrap();
        }
        let recorded: serde_json::Value = serde_json::from_str(include_str!(
            "../../../algebramesh-topology/testdata/values.json"
        ))
        .unwrap();
        assert_eq!(serde_json::Value::from(out), recorded);
    }

    /// A reply with a form too few, or a certificate the job did not ask
    /// for, is not a value.
    #[test]
    fn replies_that_do_not_fit_the_complex_are_refused() {
        let (_, arguments, reply) = cases()
            .into_iter()
            .find(|(n, ..)| n == "tetrahedron_boundary")
            .unwrap();
        let op = crate::parse(&job(arguments.clone())).unwrap();
        let mut short = reply.clone();
        short["value"]["smith_forms"].as_array_mut().unwrap().pop();
        assert!(matches!(
            crate::interpret(&op, short, &Interrupt::none()),
            Err(CasError::UnsupportedResult(m)) if m.contains("2 Smith normal forms for 3")
                || m.contains("1 Smith normal forms for 2")
        ));
        let mut bare_arguments = arguments;
        bare_arguments["certificate"] = "none".into();
        let bare = crate::parse(&job(bare_arguments)).unwrap();
        assert!(matches!(
            crate::interpret(&bare, reply, &Interrupt::none()),
            Err(CasError::UnsupportedResult(m)) if m.contains("did not ask")
        ));
    }
}

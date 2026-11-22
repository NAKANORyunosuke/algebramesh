//! The checks of the topology package (Issue #103): the values a backend
//! built from recorded Macaulay2 replies (`testdata/values.json`, kept equal
//! to them by the backend's test
//! `the_topology_values_are_those_of_the_recorded_replies`) pass with the
//! claims of `docs/claims.md`, give the groups of the acceptance criteria,
//! and values changed so that a claim is false are refused.

#![allow(clippy::unwrap_used)]

use algebramesh_core::check::Interrupt;
use algebramesh_core::guarantee::{EvidenceLevel, ResultVerifier, Summary};
use algebramesh_core::{BackendInfo, CasError, CasResult, JobSpec};
use algebramesh_ops::topology::SimplicialHomologyArguments;
use algebramesh_topology::SimplicialComplex;
use algebramesh_topology::verify::registry;
use algebramesh_verify::catalogue;
use serde_json::{Value, json};

fn cases() -> Vec<Value> {
    serde_json::from_str(include_str!("../testdata/values.json")).unwrap()
}

fn case(name: &str) -> Value {
    cases().into_iter().find(|c| c["name"] == name).unwrap()
}

fn spec(case: &Value) -> JobSpec {
    JobSpec {
        schema_version: 1,
        backend: "macaulay2".into(),
        operation: case["operation"].as_str().unwrap().into(),
        arguments: case["arguments"].clone(),
        limits: Default::default(),
        retry_policy: Default::default(),
        environment: Default::default(),
    }
}

fn result(value: Value) -> CasResult {
    CasResult {
        value,
        backend: BackendInfo {
            name: "macaulay2".into(),
            ..BackendInfo::default()
        },
        execution_host: None,
        log: vec![],
        guarantee: None,
    }
}

/// `value` checked as the result of the job of `case`; what a passing result
/// claims is what the catalogue lists.
fn check(case: &Value, value: Value) -> Result<CasResult, CasError> {
    let spec = spec(case);
    let checked = registry().verify(&spec, result(value), &Interrupt::none());
    if let Ok(CasResult {
        guarantee: Some(guarantee),
        ..
    }) = &checked
    {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/claims.md");
        let catalogue = catalogue::parse(&std::fs::read_to_string(path).unwrap());
        let problems = catalogue.check_guarantee(&spec.operation, guarantee);
        assert!(problems.is_empty(), "{problems:#?}");
    }
    checked
}

fn refused(outcome: Result<CasResult, CasError>, why: &str) {
    match outcome {
        Err(CasError::UnsupportedResult(m)) => assert!(m.contains(why), "{m}"),
        other => panic!("expected UNSUPPORTED_RESULT ({why}), got {other:?}"),
    }
}

/// The complex of the job of `case`.
fn complex(case: &Value) -> SimplicialComplex {
    let args: SimplicialHomologyArguments =
        serde_json::from_value(case["arguments"].clone()).unwrap();
    SimplicialComplex::from_data(&args.complex).unwrap()
}

/// A JSON matrix of decimal strings as 64-bit integers.
fn integers(m: &Value) -> Vec<Vec<i64>> {
    m.as_array()
        .unwrap()
        .iter()
        .map(|r| {
            r.as_array()
                .unwrap()
                .iter()
                .map(|x| x.as_str().unwrap().parse().unwrap())
                .collect()
        })
        .collect()
}

/// The product of two integer matrices, computed here and not by the
/// checker.
fn mul(a: &[Vec<i64>], b: &[Vec<i64>]) -> Vec<Vec<i64>> {
    a.iter()
        .map(|row| {
            (0..b[0].len())
                .map(|j| row.iter().zip(b).map(|(x, r)| x * r[j]).sum())
                .collect()
        })
        .collect()
}

/// H_0, ..., H_d of a value as (rank, torsion).
fn groups(value: &Value) -> Vec<(u64, Vec<&str>)> {
    value["homology"]
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
                    .map(|t| t.as_str().unwrap())
                    .collect(),
            )
        })
        .collect()
}

const CLAIMS: [&str; 5] = [
    "homology.boundary",
    "homology.snf_form",
    "homology.snf_equivalent",
    "homology.groups",
    "homology.euler_characteristic",
];

/// Issue #103: every recorded value passes, unchanged, with the claims of
/// the catalogue. Without certificates `homology.snf_equivalent` is
/// asserted, and so is `homology.groups`, which rests on it.
#[test]
fn the_recorded_homology_values_pass_with_the_claims_of_the_catalogue() {
    for case in cases() {
        let name = case["name"].as_str().unwrap();
        let checked = check(&case, case["value"].clone()).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(checked.value, case["value"], "{name}");
        let g = checked.guarantee.unwrap();
        let ids: Vec<&str> = g.claims.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, CLAIMS, "{name}");
        assert_eq!(g.summary, Summary::PartiallyVerified, "{name}");
        let with_certificate = case["arguments"]["certificate"] != "none";
        for id in CLAIMS {
            let weak = matches!(id, "homology.snf_equivalent" | "homology.groups");
            let level = if with_certificate || !weak {
                EvidenceLevel::Checked
            } else {
                EvidenceLevel::Asserted
            };
            assert_eq!(g.effective_level(id), level, "{name}: {id}");
        }
        assert!(!g.not_claimed.is_empty());
    }
}

/// Acceptance criterion of #103: the boundary of the tetrahedron has
/// H_2 = ZZ (and H_0 = ZZ, H_1 = 0).
#[test]
fn the_boundary_of_the_tetrahedron_has_h2_zz() {
    let case = case("tetrahedron_boundary");
    let checked = check(&case, case["value"].clone()).unwrap();
    assert_eq!(
        groups(&checked.value),
        [(1, vec![]), (0, vec![]), (1, vec![])]
    );
}

/// Acceptance criterion of #103: the torus with 7 vertices has H_1 = ZZ^2.
#[test]
fn the_torus_with_seven_vertices_has_h1_zz2() {
    let case = case("torus");
    assert_eq!(case["value"]["face_counts"], json!([7, 21, 14]));
    let checked = check(&case, case["value"].clone()).unwrap();
    assert_eq!(
        groups(&checked.value),
        [(1, vec![]), (2, vec![]), (1, vec![])]
    );
}

/// Acceptance criterion of #103: the real projective plane with 6 vertices
/// has H_1 = ZZ/2 and H_2 = 0.
#[test]
fn the_projective_plane_with_six_vertices_has_h1_z2_and_h2_zero() {
    let case = case("projective_plane");
    assert_eq!(case["value"]["face_counts"], json!([6, 15, 10]));
    let checked = check(&case, case["value"].clone()).unwrap();
    assert_eq!(
        groups(&checked.value),
        [(1, vec![]), (0, vec!["2"]), (0, vec![])]
    );
}

/// `homology.euler_characteristic`, recomputed: 2 for the sphere, 0 for the
/// torus, 1 for the projective plane, 2 for two points.
#[test]
fn the_euler_characteristics_of_the_recorded_complexes() {
    for (name, chi) in [
        ("tetrahedron_boundary", 2),
        ("torus", 0),
        ("projective_plane", 1),
        ("two_points", 2),
    ] {
        let case = case(name);
        assert_eq!(complex(&case).euler_characteristic(), chi, "{name}");
        assert_eq!(case["value"]["euler_characteristic"], chi, "{name}");
    }
}

/// `homology.euler_characteristic`: another number is refused.
#[test]
fn a_changed_euler_characteristic_is_refused() {
    let case = case("torus");
    let mut value = case["value"].clone();
    value["euler_characteristic"] = json!(2);
    // 7 - 21 + 14 = 0, not 2.
    refused(check(&case, value), "Euler characteristic is 2");
}

/// `homology.boundary`: face counts that are not those of the complex of the
/// job are refused.
#[test]
fn a_value_with_other_face_counts_is_refused() {
    let case = case("torus");
    let mut value = case["value"].clone();
    value["face_counts"] = json!([7, 21, 15]);
    // The 14 triangles of the job: one face count is changed.
    assert_eq!(complex(&case).faces(2).len(), 14);
    refused(check(&case, value), "face counts");
}

/// `homology.snf_form`: a diagonal whose entries do not divide one another
/// is refused.
#[test]
fn a_smith_diagonal_of_a_boundary_that_does_not_divide_is_refused() {
    let case = case("projective_plane");
    let mut value = case["value"].clone();
    // ∂_2: (1, ..., 1, 2) becomes (2, 1, ..., 1, 2); 2 does not divide 1.
    assert_eq!(value["smith_diagonals"][1][0], "1");
    value["smith_diagonals"][1][0] = json!("2");
    let entry = |i: usize| -> i64 {
        value["smith_diagonals"][1][i]
            .as_str()
            .unwrap()
            .parse()
            .unwrap()
    };
    assert_ne!(entry(1) % entry(0), 0);
    refused(check(&case, value), "dividing the next");
}

/// Acceptance criterion of #103 and `homology.snf_equivalent`: one entry of
/// a Smith normal form changed (with the groups it gives) is refused, since
/// P ∂ Q is still the old diagonal.
#[test]
fn a_changed_smith_entry_is_refused() {
    let case = case("projective_plane");
    let mut value = case["value"].clone();
    let last = value["smith_diagonals"][1].as_array().unwrap().len() - 1;
    assert_eq!(value["smith_diagonals"][1][last], "2");
    value["smith_diagonals"][1][last] = json!("4");
    value["homology"][1]["torsion"] = json!(["4"]);
    // P ∂_2 Q, computed here, has 2 at that place.
    let certificate = &value["certificate"]["boundaries"][1];
    let boundary: Vec<Vec<i64>> = complex(&case)
        .boundary(2)
        .rows()
        .iter()
        .map(|r| r.iter().map(|x| x.to_string().parse().unwrap()).collect())
        .collect();
    let paq = mul(
        &mul(&integers(&certificate["left"]["transform"]), &boundary),
        &integers(&certificate["right"]["transform"]),
    );
    assert_eq!(paq[last][last], 2);
    refused(check(&case, value), "P A Q is not the diagonal matrix");
}

/// `homology.snf_equivalent`: one entry of a transform changed is refused,
/// since its product with the inverse is no longer I.
#[test]
fn a_changed_transform_of_a_boundary_is_refused() {
    let case = case("torus");
    let mut value = case["value"].clone();
    let left = &mut value["certificate"]["boundaries"][0]["left"];
    let old: i64 = left["transform"][0][0].as_str().unwrap().parse().unwrap();
    left["transform"][0][0] = json!((old + 1).to_string());
    let product = mul(&integers(&left["transform"]), &integers(&left["inverse"]));
    assert!(
        product
            .iter()
            .enumerate()
            .any(|(i, r)| r.iter().enumerate().any(|(j, x)| *x != i64::from(i == j)))
    );
    refused(check(&case, value), "is not its inverse");
}

/// `homology.snf_equivalent`: a certificate left out by the backend, or
/// missing although the job asked for it, is refused; a level is never
/// lowered silently.
#[test]
fn a_certificate_left_out_by_the_backend_is_refused() {
    let case = case("torus");
    let mut value = case["value"].clone();
    value["certificate"] = json!({"omitted": "certificate_too_large"});
    refused(check(&case, value), "only the checker does");
    let mut value = case["value"].clone();
    value.as_object_mut().unwrap().remove("certificate");
    refused(check(&case, value), "has none");
}

/// `homology.groups`: a rank other than f_k - r_k - r_(k+1) is refused.
#[test]
fn homology_groups_other_than_the_forms_give_are_refused() {
    let case = case("torus");
    let mut value = case["value"].clone();
    // rank H_1 = f_1 - rank ∂_1 - rank ∂_2, counted here.
    let nonzero = |k: usize| {
        value["smith_diagonals"][k]
            .as_array()
            .unwrap()
            .iter()
            .filter(|d| *d != "0")
            .count()
    };
    assert_eq!(21 - nonzero(0) - nonzero(1), 2);
    // The Euler characteristic still agrees: 1 - 3 + 2 = 0.
    value["homology"][1]["rank"] = json!(3);
    value["homology"][2]["rank"] = json!(2);
    refused(check(&case, value), "not those the Smith normal forms give");
}

/// `homology.groups`: a torsion coefficient the forms do not give is
/// refused.
#[test]
fn a_torsion_coefficient_that_the_forms_do_not_give_is_refused() {
    let case = case("projective_plane");
    let mut value = case["value"].clone();
    // The entries of the diagonal of ∂_2 above 1 are the torsion of H_1.
    let above_one: Vec<&Value> = value["smith_diagonals"][1]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| *d != "0" && *d != "1")
        .collect();
    assert_eq!(above_one, [&json!("2")]);
    value["homology"][1]["torsion"] = json!([]);
    refused(check(&case, value), "not those the Smith normal forms give");
}

/// Acceptance criterion of #103: a job whose complex is over the bound on
/// its faces is refused by the checker too.
#[test]
fn a_job_over_the_bound_on_faces_is_refused() {
    let mut case = case("two_points");
    let simplex: Vec<u32> = (0..10).collect();
    case["arguments"]["complex"]["facets"] = json!([simplex]);
    refused(check(&case, case["value"].clone()), "1023 faces");
}

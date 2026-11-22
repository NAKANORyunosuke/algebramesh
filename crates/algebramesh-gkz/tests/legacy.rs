//! Records of the GKZ operations without a guarantee (V4, Issue #108, ADR
//! 0031): the values of the package tests (`testdata/values.json`) mapped by
//! `legacy_guarantee` get the claims and the levels their checker gives;
//! values recorded before `evidence_terms` and the certificates (ADR 0028,
//! ADR 0029) get the claims those support asserted, from the mapping and
//! from the check again, and are not refused.

#![allow(clippy::unwrap_used)]

use std::collections::BTreeSet;

use algebramesh_core::check::Interrupt;
use algebramesh_core::guarantee::{EvidenceLevel, Guarantee, ResultVerifier};
use algebramesh_core::{BackendInfo, CasResult, JobSpec};
use algebramesh_gkz::verify::registry;
use algebramesh_verify::catalogue;
use serde_json::Value;

const DRIVER: &str = "algebramesh-backend-macaulay2 1.1.0";

fn cases() -> Vec<Value> {
    serde_json::from_str(include_str!("../testdata/values.json")).unwrap()
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

fn backend() -> BackendInfo {
    BackendInfo {
        name: "macaulay2".into(),
        driver_version: DRIVER.into(),
        ..BackendInfo::default()
    }
}

/// The guarantee the agent records for `value`.
fn agent(case: &Value, value: &Value) -> Result<Guarantee, String> {
    let result = CasResult {
        value: value.clone(),
        backend: backend(),
        execution_host: None,
        log: vec![],
        guarantee: None,
    };
    registry()
        .verify(&spec(case), result, &Interrupt::none())
        .map(|r| r.guarantee.unwrap())
        .map_err(|e| e.to_string())
}

fn legacy(case: &Value, value: &Value) -> Guarantee {
    registry()
        .legacy_guarantee(case["operation"].as_str().unwrap(), value, DRIVER)
        .unwrap()
}

fn again(case: &Value, value: &Value) -> Guarantee {
    registry()
        .check_again(&spec(case), value, &backend(), &Interrupt::none())
        .unwrap()
}

/// Each claim with its level, its effective level, its scope and its
/// premises.
fn levels(g: &Guarantee) -> Vec<String> {
    g.claims
        .iter()
        .map(|c| {
            format!(
                "{} {:?} {:?} {:?} {:?}",
                c.id,
                c.level,
                g.effective_level(&c.id),
                c.scope,
                c.depends_on
            )
        })
        .collect()
}

fn catalogue_accepts(operation: &str, g: &Guarantee) {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/claims.md");
    let catalogue = catalogue::parse(&std::fs::read_to_string(path).unwrap());
    let problems = catalogue.check_guarantee(operation, g);
    assert!(problems.is_empty(), "{problems:#?}");
}

/// The value as writers before ADR 0028 and ADR 0029 recorded it: without
/// `evidence_terms` and without certificates.
fn old(value: &Value) -> Value {
    fn strip(v: &mut Value) {
        match v {
            Value::Object(m) => {
                m.remove("evidence_terms");
                m.remove("certificate");
                m.values_mut().for_each(strip);
            }
            Value::Array(a) => a.iter_mut().for_each(strip),
            _ => {}
        }
    }
    let mut v = value.clone();
    strip(&mut v);
    v
}

/// The claims that a certificate or `evidence_terms` supports.
fn by_newer_fields(id: &str) -> bool {
    id.starts_with("gkz.membership[")
        || id == "series.log_independent"
        || id == "series.not_a_solution"
}

/// The acceptance condition: the old record and the new record of the same
/// result get the same levels. For a value that carries no certificate and
/// no evidence, the mapping gives exactly what the agent recorded; for the
/// others, only the claims those fields support differ, and they are
/// asserted.
#[test]
fn old_and_new_records_of_a_gkz_result_get_the_same_levels() {
    for case in cases() {
        let name = case["name"].as_str().unwrap();
        let op = case["operation"].as_str().unwrap();
        let value = &case["value"];
        let new = agent(&case, value).unwrap();
        let mapped = legacy(&case, value);
        catalogue_accepts(op, &mapped);
        if old(value) == *value {
            assert_eq!(levels(&mapped), levels(&new), "{name}");
            assert_eq!(mapped.summary, new.summary, "{name}");
            continue;
        }
        assert_eq!(mapped.claims.len(), new.claims.len(), "{name}");
        for (m, n) in mapped.claims.iter().zip(&new.claims) {
            assert_eq!(m.id, n.id, "{name}");
            if m.level != n.level {
                assert!(by_newer_fields(&m.id), "{name}: {}", m.id);
                assert_eq!(m.level, EvidenceLevel::Asserted, "{name}: {}", m.id);
            }
        }
    }
}

/// A value recorded before `evidence_terms` and the certificates: the
/// mapping and the check again agree, the claims those fields support are
/// asserted, and the value is not refused. The agent, which writes them,
/// refuses a new value without its evidence.
#[test]
fn an_old_value_lowers_the_claims_of_its_missing_fields_and_is_not_refused() {
    let mut lowered = BTreeSet::new();
    for case in cases() {
        let name = case["name"].as_str().unwrap();
        let value = old(&case["value"]);
        let mapped = legacy(&case, &value);
        let checked = again(&case, &value);
        assert_eq!(levels(&mapped), levels(&checked), "{name}");
        let new = agent(&case, &case["value"]).unwrap();
        for (c, n) in checked.claims.iter().zip(&new.claims) {
            if c.level != n.level {
                assert_eq!(c.level, EvidenceLevel::Asserted, "{name}: {}", c.id);
                assert!(c.reason.is_some(), "{name}: {}", c.id);
                lowered.insert(catalogue::family(&c.id));
            }
        }
        if case["value"].to_string().contains("evidence_terms") {
            let e = agent(&case, &value).unwrap_err();
            assert!(e.contains("evidence"), "{name}: {e}");
        }
    }
    assert_eq!(
        lowered,
        BTreeSet::from([
            "gkz.membership[j]".to_owned(),
            "series.log_independent".to_owned(),
            "series.not_a_solution".to_owned(),
        ])
    );
}

/// A record whose field says that a check failed is not mapped to claims.
#[test]
fn an_old_gkz_record_that_fails_its_fields_is_refused() {
    for case in cases() {
        let name = case["name"].as_str().unwrap();
        let field = if case["operation"] == "gkz.generate" {
            "euler_operators_match"
        } else {
            "exponents_are_roots"
        };
        let mut value = case["value"].clone();
        value["checks"][field] = Value::Bool(false);
        let e = registry()
            .legacy_guarantee(case["operation"].as_str().unwrap(), &value, DRIVER)
            .unwrap_err();
        assert!(e.to_string().contains(field), "{name}: {e}");
    }
}

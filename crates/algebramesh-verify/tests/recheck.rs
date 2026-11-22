//! Old records mapped to guarantees and records checked again (V4, Issue
//! #108, ADR 0031): a record without a guarantee gets the levels a record
//! with one gets for the same result; a body of a blob that is not the one
//! its marker names, or that no scheduler holds, stops the check; the record
//! is not changed.

#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;

use algebramesh_core::blob::{BlobId, BlobRef};
use algebramesh_core::check::Interrupt;
use algebramesh_core::guarantee::{EvidenceLevel, Guarantee, ResultVerifier};
use algebramesh_core::{BackendInfo, CasResult, JobRecord, JobSpec, canonical_json, sha256_hex};
use algebramesh_verify::recheck::{BlobStatus, Body, InputCheck, Outcome, Recorded, recheck};
use algebramesh_verify::{NOT_RECORDED, Registry, catalogue};
use serde_json::{Value, json};

fn ring(domain: &str) -> Value {
    json!({"domain": domain, "variables": ["x", "y"], "order": "grevlex"})
}

fn poly(terms: &[(&str, [u32; 2])]) -> Value {
    json!({"type": "polynomial", "terms": terms.iter()
        .map(|(c, e)| json!({"coefficient": c, "exponents": e})).collect::<Vec<_>>()})
}

fn spec(operation: &str, arguments: Value) -> JobSpec {
    JobSpec {
        schema_version: 1,
        backend: "asir".into(),
        operation: operation.into(),
        arguments,
        limits: Default::default(),
        retry_policy: Default::default(),
        environment: Default::default(),
    }
}

/// 2 x^2 - 2 y^2 = 2 (x + y) (x - y) over `domain`.
fn factor_job(domain: &str) -> (JobSpec, Value) {
    let spec = spec(
        "polynomial.factor",
        json!({"ring": ring(domain), "expression": poly(&[("2", [2, 0]), ("-2", [0, 2])])}),
    );
    let value = json!({
        "type": "factorization",
        "ring": ring(domain),
        "constant": "2",
        "factors": [
            {"factor": poly(&[("1", [1, 0]), ("1", [0, 1])]), "multiplicity": 1, "display": "x+y"},
            {"factor": poly(&[("1", [1, 0]), ("-1", [0, 1])]), "multiplicity": 1, "display": "x-y"},
        ],
        "verified_product": true,
    });
    (spec, value)
}

fn backend() -> BackendInfo {
    BackendInfo {
        name: "asir".into(),
        backend_version: Some("20260331".into()),
        driver_version: "algebramesh-backend-asir 1.1.0".into(),
        ..BackendInfo::default()
    }
}

/// The guarantee the agent records for `value`.
fn agent_guarantee(spec: &JobSpec, value: &Value) -> Guarantee {
    let result = CasResult {
        value: value.clone(),
        backend: backend(),
        execution_host: None,
        log: vec![],
        guarantee: None,
    };
    Registry::standard()
        .verify(spec, result, &Interrupt::none())
        .unwrap()
        .guarantee
        .unwrap()
}

/// The record of a job of `spec` that succeeded with `result`.
fn record(spec: &JobSpec, result: Value, guarantee: Option<Guarantee>) -> JobRecord {
    let input_normalized = canonical_json(&serde_json::to_value(spec).unwrap());
    serde_json::from_value(json!({
        "job_id": "6f1c3a52-0d1e-4c55-9d43-2b8f0e6a7c11",
        "state": "SUCCEEDED",
        "spec": spec,
        "input_sha256": sha256_hex(input_normalized.as_bytes()),
        "input_normalized": input_normalized,
        "backend": backend(),
        "history": [],
        "result": result,
        "guarantee": guarantee,
    }))
    .unwrap()
}

/// Each claim with its level and its effective level.
fn levels(g: &Guarantee) -> Vec<(String, EvidenceLevel, EvidenceLevel)> {
    g.claims
        .iter()
        .map(|c| (c.id.clone(), c.level, g.effective_level(&c.id)))
        .collect()
}

fn catalogue_accepts(operation: &str, g: &Guarantee) {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/claims.md");
    let catalogue = catalogue::parse(&std::fs::read_to_string(path).unwrap());
    let problems = catalogue.check_guarantee(operation, g);
    assert!(problems.is_empty(), "{problems:#?}");
}

fn rechecked(outcome: &Outcome) -> &Guarantee {
    match outcome {
        Outcome::Rechecked { guarantee } => guarantee,
        other => panic!("not rechecked: {other:?}"),
    }
}

/// The acceptance condition: the old record (no guarantee) and the new one
/// (the agent's guarantee) of the same factorization get the same levels,
/// from the mapping and from the check again.
#[test]
fn old_and_new_records_of_a_factorization_get_the_same_levels() {
    for domain in ["QQ", "ZZ"] {
        let (spec, value) = factor_job(domain);
        let new = agent_guarantee(&spec, &value);
        let legacy = Registry::standard()
            .legacy_guarantee(&spec.operation, &value, &backend().driver_version)
            .unwrap();
        assert_eq!(levels(&legacy), levels(&new), "{domain}");
        assert_eq!(legacy.summary, new.summary);
        catalogue_accepts(&spec.operation, &legacy);
        // factor.product comes from the field, the others from the value.
        let product = &legacy.claims[0];
        assert_eq!(product.by, "algebramesh-backend-asir 1.1.0");
        assert!(
            product
                .reason
                .as_deref()
                .unwrap()
                .contains("verified_product"),
            "{product:?}"
        );

        let old = record(&spec, value.clone(), None);
        let newer = record(&spec, value.clone(), Some(new.clone()));
        let registry = Registry::standard();
        let a = recheck(&registry, &old, &BTreeMap::new(), &Interrupt::none());
        let b = recheck(&registry, &newer, &BTreeMap::new(), &Interrupt::none());
        assert!(
            matches!(&a.recorded, Recorded::LegacyMapping { guarantee } if *guarantee == legacy)
        );
        assert!(matches!(&b.recorded, Recorded::Record { guarantee } if *guarantee == new));
        assert_eq!(levels(rechecked(&a.recheck)), levels(&new));
        assert_eq!(levels(rechecked(&b.recheck)), levels(&new));
    }
}

/// A record whose field says that the product was not checked, or whose
/// factors are associates, is not mapped to claims.
#[test]
fn an_old_record_that_fails_its_fields_is_refused() {
    let (spec, mut value) = factor_job("QQ");
    value["verified_product"] = json!(false);
    let e = Registry::standard()
        .legacy_guarantee(&spec.operation, &value, "")
        .unwrap_err();
    assert!(e.to_string().contains("verified_product"), "{e}");

    let (_, mut value) = factor_job("QQ");
    value["factors"][1]["factor"] = poly(&[("2", [1, 0]), ("2", [0, 1])]);
    let e = Registry::standard()
        .legacy_guarantee(&spec.operation, &value, "")
        .unwrap_err();
    assert!(e.to_string().contains("associates"), "{e}");
}

/// A record of an operation whose values record no checks claims only its
/// value, asserted, with the reason; checked again, it gets its claims.
#[test]
fn an_old_record_of_a_newer_operation_is_asserted() {
    let spec = spec(
        "matrix.rank",
        json!({"domain": "QQ", "matrix": [[1, 2, 3], [2, 4, 6], [1, 0, 1]]}),
    );
    let value = json!({
        "type": "matrix_rank",
        "rank": 2,
        "certificate": {
            "minor": {"rows": [0, 2], "columns": [0, 1], "inverse": [["0", "1"], ["1/2", "-1/2"]]},
            "kernel": [["-1"], ["-1"], ["1"]],
        },
    });
    let legacy = Registry::standard()
        .legacy_guarantee("matrix.rank", &value, "algebramesh-backend-oscar 1.1.0")
        .unwrap();
    assert_eq!(legacy.claims.len(), 1);
    assert_eq!(legacy.claims[0].id, "value");
    assert_eq!(legacy.claims[0].level, EvidenceLevel::Asserted);
    assert_eq!(legacy.claims[0].reason.as_deref(), Some(NOT_RECORDED));

    let answer = recheck(
        &Registry::standard(),
        &record(&spec, value, None),
        &BTreeMap::new(),
        &Interrupt::none(),
    );
    let g = rechecked(&answer.recheck);
    assert!(
        g.claims.iter().all(|c| c.level == EvidenceLevel::Checked),
        "{g:?}"
    );
}

/// A value whose certificate was left out for its size is answered "cannot
/// be rechecked", with what the record claims, and no other value.
#[test]
fn a_value_whose_certificate_was_left_out_cannot_be_rechecked() {
    let spec = spec(
        "matrix.rank",
        json!({"domain": "QQ", "matrix": [[1, 2, 3], [2, 4, 6], [1, 0, 1]]}),
    );
    let value = json!({
        "type": "matrix_rank",
        "rank": 2,
        "certificate": {"omitted": "certificate_too_large"},
    });
    let answer = recheck(
        &Registry::standard(),
        &record(&spec, value, None),
        &BTreeMap::new(),
        &Interrupt::none(),
    );
    match &answer.recheck {
        Outcome::CannotRecheck { reason } => {
            assert!(reason.contains("certificate_too_large"), "{reason}");
        }
        other => panic!("{other:?}"),
    }
    assert!(!answer.rechecked());
    let json = serde_json::to_string(&answer).unwrap();
    assert!(!json.contains("\"rank\":2"), "no value is given: {json}");
}

/// A record whose result is a blob, and the body that the scheduler gives.
fn blob_record() -> (JobRecord, Value, BlobRef) {
    let (spec, value) = factor_job("QQ");
    let body = canonical_json(&value).into_bytes();
    let r = BlobRef {
        id: BlobId::of(&body),
        size: body.len() as u64,
    };
    let g = agent_guarantee(&spec, &value);
    (record(&spec, r.marker(), Some(g)), value, r)
}

#[test]
fn a_result_in_a_blob_is_checked_from_its_body() {
    let (record, value, r) = blob_record();
    let bodies = BTreeMap::from([(
        r.id.clone(),
        Body::Bytes(canonical_json(&value).into_bytes()),
    )]);
    let answer = recheck(&Registry::standard(), &record, &bodies, &Interrupt::none());
    assert_eq!(answer.input, InputCheck::Ok);
    assert_eq!(answer.blobs.len(), 1);
    assert_eq!(answer.blobs[0].status, BlobStatus::Ok);
    assert_eq!(answer.blobs[0].place, "result");
    let recorded = answer.recorded.guarantee().unwrap();
    assert_eq!(levels(rechecked(&answer.recheck)), levels(recorded));
}

/// The acceptance condition: a body with one byte changed is a hash error,
/// and nothing is checked.
#[test]
fn a_body_with_one_byte_changed_is_a_hash_error() {
    let (record, value, r) = blob_record();
    let mut body = canonical_json(&value).into_bytes();
    let at = body.iter().position(|b| *b == b'x').unwrap();
    body[at] = b'z';
    assert_ne!(BlobId::of(&body), r.id);
    let bodies = BTreeMap::from([(r.id.clone(), Body::Bytes(body))]);
    let answer = recheck(&Registry::standard(), &record, &bodies, &Interrupt::none());
    assert_eq!(answer.blobs[0].status, BlobStatus::HashMismatch);
    assert!(matches!(answer.recheck, Outcome::NotChecked { .. }));

    // A reader that found it on the way says so in the same terms.
    let bodies = BTreeMap::from([(r.id.clone(), Body::HashMismatch("damaged".into()))]);
    let answer = recheck(&Registry::standard(), &record, &bodies, &Interrupt::none());
    assert_eq!(answer.blobs[0].status, BlobStatus::HashMismatch);
    assert!(!answer.rechecked());
}

/// The acceptance condition: a blob without a body is DATA_LOSS, and
/// nothing is checked.
#[test]
fn a_blob_without_a_body_is_data_loss() {
    let (record, _, r) = blob_record();
    let bodies = BTreeMap::from([(r.id.clone(), Body::DataLoss("no scheduler holds it".into()))]);
    let answer = recheck(&Registry::standard(), &record, &bodies, &Interrupt::none());
    assert_eq!(answer.blobs[0].status, BlobStatus::DataLoss);
    assert!(matches!(answer.recheck, Outcome::NotChecked { .. }));
    let json = serde_json::to_value(&answer).unwrap();
    assert_eq!(json["blobs"][0]["status"], "data_loss");
    assert_eq!(json["recheck"]["status"], "not_checked");
}

#[test]
fn a_body_of_another_size_or_not_canonical_is_not_used() {
    let (record, value, r) = blob_record();
    let bodies = BTreeMap::from([(r.id.clone(), Body::Bytes(b"{}".to_vec()))]);
    let answer = recheck(&Registry::standard(), &record, &bodies, &Interrupt::none());
    assert_eq!(answer.blobs[0].status, BlobStatus::SizeMismatch);

    // The same value, not in canonical form, under the name of its bytes.
    let pretty = serde_json::to_vec_pretty(&value).unwrap();
    let r = BlobRef {
        id: BlobId::of(&pretty),
        size: pretty.len() as u64,
    };
    let mut record = record;
    record.result = Some(r.marker());
    let bodies = BTreeMap::from([(r.id.clone(), Body::Bytes(pretty))]);
    let answer = recheck(&Registry::standard(), &record, &bodies, &Interrupt::none());
    assert_eq!(answer.blobs[0].status, BlobStatus::NotCanonical);
    assert!(!answer.rechecked());
}

/// A record whose input does not match its hash is not checked.
#[test]
fn a_record_whose_input_does_not_match_its_hash_is_not_checked() {
    let (spec, value) = factor_job("QQ");
    let mut record = record(&spec, value, None);
    record.input_sha256 = sha256_hex(b"another input");
    let answer = recheck(
        &Registry::standard(),
        &record,
        &BTreeMap::new(),
        &Interrupt::none(),
    );
    assert!(matches!(answer.input, InputCheck::HashMismatch(_)));
    assert!(matches!(answer.recheck, Outcome::NotChecked { .. }));
}

/// The acceptance condition: the record is not changed, whatever the check
/// finds.
#[test]
fn the_record_is_not_changed() {
    let (record, value, r) = blob_record();
    let before = serde_json::to_string(&record).unwrap();
    for body in [
        Body::Bytes(canonical_json(&value).into_bytes()),
        Body::DataLoss("lost".into()),
    ] {
        let bodies = BTreeMap::from([(r.id.clone(), body)]);
        let _ = recheck(&Registry::standard(), &record, &bodies, &Interrupt::none());
        assert_eq!(serde_json::to_string(&record).unwrap(), before);
    }
}

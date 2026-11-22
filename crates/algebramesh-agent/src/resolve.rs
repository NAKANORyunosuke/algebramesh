//! Arguments in blobs read before the backend runs (ADR 0021 §3, B2): the
//! agent reads each blob a top-level argument refers to with `GetBlob`, takes
//! the transfer up where it stopped after a cut, and checks that the bytes
//! have the blob's SHA-256 and are canonical JSON before the value takes the
//! marker's place. A blob that cannot be read, or whose bytes are not a value,
//! fails the job without calling the backend, with the blob's id in the error.
//!
//! A reference `{"$ref": {"job_id", "pointer", "sha256"}}` (W2) is read from
//! the producer's committed record (`GetRecord`): the value at the pointer,
//! inline or in a blob, must have the SHA-256 the reference names. A marker's
//! origin `from` is dropped with the marker; the backend sees values only.

use std::time::Duration;

use algebramesh_core::blob::{ArgumentRef, BlobRef, RecordRef, argument_markers, argument_refs};
use algebramesh_core::{CasError, JobSpec};
use algebramesh_rpc::pb;

use crate::{Gateway, RPC_TIMEOUT, within};

/// The bytes the arguments of `spec` hold in blobs.
pub(crate) fn blob_bytes(spec: &JobSpec) -> u64 {
    argument_markers(&spec.arguments)
        .map(|m| m.iter().map(|(_, r)| r.size).sum())
        .unwrap_or(0)
}

/// Replaces each marker among the top-level arguments of `spec` by the value
/// of its blob, trying until `deadline` while a scheduler cannot answer.
pub(crate) async fn resolve_arguments(
    spec: &mut JobSpec,
    gateway: &(dyn Fn() -> Gateway + Sync),
    deadline: tokio::time::Instant,
) -> Result<(), CasError> {
    let found =
        argument_markers(&spec.arguments).map_err(|e| CasError::InvalidArguments(e.to_string()))?;
    for (key, r) in found {
        let bytes = read_patiently(gateway, &r, deadline)
            .await
            .map_err(|e| unreadable(&key, &r, &e))?;
        let value = value_of(&key, &r, &bytes)?;
        if let serde_json::Value::Object(m) = &mut spec.arguments {
            m.insert(key, value);
        }
    }
    Ok(())
}

/// The references `{"$ref": ...}` among the arguments of `spec`.
pub(crate) fn record_refs(spec: &JobSpec) -> Vec<(String, RecordRef)> {
    argument_refs(&spec.arguments)
        .map(|found| {
            found
                .into_iter()
                .filter_map(|(k, r)| match r {
                    ArgumentRef::Record(r) => Some((k, r)),
                    ArgumentRef::Blob { .. } => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Replaces each reference `{"$ref": ...}` among the top-level arguments of
/// `spec` by the value it names in its producer's committed record, after
/// checking its SHA-256; trying until `deadline` while a scheduler cannot
/// answer.
pub(crate) async fn resolve_references(
    spec: &mut JobSpec,
    gateway: &(dyn Fn() -> Gateway + Sync),
    deadline: tokio::time::Instant,
) -> Result<(), CasError> {
    for (key, r) in record_refs(spec) {
        let record = record_patiently(gateway, &r.job_id, deadline)
            .await
            .map_err(|e| {
                let what = format!(
                    "argument {key}: the record of job {} could not be read: {}: {}",
                    r.job_id,
                    e.code(),
                    e.message()
                );
                match e.code() {
                    tonic::Code::Unavailable | tonic::Code::DeadlineExceeded => {
                        CasError::Unavailable(what)
                    }
                    _ => CasError::InvalidArguments(what),
                }
            })?;
        let value = referenced_value(&key, &r, &record)?;
        let value = match BlobRef::from_marker(&value) {
            Ok(Some(b)) => {
                let bytes = read_patiently(gateway, &b, deadline)
                    .await
                    .map_err(|e| unreadable(&key, &b, &e))?;
                value_of(&key, &b, &bytes)?
            }
            _ => value,
        };
        if let serde_json::Value::Object(m) = &mut spec.arguments {
            m.insert(key, value);
        }
    }
    Ok(())
}

/// The value `r` names in the committed `record` of its producer, inline or
/// as a marker, if it has the SHA-256 `r` names.
fn referenced_value(
    key: &str,
    r: &RecordRef,
    record: &pb::JobResult,
) -> Result<serde_json::Value, CasError> {
    let wrong = |why: String| {
        CasError::InvalidArguments(format!(
            "argument {key}: {why} (reference to {:?} of job {})",
            r.pointer, r.job_id
        ))
    };
    let state = record
        .status
        .as_ref()
        .map(|s| s.state.as_str())
        .unwrap_or("");
    if state != "SUCCEEDED" {
        return Err(wrong(format!("the producer is {state}, not SUCCEEDED")));
    }
    let record: algebramesh_core::JobRecord = serde_json::from_str(&record.record_json)
        .map_err(|e| wrong(format!("the record is not readable: {e}")))?;
    let result = record
        .result
        .ok_or_else(|| wrong("the record has no result".into()))?;
    let value = algebramesh_core::blob::pointed(&result, &r.pointer)
        .map_err(|e| wrong(e.to_string()))?
        .ok_or_else(|| wrong("the result has no top-level value there".into()))?
        .clone();
    let id = algebramesh_core::blob::recorded_id(&value);
    if id != r.sha256 {
        return Err(wrong(format!("the value there is {id}, not {}", r.sha256)));
    }
    Ok(value)
}

/// The committed record of `job_id`, again after failures that may pass,
/// until `deadline`.
async fn record_patiently(
    gateway: &(dyn Fn() -> Gateway + Sync),
    job_id: &str,
    deadline: tokio::time::Instant,
) -> Result<pb::JobResult, tonic::Status> {
    let mut delay = Duration::from_millis(200);
    loop {
        let request = pb::RecordRequest {
            job_id: job_id.to_owned(),
        };
        match within(RPC_TIMEOUT, gateway().get_record(request)).await {
            Ok(r) => return Ok(r.into_inner()),
            Err(e)
                if matches!(
                    e.code(),
                    tonic::Code::Unavailable
                        | tonic::Code::Aborted
                        | tonic::Code::DeadlineExceeded
                        | tonic::Code::Unknown
                        | tonic::Code::Cancelled
                ) && tokio::time::Instant::now() + delay < deadline =>
            {
                tracing::warn!(%job_id, "reading a record failed, retrying: {e}");
            }
            Err(e) => return Err(e),
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(Duration::from_secs(5));
    }
}

/// The error of a job whose argument `key` could not be read from `r`.
fn unreadable(key: &str, r: &BlobRef, e: &tonic::Status) -> CasError {
    let what = format!(
        "argument {key}: blob {} ({} bytes) could not be read: {}: {}",
        r.id,
        r.size,
        e.code(),
        e.message()
    );
    match e.code() {
        // The scheduler did not answer in time: the outcome may differ later.
        tonic::Code::Unavailable | tonic::Code::DeadlineExceeded => CasError::Unavailable(what),
        _ => CasError::Internal(what),
    }
}

/// The value `bytes` hold, if they are the canonical JSON of one (ADR 0021
/// §3: the side that uses a value checks its form).
fn value_of(key: &str, r: &BlobRef, bytes: &[u8]) -> Result<serde_json::Value, CasError> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| {
        CasError::InvalidArguments(format!("argument {key}: blob {} is not JSON: {e}", r.id))
    })?;
    if algebramesh_core::canonical_json(&value).as_bytes() != bytes {
        return Err(CasError::InvalidArguments(format!(
            "argument {key}: blob {} is JSON but not in canonical form; send the canonical \
             JSON of the value",
            r.id
        )));
    }
    Ok(value)
}

/// Reads `r` whole, again after failures that may pass (another leader, a
/// cut connection, a damaged copy that a scheduler fetches again), until
/// `deadline`.
async fn read_patiently(
    gateway: &(dyn Fn() -> Gateway + Sync),
    r: &BlobRef,
    deadline: tokio::time::Instant,
) -> Result<Vec<u8>, tonic::Status> {
    let mut delay = Duration::from_millis(200);
    let mut bytes = Vec::with_capacity(usize::try_from(r.size).unwrap_or(0).min(1 << 30));
    loop {
        match read_from(&mut gateway(), r, &mut bytes).await {
            Ok(()) => {
                if algebramesh_core::blob::BlobId::of(&bytes) == r.id {
                    return Ok(bytes);
                }
                // Damaged on the way: read it whole again.
                bytes.clear();
                if tokio::time::Instant::now() + delay >= deadline {
                    return Err(tonic::Status::data_loss(format!(
                        "the bytes read do not have the SHA-256 {}",
                        r.id
                    )));
                }
            }
            Err(e)
                if matches!(
                    e.code(),
                    tonic::Code::Unavailable
                        | tonic::Code::Aborted
                        | tonic::Code::DeadlineExceeded
                        | tonic::Code::Unknown
                        | tonic::Code::Cancelled
                ) && tokio::time::Instant::now() + delay < deadline =>
            {
                tracing::warn!(blob = %r.id, read = bytes.len(), "reading a blob failed, retrying: {e}");
            }
            Err(e) => return Err(e),
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(Duration::from_secs(5));
    }
}

/// Reads `r` from where `bytes` ends to its end, from one scheduler.
async fn read_from(
    gateway: &mut Gateway,
    r: &BlobRef,
    bytes: &mut Vec<u8>,
) -> Result<(), tonic::Status> {
    let request = pb::BlobRequest {
        id: r.id.to_string(),
        offset: bytes.len() as u64,
    };
    let mut stream = within(RPC_TIMEOUT, gateway.get_blob(request))
        .await?
        .into_inner();
    loop {
        let Some(chunk) = within(RPC_TIMEOUT, stream.message()).await? else {
            break;
        };
        if bytes.len() as u64 + chunk.data.len() as u64 > r.size {
            return Err(tonic::Status::data_loss(format!(
                "blob {} has more than its {} bytes",
                r.id, r.size
            )));
        }
        bytes.extend_from_slice(&chunk.data);
    }
    if bytes.len() as u64 != r.size {
        return Err(tonic::Status::unavailable(format!(
            "blob {} ended after {} of its {} bytes",
            r.id,
            bytes.len(),
            r.size
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use serde_json::json;

    #[test]
    fn only_canonical_json_is_a_value() {
        let value = json!({"b": [1, 2], "a": "x"});
        let canonical = algebramesh_core::canonical_json(&value);
        let r = algebramesh_core::blob::canonical_ref(&value);
        assert_eq!(value_of("k", &r, canonical.as_bytes()).unwrap(), value);
        let spaced = b"{\"a\": \"x\", \"b\": [1, 2]}";
        let e = value_of("k", &r, spaced).unwrap_err();
        assert!(matches!(e, CasError::InvalidArguments(m) if m.contains("canonical")));
        assert!(matches!(
            value_of("k", &r, b"{not json").unwrap_err(),
            CasError::InvalidArguments(_)
        ));
    }

    #[test]
    fn a_reference_takes_only_the_value_it_names() {
        let value = json!([1, 2]);
        let r = RecordRef {
            job_id: "j".into(),
            pointer: "/a".into(),
            sha256: algebramesh_core::blob::canonical_ref(&value).id,
        };
        let record = |state: &str, result: serde_json::Value| {
            let record = json!({
                "job_id": uuid::Uuid::new_v4(), "state": state,
                "spec": {"schema_version": 1, "backend": "test", "operation": "test.op",
                         "arguments": {}},
                "input_normalized": "", "input_sha256": "",
                "backend": algebramesh_core::BackendInfo::default(),
                "history": [], "result": result,
            });
            pb::JobResult {
                status: Some(pb::JobStatus {
                    state: state.into(),
                    ..Default::default()
                }),
                record_json: serde_json::to_string(&record).unwrap(),
            }
        };
        let good = record("SUCCEEDED", json!({"a": value, "b": 3}));
        assert_eq!(referenced_value("x", &r, &good).unwrap(), value);
        for bad in [
            record("FAILED", json!({"a": value})),
            record("SUCCEEDED", json!({"a": [1, 3]})),
            record("SUCCEEDED", json!({"b": value})),
        ] {
            let e = referenced_value("x", &r, &bad).unwrap_err();
            assert!(matches!(e, CasError::InvalidArguments(_)), "{e}");
        }
    }

    #[test]
    fn a_lost_blob_is_an_error_with_its_id() {
        let r = algebramesh_core::blob::canonical_ref(&json!([1]));
        let e = unreadable("rows", &r, &tonic::Status::data_loss("gone"));
        assert!(
            matches!(&e, CasError::Internal(m) if m.contains(r.id.as_str())),
            "{e}"
        );
        let e = unreadable("rows", &r, &tonic::Status::unavailable("busy"));
        assert!(matches!(e, CasError::Unavailable(_)));
    }
}

//! Checking a recorded result again (V4, ADR 0031), as `algebramesh verify`
//! does: the record and the bodies of the blobs it refers to come in, a
//! [`Recheck`] goes out. Nothing here reads the network or changes the
//! record; the caller fetches the bodies (ADR 0021) and hands them over.
//!
//! * The record's own hash: `input_sha256` is the SHA-256 of
//!   `input_normalized`, which is the specification of the record.
//! * Each blob: its body has the size and the SHA-256 its marker names, and
//!   is canonical JSON. A body that no scheduler holds is reported as the
//!   scheduler reported it (`DATA_LOSS`). With any of these wrong, nothing
//!   is checked.
//! * What the record claims: its guarantee, or for a record without one
//!   [`Registry::legacy_guarantee`].
//! * The check: [`Registry::check_again`] on the specification and the
//!   value, with the markers replaced by the bodies. A value whose
//!   certificate was left out for its size cannot be checked again: the
//!   answer says so, and never gives another value.

use std::collections::BTreeMap;

use algebramesh_core::blob::{self, BlobId, BlobRef, MarkerPlace};
use algebramesh_core::check::Interrupt;
use algebramesh_core::guarantee::Guarantee;
use algebramesh_core::{CasError, JobRecord, JobSpec, JobState, canonical_json, sha256_hex};
use serde::Serialize;
use serde_json::Value;

use crate::Registry;

/// What fetching the body of a blob gave.
#[derive(Debug, Clone)]
pub enum Body {
    /// The bytes, to be checked against the blob's name.
    Bytes(Vec<u8>),
    /// The scheduler answered `DATA_LOSS`: no scheduler holds the body.
    DataLoss(String),
    /// The reader found bytes whose SHA-256 is not the blob's.
    HashMismatch(String),
    /// Any other failure: the body could not be read now.
    Unreadable(String),
}

/// What a blob of the record was found to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BlobStatus {
    /// The body has the blob's size and SHA-256 and is canonical JSON.
    Ok,
    HashMismatch,
    SizeMismatch,
    NotCanonical,
    /// No scheduler holds the body.
    DataLoss,
    Unreadable,
}

/// One blob of the record and what it was found to be.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BlobCheck {
    /// Where its marker stands: `result`, `result.<key>` or
    /// `arguments.<key>`.
    pub place: String,
    pub id: String,
    pub size: u64,
    pub status: BlobStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Whether `input_sha256` is the hash of `input_normalized`, and that the
/// specification of the record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", content = "message", rename_all = "snake_case")]
pub enum InputCheck {
    Ok,
    HashMismatch(String),
    SpecMismatch(String),
}

/// What the record claims.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum Recorded {
    /// The guarantee of the record.
    Record { guarantee: Guarantee },
    /// The record has none: [`Registry::legacy_guarantee`] of its value.
    LegacyMapping { guarantee: Guarantee },
    /// The record has none, and its value fails the mapping.
    LegacyRefused { error: CasError },
    /// The record has none, and no value to map.
    Nothing { reason: String },
}

impl Recorded {
    /// The guarantee the record claims, if any.
    pub fn guarantee(&self) -> Option<&Guarantee> {
        match self {
            Self::Record { guarantee } | Self::LegacyMapping { guarantee } => Some(guarantee),
            Self::LegacyRefused { .. } | Self::Nothing { .. } => None,
        }
    }
}

/// What checking the record again gave.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Outcome {
    /// The value passed the checks again, with this guarantee.
    Rechecked { guarantee: Guarantee },
    /// The value cannot be checked again: its certificate was left out, or
    /// the job has no result.
    CannotRecheck { reason: String },
    /// The checks refuse the value.
    Refused { error: CasError },
    /// Nothing was checked: the record's hash or a blob is wrong or
    /// missing.
    NotChecked { reason: String },
}

/// The answer of `algebramesh verify` for one record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Recheck {
    pub job_id: String,
    pub operation: String,
    pub state: JobState,
    pub input: InputCheck,
    pub blobs: Vec<BlobCheck>,
    pub recorded: Recorded,
    pub recheck: Outcome,
}

impl Recheck {
    /// Whether the value passed the checks again.
    pub fn rechecked(&self) -> bool {
        matches!(self.recheck, Outcome::Rechecked { .. })
    }
}

/// Why a value whose certificate was left out cannot be checked again.
pub const CERTIFICATE_LEFT_OUT: &str = "the certificate of the value was left out when it was \
                                        recorded";

/// The blobs `record` refers to, each with where its marker stands: in its
/// result, and in the arguments of its specification (ADR 0021 §3).
pub fn blobs(record: &JobRecord) -> Result<Vec<(String, BlobRef)>, String> {
    let mut found = Vec::new();
    if let Some(result) = &record.result {
        for (place, r) in blob::markers(result).map_err(|e| format!("the result: {e}"))? {
            found.push((
                match place {
                    MarkerPlace::Whole => "result".to_owned(),
                    MarkerPlace::Key(k) => format!("result.{k}"),
                },
                r,
            ));
        }
    }
    for (key, r) in
        blob::argument_markers(&record.spec.arguments).map_err(|e| format!("the arguments: {e}"))?
    {
        found.push((format!("arguments.{key}"), r));
    }
    Ok(found)
}

fn input_check(record: &JobRecord) -> InputCheck {
    let hash = sha256_hex(record.input_normalized.as_bytes());
    if hash != record.input_sha256 {
        return InputCheck::HashMismatch(format!(
            "input_normalized has the SHA-256 {hash}, not the recorded {}",
            record.input_sha256
        ));
    }
    match JobSpec::from_json(&record.input_normalized) {
        Ok(spec) if spec == record.spec => InputCheck::Ok,
        Ok(_) => InputCheck::SpecMismatch(
            "input_normalized is not the specification of the record".into(),
        ),
        Err(e) => InputCheck::SpecMismatch(format!("input_normalized cannot be read: {e}")),
    }
}

/// `body` checked against `r`: its value, or what is wrong with it.
fn check_body(r: &BlobRef, body: Option<&Body>) -> Result<Value, (BlobStatus, String)> {
    let bytes = match body {
        Some(Body::Bytes(bytes)) => bytes,
        Some(Body::DataLoss(m)) => return Err((BlobStatus::DataLoss, m.clone())),
        Some(Body::HashMismatch(m)) => return Err((BlobStatus::HashMismatch, m.clone())),
        Some(Body::Unreadable(m)) => return Err((BlobStatus::Unreadable, m.clone())),
        None => return Err((BlobStatus::Unreadable, "the body was not fetched".into())),
    };
    if bytes.len() as u64 != r.size {
        return Err((
            BlobStatus::SizeMismatch,
            format!("the body has {} bytes, not {}", bytes.len(), r.size),
        ));
    }
    let got = BlobId::of(bytes);
    if got != r.id {
        return Err((
            BlobStatus::HashMismatch,
            format!("the body has the SHA-256 {got}"),
        ));
    }
    let value: Value = serde_json::from_slice(bytes).map_err(|e| {
        (
            BlobStatus::NotCanonical,
            format!("the body is not JSON: {e}"),
        )
    })?;
    if canonical_json(&value).as_bytes() != bytes.as_slice() {
        return Err((
            BlobStatus::NotCanonical,
            "the body is not canonical JSON".into(),
        ));
    }
    Ok(value)
}

/// Puts `part` at `place` of `value` (a place of [`blobs`]).
fn put(value: &mut Value, place: &str, part: Value) {
    match place.split_once('.') {
        None => *value = part,
        Some((_, key)) => {
            if let Value::Object(m) = value {
                m.insert(key.to_owned(), part);
            }
        }
    }
}

/// `record` checked again by `registry`, with the bodies of its blobs
/// fetched in `bodies`. Long checks stop at the checkpoints of `interrupt`.
/// The record is not changed.
pub fn recheck(
    registry: &Registry,
    record: &JobRecord,
    bodies: &BTreeMap<BlobId, Body>,
    interrupt: &Interrupt,
) -> Recheck {
    let mut answer = Recheck {
        job_id: record.job_id.to_string(),
        operation: record.spec.operation.clone(),
        state: record.state,
        input: input_check(record),
        blobs: Vec::new(),
        recorded: match &record.guarantee {
            Some(g) => Recorded::Record {
                guarantee: g.clone(),
            },
            None => Recorded::Nothing {
                reason: "the record has no guarantee, and its value was not read".into(),
            },
        },
        recheck: Outcome::NotChecked {
            reason: String::new(),
        },
    };
    let found = match blobs(record) {
        Ok(found) => found,
        Err(e) => {
            answer.recheck = Outcome::NotChecked {
                reason: format!("a marker of the record is not valid: {e}"),
            };
            return answer;
        }
    };
    let mut value = record.result.clone();
    let mut spec = record.spec.clone();
    let mut wrong = 0;
    for (place, r) in &found {
        let checked = check_body(r, bodies.get(&r.id));
        let (status, message) = match &checked {
            Ok(_) => (BlobStatus::Ok, None),
            Err((status, message)) => (*status, Some(message.clone())),
        };
        answer.blobs.push(BlobCheck {
            place: place.clone(),
            id: r.id.to_string(),
            size: r.size,
            status,
            message,
        });
        match checked {
            Ok(part) if place.starts_with("arguments.") => put(&mut spec.arguments, place, part),
            Ok(part) => {
                if let Some(value) = &mut value {
                    put(value, place, part);
                }
            }
            Err(_) => wrong += 1,
        }
    }
    if answer.input != InputCheck::Ok {
        answer.recheck = Outcome::NotChecked {
            reason: "the input of the record does not match its hash".into(),
        };
        return answer;
    }
    if wrong > 0 {
        answer.recheck = Outcome::NotChecked {
            reason: format!(
                "{wrong} of the {} blobs of the record cannot be used",
                found.len()
            ),
        };
        return answer;
    }
    let value = match value {
        Some(value) if record.state == JobState::Succeeded => value,
        _ => {
            answer.recheck = Outcome::CannotRecheck {
                reason: format!("the job is {:?}: it has no result to check", record.state),
            };
            return answer;
        }
    };
    if record.guarantee.is_none() {
        answer.recorded = match registry.legacy_guarantee(
            &record.spec.operation,
            &value,
            &record.backend.driver_version,
        ) {
            Ok(guarantee) => Recorded::LegacyMapping { guarantee },
            Err(error) => Recorded::LegacyRefused { error },
        };
    }
    if let Some(reason) = value["certificate"]["omitted"].as_str() {
        answer.recheck = Outcome::CannotRecheck {
            reason: format!(
                "{CERTIFICATE_LEFT_OUT} ({reason}); the claims it supported cannot be checked \
                 again"
            ),
        };
        return answer;
    }
    answer.recheck = match registry.check_again(&spec, &value, &record.backend, interrupt) {
        Ok(guarantee) => Outcome::Rechecked { guarantee },
        Err(error) => Outcome::Refused { error },
    };
    answer
}

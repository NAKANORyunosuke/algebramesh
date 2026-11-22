use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::job::{JobId, ResourceLimits};

/// What a backend can do, reported without guessing (design §8).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    /// Allow-listed operation names, e.g. `polynomial.factor`.
    pub operations: Vec<String>,
    /// Argument/result types the backend converts losslessly.
    pub supported_types: Vec<String>,
    /// Maximum precision for numeric work, if the backend does any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_precision: Option<u32>,
    /// The job limits (`ResourceLimits` fields, e.g. [`LIMIT_MEMORY_MB`]) this
    /// backend enforces. A job asking for a limit outside this list is refused
    /// rather than run without it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enforced_limits: Vec<String>,
    /// The signature of each operation, by its name (Issue #54, ADR 0022).
    /// An operation without one has a signature that is not known: a backend
    /// built before signatures declares none.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub signatures: BTreeMap<String, Signature>,
    /// Operations whose workers declare different signatures, in the
    /// capabilities of several workers combined (`ListBackends`); sorted. Their
    /// signatures are left out of `signatures`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signature_conflicts: Vec<String>,
}

/// What an operation takes and gives, by type name (`algebramesh-ops`, ADR
/// 0022).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signature {
    /// The type of each argument, by its key in `arguments`.
    pub arguments: BTreeMap<String, String>,
    /// The type of the result value.
    pub result: String,
}

impl Signature {
    pub fn new(arguments: &[(&str, &str)], result: &str) -> Self {
        Self {
            arguments: arguments
                .iter()
                .map(|(k, t)| ((*k).to_owned(), (*t).to_owned()))
                .collect(),
            result: result.to_owned(),
        }
    }
}

/// `limits.wall_seconds`: every backend, through the common runner.
pub const LIMIT_WALL_SECONDS: &str = "wall_seconds";
/// `limits.memory_mb`: backends that start a process per job, on Unix.
pub const LIMIT_MEMORY_MB: &str = "memory_mb";

/// The largest job result (the JSON of the outcome) a scheduler records. A
/// larger result fails the job with `RESOURCE_LIMIT` when it is reported
/// (Issue #20): every record is replicated in the Raft log and kept in the
/// metadata database, whose messages and snapshots must stay bounded.
///
/// Each record goes in one Raft log entry, which a scheduler must be able to
/// send to another within one heartbeat interval; larger results need a store
/// of their own outside the log.
pub const MAX_RESULT_BYTES: usize = 8 * 1024 * 1024;

/// The largest job specification (JSON) a scheduler accepts, for the same
/// reason.
pub const MAX_SPEC_BYTES: usize = 8 * 1024 * 1024;

impl Capabilities {
    /// Declares the signature of `operation`.
    pub fn declare(&mut self, operation: &str, arguments: &[(&str, &str)], result: &str) {
        self.signatures
            .insert(operation.to_owned(), Signature::new(arguments, result));
    }

    pub fn supports(&self, operation: &str) -> bool {
        self.operations.iter().any(|op| op == operation)
    }

    pub fn enforces(&self, limit: &str) -> bool {
        self.enforced_limits.iter().any(|l| l == limit)
    }

    /// The limits `limits` asks for that this backend does not enforce.
    pub fn unenforced(&self, limits: &crate::job::ResourceLimits) -> Vec<&'static str> {
        let mut missing = Vec::new();
        if limits.memory_mb.is_some() && !self.enforces(LIMIT_MEMORY_MB) {
            missing.push(LIMIT_MEMORY_MB);
        }
        missing
    }
}

/// Versions recorded with every result.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendInfo {
    pub name: String,
    /// Version reported by the CAS itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend_version: Option<String>,
    /// Version of the AlgebraMesh driver/adapter.
    pub driver_version: String,
    /// Image digest or source commit of the CAS, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
    /// SHA-256 of the driver files the backend loaded (feature
    /// `backend_provenance`, ADR 0020).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver_sha256: Option<String>,
    /// Commit the agent was built from, with `-modified` if its working tree
    /// was changed or that is not known (feature `backend_provenance`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_build: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CasRequest {
    pub job_id: JobId,
    pub operation: String,
    pub arguments: serde_json::Value,
    pub limits: ResourceLimits,
}

/// Per-execution context: deadline and cancellation.
///
/// The deadline covers the whole execution (ADR 0016): starting or checking out
/// the CAS, the computation, receiving and decoding the answer, and converting and
/// checking the result. It is fixed once, when the job starts, and every stage
/// uses what is left of it.
#[derive(Debug, Clone)]
pub struct ExecutionContext {
    /// The whole wall-clock budget (`limits.wall_seconds`).
    pub timeout: Option<Duration>,
    pub cancel: CancellationToken,
    /// When the budget runs out. [`ExecutionContext::new`] sets it from
    /// `timeout`; the common runner sets it when the job starts.
    pub deadline: Option<tokio::time::Instant>,
}

impl ExecutionContext {
    /// A context whose deadline is `timeout` from now.
    pub fn new(timeout: Option<Duration>) -> Self {
        Self {
            timeout,
            cancel: CancellationToken::new(),
            // A timeout too long to represent has no deadline; the common
            // runner refuses such a limit (Issue #31).
            deadline: timeout.and_then(|t| tokio::time::Instant::now().checked_add(t)),
        }
    }

    /// Completes when the deadline passes; never without one.
    pub async fn expired(&self) {
        match self.deadline {
            Some(at) => tokio::time::sleep_until(at).await,
            None => std::future::pending().await,
        }
    }

    pub fn is_expired(&self) -> bool {
        self.deadline
            .is_some_and(|at| tokio::time::Instant::now() >= at)
    }

    /// The error of a job that ran out of its budget.
    pub fn timed_out(&self) -> CasError {
        CasError::Timeout(self.timeout.unwrap_or_default().as_secs())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CasResult {
    /// Typed result in the JSON form of `algebramesh-math-types`.
    pub value: serde_json::Value,
    pub backend: BackendInfo,
    /// Where the CAS ran, e.g. the peer address of the OpenXM launcher.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_host: Option<String>,
    /// Log lines kept separate from the mathematical result.
    #[serde(default)]
    pub log: Vec<String>,
    /// What the result claims, written by the verifier after the backend
    /// (ADR 0018); one a backend writes itself is discarded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guarantee: Option<crate::guarantee::Guarantee>,
}

#[derive(Debug, Clone, Error, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "code", content = "message", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CasError {
    #[error("backend {backend} does not support operation {operation}")]
    UnsupportedOperation { backend: String, operation: String },
    #[error("invalid arguments: {0}")]
    InvalidArguments(String),
    /// The CAS evaluated the request and reported an error.
    #[error("CAS error: {0}")]
    Cas(String),
    /// Communication or protocol failure before the outcome was known.
    #[error("backend unavailable: {0}")]
    Unavailable(String),
    /// The result could not be converted to a typed value.
    #[error("unsupported result: {0}")]
    UnsupportedResult(String),
    #[error("timed out after {0} s")]
    Timeout(u64),
    /// A limit of the job (output size, memory) was exceeded; the CAS was stopped.
    #[error("resource limit exceeded: {0}")]
    ResourceLimit(String),
    #[error("cancelled")]
    Cancelled,
    /// The outcome is unknown, e.g. the connection dropped while running.
    #[error("lost: {0}")]
    Lost(String),
    /// A fault of AlgebraMesh itself while running the job (a backend that
    /// panicked), not of the CAS or the input (Issue #31).
    #[error("internal error: {0}")]
    Internal(String),
}

/// A computer algebra backend (design §8).
#[async_trait::async_trait]
pub trait CasBackend: Send + Sync {
    fn name(&self) -> &'static str;
    fn capabilities(&self) -> Capabilities;
    async fn execute(&self, job: CasRequest, ctx: ExecutionContext) -> Result<CasResult, CasError>;

    /// Starts or checks the CAS before jobs arrive and returns the version it
    /// reports, if any. Agents call this once before registering.
    async fn prepare(&self) -> Result<Option<String>, CasError> {
        Ok(None)
    }

    /// Version of the AlgebraMesh driver of this backend, as its results
    /// report it in `BackendInfo::driver_version`.
    fn driver_version(&self) -> String {
        String::new()
    }

    /// The driver files the backend has the CAS load (scripts), for
    /// `BackendInfo::driver_sha256`. None for a backend without driver files.
    fn driver_files(&self) -> Vec<std::path::PathBuf> {
        Vec::new()
    }
}

/// The SHA-256 (hex) of driver files: of each file's name and length and
/// contents, in the order of their names. None if there are none.
pub fn driver_sha256(files: &[std::path::PathBuf]) -> std::io::Result<Option<String>> {
    use sha2::{Digest, Sha256};
    if files.is_empty() {
        return Ok(None);
    }
    let mut files: Vec<&std::path::PathBuf> = files.iter().collect();
    files.sort_by_key(|p| p.file_name().map(|n| n.to_owned()));
    let mut hash = Sha256::new();
    for path in files {
        let bytes = std::fs::read(path)?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        hash.update((name.len() as u64).to_le_bytes());
        hash.update(name.as_bytes());
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(&bytes);
    }
    Ok(Some(hex::encode(hash.finalize())))
}

#[cfg(test)]
mod digest_tests {
    #![allow(clippy::unwrap_used)]

    use super::driver_sha256;

    #[test]
    fn the_driver_digest_follows_every_byte_and_not_the_order_given() {
        let dir = std::env::temp_dir().join(format!("am-driver-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("a.m2"), dir.join("b.m2"));
        std::fs::write(&a, b"-- driver a\n").unwrap();
        std::fs::write(&b, b"-- driver b\n").unwrap();
        let one = driver_sha256(&[a.clone(), b.clone()]).unwrap().unwrap();
        assert_eq!(one.len(), 64);
        assert_eq!(
            driver_sha256(&[b.clone(), a.clone()]).unwrap().unwrap(),
            one
        );
        std::fs::write(&b, b"-- driver B\n").unwrap();
        assert_ne!(
            driver_sha256(&[a.clone(), b.clone()]).unwrap().unwrap(),
            one
        );
        // Moving bytes from one file to the other changes it too.
        std::fs::write(&a, b"-- driver a\n-").unwrap();
        std::fs::write(&b, b"- driver B\n").unwrap();
        assert_ne!(
            driver_sha256(&[a.clone(), b.clone()]).unwrap().unwrap(),
            one
        );
        assert_eq!(driver_sha256(&[]).unwrap(), None);
        assert!(driver_sha256(&[dir.join("missing")]).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

/// Features of the cluster agreement (ADR 0020): new meanings that a
/// scheduler or an agent uses only once every scheduler can handle them.
pub mod features {
    /// `BackendInfo::driver_sha256` and `BackendInfo::agent_build` may be
    /// recorded.
    pub const BACKEND_PROVENANCE: &str = "backend_provenance";

    /// `JobRecord::guarantee` may be recorded (ADR 0018).
    pub const RESULT_GUARANTEE: &str = "result_guarantee";

    /// Values may be replaced by blob markers (ADR 0021): the schedulers store
    /// and replicate blobs, and agents may put large results in them.
    pub const BLOB_REFS: &str = "blob_refs";

    /// The top-level values of a job's `arguments` may be blob markers (ADR
    /// 0021, B2): the schedulers check them before a job is created and keep
    /// the blobs, and the agents that name this feature read them before the
    /// backend runs. A scheduler of B1 knows `blob_refs` but not these.
    pub const BLOB_ARGUMENTS: &str = "blob_arguments";

    /// A job's arguments may refer to values of committed jobs' results (W2,
    /// ADR 0032): markers with their origin `from`, and `{"$ref": ...}`. The
    /// schedulers check both against the producer's record before a job is
    /// created and answer `AgentGateway.GetRecord`; the agents that name this
    /// feature resolve `$ref` from that record.
    pub const WORKFLOW_REFS: &str = "workflow_refs";

    /// The features this build can use, sorted as the store lists them.
    pub const SUPPORTED: &[&str] = &[
        BACKEND_PROVENANCE,
        BLOB_ARGUMENTS,
        BLOB_REFS,
        RESULT_GUARANTEE,
        WORKFLOW_REFS,
    ];
}

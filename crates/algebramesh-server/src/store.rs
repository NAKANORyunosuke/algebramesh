//! Metadata store: SQLite in WAL mode on local disk, plus a directory of artifact
//! files (design §7.3). The full [`JobRecord`] is kept as JSON; other columns exist
//! for queries. The database must not live on a network file system shared by
//! several schedulers.
//!
//! Every write is a [`Change`], applied and appended to the `changes` table in one
//! transaction, with a sequence number. In a Raft cluster (ADR 0013) the leader's
//! scheduler writes to a working copy whose changes go to a sink
//! ([`Store::set_change_sink`]) and from there into the Raft log; every scheduler
//! applies the committed changes to its own store in order ([`Store::apply_batch`]).
//! Whole copies of the database ([`Store::snapshot`], [`Store::restore`]) start
//! the working copy and carry Raft snapshots.

use std::path::{Path, PathBuf};

use algebramesh_core::blob::{BlobId, BlobRef};
use algebramesh_core::{
    EnvironmentConstraints, JobId, JobRecord, JobSpec, JobState, ResourceLimits,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("stored record is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("artifact I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("replicated change {0} does not follow the last applied change {1}")]
    OutOfOrder(i64, i64),
    #[error("not a valid metadata database: {0}")]
    InvalidDatabase(String),
}

/// One job as stored by the scheduler.
#[derive(Debug, Clone)]
pub struct JobRow {
    pub record: JobRecord,
    pub priority: i32,
    pub cancel_requested: bool,
    pub retried_as: Option<JobId>,
    /// The client's idempotency key, for a job created by a submission that
    /// carried one. Written when the job is inserted; not read back with the row.
    pub request: Option<RequestKey>,
}

/// The filters of [`Store::list_jobs`]. `state` is a state name as in
/// `JobStatus` (`QUEUED`, ...).
#[derive(Debug, Clone, Default)]
pub struct JobFilter {
    pub request_id_prefix: String,
    pub state: Option<String>,
    pub backend: Option<String>,
    pub operation: Option<String>,
}

impl JobFilter {
    fn passes(&self, job: &ListedJob) -> bool {
        let eq = |want: &Option<String>, have: &str| want.as_deref().is_none_or(|w| w == have);
        eq(&self.state, &job.state)
            && eq(&self.backend, &job.backend)
            && eq(&self.operation, &job.operation)
    }
}

/// Where a listing goes on: after this request id (a listing by prefix) or
/// after this row (in the order of submission).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListPosition {
    Request(String),
    Seq(i64),
}

/// A job as [`Store::list_jobs`] lists it: the columns beside the record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedJob {
    pub seq: i64,
    pub job_id: String,
    pub request_id: Option<String>,
    pub state: String,
    pub backend: String,
    pub operation: String,
    pub agent_id: Option<String>,
    pub retried_as: Option<String>,
    pub cancel_requested: bool,
}

/// The columns of [`ListedJob`] but the request id, which comes after the
/// record in the row; these come before it.
const LISTED_COLUMNS: &str =
    "seq, job_id, state, backend, operation, agent_id, retried_as, cancel_requested";

/// A job by its row, without its request id.
const LISTED_BY_SEQ: &str = "SELECT seq, job_id, state, backend, operation, agent_id, \
     retried_as, cancel_requested FROM jobs WHERE seq = ?1";

/// The request id of a row, from the index alone (it comes after the record
/// in the row).
const REQUEST_BY_SEQ: &str = "SELECT request_id FROM jobs INDEXED BY jobs_request_by_seq \
     WHERE seq = ?1 AND request_id IS NOT NULL";

/// The request ids from `?1` on (after it when `strict`) and their rows, from
/// the index alone.
fn keys_by_prefix(strict: bool) -> String {
    let op = if strict { ">" } else { ">=" };
    format!(
        "SELECT request_id, seq FROM jobs INDEXED BY jobs_request \
         WHERE request_id {op} ?1 ORDER BY request_id LIMIT ?2"
    )
}

/// The jobs after row `?1` in the order of submission, at most `?2`, through
/// the index of one filter (its value `?3`), if `filter` has one besides a
/// prefix.
fn scan_by_seq(filter: &JobFilter) -> (String, Option<&String>) {
    let (index, column, value) = if let Some(state) = &filter.state {
        ("INDEXED BY jobs_by_state", "state = ?3 AND", Some(state))
    } else if let Some(operation) = &filter.operation {
        (
            "INDEXED BY jobs_by_operation",
            "operation = ?3 AND",
            Some(operation),
        )
    } else if let Some(backend) = &filter.backend {
        (
            "INDEXED BY jobs_by_backend",
            "backend = ?3 AND",
            Some(backend),
        )
    } else {
        ("", "", None)
    };
    let sql = format!(
        "SELECT {LISTED_COLUMNS} FROM jobs {index} WHERE {column} seq > ?1 ORDER BY seq LIMIT ?2"
    );
    (sql, value)
}

/// One call of [`Store::list_jobs`]: the page so far and the jobs looked at.
struct Listing<'a> {
    filter: &'a JobFilter,
    limits: &'a ListLimits,
    page: JobPage,
    bytes: usize,
    scanned: usize,
    /// The bytes of every job looked at, on the page or not (Issue #76).
    scanned_bytes: usize,
    full: bool,
    /// The position of the last job looked at.
    last: Option<ListPosition>,
}

impl<'a> Listing<'a> {
    fn new(filter: &'a JobFilter, limits: &'a ListLimits) -> Self {
        Self {
            filter,
            limits,
            page: JobPage::default(),
            bytes: 0,
            scanned: 0,
            scanned_bytes: 0,
            full: false,
            last: None,
        }
    }

    /// Whether `job` goes on the page.
    fn takes(&self, job: &ListedJob) -> bool {
        !self.full && self.filter.passes(job)
    }

    /// Looks at `job`, at `position`. True when the listing stops, and then
    /// the next page begins after the last job this one looked at and did not
    /// leave:
    /// - `job` passes the filters but the page is full, or would pass
    ///   `max_bytes` with it: `job` is left for the next page. A page always
    ///   takes its first job, so that each page makes progress.
    /// - The jobs looked at, `job` included, have `max_scanned_bytes` or more:
    ///   a job whose metadata are large costs its bytes even when the filters
    ///   reject it.
    fn look(&mut self, job: ListedJob, position: ListPosition) -> bool {
        self.scanned += 1;
        let size = job.size();
        if self.filter.passes(&job) {
            let too_large = !self.page.jobs.is_empty()
                && self.bytes.saturating_add(size) > self.limits.max_bytes;
            if self.full || too_large {
                self.page.next = self.last.take();
                return true;
            }
            self.bytes += size;
            self.page.jobs.push(job);
            self.full = self.page.jobs.len() >= self.limits.page_size
                || self.bytes >= self.limits.max_bytes;
        }
        self.last = Some(position);
        self.scanned_bytes = self.scanned_bytes.saturating_add(size);
        if self.scanned_bytes >= self.limits.max_scanned_bytes {
            self.page.next = self.last.take();
            return true;
        }
        false
    }

    /// The page once the jobs in range are all looked at (`exhausted`), or
    /// the query ended: at its bound, more jobs may follow.
    fn finish(mut self, exhausted: bool, max_scanned: usize) -> JobPage {
        if !exhausted && self.scanned >= max_scanned {
            self.page.next = self.last.take();
        }
        self.page
    }
}

fn listed_job(r: &rusqlite::Row<'_>) -> rusqlite::Result<ListedJob> {
    Ok(ListedJob {
        seq: r.get(0)?,
        job_id: r.get(1)?,
        request_id: None,
        state: r.get(2)?,
        backend: r.get(3)?,
        operation: r.get(4)?,
        agent_id: r.get(5)?,
        retried_as: r.get(6)?,
        cancel_requested: r.get(7)?,
    })
}

/// A job as the store keeps it, with its record as JSON text, read from the
/// database but not deserialized yet (Issue #84).
#[derive(Debug, Clone)]
pub struct StoredJob {
    record_json: String,
    priority: i32,
    cancel_requested: bool,
    retried_as: Option<String>,
}

impl StoredJob {
    pub fn parse(self) -> Result<JobRow, StoreError> {
        Store::row_from(
            self.record_json,
            self.priority,
            self.cancel_requested,
            self.retried_as,
        )
    }
}

impl ListedJob {
    /// The bytes the job takes in a listing, about.
    fn size(&self) -> usize {
        let opt = |s: &Option<String>| s.as_ref().map_or(0, String::len);
        32 + self.job_id.len()
            + opt(&self.request_id)
            + self.state.len()
            + self.backend.len()
            + self.operation.len()
            + opt(&self.agent_id)
            + opt(&self.retried_as)
    }
}

/// The bounds of one page of [`Store::list_jobs`]: the jobs it holds, their
/// bytes (only a page of one job may pass the bound), and the jobs looked at
/// to find them, in number and in bytes. A job is read before its bytes are
/// counted, so a page reads at most `max_scanned_bytes` and one job more.
#[derive(Debug, Clone, Copy)]
pub struct ListLimits {
    pub page_size: usize,
    pub max_bytes: usize,
    pub max_scanned: usize,
    pub max_scanned_bytes: usize,
}

/// A page of [`Store::list_jobs`]: the jobs, and where to go on if more may
/// pass the filters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobPage {
    pub jobs: Vec<ListedJob>,
    pub next: Option<ListPosition>,
}

impl JobRow {
    pub fn new(record: JobRecord, priority: i32) -> Self {
        Self {
            record,
            priority,
            cancel_requested: false,
            retried_as: None,
            request: None,
        }
    }
}

/// A client-generated submission key and the fingerprint of what it submitted.
/// Kept as long as the job (jobs are not deleted).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestKey {
    pub request_id: String,
    pub fingerprint: String,
}

pub struct Store {
    conn: Connection,
    path: Option<PathBuf>,
    /// Whether `path` is a temporary file that [`Self::restore`] made for a
    /// store opened in memory: removed when the store is dropped.
    temporary: bool,
    artifacts: Option<PathBuf>,
    /// Receives every committed change (sequence number, JSON).
    sink: Option<ChangeSink>,
    /// Terminal jobs whose `record.json` could not be written (Issue #21), and
    /// when that was last tried. The database is the record; artifacts are
    /// written again from it.
    pending_artifacts: std::sync::Mutex<(
        std::collections::BTreeSet<String>,
        Option<std::time::Instant>,
    )>,
}

/// How often writes try again the artifacts that could not be written.
const ARTIFACT_RETRY: std::time::Duration = std::time::Duration::from_secs(5);

/// Where a store sends the changes it commits.
pub type ChangeSink = tokio::sync::mpsc::UnboundedSender<(i64, String)>;

/// A job row as carried in a [`Change`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredRow {
    pub record: JobRecord,
    pub priority: i32,
    pub cancel_requested: bool,
    pub retried_as: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<RequestKey>,
}

impl From<&JobRow> for StoredRow {
    fn from(row: &JobRow) -> Self {
        Self {
            record: row.record.clone(),
            priority: row.priority,
            cancel_requested: row.cancel_requested,
            retried_as: row.retried_as.map(|j| j.to_string()),
            request: row.request.clone(),
        }
    }
}

/// One write to the store, as replicated to the other schedulers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Change {
    InsertJob {
        row: StoredRow,
    },
    UpdateJob {
        row: StoredRow,
    },
    Event {
        job_id: String,
        at: String,
        note: String,
    },
    Agent {
        agent_id: String,
        name: String,
        host: String,
        registered_at: String,
        info_json: String,
    },
    /// Features every scheduler of the cluster can handle, agreed on by the
    /// leader (ADR 0020); added to those already enabled. Also the oldest
    /// version a scheduler may have to start on this state.
    EnableFeatures {
        features: Vec<String>,
        min_binary_version: String,
    },
    /// Several changes that belong together, committed and replicated as one
    /// (one sequence number, one transaction, one Raft entry): a scheduler
    /// that stops never leaves a part of them (Issue #27).
    All {
        changes: Vec<Change>,
    },
}

/// What placement needs of a job (Issue #19), kept in its own small column so
/// that choosing a job for a worker does not read the records of the others.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Placement {
    pub backend: String,
    pub operation: String,
    #[serde(default)]
    pub limits: ResourceLimits,
    #[serde(default)]
    pub environment: EnvironmentConstraints,
    /// The arguments hold blob markers: only an agent that reads them (the
    /// feature `blob_arguments`, ADR 0021 §12) may run the job.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub needs_blob_refs: bool,
    /// The arguments hold references `{"$ref": ...}` to values of other
    /// jobs' records: only an agent that resolves them (the feature
    /// `workflow_refs`, W2) may run the job.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub needs_record_refs: bool,
}

impl Placement {
    pub fn of(spec: &JobSpec) -> Self {
        Self {
            backend: spec.backend.clone(),
            operation: spec.operation.clone(),
            limits: spec.limits.clone(),
            environment: spec.environment.clone(),
            needs_blob_refs: algebramesh_core::blob::argument_markers(&spec.arguments)
                .is_ok_and(|m| !m.is_empty()),
            needs_record_refs: algebramesh_core::blob::argument_refs(&spec.arguments).is_ok_and(
                |m| {
                    m.iter()
                        .any(|(_, r)| matches!(r, algebramesh_core::blob::ArgumentRef::Record(_)))
                },
            ),
        }
    }
}

/// The columns of a job a heartbeat needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobBrief {
    pub agent_id: Option<String>,
    pub state: JobState,
    pub cancel_requested: bool,
}

/// The longest event note kept, in bytes.
pub const MAX_EVENT_BYTES: usize = 4096;

fn bounded_note(note: &str) -> String {
    if note.len() <= MAX_EVENT_BYTES {
        return note.to_owned();
    }
    let mut end = MAX_EVENT_BYTES;
    while !note.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}... ({} bytes in all)", &note[..end], note.len())
}

/// Changes kept in the `changes` table; older ones are deleted.
const KEPT_CHANGES: i64 = 100_000;

fn state_name(state: JobState) -> String {
    serde_json::to_value(state)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS jobs (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    job_id TEXT NOT NULL UNIQUE,
    state TEXT NOT NULL,
    backend TEXT NOT NULL,
    operation TEXT NOT NULL,
    priority INTEGER NOT NULL,
    agent_id TEXT,
    cancel_requested INTEGER NOT NULL DEFAULT 0,
    retried_as TEXT,
    record_json TEXT NOT NULL,
    request_id TEXT,
    request_fingerprint TEXT,
    placement TEXT
);
CREATE INDEX IF NOT EXISTS jobs_state ON jobs(state, priority DESC, seq);
CREATE TABLE IF NOT EXISTS events (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    job_id TEXT NOT NULL,
    at TEXT NOT NULL,
    note TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS agents (
    agent_id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    host TEXT NOT NULL,
    registered_at TEXT NOT NULL,
    info_json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS changes (
    seq INTEGER PRIMARY KEY,
    change_json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS blobs (
    sha256 TEXT PRIMARY KEY,
    size INTEGER NOT NULL,
    first_seq INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS blob_refs (
    sha256 TEXT NOT NULL,
    job_id TEXT NOT NULL,
    role TEXT NOT NULL,
    PRIMARY KEY (job_id, role, sha256)
);
CREATE INDEX IF NOT EXISTS blob_refs_by_blob ON blob_refs(sha256);
";

/// `meta` key of the version of the tables derived from the records
/// (`blobs`, `blob_refs`, ADR 0021 §7), and that version. A store whose
/// tables have another version has them rebuilt when it is opened.
const BLOB_TABLES_VERSION: &str = "blob_tables_version";
const BLOB_TABLES_CURRENT: &str = "2";
/// The role of a blob in a job: its result (B1) or one of its arguments (B2).
const ROLE_RESULT: &str = "result";
const ROLE_ARGUMENT: &str = "arguments";

/// Records in `blobs` and `blob_refs` the blobs that `record` refers to,
/// replacing what was recorded for the job before: those of its result, and,
/// with `arguments` (the feature `blob_arguments` is enabled), those of its
/// arguments.
fn derive_blob_refs(
    conn: &Connection,
    record: &JobRecord,
    arguments: bool,
) -> Result<(), StoreError> {
    let job_id = record.job_id.to_string();
    conn.execute("DELETE FROM blob_refs WHERE job_id = ?1", params![job_id])?;
    // A scheduler accepts no marker of a malformed shape; a value recorded
    // before the cluster used blobs refers to none.
    let mut found: Vec<(BlobRef, &str)> = Vec::new();
    if let Some(result) = &record.result
        && let Ok(m) = algebramesh_core::blob::markers(result)
    {
        found.extend(m.into_iter().map(|(_, r)| (r, ROLE_RESULT)));
    }
    if arguments && let Ok(m) = algebramesh_core::blob::argument_markers(&record.spec.arguments) {
        found.extend(m.into_iter().map(|(_, r)| (r, ROLE_ARGUMENT)));
    }
    for (r, role) in found {
        conn.execute(
            "INSERT OR IGNORE INTO blobs (sha256, size, first_seq)
             VALUES (?1, ?2, (SELECT COALESCE(MAX(seq), 0) + 1 FROM changes))",
            params![r.id.as_str(), r.size as i64],
        )?;
        conn.execute(
            "INSERT OR IGNORE INTO blob_refs (sha256, job_id, role) VALUES (?1, ?2, ?3)",
            params![r.id.as_str(), job_id, role],
        )?;
    }
    Ok(())
}

/// Whether the cluster has enabled `feature`, as `conn` records.
fn uses(conn: &Connection, feature: &str) -> Result<bool, StoreError> {
    let enabled: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = ?1",
            params![ENABLED_FEATURES],
            |r| r.get(0),
        )
        .optional()?;
    Ok(enabled
        .and_then(|v| serde_json::from_str::<Vec<String>>(&v).ok())
        .is_some_and(|f| f.iter().any(|f| f == feature)))
}

/// The derivation of the blob tables the enabled features ask for: none,
/// results, or results and arguments.
fn blob_derivation(conn: &Connection) -> Result<Option<bool>, StoreError> {
    if !uses(conn, algebramesh_core::features::BLOB_REFS)? {
        return Ok(None);
    }
    Ok(Some(uses(
        conn,
        algebramesh_core::features::BLOB_ARGUMENTS,
    )?))
}

/// Rebuilds `blobs` and `blob_refs` from the records if they are of another
/// version than this build's (ADR 0021 §7). Only then: a scan of every record
/// on each opening would slow down every election and snapshot.
fn rebuild_blob_tables(conn: &Connection) -> Result<(), StoreError> {
    let version: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = ?1",
            params![BLOB_TABLES_VERSION],
            |r| r.get(0),
        )
        .optional()?;
    if version.as_deref() == Some(BLOB_TABLES_CURRENT) {
        return Ok(());
    }
    conn.execute_batch("DELETE FROM blob_refs; DELETE FROM blobs;")?;
    if let Some(arguments) = blob_derivation(conn)? {
        let records: Vec<String> = conn
            .prepare("SELECT record_json FROM jobs WHERE instr(record_json, '\"$blob\"') > 0")?
            .query_map([], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        for json in records {
            let record: JobRecord = serde_json::from_str(&json)?;
            derive_blob_refs(conn, &record, arguments)?;
        }
    }
    conn.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
        params![BLOB_TABLES_VERSION, BLOB_TABLES_CURRENT],
    )?;
    Ok(())
}

/// Checks that the file at `path` is a SQLite database with the tables of a
/// metadata store, without changing it (Issue #28).
fn validate_database(path: &Path) -> Result<(), StoreError> {
    let invalid = |e: rusqlite::Error| StoreError::InvalidDatabase(e.to_string());
    let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(invalid)?;
    let check: String = conn
        .query_row("PRAGMA quick_check", [], |r| r.get(0))
        .map_err(invalid)?;
    if check != "ok" {
        return Err(StoreError::InvalidDatabase(format!("quick check: {check}")));
    }
    for table in ["jobs", "events", "agents", "changes", "meta"] {
        let found: bool = conn
            .prepare("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1")
            .and_then(|mut s| s.exists([table]))
            .map_err(invalid)?;
        if !found {
            return Err(StoreError::InvalidDatabase(format!("no table {table}")));
        }
    }
    Ok(())
}

/// Makes a rename in the directory of `path` durable (Unix; elsewhere the
/// rename is left to the file system).
pub(crate) fn sync_directory(path: &Path) {
    #[cfg(unix)]
    if let Some(dir) = path.parent()
        && let Err(e) = std::fs::File::open(dir).and_then(|d| d.sync_all())
    {
        tracing::warn!(dir = %dir.display(), "cannot sync the directory: {e}");
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// `meta` keys of the feature agreement (ADR 0020).
const ENABLED_FEATURES: &str = "enabled_features";
const MIN_BINARY_VERSION: &str = "min_binary_version";

/// Whether version `a` is older than `b`, comparing the numbers of
/// `major.minor.patch` (a suffix such as `-rc1` is ignored).
pub fn version_older(a: &str, b: &str) -> bool {
    let parse = |v: &str| -> Vec<u64> {
        v.split(['-', '+'])
            .next()
            .unwrap_or("")
            .split('.')
            .map(|p| p.parse().unwrap_or(0))
            .collect()
    };
    parse(a) < parse(b)
}

/// The error of a database that could not be opened again after a restore: the
/// store has no database until the scheduler is started again.
fn reopen_failed(path: &Path, e: &StoreError) -> StoreError {
    StoreError::InvalidDatabase(format!(
        "{} could not be opened again ({e}); start the scheduler again",
        path.display()
    ))
}

/// Removes the temporary files that a scheduler stopped in the middle of a
/// snapshot or a restore leaves in its data directory (Issue #37): copies made
/// by `VACUUM INTO` (`snapshot-<uuid>.sqlite3`) and staged databases
/// (`*.restore`). Called before the stores of `dir` are opened; the database
/// they belong to is whole without them.
pub fn remove_stale_files(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let stale = (name.starts_with("snapshot-") && name.ends_with(".sqlite3"))
            || name.ends_with(".restore");
        if stale && entry.path().is_file() {
            match std::fs::remove_file(entry.path()) {
                Ok(()) => {
                    tracing::info!(file = %name, "removed a file left by an interrupted snapshot or restore")
                }
                Err(e) => tracing::warn!(file = %name, "cannot remove a stale file: {e}"),
            }
        }
    }
}

/// Opens the database in WAL mode with `synchronous = FULL` (Issue #23): a
/// committed transaction survives a power loss or an OS crash, not only a
/// crash of the process. In a cluster this store is the Raft state machine;
/// Raft drops the log up to a snapshot of it, so its applied changes and the
/// applied log position must be as durable as the log. The leader's working
/// copy, made again in every term, is relaxed with [`Store::relax_durability`].
fn open_connection(path: Option<&Path>) -> Result<Connection, StoreError> {
    let conn = match path {
        Some(p) => {
            if let Some(dir) = p.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let conn = Connection::open(p)?;
            conn.pragma_update(None, "journal_mode", "WAL")?;
            conn.pragma_update(None, "synchronous", "FULL")?;
            conn
        }
        None => Connection::open_in_memory()?,
    };
    conn.execute_batch(SCHEMA)?;
    migrate(&conn)?;
    Ok(conn)
}

/// Brings a database of an earlier version up to [`SCHEMA`].
/// Columns added to `jobs` after its first version, with their types.
const ADDED_JOB_COLUMNS: [(&str, &str); 3] = [
    ("request_id", "TEXT"),
    ("request_fingerprint", "TEXT"),
    ("placement", "TEXT"),
];

/// Brings an older database up to the current schema, in one transaction
/// (Issue #33): each added column is checked on its own and added if it is
/// missing, the placements are filled, and the indexes are made. A migration
/// interrupted in the middle left nothing (SQLite's `ALTER TABLE` is
/// transactional); one that a scheduler from before this check left half done
/// (one column of two) is completed.
fn migrate(conn: &Connection) -> Result<(), StoreError> {
    let tx = conn.unchecked_transaction()?;
    for (column, kind) in ADDED_JOB_COLUMNS {
        let present: bool = tx
            .prepare("SELECT 1 FROM pragma_table_info('jobs') WHERE name = ?1")?
            .exists([column])?;
        if !present {
            tx.execute_batch(&format!("ALTER TABLE jobs ADD COLUMN {column} {kind};"))?;
        }
    }
    // Fill the placement of jobs written before the column existed (and of a
    // database restored from such a scheduler).
    let missing: Vec<(String, String)> = tx
        .prepare("SELECT job_id, record_json FROM jobs WHERE placement IS NULL")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    for (job_id, json) in missing {
        let record: JobRecord = serde_json::from_str(&json)?;
        tx.execute(
            "UPDATE jobs SET placement = ?2 WHERE job_id = ?1",
            params![job_id, serde_json::to_string(&Placement::of(&record.spec))?],
        )?;
    }
    tx.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS jobs_request ON jobs(request_id)
         WHERE request_id IS NOT NULL;
         CREATE INDEX IF NOT EXISTS jobs_agent ON jobs(agent_id, state);
         CREATE INDEX IF NOT EXISTS jobs_queue
             ON jobs(state, backend, priority DESC, seq, job_id, placement);
         CREATE INDEX IF NOT EXISTS jobs_by_state ON jobs(state, seq);
         CREATE INDEX IF NOT EXISTS jobs_by_backend ON jobs(backend, seq);
         CREATE INDEX IF NOT EXISTS jobs_by_operation ON jobs(operation, seq);
         CREATE INDEX IF NOT EXISTS jobs_request_by_seq ON jobs(seq, request_id)
             WHERE request_id IS NOT NULL;",
    )?;
    rebuild_blob_tables(&tx)?;
    tx.commit()?;
    Ok(())
}

impl Drop for Store {
    /// Removes the temporary file of a store opened in memory, with its WAL
    /// and shared memory, once the connection to it is closed: a file left
    /// behind would outlive the process (Windows does not remove an open
    /// file, so the connection is closed first).
    fn drop(&mut self) {
        if !self.temporary {
            return;
        }
        let Some(path) = self.path.take() else {
            return;
        };
        if let Ok(memory) = Connection::open_in_memory() {
            drop(std::mem::replace(&mut self.conn, memory));
        }
        for suffix in ["", "-wal", "-shm"] {
            let mut name = path.clone().into_os_string();
            name.push(suffix);
            let _ = std::fs::remove_file(PathBuf::from(name));
        }
    }
}

impl Store {
    /// Opens (or creates) the database. `None` keeps it in memory, for tests.
    pub fn open(path: Option<&Path>, artifacts: Option<PathBuf>) -> Result<Self, StoreError> {
        let conn = open_connection(path)?;
        // Artifacts are written again later if the directory cannot be made
        // now (Issue #21); the database alone decides whether the store opens.
        if let Some(dir) = &artifacts
            && let Err(e) = std::fs::create_dir_all(dir)
        {
            tracing::error!(dir = %dir.display(), "cannot create the artifact directory: {e}");
        }
        let store = Self {
            conn,
            path: path.map(Path::to_path_buf),
            temporary: false,
            artifacts,
            sink: None,
            pending_artifacts: std::sync::Mutex::default(),
        };
        store.find_missing_artifacts()?;
        store.write_pending_artifacts();
        Ok(store)
    }

    /// The database file, unless the store is in memory.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Commits with `synchronous = NORMAL`: durable across a crash of the
    /// process, not of the machine. For the leader's working copy, which is
    /// rebuilt from the state machine whenever the node leads.
    pub fn relax_durability(&self) -> Result<(), StoreError> {
        self.conn.pragma_update(None, "synchronous", "NORMAL")?;
        Ok(())
    }

    /// Sends every change committed from now on to `sink`, in order.
    pub fn set_change_sink(&mut self, sink: Option<ChangeSink>) {
        self.sink = sink;
    }

    pub fn insert_job(&self, row: &JobRow) -> Result<(), StoreError> {
        self.commit(&Change::InsertJob { row: row.into() })
    }

    /// Writes the row back; terminal records are also written as artifacts.
    pub fn update_job(&self, row: &JobRow) -> Result<(), StoreError> {
        self.commit(&Change::UpdateJob { row: row.into() })
    }

    /// Records an event of a job. A note longer than [`MAX_EVENT_BYTES`] is cut:
    /// events are replicated and kept with the job, so their size must not
    /// follow what a client or an agent sends (Issue #22).
    pub fn add_event(&self, job_id: &JobId, note: &str) -> Result<(), StoreError> {
        self.commit(&Self::event(job_id, note))
    }

    /// An event as a change, for [`Self::commit_all`].
    pub fn event(job_id: &JobId, note: &str) -> Change {
        Change::Event {
            job_id: job_id.to_string(),
            at: chrono::Utc::now().to_rfc3339(),
            note: bounded_note(note),
        }
    }

    /// Commits `changes` as one: all of them or none, under one sequence
    /// number (Issue #27).
    pub fn commit_all(&self, changes: Vec<Change>) -> Result<(), StoreError> {
        self.commit(&Change::All { changes })
    }

    /// Enables `features` for the cluster (ADR 0020).
    pub fn enable_features(
        &self,
        features: &[String],
        min_binary_version: &str,
    ) -> Result<(), StoreError> {
        self.commit(&Change::EnableFeatures {
            features: features.to_vec(),
            min_binary_version: min_binary_version.to_owned(),
        })
    }

    /// The features the cluster has enabled, sorted.
    pub fn enabled_features(&self) -> Result<Vec<String>, StoreError> {
        Ok(self
            .meta(ENABLED_FEATURES)?
            .and_then(|v| serde_json::from_str(&v).ok())
            .unwrap_or_default())
    }

    /// The oldest version a scheduler may have to start on this store, if any
    /// feature was enabled.
    pub fn min_binary_version(&self) -> Result<Option<String>, StoreError> {
        self.meta(MIN_BINARY_VERSION)
    }

    pub fn record_agent(
        &self,
        agent_id: &str,
        name: &str,
        host: &str,
        info_json: &str,
    ) -> Result<(), StoreError> {
        self.commit(&Change::Agent {
            agent_id: agent_id.to_owned(),
            name: name.to_owned(),
            host: host.to_owned(),
            registered_at: chrono::Utc::now().to_rfc3339(),
            info_json: info_json.to_owned(),
        })
    }

    /// Applies `change` and appends it to the change log, in one transaction.
    fn commit(&self, change: &Change) -> Result<(), StoreError> {
        let json = serde_json::to_string(change)?;
        let tx = self.conn.unchecked_transaction()?;
        self.apply(change)?;
        let seq = self.last_seq()? + 1;
        tx.execute(
            "INSERT INTO changes (seq, change_json) VALUES (?1, ?2)",
            params![seq, json],
        )?;
        self.prune(seq)?;
        tx.commit()?;
        if let Some(sink) = &self.sink {
            // A closed sink means that the working copy is being dropped.
            let _ = sink.send((seq, json));
        }
        // The change is committed: what follows cannot make it fail.
        self.after_apply(change);
        Ok(())
    }

    fn prune(&self, seq: i64) -> Result<(), StoreError> {
        if seq % 1000 == 0 {
            self.conn.execute(
                "DELETE FROM changes WHERE seq <= ?1",
                params![seq - KEPT_CHANGES],
            )?;
        }
        Ok(())
    }

    fn apply(&self, change: &Change) -> Result<(), StoreError> {
        match change {
            Change::InsertJob { row } => {
                self.conn.execute(
                    "INSERT INTO jobs (job_id, state, backend, operation, priority, agent_id,
                                       cancel_requested, retried_as, record_json,
                                       request_id, request_fingerprint, placement)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                    params![
                        row.record.job_id.to_string(),
                        state_name(row.record.state),
                        row.record.spec.backend,
                        row.record.spec.operation,
                        row.priority,
                        row.record.agent_id,
                        row.cancel_requested,
                        row.retried_as,
                        serde_json::to_string(&row.record)?,
                        row.request.as_ref().map(|k| &k.request_id),
                        row.request.as_ref().map(|k| &k.fingerprint),
                        serde_json::to_string(&Placement::of(&row.record.spec))?,
                    ],
                )?;
                if let Some(arguments) = blob_derivation(&self.conn)? {
                    derive_blob_refs(&self.conn, &row.record, arguments)?;
                }
            }
            Change::UpdateJob { row } => {
                self.conn.execute(
                    "UPDATE jobs SET state = ?2, agent_id = ?3, cancel_requested = ?4,
                                     retried_as = ?5, record_json = ?6
                     WHERE job_id = ?1",
                    params![
                        row.record.job_id.to_string(),
                        state_name(row.record.state),
                        row.record.agent_id,
                        row.cancel_requested,
                        row.retried_as,
                        serde_json::to_string(&row.record)?,
                    ],
                )?;
                if let Some(arguments) = blob_derivation(&self.conn)? {
                    derive_blob_refs(&self.conn, &row.record, arguments)?;
                }
            }
            Change::Event { job_id, at, note } => {
                self.conn.execute(
                    "INSERT INTO events (job_id, at, note) VALUES (?1, ?2, ?3)",
                    params![job_id, at, note],
                )?;
            }
            Change::Agent {
                agent_id,
                name,
                host,
                registered_at,
                info_json,
            } => {
                self.conn.execute(
                    "INSERT OR REPLACE INTO agents (agent_id, name, host, registered_at, info_json)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![agent_id, name, host, registered_at, info_json],
                )?;
            }
            Change::EnableFeatures {
                features,
                min_binary_version,
            } => {
                let mut enabled = self.enabled_features()?;
                for f in features {
                    if !enabled.contains(f) {
                        enabled.push(f.clone());
                    }
                }
                enabled.sort();
                self.set_meta(ENABLED_FEATURES, &serde_json::to_string(&enabled)?)?;
                let older = self
                    .min_binary_version()?
                    .is_none_or(|v| version_older(&v, min_binary_version));
                if older {
                    self.set_meta(MIN_BINARY_VERSION, min_binary_version)?;
                }
            }
            Change::All { changes } => {
                for change in changes {
                    self.apply(change)?;
                }
            }
        }
        Ok(())
    }

    /// File effects of a committed change: artifacts of terminal records.
    /// Writes the artifact of a job that became terminal. A failure (a full or
    /// read-only disk) does not undo or fail the committed change (Issue #21):
    /// it is logged, and the artifact is written again later from the
    /// database ([`Self::write_pending_artifacts`]).
    fn after_apply(&self, change: &Change) {
        match change {
            Change::UpdateJob { row } if row.record.state.is_terminal() => {
                self.write_or_defer(&row.record);
            }
            Change::All { changes } => {
                for change in changes {
                    if let Change::UpdateJob { row } = change
                        && row.record.state.is_terminal()
                    {
                        self.write_or_defer(&row.record);
                    }
                }
            }
            _ => {}
        }
        let due = {
            let pending = self.lock_pending();
            !pending.0.is_empty() && pending.1.is_none_or(|t| t.elapsed() >= ARTIFACT_RETRY)
        };
        if due {
            self.write_pending_artifacts();
        }
    }

    fn lock_pending(
        &self,
    ) -> std::sync::MutexGuard<
        '_,
        (
            std::collections::BTreeSet<String>,
            Option<std::time::Instant>,
        ),
    > {
        self.pending_artifacts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    fn write_or_defer(&self, record: &JobRecord) {
        if let Err(e) = self.write_artifact(record) {
            tracing::error!(job_id = %record.job_id, "cannot write the job artifact; will try again: {e}");
            let mut pending = self.lock_pending();
            pending.0.insert(record.job_id.to_string());
            pending.1 = Some(std::time::Instant::now());
        }
    }

    /// Writes again, from the database, the artifacts that could not be
    /// written. Returns how many are still missing.
    pub fn write_pending_artifacts(&self) -> usize {
        let ids: Vec<String> = {
            let mut pending = self.lock_pending();
            pending.1 = Some(std::time::Instant::now());
            pending.0.iter().cloned().collect()
        };
        for id in ids {
            let written = match self.get_job(&id) {
                Ok(Some(row)) => self.write_artifact(&row.record).is_ok(),
                // The job is gone (a restored database): nothing to write.
                Ok(None) => true,
                Err(_) => false,
            };
            if written {
                self.lock_pending().0.remove(&id);
            }
        }
        self.lock_pending().0.len()
    }

    /// Sequence number of the last change, 0 if there is none.
    pub fn last_seq(&self) -> Result<i64, StoreError> {
        Ok(self
            .conn
            .query_row("SELECT COALESCE(MAX(seq), 0) FROM changes", [], |r| {
                r.get(0)
            })?)
    }

    /// Applies a change written by another store. Changes must come in order.
    pub fn apply_replicated(&self, seq: i64, change_json: &str) -> Result<(), StoreError> {
        self.apply_batch(&[(seq, change_json.to_owned())], &[])
    }

    /// Applies changes written by another store, in order, and sets `meta` keys, in
    /// one transaction. Each change must follow the last one applied.
    pub fn apply_batch(
        &self,
        changes: &[(i64, String)],
        meta: &[(&str, String)],
    ) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        let mut last = self.last_seq()?;
        let mut applied = Vec::with_capacity(changes.len());
        for (seq, json) in changes {
            if *seq != last + 1 {
                return Err(StoreError::OutOfOrder(*seq, last));
            }
            let change: Change = serde_json::from_str(json)?;
            self.apply(&change)?;
            tx.execute(
                "INSERT INTO changes (seq, change_json) VALUES (?1, ?2)",
                params![seq, json],
            )?;
            self.prune(*seq)?;
            last = *seq;
            applied.push(change);
        }
        for (key, value) in meta {
            self.set_meta(key, value)?;
        }
        tx.commit()?;
        // Committed with the applied log position: an artifact that cannot be
        // written must not fail the state machine (Issue #21).
        for change in &applied {
            self.after_apply(change);
        }
        Ok(())
    }

    /// Those of `ids` that a job record refers to (ADR 0021 §7).
    pub fn registered_blobs(&self, ids: &[BlobId]) -> Result<Vec<BlobId>, StoreError> {
        let mut found = Vec::new();
        let mut stmt = self
            .conn
            .prepare_cached("SELECT 1 FROM blobs WHERE sha256 = ?1")?;
        for id in ids {
            if stmt.exists(params![id.as_str()])? {
                found.push(id.clone());
            }
        }
        Ok(found)
    }

    /// A summary of the blobs the records refer to, which changes when a
    /// record refers to a new one: their number and the last change that
    /// added one.
    pub fn blob_signature(&self) -> Result<(i64, i64), StoreError> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*), COALESCE(MAX(first_seq), 0) FROM blobs",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    }

    /// Every blob a job record refers to, with its size.
    pub fn referenced_blobs(&self) -> Result<Vec<BlobRef>, StoreError> {
        let rows: Vec<(String, i64)> = self
            .conn
            .prepare("SELECT sha256, size FROM blobs ORDER BY sha256")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        rows.into_iter()
            .map(|(id, size)| {
                Ok(BlobRef {
                    id: BlobId::parse(&id)
                        .map_err(|e| StoreError::InvalidDatabase(e.to_string()))?,
                    size: size as u64,
                })
            })
            .collect()
    }

    /// The jobs whose records refer to the blob `id`.
    pub fn jobs_referring_to(&self, id: &BlobId) -> Result<Vec<String>, StoreError> {
        Ok(self
            .conn
            .prepare("SELECT DISTINCT job_id FROM blob_refs WHERE sha256 = ?1 ORDER BY job_id")?
            .query_map(params![id.as_str()], |r| r.get(0))?
            .collect::<Result<_, _>>()?)
    }

    /// A value of the `meta` table.
    pub fn meta(&self, key: &str) -> Result<Option<String>, StoreError> {
        Ok(self
            .conn
            .query_row("SELECT value FROM meta WHERE key = ?1", params![key], |r| {
                r.get(0)
            })
            .optional()?)
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
        Ok(())
    }

    /// Epoch (Raft term) of the leader that writes to this store; 0 without one.
    pub fn epoch(&self) -> Result<u64, StoreError> {
        Ok(self
            .meta("epoch")?
            .and_then(|v| v.parse().ok())
            .unwrap_or(0))
    }

    pub fn set_epoch(&self, epoch: u64) -> Result<(), StoreError> {
        self.set_meta("epoch", &epoch.to_string())
    }

    /// A consistent copy of the database.
    pub fn snapshot(&self) -> Result<Vec<u8>, StoreError> {
        let dir = self
            .path
            .as_ref()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .unwrap_or_else(std::env::temp_dir);
        let file = dir.join(format!("snapshot-{}.sqlite3", uuid::Uuid::new_v4()));
        self.conn.execute(
            "VACUUM INTO ?1",
            params![file.to_string_lossy().into_owned()],
        )?;
        crate::fault::point(&file, crate::fault::SNAPSHOT_COPIED);
        let bytes = std::fs::read(&file);
        let _ = std::fs::remove_file(&file);
        Ok(bytes?)
    }

    /// Replaces the whole database with a copy from [`Self::snapshot`], and writes
    /// the artifacts of its terminal records. A store opened in memory keeps the
    /// copy in a temporary file, which it removes when it is dropped.
    pub fn restore(&mut self, database: &[u8]) -> Result<(), StoreError> {
        let temporary = self.path.is_none();
        let path = self.path.clone().unwrap_or_else(|| {
            std::env::temp_dir().join(format!("algebramesh-{}.sqlite3", uuid::Uuid::new_v4()))
        });
        // Replaced so that a crash at any point leaves either the old database
        // or the new one, whole (Issue #23), and so that a database that cannot
        // be used never replaces a good one (Issue #28):
        // 1. the new database is written next to the old one and synced;
        // 2. it is opened on its own and checked: SQLite's quick check and the
        //    tables of a metadata store. If it fails, the old database is
        //    left as it is;
        // 3. the old one is checkpointed, so its WAL holds nothing, and closed;
        // 4. its WAL and shared-memory files are removed (empty: the old
        //    database file alone is complete);
        // 5. the new file is renamed over the old one, atomically, and the
        //    directory is synced so that the rename itself survives.
        // If a step after 3 fails, the old database is opened again.
        let incoming = path.with_extension("restore");
        let staged = (|| -> Result<(), StoreError> {
            use std::io::Write;
            let mut file = std::fs::File::create(&incoming)?;
            file.write_all(database)?;
            file.sync_all()?;
            drop(file);
            crate::fault::point(&incoming, crate::fault::RESTORE_STAGED);
            validate_database(&incoming)
        })();
        if let Err(e) = staged {
            let _ = std::fs::remove_file(&incoming);
            return Err(e);
        }
        let had_file = self.path.is_some();
        if had_file {
            self.conn
                .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
        }
        // A file cannot be replaced while it is open (Windows): the old
        // database is closed first, and opened again if the replacement fails.
        // An in-memory database is kept until it is replaced.
        let mut old = Some(std::mem::replace(
            &mut self.conn,
            Connection::open_in_memory()?,
        ));
        if had_file {
            drop(old.take());
        }
        let replaced = (|| -> Result<(), StoreError> {
            for suffix in ["-wal", "-shm"] {
                let mut name = path.clone().into_os_string();
                name.push(suffix);
                match std::fs::remove_file(PathBuf::from(name)) {
                    Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
                    _ => {}
                }
            }
            crate::fault::point(&path, crate::fault::RESTORE_CLOSED);
            std::fs::rename(&incoming, &path)?;
            crate::fault::point(&path, crate::fault::RESTORE_RENAMED);
            sync_directory(&path);
            Ok(())
        })();
        if let Err(e) = replaced {
            let _ = std::fs::remove_file(&incoming);
            // The old database is whole (checkpointed): use it again.
            self.conn = match old {
                Some(conn) => conn,
                None => open_connection(Some(&path)).map_err(|o| reopen_failed(&path, &o))?,
            };
            return Err(e);
        }
        drop(old);
        // Marked before it is opened: a temporary file that cannot be opened
        // is still removed with the store.
        if temporary {
            self.path = Some(path.clone());
            self.temporary = true;
        }
        self.conn = open_connection(Some(&path)).map_err(|e| reopen_failed(&path, &e))?;
        self.path = Some(path);
        let mut stmt = self.conn.prepare("SELECT record_json FROM jobs")?;
        let records: Vec<String> = stmt
            .query_map([], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        drop(stmt);
        *self.lock_pending() = Default::default();
        for json in records {
            let record: JobRecord = serde_json::from_str(&json)?;
            if record.state.is_terminal() {
                self.write_or_defer(&record);
            }
        }
        Ok(())
    }

    /// Writes `artifacts/<job-id>/record.json` whole: a temporary file,
    /// synced, then renamed (Issue #37: after a power loss the file is either
    /// absent, and written again when the store opens, or complete).
    fn write_artifact(&self, record: &JobRecord) -> Result<(), StoreError> {
        use std::io::Write;
        if let Some(dir) = &self.artifacts {
            let dir = dir.join(record.job_id.to_string());
            std::fs::create_dir_all(&dir)?;
            let tmp = dir.join("record.json.tmp");
            let mut file = std::fs::File::create(&tmp)?;
            file.write_all(&serde_json::to_vec_pretty(record)?)?;
            file.sync_all()?;
            drop(file);
            let path = dir.join("record.json");
            std::fs::rename(tmp, &path)?;
            sync_directory(&path);
        }
        Ok(())
    }

    /// Marks for writing the artifacts of terminal jobs that are missing (Issue
    /// #37): the set of artifacts that could not be written is kept in memory
    /// only, so a scheduler that stopped, or lost unsynced files to a power
    /// loss, finds them again here when it opens.
    fn find_missing_artifacts(&self) -> Result<(), StoreError> {
        let Some(dir) = &self.artifacts else {
            return Ok(());
        };
        let terminal: Vec<String> = [
            JobState::Succeeded,
            JobState::Failed,
            JobState::TimedOut,
            JobState::Cancelled,
            JobState::Lost,
        ]
        .into_iter()
        .map(state_name)
        .collect();
        let placeholders = vec!["?"; terminal.len()].join(",");
        let mut stmt = self.conn.prepare(&format!(
            "SELECT job_id FROM jobs WHERE state IN ({placeholders})"
        ))?;
        let ids: Vec<String> = stmt
            .query_map(rusqlite::params_from_iter(terminal.iter()), |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        let missing: Vec<String> = ids
            .into_iter()
            .filter(|id| !dir.join(id).join("record.json").exists())
            .collect();
        if !missing.is_empty() {
            tracing::warn!(
                count = missing.len(),
                "artifacts of finished jobs are missing; writing them again"
            );
            self.lock_pending().0.extend(missing);
        }
        Ok(())
    }

    fn row_from(
        record_json: String,
        priority: i32,
        cancel_requested: bool,
        retried_as: Option<String>,
    ) -> Result<JobRow, StoreError> {
        Ok(JobRow {
            record: serde_json::from_str(&record_json)?,
            priority,
            cancel_requested,
            retried_as: retried_as.and_then(|s| s.parse().ok()).map(JobId),
            request: None,
        })
    }

    /// The row of the job a submission with `request_id` created, as stored:
    /// the JSON of its record is read and copied here, and deserialized only
    /// by [`StoredJob::parse`] (Issues #63, #84).
    pub fn stored_job_for_request(
        &self,
        request_id: &str,
    ) -> Result<Option<StoredJob>, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT record_json, priority, cancel_requested, retried_as
                 FROM jobs WHERE request_id = ?1",
                params![request_id],
                |r| {
                    Ok(StoredJob {
                        record_json: r.get(0)?,
                        priority: r.get(1)?,
                        cancel_requested: r.get(2)?,
                        retried_as: r.get(3)?,
                    })
                },
            )
            .optional()?)
    }

    /// The job a submission with `request_id` created, and its fingerprint.
    pub fn job_for_request(
        &self,
        request_id: &str,
    ) -> Result<Option<(JobRow, String)>, StoreError> {
        let raw = self
            .conn
            .query_row(
                "SELECT record_json, priority, cancel_requested, retried_as, request_fingerprint
                 FROM jobs WHERE request_id = ?1",
                params![request_id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get::<_, Option<String>>(4)?,
                    ))
                },
            )
            .optional()?;
        raw.map(|(j, p, c, r, f)| Ok((Self::row_from(j, p, c, r)?, f.unwrap_or_default())))
            .transpose()
    }

    /// The agent, state and cancellation request of a job, without reading its
    /// record (Issue #16).
    pub fn job_brief(&self, job_id: &str) -> Result<Option<JobBrief>, StoreError> {
        let raw: Option<(Option<String>, String, bool)> = self
            .conn
            .query_row(
                "SELECT agent_id, state, cancel_requested FROM jobs WHERE job_id = ?1",
                params![job_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        raw.map(|(agent_id, state, cancel_requested)| {
            Ok(JobBrief {
                agent_id,
                state: serde_json::from_value(serde_json::Value::String(state))?,
                cancel_requested,
            })
        })
        .transpose()
    }

    pub fn get_job(&self, job_id: &str) -> Result<Option<JobRow>, StoreError> {
        let raw = self
            .conn
            .query_row(
                "SELECT record_json, priority, cancel_requested, retried_as
                 FROM jobs WHERE job_id = ?1",
                params![job_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        raw.map(|(j, p, c, r)| Self::row_from(j, p, c, r))
            .transpose()
    }

    /// Jobs in the given states, highest priority first, then oldest first.
    /// The ids of the jobs of `agent_id` in one of `states`, without reading
    /// their records (Issue #16; index `jobs_agent`).
    pub fn job_ids_for_agent(
        &self,
        agent_id: &str,
        states: &[JobState],
    ) -> Result<Vec<String>, StoreError> {
        let names: Vec<String> = states.iter().map(|s| state_name(*s)).collect();
        let placeholders = vec!["?"; names.len()].join(",");
        let mut stmt = self.conn.prepare(&format!(
            "SELECT job_id FROM jobs WHERE agent_id = ? AND state IN ({placeholders})"
        ))?;
        let params = std::iter::once(agent_id.to_owned()).chain(names);
        let ids = stmt
            .query_map(rusqlite::params_from_iter(params), |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        Ok(ids)
    }

    /// The queued jobs of `backends`, highest priority first and then oldest
    /// first, as their ids and placements, without their records (Issue #19).
    /// The index `jobs_queue` holds every column the query reads, so SQLite
    /// does not visit the rows, whose large `record_json` comes before
    /// `placement`.
    pub fn queued_placements(
        &self,
        backends: &[String],
    ) -> Result<Vec<(String, Placement)>, StoreError> {
        if backends.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = vec!["?"; backends.len()].join(",");
        let mut stmt = self.conn.prepare(&format!(
            "SELECT job_id, placement FROM jobs
             WHERE state = ? AND backend IN ({placeholders})
             ORDER BY priority DESC, seq ASC"
        ))?;
        let params = std::iter::once(state_name(JobState::Queued)).chain(backends.iter().cloned());
        let rows: Vec<(String, Option<String>)> = stmt
            .query_map(rusqlite::params_from_iter(params), |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?
            .collect::<Result<_, _>>()?;
        let mut out = Vec::with_capacity(rows.len());
        for (id, placement) in rows {
            let placement = match placement {
                Some(json) => serde_json::from_str(&json)?,
                // Not filled in yet: from the record (only for such rows).
                None => match self.get_job(&id)? {
                    Some(row) => Placement::of(&row.record.spec),
                    None => continue,
                },
            };
            out.push((id, placement));
        }
        Ok(out)
    }

    /// The number of jobs in `state`, from the index `jobs_queue` alone.
    pub fn count_jobs(&self, state: JobState) -> Result<u64, StoreError> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM jobs WHERE state = ?1",
            [state_name(state)],
            |r| r.get(0),
        )?;
        Ok(n as u64)
    }

    /// A page of the jobs that pass `filter`, after `after` (Issue #63).
    ///
    /// It reads the columns beside the record, never the record: the request
    /// id, which comes after the record in the row, is read from an index. At
    /// most `max_scanned` jobs are looked at, in the order of an index: of the
    /// request ids when `filter` has a prefix, and otherwise of the column of
    /// one other filter, in the order of submission; the other filters are
    /// applied to the jobs looked at. So a page can hold fewer than
    /// `page_size` jobs, or none, with a position to go on from. A full page
    /// has a position only if a job after it passes the filters, or if the
    /// bound stopped the look for one.
    pub fn list_jobs(
        &self,
        filter: &JobFilter,
        after: Option<&ListPosition>,
        limits: &ListLimits,
    ) -> Result<JobPage, StoreError> {
        let max_scanned = limits.max_scanned.max(1);
        let limit = i64::try_from(max_scanned).unwrap_or(i64::MAX);
        let mut listing = Listing::new(filter, limits);
        if !filter.request_id_prefix.is_empty() {
            let prefix = filter.request_id_prefix.as_str();
            let (strict, from) = match after {
                Some(ListPosition::Request(r)) if r.as_str() >= prefix => (true, r.as_str()),
                _ => (false, prefix),
            };
            let mut keys = self.conn.prepare(&keys_by_prefix(strict))?;
            let mut look = self.conn.prepare(LISTED_BY_SEQ)?;
            let mut rows = keys.query(params![from, limit])?;
            while let Some(row) = rows.next()? {
                let (request_id, seq): (String, i64) = (row.get(0)?, row.get(1)?);
                if !request_id.starts_with(prefix) {
                    return Ok(listing.finish(true, max_scanned));
                }
                let mut job = look.query_row([seq], listed_job)?;
                job.request_id = Some(request_id.clone());
                if listing.look(job, ListPosition::Request(request_id)) {
                    return Ok(listing.page);
                }
            }
            return Ok(listing.finish(false, max_scanned));
        }
        let after = match after {
            Some(ListPosition::Seq(seq)) => *seq,
            _ => 0,
        };
        let (sql, value) = scan_by_seq(filter);
        let mut stmt = self.conn.prepare(&sql)?;
        let mut request = self.conn.prepare(REQUEST_BY_SEQ)?;
        let mut rows = match value {
            Some(v) => stmt.query(params![after, limit, v])?,
            None => stmt.query(params![after, limit])?,
        };
        while let Some(row) = rows.next()? {
            let mut job = listed_job(row)?;
            let seq = job.seq;
            if listing.takes(&job) {
                job.request_id = request.query_row([seq], |r| r.get(0)).optional()?;
            }
            if listing.look(job, ListPosition::Seq(seq)) {
                return Ok(listing.page);
            }
        }
        Ok(listing.finish(false, max_scanned))
    }

    pub fn jobs_in_states(&self, states: &[JobState]) -> Result<Vec<JobRow>, StoreError> {
        let names: Vec<String> = states.iter().map(|s| state_name(*s)).collect();
        let placeholders = vec!["?"; names.len()].join(",");
        let mut stmt = self.conn.prepare(&format!(
            "SELECT record_json, priority, cancel_requested, retried_as FROM jobs
             WHERE state IN ({placeholders}) ORDER BY priority DESC, seq ASC"
        ))?;
        let rows = stmt.query_map(rusqlite::params_from_iter(names.iter()), |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?;
        rows.map(|r| {
            let (j, p, c, rt) = r?;
            Self::row_from(j, p, c, rt)
        })
        .collect()
    }

    pub fn events(&self, job_id: &JobId) -> Result<Vec<String>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT at, note FROM events WHERE job_id = ?1 ORDER BY seq")?;
        let rows = stmt.query_map(params![job_id.to_string()], |r| {
            Ok(format!(
                "{} {}",
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?
            ))
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use algebramesh_core::JobSpec;
    use chrono::Utc;

    fn record(priority_hint: &str) -> JobRecord {
        let spec = JobSpec::from_json(&format!(
            r#"{{"schema_version": 1, "backend": "asir", "operation": "polynomial.factor",
                "arguments": {{"hint": "{priority_hint}"}}}}"#
        ))
        .unwrap();
        JobRecord::new(JobId::new(), spec, Utc::now())
    }

    /// The blobs records refer to are derived from them once the cluster
    /// uses blobs, follow their updates, and are rebuilt when the tables are
    /// of another version (ADR 0021 §7).
    #[test]
    fn blob_tables_follow_the_records() {
        let store = Store::open(None, None).unwrap();
        let a = BlobRef {
            id: BlobId::of(b"a"),
            size: 1,
        };
        let b = BlobRef {
            id: BlobId::of(b"b"),
            size: 1,
        };
        let mut row = JobRow::new(record("blobs"), 0);
        row.record.result = Some(serde_json::json!({"x": a.marker(), "n": 1}));
        store.insert_job(&row).unwrap();
        // Before the feature, markers are not derived.
        assert!(store.referenced_blobs().unwrap().is_empty());
        store
            .enable_features(&[algebramesh_core::features::BLOB_REFS.into()], "1.1.0")
            .unwrap();
        store.update_job(&row).unwrap();
        assert_eq!(store.referenced_blobs().unwrap(), vec![a.clone()]);
        let job = row.record.job_id.to_string();
        assert_eq!(store.jobs_referring_to(&a.id).unwrap(), vec![job.clone()]);
        assert_eq!(
            store
                .registered_blobs(&[a.id.clone(), b.id.clone()])
                .unwrap(),
            vec![a.id.clone()]
        );
        // The record now refers to `b` instead.
        row.record.result = Some(b.marker());
        store.update_job(&row).unwrap();
        assert_eq!(store.jobs_referring_to(&b.id).unwrap(), vec![job.clone()]);
        assert!(store.jobs_referring_to(&a.id).unwrap().is_empty());
        // Tables of another version are rebuilt from the records.
        store
            .conn
            .execute_batch("DELETE FROM blob_refs; DELETE FROM blobs; DELETE FROM meta WHERE key = 'blob_tables_version';")
            .unwrap();
        migrate(&store.conn).unwrap();
        assert_eq!(store.referenced_blobs().unwrap(), vec![b.clone()]);
        assert_eq!(store.jobs_referring_to(&b.id).unwrap(), vec![job]);
    }

    #[test]
    fn placements_follow_the_queue_and_are_filled_for_older_rows() {
        let store = Store::open(None, None).unwrap();
        let low = JobRow::new(record("low"), 0);
        let high = JobRow::new(record("high"), 5);
        let low2 = JobRow::new(record("low2"), 0);
        for r in [&low, &high, &low2] {
            store.insert_job(r).unwrap();
        }
        let ids = |store: &Store, backends: &[&str]| -> Vec<String> {
            let backends: Vec<String> = backends.iter().map(|b| b.to_string()).collect();
            store
                .queued_placements(&backends)
                .unwrap()
                .into_iter()
                .map(|(id, p)| {
                    assert_eq!(p, Placement::of(&low.record.spec));
                    id
                })
                .collect()
        };
        let expected =
            [high.record.job_id, low.record.job_id, low2.record.job_id].map(|j| j.to_string());
        assert_eq!(ids(&store, &["asir"]), expected);
        assert!(ids(&store, &["m2"]).is_empty());
        // Rows written before the column existed are read from their records,
        // and get the column when the database is opened again.
        store
            .conn
            .execute("UPDATE jobs SET placement = NULL", [])
            .unwrap();
        assert_eq!(ids(&store, &["asir"]), expected);
        migrate(&store.conn).unwrap();
        let filled: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM jobs WHERE placement IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(filled, 3);
        assert_eq!(ids(&store, &["asir"]), expected);
    }

    /// Issue #21: an artifact that cannot be written fails neither the commit
    /// nor a replicated apply, and is written later from the database.
    #[test]
    fn an_unwritable_artifact_does_not_fail_a_committed_change() {
        let temp = tempfile::Builder::new()
            .prefix("am-artifacts-")
            .tempdir()
            .unwrap();
        let dir = temp.path().to_path_buf();
        std::fs::create_dir_all(&dir).unwrap();
        let artifacts = dir.join("artifacts");
        let store = Store::open(None, Some(artifacts.clone())).unwrap();
        // Make the artifact directory unwritable: a file where it should be.
        std::fs::remove_dir_all(&artifacts).unwrap();
        std::fs::write(&artifacts, b"not a directory").unwrap();

        let mut row = JobRow::new(record("a"), 0);
        store.insert_job(&row).unwrap();
        for state in [JobState::Assigned, JobState::Running, JobState::Succeeded] {
            row.record.transition(state, Utc::now()).unwrap();
        }
        row.record.result = Some(serde_json::json!(42));
        // The commit succeeds and the database has the terminal job.
        store.update_job(&row).unwrap();
        let stored = store
            .get_job(&row.record.job_id.to_string())
            .unwrap()
            .unwrap();
        assert_eq!(stored.record.state, JobState::Succeeded);
        assert_eq!(store.write_pending_artifacts(), 1);

        // A follower applying the same changes is not stopped either.
        let follower = Store::open(None, Some(artifacts.clone())).unwrap();
        let changes = {
            let (sink, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let mut source = Store::open(None, None).unwrap();
            source.set_change_sink(Some(sink));
            source
                .insert_job(&JobRow::new(stored.record.clone(), 0))
                .unwrap();
            source.update_job(&stored).unwrap();
            let mut out = Vec::new();
            while let Ok(c) = rx.try_recv() {
                out.push(c);
            }
            out
        };
        follower
            .apply_batch(&changes, &[("raft_applied", "1".into())])
            .unwrap();
        assert_eq!(follower.meta("raft_applied").unwrap().as_deref(), Some("1"));

        // Once the directory can be written, the artifact is written from the
        // database and matches it.
        std::fs::remove_file(&artifacts).unwrap();
        std::fs::create_dir_all(&artifacts).unwrap();
        assert_eq!(store.write_pending_artifacts(), 0);
        let written: JobRecord = serde_json::from_slice(
            &std::fs::read(
                artifacts
                    .join(row.record.job_id.to_string())
                    .join("record.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(written, stored.record);
        assert_eq!(follower.write_pending_artifacts(), 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A store opened in memory keeps a restored database in a temporary
    /// file, the same one for each restore, and removes it with its WAL when
    /// it is dropped; a store over a file of its own leaves the file.
    #[test]
    fn the_temporary_file_of_a_store_in_memory_is_removed_with_it() {
        let database = Store::open(None, None).unwrap().snapshot().unwrap();
        let mut store = Store::open(None, None).unwrap();
        store.restore(&database).unwrap();
        let path = store.path().unwrap().to_path_buf();
        assert!(path.exists());
        store.restore(&database).unwrap();
        assert_eq!(store.path(), Some(path.as_path()));
        drop(store);
        for suffix in ["", "-wal", "-shm"] {
            let mut name = path.clone().into_os_string();
            name.push(suffix);
            assert!(!PathBuf::from(&name).exists(), "{name:?} is left");
        }
        assert!(!path.with_extension("restore").exists());

        let dir = tempfile::Builder::new()
            .prefix("am-restore-")
            .tempdir()
            .unwrap();
        let own = dir.path().join("metadata.sqlite3");
        let mut store = Store::open(Some(&own), None).unwrap();
        store.restore(&database).unwrap();
        drop(store);
        assert!(own.exists());
    }

    /// Issue #23: the state machine commits with `synchronous = FULL`, and a
    /// restore replaces a database that has a WAL without mixing the two.
    #[test]
    fn restores_replace_the_database_whole() {
        let temp = tempfile::Builder::new()
            .prefix("am-restore-")
            .tempdir()
            .unwrap();
        let dir = temp.path().to_path_buf();
        let path = dir.join("metadata.sqlite3");
        let mut store = Store::open(Some(&path), None).unwrap();
        let sync: i64 = store
            .conn
            .query_row("PRAGMA synchronous", [], |r| r.get(0))
            .unwrap();
        assert_eq!(sync, 2, "FULL");
        // Changes still in the WAL of the old database.
        let old = JobRow::new(record("old"), 0);
        store.insert_job(&old).unwrap();
        let other = Store::open(None, None).unwrap();
        let new = JobRow::new(record("new"), 0);
        other.insert_job(&new).unwrap();
        store.restore(&other.snapshot().unwrap()).unwrap();
        let ids = |s: &Store| -> Vec<String> {
            s.jobs_in_states(&[JobState::Queued])
                .unwrap()
                .iter()
                .map(|r| r.record.job_id.to_string())
                .collect()
        };
        assert_eq!(ids(&store), [new.record.job_id.to_string()]);
        assert!(!path.with_extension("restore").exists());
        // Opened again from the files alone: the restored database, whole.
        drop(store);
        let again = Store::open(Some(&path), None).unwrap();
        assert_eq!(ids(&again), [new.record.job_id.to_string()]);
        // A crash after the new file was written but before the rename leaves
        // the current database as it was.
        std::fs::write(path.with_extension("restore"), b"partial").unwrap();
        drop(again);
        let after_crash = Store::open(Some(&path), None).unwrap();
        assert_eq!(ids(&after_crash), [new.record.job_id.to_string()]);
        // The working copy may be relaxed.
        after_crash.relax_durability().unwrap();
        let sync: i64 = after_crash
            .conn
            .query_row("PRAGMA synchronous", [], |r| r.get(0))
            .unwrap();
        assert_eq!(sync, 1, "NORMAL");
        drop(after_crash);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn queue_order_and_roundtrip() {
        let store = Store::open(None, None).unwrap();
        let low = JobRow::new(record("low"), 0);
        let high = JobRow::new(record("high"), 5);
        let low2 = JobRow::new(record("low2"), 0);
        for r in [&low, &high, &low2] {
            store.insert_job(r).unwrap();
        }
        let queued = store.jobs_in_states(&[JobState::Queued]).unwrap();
        let ids: Vec<_> = queued.iter().map(|r| r.record.job_id).collect();
        assert_eq!(
            ids,
            [high.record.job_id, low.record.job_id, low2.record.job_id]
        );

        let mut row = store
            .get_job(&low.record.job_id.to_string())
            .unwrap()
            .unwrap();
        row.record
            .transition(JobState::Cancelled, Utc::now())
            .unwrap();
        store.update_job(&row).unwrap();
        assert_eq!(store.jobs_in_states(&[JobState::Queued]).unwrap().len(), 2);
        assert!(store.get_job("missing").unwrap().is_none());
    }

    #[test]
    fn terminal_records_become_artifacts() {
        let temp = tempfile::Builder::new()
            .prefix("algebramesh-store-")
            .tempdir()
            .unwrap();
        let dir = temp.path().to_path_buf();
        let store =
            Store::open(Some(&dir.join("meta.sqlite3")), Some(dir.join("artifacts"))).unwrap();
        let mut row = JobRow::new(record("x"), 0);
        store.insert_job(&row).unwrap();
        row.record
            .transition(JobState::Cancelled, Utc::now())
            .unwrap();
        store.update_job(&row).unwrap();
        let path = dir
            .join("artifacts")
            .join(row.record.job_id.to_string())
            .join("record.json");
        let saved: JobRecord = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(saved.state, JobState::Cancelled);
        drop(store);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Issue #37: an artifact missing after a stop (it could not be written,
    /// or a power loss dropped it) is written again when the store opens.
    #[test]
    fn missing_artifacts_are_written_when_the_store_opens() {
        let temp = tempfile::Builder::new()
            .prefix("algebramesh-store-")
            .tempdir()
            .unwrap();
        let dir = temp.path().to_path_buf();
        let open = || Store::open(Some(&dir.join("meta.sqlite3")), Some(dir.join("artifacts")));
        let store = open().unwrap();
        let mut row = JobRow::new(record("x"), 0);
        store.insert_job(&row).unwrap();
        row.record
            .transition(JobState::Cancelled, Utc::now())
            .unwrap();
        store.update_job(&row).unwrap();
        drop(store);
        let path = dir
            .join("artifacts")
            .join(row.record.job_id.to_string())
            .join("record.json");
        std::fs::remove_file(&path).unwrap();
        let _store = open().unwrap();
        let saved: JobRecord = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved.state, JobState::Cancelled);
        drop(_store);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Issue #37: the copies of an interrupted snapshot or restore are
    /// removed; other files stay.
    #[test]
    fn stale_files_of_a_snapshot_or_restore_are_removed() {
        let temp = tempfile::Builder::new()
            .prefix("algebramesh-stale-")
            .tempdir()
            .unwrap();
        let dir = temp.path().to_path_buf();
        std::fs::create_dir_all(&dir).unwrap();
        for name in [
            "snapshot-0a1b.sqlite3",
            "metadata.restore",
            "metadata.sqlite3",
            "raft.sqlite3",
            "working-3.sqlite3",
        ] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }
        remove_stale_files(&dir);
        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(
            left,
            ["metadata.sqlite3", "raft.sqlite3", "working-3.sqlite3"]
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_copy_follows_the_changes_of_another_store() {
        let temp = tempfile::Builder::new()
            .prefix("algebramesh-replica-")
            .tempdir()
            .unwrap();
        let dir = temp.path().to_path_buf();
        let mut source = Store::open(None, None).unwrap();
        source.set_epoch(3).unwrap();
        let mut row = JobRow::new(record("a"), 0);
        source.insert_job(&row).unwrap();
        source.add_event(&row.record.job_id, "queued").unwrap();
        assert_eq!(source.last_seq().unwrap(), 2);

        // A whole copy, then the changes after it.
        let mut copy =
            Store::open(Some(&dir.join("meta.sqlite3")), Some(dir.join("artifacts"))).unwrap();
        copy.restore(&source.snapshot().unwrap()).unwrap();
        assert_eq!(copy.last_seq().unwrap(), 2);
        assert_eq!(copy.epoch().unwrap(), 3);
        let (sink, mut changes) = tokio::sync::mpsc::unbounded_channel();
        source.set_change_sink(Some(sink));
        row.record
            .transition(JobState::Cancelled, Utc::now())
            .unwrap();
        source.update_job(&row).unwrap();
        source
            .record_agent("agent", "worker", "host", "{}")
            .unwrap();
        let mut after = Vec::new();
        while let Ok(change) = changes.try_recv() {
            after.push(change);
        }
        assert_eq!(after.len(), 2);
        for (seq, change) in &after {
            copy.apply_replicated(*seq, change).unwrap();
        }
        let copied = copy
            .get_job(&row.record.job_id.to_string())
            .unwrap()
            .unwrap();
        assert_eq!(copied.record.state, JobState::Cancelled);
        assert_eq!(
            copy.events(&row.record.job_id).unwrap(),
            source.events(&row.record.job_id).unwrap()
        );
        // The terminal record was written as an artifact of the copy too.
        assert!(
            dir.join("artifacts")
                .join(row.record.job_id.to_string())
                .join("record.json")
                .exists()
        );
        // Changes must come in order.
        assert!(matches!(
            copy.apply_replicated(9, &after[0].1),
            Err(StoreError::OutOfOrder(9, 4))
        ));
        drop(copy);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_request_key_finds_its_job_and_is_unique() {
        let store = Store::open(None, None).unwrap();
        let mut row = JobRow::new(record("a"), 0);
        row.request = Some(RequestKey {
            request_id: "key-1".into(),
            fingerprint: "f1".into(),
        });
        store.insert_job(&row).unwrap();
        let (found, fingerprint) = store.job_for_request("key-1").unwrap().unwrap();
        assert_eq!(found.record.job_id, row.record.job_id);
        assert_eq!(fingerprint, "f1");
        assert!(store.job_for_request("key-2").unwrap().is_none());
        // Jobs without a key do not collide with each other.
        store.insert_job(&JobRow::new(record("b"), 0)).unwrap();
        store.insert_job(&JobRow::new(record("c"), 0)).unwrap();
        // A second job with the same key is refused by the database.
        let mut again = JobRow::new(record("d"), 0);
        again.request = row.request.clone();
        assert!(store.insert_job(&again).is_err());
    }

    /// A job of `backend` and `operation` in `state`, with `request_id`.
    fn listed(
        store: &Store,
        backend: &str,
        operation: &str,
        state: JobState,
        request_id: Option<&str>,
    ) -> String {
        let spec = JobSpec::from_json(&format!(
            r#"{{"schema_version": 1, "backend": "{backend}", "operation": "{operation}",
                "arguments": {{}}}}"#
        ))
        .unwrap();
        let mut record = JobRecord::new(JobId::new(), spec, Utc::now());
        record.state = state;
        let mut row = JobRow::new(record, 0);
        row.request = request_id.map(|r| RequestKey {
            request_id: r.into(),
            fingerprint: "f".into(),
        });
        store.insert_job(&row).unwrap();
        row.record.job_id.to_string()
    }

    /// Every job of a listing, page after page, and the number of pages.
    fn every_page(store: &Store, filter: &JobFilter, limits: ListLimits) -> (Vec<String>, usize) {
        let (mut jobs, mut pages, mut after) = (Vec::new(), 0, None);
        loop {
            let page = store.list_jobs(filter, after.as_ref(), &limits).unwrap();
            pages += 1;
            assert!(page.jobs.len() <= limits.page_size);
            jobs.extend(page.jobs.into_iter().map(|j| j.job_id));
            match page.next {
                Some(next) => after = Some(next),
                None => return (jobs, pages),
            }
            assert!(pages < 10_000, "the listing does not end");
        }
    }

    /// Issue #63: a listing gives, over its pages, exactly the jobs that pass
    /// its filters, in the order of submission, or of the request ids for a
    /// prefix, whatever the bounds of a page.
    #[test]
    fn a_listing_gives_the_jobs_that_pass_its_filters() {
        let store = Store::open(None, None).unwrap();
        let mut all = Vec::new();
        let states = [JobState::Queued, JobState::Succeeded, JobState::Failed];
        for i in 0..30 {
            let backend = ["asir", "m2"][i % 2];
            let operation = ["polynomial.factor", "gkz.series", "matrix.rank"][i % 3];
            let state = states[(i / 2) % 3];
            // Request ids out of the order of submission; every third job has none.
            let request = (i % 3 != 2).then(|| format!("wf/{}/{:02}", i % 4, 29 - i));
            let id = listed(&store, backend, operation, state, request.as_deref());
            all.push((id, backend, operation, state_name(state), request));
        }
        let filters = [
            JobFilter::default(),
            JobFilter {
                state: Some("SUCCEEDED".into()),
                ..Default::default()
            },
            JobFilter {
                backend: Some("m2".into()),
                operation: Some("gkz.series".into()),
                ..Default::default()
            },
            JobFilter {
                operation: Some("matrix.rank".into()),
                state: Some("QUEUED".into()),
                ..Default::default()
            },
            JobFilter {
                request_id_prefix: "wf/1/".into(),
                ..Default::default()
            },
            JobFilter {
                request_id_prefix: "wf/".into(),
                backend: Some("asir".into()),
                ..Default::default()
            },
            JobFilter {
                request_id_prefix: "zz".into(),
                ..Default::default()
            },
            JobFilter {
                backend: Some("asir".into()),
                ..Default::default()
            },
            JobFilter {
                backend: Some("m2".into()),
                state: Some("FAILED".into()),
                ..Default::default()
            },
        ];
        for filter in &filters {
            let mut expected: Vec<_> = all
                .iter()
                .filter(|(_, b, o, s, r)| {
                    let prefix = &filter.request_id_prefix;
                    (prefix.is_empty() || r.as_deref().is_some_and(|r| r.starts_with(prefix)))
                        && filter.backend.as_deref().is_none_or(|x| x == *b)
                        && filter.operation.as_deref().is_none_or(|x| x == *o)
                        && filter.state.as_deref().is_none_or(|x| x == s)
                })
                .collect();
            if !filter.request_id_prefix.is_empty() {
                expected.sort_by(|a, b| a.4.cmp(&b.4));
            }
            let expected: Vec<String> = expected.into_iter().map(|j| j.0.clone()).collect();
            for (page_size, max_scanned) in [(100, 10_000), (1, 10_000), (4, 3), (2, 1)] {
                let limits = ListLimits {
                    page_size,
                    max_bytes: 1 << 20,
                    max_scanned,
                    max_scanned_bytes: usize::MAX,
                };
                let (jobs, pages) = every_page(&store, filter, limits);
                assert_eq!(jobs, expected, "{filter:?} {limits:?}");
                // Without the bound on the jobs looked at, no page is empty
                // but the first (when no job passes), and no page follows the
                // last job.
                if max_scanned == 10_000 {
                    assert_eq!(pages, expected.len().div_ceil(page_size).max(1));
                }
            }
        }
        // The columns of a listed job are those of the job.
        let page = store
            .list_jobs(
                &JobFilter::default(),
                None,
                &ListLimits {
                    page_size: 1,
                    max_bytes: 1 << 20,
                    max_scanned: 10,
                    max_scanned_bytes: usize::MAX,
                },
            )
            .unwrap();
        let job = &page.jobs[0];
        assert_eq!(job.job_id, all[0].0);
        assert_eq!(job.request_id, all[0].4);
        assert_eq!(
            (job.backend.as_str(), job.operation.as_str()),
            (all[0].1, all[0].2)
        );
        assert_eq!(job.state, all[0].3);
        assert_eq!(page.next, Some(ListPosition::Seq(job.seq)));
    }

    /// Issue #63: the request ids, which come after the record in a row, are
    /// read from indexes alone, so that a listing never reads the pages of the
    /// records.
    #[test]
    fn the_request_ids_of_a_listing_come_from_indexes() {
        let store = Store::open(None, None).unwrap();
        let plan = |sql: &str| -> String {
            let mut stmt = store
                .conn
                .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
                .unwrap();
            let rows = stmt.query_map([], |r| r.get::<_, String>(3)).unwrap();
            rows.map(Result::unwrap).collect::<Vec<_>>().join("; ")
        };
        let request = plan(&REQUEST_BY_SEQ.replace("?1", "1"));
        assert!(
            request.contains("COVERING INDEX jobs_request_by_seq"),
            "{request}"
        );
        for strict in [false, true] {
            let keys = plan(
                &keys_by_prefix(strict)
                    .replace("?1", "'a'")
                    .replace("?2", "10"),
            );
            assert!(keys.contains("COVERING INDEX jobs_request"), "{keys}");
        }
        for (filter, index) in [
            (Some("state"), "jobs_by_state"),
            (Some("operation"), "jobs_by_operation"),
            (Some("backend"), "jobs_by_backend"),
            (None, "INTEGER PRIMARY KEY"),
        ] {
            let mut f = JobFilter::default();
            match filter {
                Some("state") => f.state = Some("QUEUED".into()),
                Some("operation") => f.operation = Some("op".into()),
                Some("backend") => f.backend = Some("asir".into()),
                _ => {}
            }
            let (sql, _) = scan_by_seq(&f);
            let sql = sql
                .replace("?1", "0")
                .replace("?2", "10")
                .replace("?3", "'x'");
            let scan = plan(&sql);
            assert!(
                scan.contains(index) && !scan.contains("TEMP B-TREE"),
                "{scan}"
            );
        }
    }

    /// Issue #63: a listing reads the columns beside the record and never the
    /// record, and a call looks at a bounded number of jobs and returns a
    /// bounded page, with 10 000 jobs as with a few.
    #[test]
    fn a_listing_is_bounded_and_does_not_read_the_records() {
        let store = Store::open(None, None).unwrap();
        for i in 0..10_000 {
            let state = if i % 1000 == 0 {
                JobState::Failed
            } else {
                JobState::Succeeded
            };
            listed(
                &store,
                "asir",
                "polynomial.factor",
                state,
                Some(&format!("k{i:05}")),
            );
        }
        // Records that cannot be read: a listing that read them would fail.
        store
            .conn
            .execute("UPDATE jobs SET record_json = 'not json'", [])
            .unwrap();
        let limits = ListLimits {
            page_size: 100,
            max_bytes: 1 << 20,
            max_scanned: 1000,
            max_scanned_bytes: usize::MAX,
        };
        let started = std::time::Instant::now();
        let page = store
            .list_jobs(&JobFilter::default(), None, &limits)
            .unwrap();
        let first = started.elapsed();
        assert_eq!(page.jobs.len(), 100);
        assert!(first < std::time::Duration::from_secs(1), "{first:?}");
        // Ten failed jobs among the 10 000: the state leads the scan, which
        // looks at them alone.
        let filter = JobFilter {
            backend: Some("asir".into()),
            state: Some("FAILED".into()),
            ..Default::default()
        };
        let (jobs, pages) = every_page(&store, &filter, limits);
        assert_eq!((jobs.len(), pages), (10, 1));
        // No job passes, and the operation leads the scan: each call looks at
        // 1000 jobs.
        let filter = JobFilter {
            operation: Some("polynomial.factor".into()),
            backend: Some("m2".into()),
            ..Default::default()
        };
        let (jobs, pages) = every_page(&store, &filter, limits);
        assert!(jobs.is_empty());
        assert_eq!(pages, 11, "1000 jobs looked at by each call");
        let (jobs, _) = every_page(
            &store,
            &JobFilter {
                request_id_prefix: "k01".into(),
                ..Default::default()
            },
            limits,
        );
        assert_eq!(jobs.len(), 1000);
        // A page holds at most `max_bytes`, and the next job would pass it
        // (Issue #76: the job that would is left for the next page).
        let small = ListLimits {
            page_size: 1000,
            max_bytes: 1000,
            max_scanned: 1000,
            max_scanned_bytes: usize::MAX,
        };
        let page = store
            .list_jobs(&JobFilter::default(), None, &small)
            .unwrap();
        let bytes: usize = page.jobs.iter().map(ListedJob::size).sum();
        let last = page.jobs.last().unwrap().size();
        assert!(bytes <= 1000 && bytes + last > 1000, "{bytes}");
    }

    /// Issue #76: names of 1 MiB, which an older scheduler accepted. A call
    /// counts the bytes of the jobs it looks at, those the filters reject
    /// included, and a page passes `max_bytes` only with a single job; the
    /// pages still give every job once, in order.
    #[test]
    fn large_names_bound_the_jobs_a_call_looks_at() {
        let store = Store::open(None, None).unwrap();
        let ids: Vec<String> = (0..20)
            .map(|_| listed(&store, "asir", "polynomial.factor", JobState::Queued, None))
            .collect();
        let huge = "a".repeat(1 << 20);
        store
            .conn
            .execute(
                "UPDATE jobs SET backend = ?1 WHERE seq IN \
                 (SELECT seq FROM jobs ORDER BY seq LIMIT 10)",
                [&huge],
            )
            .unwrap();
        let limits = ListLimits {
            page_size: 100,
            max_bytes: 64 << 10,
            max_scanned: 10_000,
            max_scanned_bytes: 3 << 20,
        };
        // The ten large jobs are rejected by the backend, three by each call
        // (their bytes reach 3 MiB), and the ten small ones follow.
        let asir = JobFilter {
            operation: Some("polynomial.factor".into()),
            backend: Some("asir".into()),
            ..Default::default()
        };
        let (jobs, pages) = every_page(&store, &asir, limits);
        assert_eq!(jobs, ids[10..]);
        assert_eq!(pages, 4);
        // Without filters, each large job is on a page of its own.
        let (mut after, mut sizes, mut jobs) = (None, Vec::new(), Vec::new());
        loop {
            let page = store
                .list_jobs(&JobFilter::default(), after.as_ref(), &limits)
                .unwrap();
            let bytes: usize = page.jobs.iter().map(ListedJob::size).sum();
            assert!(bytes <= limits.max_bytes || page.jobs.len() == 1, "{bytes}");
            sizes.push(page.jobs.len());
            jobs.extend(page.jobs.into_iter().map(|j| j.job_id));
            match page.next {
                Some(next) => after = Some(next),
                None => break,
            }
        }
        assert_eq!(jobs, ids);
        assert_eq!(sizes, [1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 10]);
    }

    #[test]
    fn an_older_database_gains_the_request_columns() {
        // Issue #33: the first schema, the first schema with one of the two
        // request columns (a migration interrupted by an older scheduler),
        // every column without the indexes, and the current schema.
        let first = "CREATE TABLE jobs (seq INTEGER PRIMARY KEY AUTOINCREMENT,
            job_id TEXT NOT NULL UNIQUE, state TEXT NOT NULL, backend TEXT NOT NULL,
            operation TEXT NOT NULL, priority INTEGER NOT NULL, agent_id TEXT,
            cancel_requested INTEGER NOT NULL DEFAULT 0, retried_as TEXT,
            record_json TEXT NOT NULL);";
        let cases = [
            first.to_owned(),
            format!("{first} ALTER TABLE jobs ADD COLUMN request_id TEXT;"),
            format!(
                "{first} ALTER TABLE jobs ADD COLUMN request_id TEXT;
                 ALTER TABLE jobs ADD COLUMN request_fingerprint TEXT;
                 ALTER TABLE jobs ADD COLUMN placement TEXT;"
            ),
            String::new(),
        ];
        for (n, setup) in cases.iter().enumerate() {
            let temp = tempfile::Builder::new()
                .prefix("algebramesh-migrate-")
                .tempdir()
                .unwrap();
            let dir = temp.path().to_path_buf();
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("meta.sqlite3");
            {
                let conn = Connection::open(&path).unwrap();
                conn.execute_batch(setup).unwrap();
                // A job of the old database, without a placement.
                if !setup.is_empty() {
                    let old = record("old");
                    conn.execute(
                        "INSERT INTO jobs (job_id, state, backend, operation, priority, record_json)
                         VALUES (?1, ?2, 'asir', 'polynomial.factor', 0, ?3)",
                        params![
                            old.job_id.to_string(),
                            state_name(JobState::Queued),
                            serde_json::to_string(&old).unwrap()
                        ],
                    )
                    .unwrap();
                }
            }
            for _ in 0..2 {
                // Opened twice: the second time finds nothing to do.
                let store = Store::open(Some(&path), None).unwrap();
                drop(store);
            }
            let store = Store::open(Some(&path), None).unwrap();
            let mut row = JobRow::new(record("a"), 0);
            row.request = Some(RequestKey {
                request_id: "k".into(),
                fingerprint: "f".into(),
            });
            store.insert_job(&row).unwrap();
            let (found, fingerprint) = store.job_for_request("k").unwrap().unwrap();
            assert_eq!(found.record.job_id, row.record.job_id, "case {n}");
            assert_eq!(fingerprint, "f", "case {n}");
            // The indexes of the listings are there (Issue #63).
            for index in ["jobs_by_state", "jobs_by_backend", "jobs_by_operation"] {
                let present = store
                    .conn
                    .prepare("SELECT 1 FROM sqlite_master WHERE type = 'index' AND name = ?1")
                    .unwrap()
                    .exists([index])
                    .unwrap();
                assert!(present, "case {n}: {index}");
            }
            // Every queued job, the old one included, has its placement.
            let placed = store.queued_placements(&["asir".into()]).unwrap();
            assert_eq!(
                placed.len(),
                if setup.is_empty() { 1 } else { 2 },
                "case {n}"
            );
            drop(store);
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn a_working_copy_sends_its_changes_to_the_sink() {
        let state = Store::open(None, None).unwrap();
        state.insert_job(&JobRow::new(record("a"), 0)).unwrap();
        let mut working = Store::open(None, None).unwrap();
        working.restore(&state.snapshot().unwrap()).unwrap();
        let (sink, mut changes) = tokio::sync::mpsc::unbounded_channel();
        working.set_change_sink(Some(sink));

        let mut row = JobRow::new(record("b"), 0);
        working.insert_job(&row).unwrap();
        row.record
            .transition(JobState::Cancelled, Utc::now())
            .unwrap();
        working.update_job(&row).unwrap();
        let mut batch = Vec::new();
        while let Ok(change) = changes.try_recv() {
            batch.push(change);
        }
        assert_eq!(batch.iter().map(|c| c.0).collect::<Vec<_>>(), [2, 3]);

        // Applied elsewhere in one transaction, with a meta key.
        state
            .apply_batch(&batch, &[("raft_applied", "x".into())])
            .unwrap();
        assert_eq!(state.last_seq().unwrap(), 3);
        assert_eq!(state.meta("raft_applied").unwrap().as_deref(), Some("x"));
        let copy = state
            .get_job(&row.record.job_id.to_string())
            .unwrap()
            .unwrap();
        assert_eq!(copy.record.state, JobState::Cancelled);
        // A batch that does not follow the last change is refused as a whole.
        assert!(matches!(
            state.apply_batch(&batch, &[("raft_applied", "y".into())]),
            Err(StoreError::OutOfOrder(2, 3))
        ));
        assert_eq!(state.meta("raft_applied").unwrap().as_deref(), Some("x"));
    }
}

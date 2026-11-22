//! Job queue, placement, agent liveness and state changes (design §7.2).
//!
//! All state changes go through [`JobRecord::transition`]/[`JobRecord::complete`],
//! so the recorded history always follows the state machine. Exactly-once execution
//! is not promised: a job whose agent stops sending heartbeats becomes `LOST`, and
//! only jobs with `retry_policy = if_deterministic` are resubmitted, as a new job.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use algebramesh_core::blob::BlobRef;
use algebramesh_core::{
    Capabilities, CasError, CasResult, JobId, JobRecord, JobSpec, JobState, RetryPolicy,
};
use algebramesh_rpc::{PeerIdentity, pb};
use chrono::Utc;
use thiserror::Error;
use tokio::sync::Notify;

use crate::store::{Change, JobRow, ListedJob, Placement, RequestKey, Store, StoreError};

#[derive(Debug, Error)]
pub enum SchedulerError {
    #[error("invalid job specification: {0}")]
    InvalidSpec(String),
    #[error("unknown job {0}")]
    UnknownJob(String),
    #[error("unknown agent {0}")]
    UnknownAgent(String),
    #[error("{0}")]
    Forbidden(String),
    #[error("{0}")]
    Conflict(String),
    /// No worker can run the job as asked (e.g. none enforces its limits).
    #[error("{0}")]
    Unplaceable(String),
    /// The queue is full (Issue #40).
    #[error("{0}")]
    Exhausted(String),
    #[error(transparent)]
    Store(#[from] StoreError),
}

#[derive(Debug, Clone)]
pub struct SchedulerOptions {
    pub heartbeat_interval: Duration,
    /// An agent silent for this long is offline and its jobs are `LOST`.
    pub heartbeat_timeout: Duration,
    /// Upper bound for automatic attempts of deterministic jobs.
    pub max_attempts: u32,
    pub max_poll_wait: Duration,
    /// Upper bound for the jobs in `QUEUED` (Issue #40). A submission of a new
    /// job beyond it is refused; a known `request_id` and the retry of a `LOST`
    /// job are not.
    pub max_queued_jobs: u64,
    /// The blob store of the node (ADR 0021).
    pub blobs: crate::blobs::BlobOptions,
}

/// The default of [`SchedulerOptions::max_queued_jobs`].
pub const DEFAULT_MAX_QUEUED_JOBS: u64 = 10_000;

impl Default for SchedulerOptions {
    fn default() -> Self {
        Self {
            heartbeat_interval: Duration::from_secs(2),
            heartbeat_timeout: Duration::from_secs(15),
            max_attempts: 3,
            max_poll_wait: Duration::from_secs(20),
            max_queued_jobs: DEFAULT_MAX_QUEUED_JOBS,
            blobs: crate::blobs::BlobOptions::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AgentInfo {
    pub agent_id: String,
    pub name: String,
    pub host: String,
    pub offers: Vec<pb::BackendOffer>,
    pub last_heartbeat: Instant,
    pub last_heartbeat_unix_ms: i64,
    pub online: bool,
    /// The verified client certificate the agent registered with (mutual TLS).
    pub identity: Option<PeerIdentity>,
    /// When each job assigned to the agent was last listed in its heartbeat (or
    /// assigned). A job missing for longer than the heartbeat timeout is `LOST`.
    pub jobs_seen: HashMap<String, Instant>,
    /// The features the agent said it can use (`RegisterRequest.features`).
    pub features: Vec<String>,
}

impl AgentInfo {
    fn offer(&self, backend: &str) -> Option<&pb::BackendOffer> {
        self.offers.iter().find(|o| o.name == backend)
    }

    fn capabilities(&self, backend: &str) -> Option<Capabilities> {
        self.offer(backend)
            .map(|offer| serde_json::from_str(&offer.capabilities_json).unwrap_or_default())
    }

    /// The agent offers the backend and operation, the environment allows it,
    /// and it enforces every limit the job asks for. Decided from the job's
    /// placement alone, without its record (Issue #19).
    fn can_place(&self, job: &Placement) -> bool {
        self.offer(&job.backend).is_some_and(|offer| {
            let caps: Capabilities =
                serde_json::from_str(&offer.capabilities_json).unwrap_or_default();
            caps.supports(&job.operation)
                && job.environment.allows(&self.name, &offer.build)
                && caps.unenforced(&job.limits).is_empty()
                && (!job.needs_blob_refs || self.reads_blob_arguments())
                && (!job.needs_record_refs || self.reads_record_refs())
        })
    }

    /// The agent resolves references `{"$ref": ...}` in arguments (W2).
    fn reads_record_refs(&self) -> bool {
        self.features
            .iter()
            .any(|f| f == algebramesh_core::features::WORKFLOW_REFS)
    }

    /// The agent reads blob markers in arguments (ADR 0021 §12).
    fn reads_blob_arguments(&self) -> bool {
        self.features
            .iter()
            .any(|f| f == algebramesh_core::features::BLOB_ARGUMENTS)
    }
}

struct Inner {
    store: Store,
    agents: HashMap<String, AgentInfo>,
}

pub struct Scheduler {
    inner: Mutex<Inner>,
    /// Wakes long-polling agents when a job is queued.
    work: Notify,
    pub options: SchedulerOptions,
    /// Epoch (the Raft term of the leader, ADR 0013), from the store.
    epoch: u64,
}

const ACTIVE: [JobState; 3] = [JobState::Assigned, JobState::Running, JobState::Cancelling];

/// The capabilities of several workers of one backend, combined (Issue #15):
///
/// * `operations`, `supported_types`: what any of them offers.
/// * `enforced_limits`: the limits every one of them enforces, so that they
///   hold wherever a job runs.
/// * `max_precision`: the smallest, if every one reports one.
/// * `signatures`: the signature of each operation that the workers which
///   declare one declare alike. A worker that declares none (built before
///   signatures) does not count. An operation declared with different
///   signatures goes in `signature_conflicts` instead (Issue #54, ADR 0022).
///
/// Also returns the limits at least one of them enforces. A job asking for
/// one of those is accepted only if a worker that offers its operation
/// enforces it (Issue #55), and is placed only on such a worker.
pub fn combine(offers: &[Capabilities]) -> (Capabilities, Vec<String>) {
    let mut combined = Capabilities::default();
    let mut available: Vec<String> = Vec::new();
    let mut conflicts = std::collections::BTreeSet::new();
    for caps in offers {
        for (op, signature) in &caps.signatures {
            match combined.signatures.get(op) {
                Some(seen) if seen != signature => {
                    conflicts.insert(op.clone());
                }
                Some(_) => {}
                None => {
                    combined.signatures.insert(op.clone(), signature.clone());
                }
            }
        }
        for op in &caps.operations {
            if !combined.operations.contains(op) {
                combined.operations.push(op.clone());
            }
        }
        for ty in &caps.supported_types {
            if !combined.supported_types.contains(ty) {
                combined.supported_types.push(ty.clone());
            }
        }
        for limit in &caps.enforced_limits {
            if !available.contains(limit) {
                available.push(limit.clone());
            }
        }
    }
    combined.enforced_limits = available
        .iter()
        .filter(|l| offers.iter().all(|c| c.enforces(l)))
        .cloned()
        .collect();
    combined.max_precision = offers
        .iter()
        .map(|c| c.max_precision)
        .collect::<Option<Vec<u32>>>()
        .and_then(|p| p.into_iter().min());
    for op in &conflicts {
        combined.signatures.remove(op);
    }
    combined.signature_conflicts = conflicts.into_iter().collect();
    (combined, available)
}

pub fn state_name(state: JobState) -> String {
    serde_json::to_value(state)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// A job as `ListJobs` lists it (Issue #63).
pub fn summary_of(job: ListedJob) -> pb::JobSummary {
    pb::JobSummary {
        job_id: job.job_id,
        request_id: job.request_id.unwrap_or_default(),
        state: job.state,
        backend: job.backend,
        operation: job.operation,
        agent_id: job.agent_id.unwrap_or_default(),
        retried_as: job.retried_as.unwrap_or_default(),
        cancel_requested: job.cancel_requested,
    }
}

pub fn status_of(row: &JobRow) -> pb::JobStatus {
    let r = &row.record;
    pb::JobStatus {
        job_id: r.job_id.to_string(),
        state: state_name(r.state),
        backend: r.spec.backend.clone(),
        operation: r.spec.operation.clone(),
        attempt: r.attempt,
        agent_id: r.agent_id.clone().unwrap_or_default(),
        error_json: r
            .error
            .as_ref()
            .and_then(|e| serde_json::to_string(e).ok())
            .unwrap_or_default(),
        retried_as: row.retried_as.map(|j| j.to_string()).unwrap_or_default(),
        cancel_requested: row.cancel_requested,
    }
}

/// The prefix of a request fingerprint of version 2 (ADR 0021 §13); one of
/// version 1 has none.
const FINGERPRINT_V2: &str = "v2:";

/// The request fingerprint of version 1: the hash of the recorded spec and
/// the priority.
fn fingerprint_v1(input_sha256: &str, priority: i32) -> String {
    algebramesh_core::sha256_hex(format!("{input_sha256}\n{priority}").as_bytes())
}

/// The request fingerprint of version 2, without its prefix: the hash of the
/// spec with its arguments in content form, and the priority. The same
/// arguments give the same fingerprint inline or in blobs.
fn fingerprint_v2(spec: &JobSpec, priority: i32) -> String {
    let mut value = serde_json::to_value(spec).unwrap_or_default();
    value["arguments"] = algebramesh_core::blob::content_form(&spec.arguments);
    let canonical = algebramesh_core::canonical_json(&value);
    algebramesh_core::sha256_hex(format!("{canonical}\n{priority}").as_bytes())
}

/// The blobs the arguments of `spec` refer to (ADR 0021 §3): an invalid spec
/// if a top-level value looks like a marker but is not one.
pub fn argument_blobs(spec: &JobSpec) -> Result<Vec<BlobRef>, SchedulerError> {
    algebramesh_core::blob::argument_markers(&spec.arguments)
        .map(|m| m.into_iter().map(|(_, r)| r).collect())
        .map_err(|e| SchedulerError::InvalidSpec(format!("arguments: {e}")))
}

/// The references among the arguments of `spec`: markers and `{"$ref": ...}`
/// (W2); an invalid spec if a top-level value looks like one but is not.
pub fn argument_references(
    spec: &JobSpec,
) -> Result<Vec<(String, algebramesh_core::blob::ArgumentRef)>, SchedulerError> {
    algebramesh_core::blob::argument_refs(&spec.arguments)
        .map_err(|e| SchedulerError::InvalidSpec(format!("arguments: {e}")))
}

/// Refuses the references to other jobs' results (`$ref`, and markers with
/// `from`) among `refs` while the cluster has not enabled `workflow_refs`:
/// a scheduler without it would neither check them nor keep their
/// placement (W2, ADR 0032).
pub fn check_references_allowed(
    refs: &[(String, algebramesh_core::blob::ArgumentRef)],
    workflow_refs: bool,
) -> Result<(), SchedulerError> {
    if workflow_refs {
        return Ok(());
    }
    match refs.iter().find(|(_, r)| r.origin().is_some()) {
        Some((key, _)) => Err(SchedulerError::Unplaceable(format!(
            "argument {key} refers to a value of another job's result, which this cluster \
             does not check yet (feature workflow_refs); upgrade every scheduler, or send \
             the value itself"
        ))),
        None => Ok(()),
    }
}

impl Scheduler {
    /// Opens the scheduler over `store`. Jobs that were assigned to an agent when the
    /// previous scheduler stopped have an unknown outcome and become `LOST`.
    pub fn new(store: Store, options: SchedulerOptions) -> Result<Self, SchedulerError> {
        let epoch = store.epoch()?;
        let scheduler = Self {
            inner: Mutex::new(Inner {
                store,
                agents: HashMap::new(),
            }),
            work: Notify::new(),
            options,
            epoch,
        };
        {
            let inner = scheduler.lock();
            let stale = inner.store.jobs_in_states(&ACTIVE)?;
            for row in stale {
                scheduler.declare_lost(
                    &inner,
                    row,
                    "the scheduler restarted while the job was assigned",
                )?;
            }
        }
        Ok(scheduler)
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Runs `f` on the store, for replication; scheduling waits meanwhile.
    pub fn with_store<R>(&self, f: impl FnOnce(&Store) -> R) -> R {
        f(&self.lock().store)
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A poisoned lock means a panic while holding it; the data is still usable.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn job(inner: &Inner, job_id: &str) -> Result<JobRow, SchedulerError> {
        inner
            .store
            .get_job(job_id)?
            .ok_or_else(|| SchedulerError::UnknownJob(job_id.to_owned()))
    }

    // ----------------------------------------------------------------- clients

    /// Creates a job. With a `request_id` (an idempotency key), a key already
    /// seen returns the job it created, so that a client may send a submission
    /// again after losing the answer; the same key with another specification or
    /// priority is refused. A key already seen is resolved before the limits are
    /// checked against the workers online now, so the answer for a known key does
    /// not depend on which workers are online.
    pub fn submit(
        &self,
        spec_json: &str,
        priority: i32,
        request_id: &str,
    ) -> Result<pb::JobStatus, SchedulerError> {
        self.submit_changed(spec_json, priority, request_id)
            .map(|(status, _)| status)
    }

    /// [`Self::submit`], with whether it wrote to the store: `false` for a
    /// known `request_id`. Decided under the lock of the submission itself, so
    /// that other calls' writes do not count (Issue #92).
    pub(crate) fn submit_changed(
        &self,
        spec_json: &str,
        priority: i32,
        request_id: &str,
    ) -> Result<(pb::JobStatus, bool), SchedulerError> {
        if spec_json.len() > algebramesh_core::MAX_SPEC_BYTES {
            return Err(SchedulerError::InvalidSpec(format!(
                "the specification of {} bytes exceeds the limit of {} bytes",
                spec_json.len(),
                algebramesh_core::MAX_SPEC_BYTES
            )));
        }
        let spec = JobSpec::from_json(spec_json)
            .map_err(|e| SchedulerError::InvalidSpec(e.to_string()))?;
        let mut row = JobRow::new(JobRecord::new(JobId::new(), spec, Utc::now()), priority);
        let request_id = request_id.trim();
        if request_id.len() > 128 {
            return Err(SchedulerError::InvalidSpec(
                "request_id is longer than 128 characters".into(),
            ));
        }
        {
            let inner = self.lock();
            let enabled = inner.store.enabled_features()?;
            let blob_arguments = enabled
                .iter()
                .any(|f| f == algebramesh_core::features::BLOB_ARGUMENTS);
            // Markers in the arguments only once every scheduler keeps them
            // (ADR 0021 §12); the service checked that a majority holds their
            // blobs before this call. Before, the arguments are plain values,
            // as they always were.
            let (has_markers, has_record_refs) = if blob_arguments {
                let refs = argument_references(&row.record.spec)?;
                check_references_allowed(
                    &refs,
                    enabled
                        .iter()
                        .any(|f| f == algebramesh_core::features::WORKFLOW_REFS),
                )?;
                (
                    refs.iter().any(|(_, r)| {
                        matches!(r, algebramesh_core::blob::ArgumentRef::Blob { .. })
                    }),
                    refs.iter()
                        .any(|(_, r)| matches!(r, algebramesh_core::blob::ArgumentRef::Record(_))),
                )
            } else {
                (false, false)
            };
            if !request_id.is_empty() {
                if let Some((existing, known)) = inner.store.job_for_request(request_id)? {
                    let same = if let Some(v2) = known.strip_prefix(FINGERPRINT_V2) {
                        v2 == fingerprint_v2(&row.record.spec, priority)
                    } else if has_markers {
                        return Err(SchedulerError::Conflict(format!(
                            "request_id {request_id} was used for job {} before this cluster \
                             took blobs in arguments; it cannot be compared with a \
                             specification that refers to blobs: use a new request_id",
                            existing.record.job_id
                        )));
                    } else {
                        known == fingerprint_v1(&row.record.input_sha256, priority)
                    };
                    if !same {
                        return Err(SchedulerError::Conflict(format!(
                            "request_id {request_id} was used for job {} with a different \
                             specification or priority",
                            existing.record.job_id
                        )));
                    }
                    return Ok((status_of(&existing), false));
                }
                let fingerprint = if blob_arguments {
                    format!(
                        "{FINGERPRINT_V2}{}",
                        fingerprint_v2(&row.record.spec, priority)
                    )
                } else {
                    fingerprint_v1(&row.record.input_sha256, priority)
                };
                row.request = Some(RequestKey {
                    request_id: request_id.to_owned(),
                    fingerprint,
                });
            }
            // Only a new job is checked against the queue and the workers online
            // now; under the same lock as the insertion.
            let queued = inner.store.count_jobs(JobState::Queued)?;
            if queued >= self.options.max_queued_jobs {
                return Err(SchedulerError::Exhausted(format!(
                    "{queued} jobs are queued, the limit of the scheduler \
                     (max_queued_jobs = {}); submit again when some have started",
                    self.options.max_queued_jobs
                )));
            }
            Self::check_placeable(&inner, &row.record.spec, has_markers, has_record_refs)?;
            inner.store.insert_job(&row)?;
            inner.store.add_event(&row.record.job_id, "submitted")?;
        }
        self.work.notify_waiters();
        Ok((status_of(&row), true))
    }

    /// Refuses a job that no online worker of its backend could run: none
    /// offers its operation (Issue #55), or none of those that offer it
    /// enforces its limits (ADR 0014), as placement needs both of one worker.
    /// With no worker of the backend online the job is queued: one may still
    /// register, as every agent does again after a change of leader.
    fn check_placeable(
        inner: &Inner,
        spec: &JobSpec,
        needs_blob_refs: bool,
        needs_record_refs: bool,
    ) -> Result<(), SchedulerError> {
        let offers: Vec<Capabilities> = inner
            .agents
            .values()
            .filter(|a| a.online)
            .filter(|a| !needs_blob_refs || a.reads_blob_arguments())
            .filter(|a| !needs_record_refs || a.reads_record_refs())
            .filter_map(|a| a.capabilities(&spec.backend))
            .filter(|c| c.supports(&spec.operation))
            .collect();
        if offers.is_empty() {
            let online = inner
                .agents
                .values()
                .any(|a| a.online && a.offer(&spec.backend).is_some());
            if !online {
                return Ok(());
            }
            if needs_blob_refs || needs_record_refs {
                let what = match (needs_blob_refs, needs_record_refs) {
                    (true, true) => "blobs and references",
                    (false, true) => "references",
                    _ => "blobs",
                };
                return Err(SchedulerError::Unplaceable(format!(
                    "no online worker of backend {} reads {what} in arguments; send the \
                     arguments inline, or upgrade the workers",
                    spec.backend
                )));
            }
            return Err(SchedulerError::Unplaceable(format!(
                "no online worker of backend {} offers the operation {}; check the name, \
                 or run a worker whose backend offers it (algebramesh backends lists them)",
                spec.backend, spec.operation
            )));
        }
        if offers.iter().any(|c| c.unenforced(&spec.limits).is_empty()) {
            return Ok(());
        }
        let missing = offers[0].unenforced(&spec.limits);
        Err(SchedulerError::Unplaceable(format!(
            "no online worker of backend {} that offers {} enforces limits.{}; leave the \
             limit out, or run a worker whose backend enforces it",
            spec.backend,
            spec.operation,
            missing.join(", limits.")
        )))
    }

    pub fn status(&self, job_id: &str) -> Result<pb::JobStatus, SchedulerError> {
        Self::status_in(&self.lock().store, job_id)
    }

    pub fn result(&self, job_id: &str) -> Result<pb::JobResult, SchedulerError> {
        Self::result_in(&self.lock().store, job_id)
    }

    /// The status of a job as `store` records it. A cluster answers clients
    /// from its committed state with this (Issue #13), not from the leader's
    /// working copy.
    pub fn status_in(store: &Store, job_id: &str) -> Result<pb::JobStatus, SchedulerError> {
        let row = store
            .get_job(job_id)?
            .ok_or_else(|| SchedulerError::UnknownJob(job_id.to_owned()))?;
        Ok(status_of(&row))
    }

    /// The status of a job and, once it is terminal, its record, as `store`
    /// records them.
    pub fn result_in(store: &Store, job_id: &str) -> Result<pb::JobResult, SchedulerError> {
        let row = store
            .get_job(job_id)?
            .ok_or_else(|| SchedulerError::UnknownJob(job_id.to_owned()))?;
        let record_json = if row.record.state.is_terminal() {
            serde_json::to_string(&row.record).map_err(StoreError::from)?
        } else {
            String::new()
        };
        Ok(pb::JobResult {
            status: Some(status_of(&row)),
            record_json,
        })
    }

    pub fn cancel(&self, job_id: &str) -> Result<pb::JobStatus, SchedulerError> {
        self.cancel_changed(job_id).map(|(status, _)| status)
    }

    /// [`Self::cancel`], with whether it wrote to the store: `false` for a
    /// finished job. Decided under the lock of the cancellation itself, so
    /// that other calls' writes do not count (Issue #92).
    pub(crate) fn cancel_changed(
        &self,
        job_id: &str,
    ) -> Result<(pb::JobStatus, bool), SchedulerError> {
        let inner = self.lock();
        let mut row = Self::job(&inner, job_id)?;
        let now = Utc::now();
        match row.record.state {
            JobState::Queued => {
                row.record
                    .transition(JobState::Cancelled, now)
                    .map_err(|e| SchedulerError::InvalidSpec(e.to_string()))?;
                row.record.error = Some(CasError::Cancelled);
            }
            JobState::Running => {
                row.cancel_requested = true;
                row.record
                    .transition(JobState::Cancelling, now)
                    .map_err(|e| SchedulerError::InvalidSpec(e.to_string()))?;
            }
            // The agent reports RUNNING first; the request is delivered afterwards.
            JobState::Assigned | JobState::Cancelling => row.cancel_requested = true,
            _ => return Ok((status_of(&row), false)),
        }
        inner.store.update_job(&row)?;
        inner
            .store
            .add_event(&row.record.job_id, "cancel requested")?;
        Ok((status_of(&row), true))
    }

    /// The backends of the online workers, each with its capabilities combined
    /// over those workers (Issue #15; see [`combine`]), and the number of queued
    /// jobs with its limit (Issue #40).
    pub fn backends(&self) -> Result<pb::ListBackendsResponse, SchedulerError> {
        let inner = self.lock();
        let queued_jobs = inner.store.count_jobs(JobState::Queued)?;
        let mut by_name: HashMap<String, (Vec<Capabilities>, Vec<String>)> = HashMap::new();
        for agent in inner.agents.values().filter(|a| a.online) {
            for offer in &agent.offers {
                let caps: Capabilities =
                    serde_json::from_str(&offer.capabilities_json).unwrap_or_default();
                let entry = by_name.entry(offer.name.clone()).or_default();
                entry.0.push(caps);
                entry.1.push(agent.agent_id.clone());
            }
        }
        let mut backends: Vec<_> = by_name
            .into_iter()
            .map(|(name, (offers, agent_ids))| {
                let (caps, available_limits) = combine(&offers);
                pb::BackendSummary {
                    name,
                    capabilities_json: serde_json::to_string(&caps).unwrap_or_default(),
                    agent_ids,
                    available_limits,
                }
            })
            .collect();
        backends.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(pb::ListBackendsResponse {
            backends,
            queued_jobs,
            max_queued_jobs: self.options.max_queued_jobs,
            server_features: inner.store.enabled_features()?,
        })
    }

    pub fn workers(&self) -> Result<pb::ListWorkersResponse, SchedulerError> {
        let inner = self.lock();
        let active = inner.store.jobs_in_states(&ACTIVE)?;
        let mut workers: Vec<_> = inner
            .agents
            .values()
            .map(|a| pb::WorkerSummary {
                agent_id: a.agent_id.clone(),
                name: a.name.clone(),
                host: a.host.clone(),
                online: a.online,
                last_heartbeat_unix_ms: a.last_heartbeat_unix_ms,
                backends: a.offers.clone(),
                running_job_ids: active
                    .iter()
                    .filter(|r| r.record.agent_id.as_deref() == Some(&a.agent_id))
                    .map(|r| r.record.job_id.to_string())
                    .collect(),
            })
            .collect();
        workers.sort_by(|a, b| a.name.cmp(&b.name).then(a.agent_id.cmp(&b.agent_id)));
        Ok(pb::ListWorkersResponse { workers })
    }

    // ------------------------------------------------------------------ agents

    /// Registers an agent. With a client certificate (`identity`), the agent must
    /// register under the name in the certificate, and every later call for the
    /// new agent id must come with the same certificate ([`Self::authorize`]).
    pub fn register(
        &self,
        request: pb::RegisterRequest,
        identity: Option<&PeerIdentity>,
    ) -> Result<pb::RegisterResponse, SchedulerError> {
        if let Some(id) = identity
            && id.name != request.name
        {
            return Err(SchedulerError::Forbidden(format!(
                "the client certificate is for agent {:?}, not {:?}",
                id.name, request.name
            )));
        }
        let agent_id = uuid::Uuid::new_v4().to_string();
        let info = AgentInfo {
            agent_id: agent_id.clone(),
            name: request.name.clone(),
            host: request.host.clone(),
            offers: request.backends.clone(),
            last_heartbeat: Instant::now(),
            last_heartbeat_unix_ms: Utc::now().timestamp_millis(),
            online: true,
            identity: identity.cloned(),
            jobs_seen: HashMap::new(),
            features: request.features.clone(),
        };
        let features = {
            let mut inner = self.lock();
            let offers: Vec<_> = request
                .backends
                .iter()
                .map(|o| {
                    serde_json::json!({
                        "name": o.name, "slots": o.slots, "build": o.build,
                        "backend_version": o.backend_version,
                        "capabilities": serde_json::from_str::<serde_json::Value>(&o.capabilities_json).ok(),
                    })
                })
                .collect();
            let mut info_json = serde_json::json!({
                "agent_version": request.agent_version, "backends": offers,
                "certificate_serial": identity.map(|i| i.serial.clone()),
            });
            // Only when there are some, so that the record of an agent that
            // names none stays as before.
            if !request.features.is_empty() {
                info_json["features"] = serde_json::json!(request.features);
            }
            inner.store.record_agent(
                &agent_id,
                &request.name,
                &request.host,
                &info_json.to_string(),
            )?;
            inner.agents.insert(agent_id.clone(), info);
            inner.store.enabled_features()?
        };
        tracing::info!(%agent_id, name = %request.name, host = %request.host, "agent registered");
        self.work.notify_waiters();
        Ok(pb::RegisterResponse {
            agent_id,
            heartbeat_interval_ms: self.options.heartbeat_interval.as_millis() as u32,
            epoch: self.epoch,
            features,
        })
    }

    /// Checks that a call for `agent_id` comes with the certificate the agent
    /// registered with. Without mutual TLS (`identity` is `None`) any caller with
    /// the agent token may act for any agent. An unknown agent id passes, so that
    /// the call itself reports it and the agent registers again.
    pub fn authorize(
        &self,
        agent_id: &str,
        identity: Option<&PeerIdentity>,
    ) -> Result<(), SchedulerError> {
        let Some(identity) = identity else {
            return Ok(());
        };
        match self.lock().agents.get(agent_id) {
            Some(agent) if agent.identity.as_ref() != Some(identity) => {
                Err(SchedulerError::Forbidden(format!(
                    "agent {agent_id} registered with another client certificate"
                )))
            }
            _ => Ok(()),
        }
    }

    pub fn heartbeat(
        &self,
        request: &pb::HeartbeatRequest,
    ) -> Result<pb::HeartbeatResponse, SchedulerError> {
        let mut inner = self.lock();
        let Some(agent) = inner.agents.get_mut(&request.agent_id) else {
            return Ok(pb::HeartbeatResponse {
                cancel_job_ids: vec![],
                known: false,
                epoch: self.epoch,
                features: Vec::new(),
            });
        };
        if !agent.online {
            // Its jobs were already declared LOST; it must start over.
            return Ok(pb::HeartbeatResponse {
                cancel_job_ids: request.running_job_ids.clone(),
                known: false,
                epoch: self.epoch,
                features: Vec::new(),
            });
        }
        let now = Instant::now();
        agent.last_heartbeat = now;
        agent.last_heartbeat_unix_ms = Utc::now().timestamp_millis();
        for id in &request.running_job_ids {
            agent.jobs_seen.insert(id.clone(), now);
        }
        let mut cancel = Vec::new();
        // Only the columns needed, not the records (Issue #16).
        for id in &request.running_job_ids {
            match inner.store.job_brief(id)? {
                Some(job)
                    if job.agent_id.as_deref() == Some(&request.agent_id)
                        && !job.state.is_terminal() =>
                {
                    if job.cancel_requested {
                        cancel.push(id.clone());
                    }
                }
                // Unknown, finished elsewhere or declared LOST: stop it.
                _ => cancel.push(id.clone()),
            }
        }
        self.reconcile(&mut inner, &request.agent_id, now)?;
        Ok(pb::HeartbeatResponse {
            cancel_job_ids: cancel,
            known: true,
            epoch: self.epoch,
            features: inner.store.enabled_features()?,
        })
    }

    /// Declares `LOST` the active jobs of `agent_id` that its heartbeats have not
    /// listed for longer than the heartbeat timeout (Issue #10): the agent no
    /// longer runs them and could not deliver their result, so they would
    /// otherwise stay active forever. The timeout covers the time between an
    /// assignment and the agent's next heartbeat.
    ///
    /// Reads only the ids of this agent's active jobs (an index on the agent and
    /// the state), and the records of the few it declares `LOST` (Issue #16):
    /// the work of a heartbeat grows with the agent's own jobs, not with all
    /// active jobs or the size of their records.
    fn reconcile(
        &self,
        inner: &mut Inner,
        agent_id: &str,
        now: Instant,
    ) -> Result<(), SchedulerError> {
        let active = inner.store.job_ids_for_agent(agent_id, &ACTIVE)?;
        let Some(agent) = inner.agents.get_mut(agent_id) else {
            return Ok(());
        };
        let timeout = self.options.heartbeat_timeout;
        let mut missing = Vec::new();
        for id in &active {
            let seen = *agent.jobs_seen.entry(id.clone()).or_insert(now);
            if now.duration_since(seen) > timeout {
                missing.push(id.clone());
            }
        }
        // Forget the jobs that are no longer active on the agent.
        let ids: std::collections::HashSet<&String> = active.iter().collect();
        agent.jobs_seen.retain(|id, _| ids.contains(id));
        for id in &missing {
            agent.jobs_seen.remove(id);
        }
        for id in missing {
            let Some(row) = inner.store.get_job(&id)? else {
                continue;
            };
            self.declare_lost(
                inner,
                row,
                &format!(
                    "agent {agent_id} has not listed the job in its heartbeats for {timeout:?}; \
                     its result was not delivered"
                ),
            )?;
        }
        Ok(())
    }

    fn try_assign(
        &self,
        agent_id: &str,
        backends: &[String],
    ) -> Result<Option<pb::Assignment>, SchedulerError> {
        let mut inner = self.lock();
        let Some(agent) = inner.agents.get(agent_id).filter(|a| a.online).cloned() else {
            return Err(SchedulerError::UnknownAgent(agent_id.to_owned()));
        };
        // Choose from the placement columns, in queue order, and read the
        // record of the chosen job only (Issue #19).
        let chosen = inner
            .store
            .queued_placements(backends)?
            .into_iter()
            .find(|(_, placement)| agent.can_place(placement));
        if let Some((job_id, _)) = chosen {
            let Some(mut row) = inner.store.get_job(&job_id)? else {
                return Ok(None);
            };
            row.record
                .transition(JobState::Assigned, Utc::now())
                .map_err(|e| SchedulerError::InvalidSpec(e.to_string()))?;
            row.record.agent_id = Some(agent_id.to_owned());
            if let Some(a) = inner.agents.get_mut(agent_id) {
                a.jobs_seen
                    .insert(row.record.job_id.to_string(), Instant::now());
            }
            if let Some(offer) = agent.offer(&row.record.spec.backend) {
                row.record.backend.build = Some(offer.build.clone()).filter(|b| !b.is_empty());
            }
            inner.store.update_job(&row)?;
            inner.store.add_event(
                &row.record.job_id,
                &format!("assigned to {} ({agent_id})", agent.name),
            )?;
            let spec_json = serde_json::to_string(&row.record.spec).map_err(StoreError::from)?;
            return Ok(Some(pb::Assignment {
                job_id: row.record.job_id.to_string(),
                attempt: row.record.attempt,
                spec_json,
            }));
        }
        Ok(None)
    }

    /// Long poll: returns a job for one of `backends`, or `None` after `wait`.
    pub async fn poll(
        &self,
        agent_id: &str,
        backends: &[String],
        wait: Duration,
    ) -> Result<Option<pb::Assignment>, SchedulerError> {
        let deadline = tokio::time::Instant::now() + wait.min(self.options.max_poll_wait);
        loop {
            let notified = self.work.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(a) = self.try_assign(agent_id, backends)? {
                return Ok(Some(a));
            }
            tokio::select! {
                _ = notified => {}
                _ = tokio::time::sleep_until(deadline) => return Ok(None),
            }
        }
    }

    fn agent_job(
        inner: &Inner,
        agent_id: &str,
        job_id: &str,
        attempt: u32,
    ) -> Result<Result<JobRow, String>, SchedulerError> {
        let row = Self::job(inner, job_id)?;
        if row.record.agent_id.as_deref() != Some(agent_id) || row.record.attempt != attempt {
            return Ok(Err("the job is not assigned to this agent".into()));
        }
        if row.record.state.is_terminal() {
            return Ok(Err(format!(
                "the job is already {}",
                state_name(row.record.state)
            )));
        }
        Ok(Ok(row))
    }

    pub fn started(&self, event: &pb::JobEvent) -> Result<pb::Ack, SchedulerError> {
        let inner = self.lock();
        let mut row = match Self::agent_job(&inner, &event.agent_id, &event.job_id, event.attempt)?
        {
            Ok(row) => row,
            Err(reason) => {
                return Ok(pb::Ack {
                    accepted: false,
                    reason,
                });
            }
        };
        if row.record.state == JobState::Assigned {
            let now = Utc::now();
            let _ = row.record.transition(JobState::Running, now);
            if row.cancel_requested {
                let _ = row.record.transition(JobState::Cancelling, now);
            }
            inner.store.update_job(&row)?;
        }
        Ok(pb::Ack {
            accepted: true,
            reason: String::new(),
        })
    }

    pub fn finished(&self, report: &pb::JobFinished) -> Result<pb::Ack, SchedulerError> {
        let inner = self.lock();
        let mut row =
            match Self::agent_job(&inner, &report.agent_id, &report.job_id, report.attempt)? {
                Ok(row) => row,
                Err(reason) => {
                    // Keep late reports for audit without changing the recorded
                    // state: who sent which outcome, by its size and digest, not
                    // the outcome itself, which may be large and is replicated
                    // with every event (Issue #22). A report sent again (its
                    // answer was lost) is recorded once.
                    if let Ok(id) = report.job_id.parse() {
                        let id = JobId(id);
                        let digest = algebramesh_core::sha256_hex(report.outcome_json.as_bytes());
                        let note = format!(
                            "ignored late report from {} (attempt {}): {reason}; outcome of {} \
                             bytes, sha256 {digest}",
                            report.agent_id,
                            report.attempt,
                            report.outcome_json.len()
                        );
                        if !inner.store.events(&id)?.iter().any(|e| e.ends_with(&note)) {
                            inner.store.add_event(&id, &note)?;
                        }
                    }
                    return Ok(pb::Ack {
                        accepted: false,
                        reason,
                    });
                }
            };
        let outcome: Result<CasResult, CasError> =
            if report.outcome_json.len() > algebramesh_core::MAX_RESULT_BYTES {
                // Too large to replicate and keep (Issue #20): the job fails
                // with a clear error instead of a write that cannot commit.
                Err(CasError::ResourceLimit(format!(
                    "the result of {} bytes exceeds the scheduler's limit of {} bytes",
                    report.outcome_json.len(),
                    algebramesh_core::MAX_RESULT_BYTES
                )))
            } else {
                match serde_json::from_str(&report.outcome_json) {
                    Ok(o) => o,
                    Err(e) => Err(CasError::UnsupportedResult(format!(
                        "malformed outcome from agent: {e}"
                    ))),
                }
            };
        // Markers of blobs (ADR 0021): only a cluster that has enabled blobs
        // records them, and there every value that looks like one must be
        // one. Before, a value with `$` keys that is not a marker is data, as
        // it was.
        let blob_refs = inner
            .store
            .enabled_features()?
            .iter()
            .any(|f| f == algebramesh_core::features::BLOB_REFS);
        let outcome = match outcome {
            Ok(result) => match algebramesh_core::blob::markers(&result.value) {
                Err(e) if blob_refs => Err(CasError::UnsupportedResult(format!(
                    "the result refers to blobs wrongly: {e}"
                ))),
                Ok(found) if !found.is_empty() && !blob_refs => {
                    Err(CasError::UnsupportedResult(format!(
                        "the result refers to blobs, but the schedulers have not enabled the                          feature {}",
                        algebramesh_core::features::BLOB_REFS
                    )))
                }
                _ => Ok(result),
            },
            Err(e) => Err(e),
        };
        let now = Utc::now();
        if row.record.state == JobState::Assigned {
            // The start was not reported (Issue #26): pass through RUNNING,
            // and through CANCELLING if a cancellation was requested, as
            // `started` would have, so that the result of a cancelled job
            // ends CANCELLED.
            let _ = row.record.transition(JobState::Running, now);
            if row.cancel_requested {
                let _ = row.record.transition(JobState::Cancelling, now);
            }
        }
        if let Err(e) = row.record.complete(outcome, now) {
            return Ok(pb::Ack {
                accepted: false,
                reason: e.to_string(),
            });
        }
        // Fields of a feature the cluster has not agreed on stay out of the
        // record (ADR 0020): a scheduler that does not know them would drop
        // them, and the records would differ between schedulers.
        let enabled = inner.store.enabled_features()?;
        let uses = |feature: &str| enabled.iter().any(|f| f == feature);
        if !uses(algebramesh_core::features::BACKEND_PROVENANCE) {
            row.record.backend.driver_sha256 = None;
            row.record.backend.agent_build = None;
        }
        if !uses(algebramesh_core::features::RESULT_GUARANTEE) {
            row.record.guarantee = None;
        }
        inner.store.update_job(&row)?;
        inner.store.add_event(
            &row.record.job_id,
            &format!("finished as {}", state_name(row.record.state)),
        )?;
        Ok(pb::Ack {
            accepted: true,
            reason: String::new(),
        })
    }

    // --------------------------------------------------------------- liveness

    fn declare_lost(
        &self,
        inner: &Inner,
        mut row: JobRow,
        reason: &str,
    ) -> Result<(), SchedulerError> {
        let now = Utc::now();
        // A job whose cancellation was requested is never run again (Issue
        // #26), whatever happened to its run.
        let cancelled = row.cancel_requested || row.record.state == JobState::Cancelling;
        if row.record.transition(JobState::Lost, now).is_err() {
            return Ok(());
        }
        row.record.error = Some(CasError::Lost(reason.to_owned()));
        let retry = !cancelled
            && row.record.spec.retry_policy == RetryPolicy::IfDeterministic
            && row.record.attempt < self.options.max_attempts;
        // The job, its retry and the link between them are one change
        // (Issue #27): a scheduler that stops in between leaves either the job
        // as it was, to be declared lost again, or both.
        let mut changes = Vec::new();
        if retry {
            let mut next = JobRecord::new(JobId::new(), row.record.spec.clone(), now);
            next.attempt = row.record.attempt + 1;
            next.retry_of = Some(row.record.job_id);
            let next_row = JobRow::new(next, row.priority);
            changes.push(Change::InsertJob {
                row: (&next_row).into(),
            });
            changes.push(Store::event(
                &next_row.record.job_id,
                &format!("retry {} of {}", next_row.record.attempt, row.record.job_id),
            ));
            row.retried_as = Some(next_row.record.job_id);
        }
        changes.push(Change::UpdateJob { row: (&row).into() });
        let note = if cancelled {
            format!("lost: {reason}; not retried, its cancellation was requested")
        } else {
            format!("lost: {reason}")
        };
        changes.push(Store::event(&row.record.job_id, &note));
        inner.store.commit_all(changes)?;
        tracing::warn!(job_id = %row.record.job_id, reason, retry, "job lost");
        if retry {
            self.work.notify_waiters();
        }
        Ok(())
    }

    /// Starts the heartbeat timeout of every agent again, after a time in which
    /// this scheduler could not answer calls (a leader without a majority): the
    /// agents' silence was not theirs.
    pub fn restart_heartbeat_clocks(&self) {
        let now = Instant::now();
        for agent in self.lock().agents.values_mut() {
            agent.last_heartbeat = now;
            for seen in agent.jobs_seen.values_mut() {
                *seen = now;
            }
        }
    }

    /// Marks silent agents offline and their jobs `LOST`. Called periodically.
    ///
    /// An agent is marked offline only once every one of its active jobs is
    /// `LOST` (Issue #86): if the store fails in between, the agent stays
    /// online and silent, and the next sweep declares its remaining jobs
    /// `LOST`. A job already declared `LOST` is no longer active and is not
    /// declared again, so it is retried at most once.
    pub fn sweep(&self) -> Result<(), SchedulerError> {
        let mut inner = self.lock();
        inner.store.write_pending_artifacts();
        let timeout = self.options.heartbeat_timeout;
        let expired: Vec<String> = inner
            .agents
            .values()
            .filter(|a| a.online && a.last_heartbeat.elapsed() > timeout)
            .map(|a| a.agent_id.clone())
            .collect();
        for agent_id in expired {
            let rows: Vec<JobRow> = inner
                .store
                .jobs_in_states(&ACTIVE)?
                .into_iter()
                .filter(|r| r.record.agent_id.as_deref() == Some(agent_id.as_str()))
                .collect();
            for row in rows {
                self.declare_lost(
                    &inner,
                    row,
                    &format!("agent {agent_id} missed heartbeats for {timeout:?}"),
                )?;
            }
            if let Some(a) = inner.agents.get_mut(&agent_id) {
                a.online = false;
                tracing::warn!(%agent_id, name = %a.name, "agent missed heartbeats");
            }
        }
        Ok(())
    }

    pub fn events(&self, job_id: &str) -> Result<Vec<String>, SchedulerError> {
        let inner = self.lock();
        let row = Self::job(&inner, job_id)?;
        Ok(inner.store.events(&row.record.job_id)?)
    }
}

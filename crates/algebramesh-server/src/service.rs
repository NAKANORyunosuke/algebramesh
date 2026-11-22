//! gRPC front ends of the [`Scheduler`], through the [`Node`] that says whether
//! this process serves calls (it is the leader of its cluster, or alone). A call
//! that changes the store is answered after its changes are committed
//! ([`Node::committed`]). `Status`, `Result`, `LookupRequest` and `ListJobs`
//! read the committed state ([`Node::read_committed`]), never changes that are
//! not committed yet.

use std::sync::Arc;
use std::time::Duration;

use std::collections::HashSet;

use algebramesh_rpc::pb::agent_gateway_server::AgentGateway;
use algebramesh_rpc::pb::scheduler_server::Scheduler as SchedulerApi;
use algebramesh_rpc::{PeerIdentity, pb, peer_identity};
use tonic::{Request, Response, Status};

use algebramesh_core::JobState;

use algebramesh_core::blob::{self, BlobRef};

use crate::cluster::Node;
use crate::cluster::blob_sync::ChunkStream;
use crate::scheduler::{Scheduler, SchedulerError, status_of, summary_of};
use crate::store::{JobFilter, ListLimits, ListPosition};

/// The jobs of a page of `ListJobs` when the call asks for no number, and the
/// most it can ask for (Issue #63).
pub const DEFAULT_PAGE_SIZE: usize = 100;
pub const MAX_PAGE_SIZE: usize = 1000;
/// The bytes of the jobs of a page, about; only a page of one job may pass
/// it.
pub const MAX_PAGE_BYTES: usize = 1 << 20;
/// The jobs one call of `ListJobs` looks at, while it holds the state machine,
/// and their bytes, those the filters reject included (Issue #76).
pub const MAX_SCANNED_JOBS: usize = 10_000;
pub const MAX_SCANNED_BYTES: usize = 4 << 20;

/// The filters, position and bounds of a `ListJobs` call.
fn list_request(
    r: pb::ListJobsRequest,
) -> Result<(JobFilter, Option<ListPosition>, ListLimits), Status> {
    let given = |s: String| (!s.is_empty()).then_some(s);
    let prefix = r.request_id_prefix;
    if prefix.len() > 128 {
        return Err(Status::invalid_argument(
            "request_id_prefix is longer than 128 characters",
        ));
    }
    let state = given(r.state);
    if let Some(s) = &state
        && serde_json::from_value::<JobState>(serde_json::Value::String(s.clone())).is_err()
    {
        return Err(Status::invalid_argument(format!(
            "unknown state {s}; one of QUEUED, ASSIGNED, RUNNING, SUCCEEDED, FAILED, \
             TIMED_OUT, CANCELLING, CANCELLED, LOST"
        )));
    }
    let page_size = match r.page_size as usize {
        0 => DEFAULT_PAGE_SIZE,
        n => n.min(MAX_PAGE_SIZE),
    };
    let filter = JobFilter {
        request_id_prefix: prefix,
        state,
        backend: given(r.backend),
        operation: given(r.operation),
    };
    let after = match r.page_token.as_str() {
        "" => None,
        token => Some(page_position(token, &filter).ok_or_else(|| {
            Status::invalid_argument("page_token is not one that a listing with these filters gave")
        })?),
    };
    let limits = ListLimits {
        page_size,
        max_bytes: MAX_PAGE_BYTES,
        max_scanned: MAX_SCANNED_JOBS,
        max_scanned_bytes: MAX_SCANNED_BYTES,
    };
    Ok((filter, after, limits))
}

/// A digest of the filters (64-bit FNV-1a, in hexadecimal), so that a token
/// is taken only with the filters of the listing that gave it.
fn filters_digest(filter: &JobFilter) -> String {
    let fields = [
        Some(filter.request_id_prefix.as_str()),
        filter.state.as_deref(),
        filter.backend.as_deref(),
        filter.operation.as_deref(),
    ];
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for field in fields {
        // Each field with its length, so that no two lists of fields match.
        let text = field.map_or_else(|| "-".to_owned(), |f| format!("{}:{f}", f.len()));
        for byte in text.bytes().chain([0]) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    }
    format!("{hash:016x}")
}

/// The token of a position: `r` and a request id in a listing by prefix, `s`
/// and a row otherwise, after the digest of the filters.
fn page_token(position: &ListPosition, filter: &JobFilter) -> String {
    let digest = filters_digest(filter);
    match position {
        ListPosition::Request(r) => format!("r{digest}{r}"),
        ListPosition::Seq(s) => format!("s{digest}{s}"),
    }
}

fn page_position(token: &str, filter: &JobFilter) -> Option<ListPosition> {
    let (kind, rest) = token.split_at_checked(1)?;
    let (digest, position) = rest.split_at_checked(16)?;
    if digest != filters_digest(filter) {
        return None;
    }
    match (kind, filter.request_id_prefix.is_empty()) {
        ("r", false) => Some(ListPosition::Request(position.to_owned())),
        ("s", true) => position.parse().ok().map(ListPosition::Seq),
        _ => None,
    }
}

/// The status of an error of `scheduler`. An error of its store on the leader
/// means that its working store cannot be written (a full disk): the node gives
/// up the term, so that another node leads (Issue #37).
fn failed(node: &Node, scheduler: &Scheduler, e: SchedulerError) -> Status {
    if matches!(e, SchedulerError::Store(_)) {
        node.store_failed(scheduler);
    }
    status(e)
}

fn status(e: SchedulerError) -> Status {
    match e {
        SchedulerError::InvalidSpec(m) => Status::invalid_argument(m),
        SchedulerError::UnknownJob(m) => Status::not_found(format!("unknown job {m}")),
        SchedulerError::UnknownAgent(m) => {
            Status::failed_precondition(format!("unknown or offline agent {m}; register again"))
        }
        SchedulerError::Forbidden(m) => Status::permission_denied(m),
        SchedulerError::Conflict(m) => Status::already_exists(m),
        SchedulerError::Unplaceable(m) => Status::failed_precondition(m),
        SchedulerError::Exhausted(m) => Status::resource_exhausted(m),
        SchedulerError::Store(e) => {
            tracing::error!("store error: {e}");
            Status::internal("metadata store error")
        }
    }
}

#[derive(Clone)]
pub struct ClientService(pub Arc<Node>);

impl ClientService {
    /// Runs `change` on the scheduler and answers once its effect is committed.
    ///
    /// A call that wrote nothing (a submission with a known `request_id`, a
    /// cancellation of a finished job) does not wait for the commit of other
    /// calls' changes (Issue #18): if the committed state already shows the job
    /// as `change` reports it, that is the answer. Otherwise (the job's own
    /// changes are not committed yet, or this call wrote something), it waits
    /// for the commit as every write does, so nothing uncommitted is reported
    /// as done.
    ///
    /// `change` reports whether it wrote, decided under the scheduler's lock
    /// as it ran (Issue #92): other calls' writes in the meantime do not make
    /// a call that wrote nothing wait for their commit when its job is
    /// committed as reported. When it is not, the call waits for the commit of
    /// everything written so far, those writes included.
    async fn write(
        &self,
        change: impl FnOnce(&Scheduler) -> Result<(pb::JobStatus, bool), SchedulerError>,
    ) -> Result<pb::JobStatus, Status> {
        let scheduler = self.0.scheduler()?;
        let (reply, wrote) = change(&scheduler).map_err(|e| failed(&self.0, &scheduler, e))?;
        if !wrote {
            let committed = self
                .0
                .read_committed(|store| Scheduler::status_in(store, &reply.job_id).ok())
                .await?;
            if committed.as_ref() == Some(&reply) {
                return Ok(reply);
            }
        }
        self.0.committed(&scheduler).await?;
        Ok(reply)
    }
}

#[tonic::async_trait]
impl SchedulerApi for ClientService {
    async fn submit(
        &self,
        r: Request<pb::SubmitRequest>,
    ) -> Result<Response<pb::JobStatus>, Status> {
        let r = r.into_inner();
        // The blobs the arguments refer to must be held by a majority before
        // a job does (ADR 0021 §5, B2); checked outside the scheduler's lock.
        let references = argument_refs(&self.0, &r.spec_json)?;
        let blobs: Vec<BlobRef> = references
            .iter()
            .filter_map(|(_, r)| match r {
                blob::ArgumentRef::Blob { blob, .. } => Some(blob.clone()),
                blob::ArgumentRef::Record(_) => None,
            })
            .collect();
        self.0.ensure_blobs(&blobs).await?;
        // The values that come from other jobs' results are those results'
        // (W2): checked against the committed records of their producers.
        ensure_origins(&self.0, &references).await?;
        self.write(|s| s.submit_changed(&r.spec_json, r.priority, &r.request_id))
            .await
            .map(Response::new)
    }

    async fn status(&self, r: Request<pb::JobRef>) -> Result<Response<pb::JobStatus>, Status> {
        let job_id = r.into_inner().job_id;
        self.0
            .read_committed(|store| Scheduler::status_in(store, &job_id))
            .await?
            .map(Response::new)
            .map_err(status)
    }

    async fn result(&self, r: Request<pb::JobRef>) -> Result<Response<pb::JobResult>, Status> {
        let r = r.into_inner();
        let mut result = self
            .0
            .read_committed(|store| Scheduler::result_in(store, &r.job_id))
            .await?
            .map_err(status)?;
        // The record is copied out of the state machine before its markers
        // are expanded for a client that does not resolve them (ADR 0021 §8).
        if !r.accept_blob_refs {
            result.record_json = self
                .0
                .expand_record(std::mem::take(&mut result.record_json))
                .await?;
        }
        Ok(Response::new(result))
    }

    async fn lookup_request(
        &self,
        r: Request<pb::RequestRef>,
    ) -> Result<Response<pb::JobStatus>, Status> {
        // Keys are kept and bounded as `Submit` keeps and bounds them.
        let request_id = r.into_inner().request_id.trim().to_owned();
        if request_id.is_empty() {
            return Err(Status::invalid_argument("request_id is empty"));
        }
        if request_id.len() > 128 {
            return Err(Status::invalid_argument(
                "request_id is longer than 128 characters",
            ));
        }
        // The row, its record included, is read and copied while the state
        // machine is held; only its deserialization comes after (Issue #84).
        let found = self
            .0
            .read_committed(|store| store.stored_job_for_request(&request_id))
            .await?
            .map_err(|e| status(e.into()))?;
        match found {
            Some(stored) => {
                let row = stored.parse().map_err(|e| status(e.into()))?;
                Ok(Response::new(status_of(&row)))
            }
            None => Err(Status::not_found(format!(
                "no job was submitted with request_id {request_id}"
            ))),
        }
    }

    async fn list_jobs(
        &self,
        r: Request<pb::ListJobsRequest>,
    ) -> Result<Response<pb::ListJobsResponse>, Status> {
        let (filter, after, limits) = list_request(r.into_inner())?;
        // The state machine is held while the rows are copied; the answer is
        // made after it is let go.
        let page = self
            .0
            .read_committed(|store| store.list_jobs(&filter, after.as_ref(), &limits))
            .await?
            .map_err(|e| status(e.into()))?;
        let next_page_token = page.next.as_ref().map(|p| page_token(p, &filter));
        Ok(Response::new(pb::ListJobsResponse {
            next_page_token: next_page_token.unwrap_or_default(),
            jobs: page.jobs.into_iter().map(summary_of).collect(),
        }))
    }

    async fn cancel(&self, r: Request<pb::JobRef>) -> Result<Response<pb::JobStatus>, Status> {
        let job_id = r.into_inner().job_id;
        self.write(|s| s.cancel_changed(&job_id))
            .await
            .map(Response::new)
    }

    async fn list_backends(
        &self,
        _: Request<pb::Empty>,
    ) -> Result<Response<pb::ListBackendsResponse>, Status> {
        self.0
            .scheduler()?
            .backends()
            .map(Response::new)
            .map_err(status)
    }

    async fn list_workers(
        &self,
        _: Request<pb::Empty>,
    ) -> Result<Response<pb::ListWorkersResponse>, Status> {
        self.0
            .scheduler()?
            .workers()
            .map(Response::new)
            .map_err(status)
    }

    async fn put_blob(
        &self,
        r: Request<tonic::Streaming<pb::BlobUpload>>,
    ) -> Result<Response<pb::BlobRef>, Status> {
        // Uploads of clients count against their token (ADR 0021, B3);
        // those of agents do not: the output limits of the drivers bound them.
        let caller = r
            .extensions()
            .get::<algebramesh_rpc::TokenId>()
            .map(|t| t.0.clone());
        self.0
            .put_blob_as(caller.as_deref(), r.into_inner())
            .await
            .map(Response::new)
    }

    type GetBlobStream = ChunkStream;

    async fn get_blob(
        &self,
        r: Request<pb::BlobRequest>,
    ) -> Result<Response<Self::GetBlobStream>, Status> {
        let r = r.into_inner();
        self.0.get_blob(&r.id, r.offset).await.map(Response::new)
    }

    async fn stat_blob(
        &self,
        r: Request<pb::BlobRequest>,
    ) -> Result<Response<pb::BlobStat>, Status> {
        self.0
            .stat_blob(&r.into_inner().id)
            .await
            .map(Response::new)
    }
}

/// How agents prove who they are, besides the agent token.
#[derive(Clone, Default)]
pub struct AgentAuth {
    /// Every agent call must carry a client certificate verified against the
    /// agent CA (the TLS layer checks the signature; see `server_tls`).
    pub require_certificate: bool,
    /// Serial numbers (lowercase hex) of agent certificates that are refused.
    pub revoked_serials: HashSet<String>,
}

#[derive(Clone)]
pub struct AgentService {
    pub node: Arc<Node>,
    pub auth: Arc<AgentAuth>,
}

impl AgentService {
    fn identity<T>(&self, r: &Request<T>) -> Result<Option<PeerIdentity>, Status> {
        if !self.auth.require_certificate {
            return Ok(None);
        }
        let identity = r
            .peer_certs()
            .and_then(|certs| certs.first().and_then(|c| peer_identity(c)))
            .ok_or_else(|| Status::unauthenticated("agents must present a client certificate"))?;
        if self.auth.revoked_serials.contains(&identity.serial) {
            tracing::warn!(name = %identity.name, serial = %identity.serial, "revoked agent certificate");
            return Err(Status::permission_denied(
                "the client certificate is revoked",
            ));
        }
        Ok(Some(identity))
    }

    fn authorize<T>(&self, r: &Request<T>, agent_id: &str) -> Result<(), Status> {
        let identity = self.identity(r)?;
        self.node
            .scheduler()?
            .authorize(agent_id, identity.as_ref())
            .map_err(status)
    }
}

#[tonic::async_trait]
impl AgentGateway for AgentService {
    async fn register(
        &self,
        r: Request<pb::RegisterRequest>,
    ) -> Result<Response<pb::RegisterResponse>, Status> {
        let identity = self.identity(&r)?;
        let scheduler = self.node.scheduler()?;
        let reply = scheduler
            .register(r.into_inner(), identity.as_ref())
            .map_err(|e| failed(&self.node, &scheduler, e))?;
        self.node.committed(&scheduler).await?;
        Ok(Response::new(reply))
    }

    async fn heartbeat(
        &self,
        r: Request<pb::HeartbeatRequest>,
    ) -> Result<Response<pb::HeartbeatResponse>, Status> {
        self.authorize(&r, &r.get_ref().agent_id)?;
        let scheduler = self.node.scheduler()?;
        let reply = scheduler
            .heartbeat(&r.into_inner())
            .map_err(|e| failed(&self.node, &scheduler, e))?;
        self.node.committed(&scheduler).await?;
        Ok(Response::new(reply))
    }

    async fn poll_assignment(
        &self,
        r: Request<pb::PollRequest>,
    ) -> Result<Response<pb::PollResponse>, Status> {
        self.authorize(&r, &r.get_ref().agent_id)?;
        let r = r.into_inner();
        let scheduler = self.node.scheduler()?;
        let assignment = scheduler
            .poll(
                &r.agent_id,
                &r.backends,
                Duration::from_millis(r.wait_ms.into()),
            )
            .await
            .map_err(|e| failed(&self.node, &scheduler, e))?;
        self.node.committed(&scheduler).await?;
        Ok(Response::new(pb::PollResponse { assignment }))
    }

    async fn report_started(&self, r: Request<pb::JobEvent>) -> Result<Response<pb::Ack>, Status> {
        self.authorize(&r, &r.get_ref().agent_id)?;
        let scheduler = self.node.scheduler()?;
        let reply = scheduler
            .started(&r.into_inner())
            .map_err(|e| failed(&self.node, &scheduler, e))?;
        self.node.committed(&scheduler).await?;
        Ok(Response::new(reply))
    }

    async fn report_finished(
        &self,
        r: Request<pb::JobFinished>,
    ) -> Result<Response<pb::Ack>, Status> {
        self.authorize(&r, &r.get_ref().agent_id)?;
        let report = r.into_inner();
        // The blobs a result refers to must be held by a majority before the
        // record does (ADR 0021 §5); checked outside the scheduler's lock.
        let refs = result_blobs(&report.outcome_json);
        self.node.ensure_blobs(&refs).await?;
        let scheduler = self.node.scheduler()?;
        let reply = scheduler
            .finished(&report)
            .map_err(|e| failed(&self.node, &scheduler, e))?;
        self.node.committed(&scheduler).await?;
        Ok(Response::new(reply))
    }

    async fn put_blob(
        &self,
        r: Request<tonic::Streaming<pb::BlobUpload>>,
    ) -> Result<Response<pb::BlobRef>, Status> {
        self.identity(&r)?;
        self.node.put_blob(r.into_inner()).await.map(Response::new)
    }

    type GetBlobStream = ChunkStream;

    async fn get_blob(
        &self,
        r: Request<pb::BlobRequest>,
    ) -> Result<Response<Self::GetBlobStream>, Status> {
        self.identity(&r)?;
        let r = r.into_inner();
        self.node.get_blob(&r.id, r.offset).await.map(Response::new)
    }

    async fn stat_blob(
        &self,
        r: Request<pb::BlobRequest>,
    ) -> Result<Response<pb::BlobStat>, Status> {
        self.identity(&r)?;
        self.node
            .stat_blob(&r.into_inner().id)
            .await
            .map(Response::new)
    }

    async fn get_record(
        &self,
        r: Request<pb::RecordRequest>,
    ) -> Result<Response<pb::JobResult>, Status> {
        self.identity(&r)?;
        if !uses(&self.node, algebramesh_core::features::WORKFLOW_REFS) {
            return Err(Status::failed_precondition(
                "the cluster has not enabled the feature workflow_refs",
            ));
        }
        let job_id = r.into_inner().job_id;
        // The record is copied out of the committed state as `Result` does;
        // its markers stay as they are.
        self.node
            .read_committed(|store| Scheduler::result_in(store, &job_id))
            .await?
            .map(Response::new)
            .map_err(status)
    }
}

/// Whether the cluster has enabled `feature`, as this node has applied it.
fn uses(node: &Node, feature: &str) -> bool {
    node.enabled_features().iter().any(|f| f == feature)
}

/// Checks each value of `references` that says where it came from (W2): its
/// producer is a job that SUCCEEDED, and the value its committed record holds
/// at the pointer, inline or as a marker, has the SHA-256 the reference
/// names. Pointers are top-level, so no blob is read. FAILED_PRECONDITION
/// otherwise, before anything is written.
async fn ensure_origins(
    node: &Node,
    references: &[(String, blob::ArgumentRef)],
) -> Result<(), Status> {
    let mut records: std::collections::HashMap<String, pb::JobResult> =
        std::collections::HashMap::new();
    for (key, r) in references {
        let Some(origin) = r.origin() else { continue };
        if !records.contains_key(&origin.job_id) {
            let job_id = origin.job_id.clone();
            let found = node
                .read_committed(|store| Scheduler::result_in(store, &job_id))
                .await?;
            let found = match found {
                Ok(found) => found,
                Err(SchedulerError::UnknownJob(_)) => {
                    return Err(Status::failed_precondition(format!(
                        "argument {key} comes from job {}, which is unknown",
                        origin.job_id
                    )));
                }
                Err(e) => return Err(status(e)),
            };
            records.insert(origin.job_id.clone(), found);
        }
        let found = &records[&origin.job_id];
        let state = found
            .status
            .as_ref()
            .map(|s| s.state.as_str())
            .unwrap_or("");
        if state != "SUCCEEDED" {
            return Err(Status::failed_precondition(format!(
                "argument {key} comes from job {}, which is {state}, not SUCCEEDED",
                origin.job_id
            )));
        }
        let record: algebramesh_core::JobRecord = serde_json::from_str(&found.record_json)
            .map_err(|e| Status::internal(format!("the record of job {}: {e}", origin.job_id)))?;
        let value = record
            .result
            .as_ref()
            .map(|result| blob::pointed(result, &origin.pointer))
            .transpose()
            .map_err(|e| Status::invalid_argument(format!("argument {key}: {e}")))?
            .flatten();
        let Some(value) = value else {
            return Err(Status::failed_precondition(format!(
                "argument {key} comes from {:?} of the result of job {}, which has no top-level \
                 value there (a result that is one blob holds none)",
                origin.pointer, origin.job_id
            )));
        };
        let recorded = blob::recorded_id(value);
        if &recorded != r.id() {
            return Err(Status::failed_precondition(format!(
                "argument {key} is {} but the value at {:?} of the result of job {} is {recorded}",
                r.id(),
                origin.pointer,
                origin.job_id
            )));
        }
    }
    Ok(())
}

/// The references among the arguments of `spec_json` (markers, and from W2
/// `{"$ref": ...}`), once the cluster takes blobs in arguments; none before,
/// or for a spec the scheduler will refuse anyway. References to other
/// jobs' results are refused while the cluster does not check them.
fn argument_refs(node: &Node, spec_json: &str) -> Result<Vec<(String, blob::ArgumentRef)>, Status> {
    if spec_json.len() > algebramesh_core::MAX_SPEC_BYTES
        || !uses(node, algebramesh_core::features::BLOB_ARGUMENTS)
    {
        return Ok(Vec::new());
    }
    let Ok(spec) = algebramesh_core::JobSpec::from_json(spec_json) else {
        return Ok(Vec::new());
    };
    let refs = crate::scheduler::argument_references(&spec).map_err(status)?;
    crate::scheduler::check_references_allowed(
        &refs,
        uses(node, algebramesh_core::features::WORKFLOW_REFS),
    )
    .map_err(status)?;
    Ok(refs)
}

/// The blobs the result in `outcome_json` refers to; none if it has no
/// well-formed markers (the scheduler then refuses the result itself).
fn result_blobs(outcome_json: &str) -> Vec<BlobRef> {
    if !outcome_json.contains("\"$blob\"")
        || outcome_json.len() > algebramesh_core::MAX_RESULT_BYTES
    {
        return Vec::new();
    }
    #[derive(serde::Deserialize)]
    enum Outcome {
        Ok { value: serde_json::Value },
        Err(serde::de::IgnoredAny),
    }
    match serde_json::from_str::<Outcome>(outcome_json) {
        Ok(Outcome::Ok { value }) => blob::markers(&value)
            .map(|found| found.into_iter().map(|(_, r)| r).collect())
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(
        r: pb::ListJobsRequest,
    ) -> Result<(JobFilter, Option<ListPosition>, ListLimits), Status> {
        list_request(r)
    }

    #[test]
    fn list_requests_are_bounded_and_checked() {
        let (filter, after, limits) = request(pb::ListJobsRequest::default()).unwrap();
        assert!(filter.request_id_prefix.is_empty() && filter.state.is_none());
        assert_eq!((after, limits.page_size), (None, DEFAULT_PAGE_SIZE));
        let big = pb::ListJobsRequest {
            page_size: 5000,
            ..Default::default()
        };
        assert_eq!(request(big).unwrap().2.page_size, MAX_PAGE_SIZE);
        let state = |s: &str| pb::ListJobsRequest {
            state: s.into(),
            ..Default::default()
        };
        assert_eq!(
            request(state("FAILED")).unwrap().0.state.as_deref(),
            Some("FAILED")
        );
        let err = request(state("failed")).unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
        let long = pb::ListJobsRequest {
            request_id_prefix: "x".repeat(129),
            ..Default::default()
        };
        assert_eq!(
            request(long).unwrap_err().code(),
            tonic::Code::InvalidArgument
        );
    }

    /// A token is taken back only with the filters of the listing that gave
    /// it, and gives back its position.
    #[test]
    fn page_tokens_are_bound_to_their_filters() {
        let by_prefix = JobFilter {
            request_id_prefix: "wf/a/".into(),
            ..Default::default()
        };
        let failed = JobFilter {
            state: Some("FAILED".into()),
            ..Default::default()
        };
        let queued = JobFilter {
            state: Some("QUEUED".into()),
            ..Default::default()
        };
        let at = ListPosition::Request("wf/a/7".into());
        let token = page_token(&at, &by_prefix);
        assert_eq!(page_position(&token, &by_prefix), Some(at));
        let seq = ListPosition::Seq(5000);
        let token = page_token(&seq, &failed);
        assert_eq!(page_position(&token, &failed), Some(seq.clone()));
        assert_eq!(page_position(&token, &queued), None);
        assert_eq!(page_position(&token, &by_prefix), None);
        assert_eq!(page_position("s5000", &failed), None);
        let with_token = |token: String, filter: JobFilter| pb::ListJobsRequest {
            state: filter.state.unwrap_or_default(),
            page_token: token,
            ..Default::default()
        };
        let reused = with_token(page_token(&seq, &failed), queued.clone());
        assert_eq!(
            request(reused).unwrap_err().code(),
            tonic::Code::InvalidArgument
        );
        let kept = with_token(page_token(&seq, &queued), queued);
        assert_eq!(request(kept).unwrap().1, Some(seq));
        // Filters that differ only in where a value ends do not share a digest.
        let a = JobFilter {
            backend: Some("ab".into()),
            operation: Some("c".into()),
            ..Default::default()
        };
        let b = JobFilter {
            backend: Some("a".into()),
            operation: Some("bc".into()),
            ..Default::default()
        };
        assert_ne!(filters_digest(&a), filters_digest(&b));
    }

    /// A cluster of one scheduler, in memory, once it serves.
    async fn one_scheduler() -> Arc<Node> {
        use crate::cluster::{ClusterConfig, Member, SNAPSHOT_CHUNK_BYTES, supported_features};
        let member = Member {
            name: "s1".into(),
            url: "https://127.0.0.1:1".into(),
            domain: "localhost".into(),
        };
        let config = ClusterConfig {
            node_id: 1,
            members: std::collections::BTreeMap::from([(1, member)]),
            join: false,
            ca_pem: Vec::new(),
            peer_token: "peer".into(),
            election_timeout: Duration::from_millis(200),
            data_dir: None,
            snapshot_chunk_bytes: SNAPSHOT_CHUNK_BYTES,
            features: supported_features(),
        };
        let store = crate::Store::open(None, None).unwrap();
        let node = Node::start_cluster(store, Default::default(), config)
            .await
            .unwrap();
        let started = std::time::Instant::now();
        while node.scheduler().is_err() {
            assert!(started.elapsed() < Duration::from_secs(20), "no leader");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        node
    }

    fn spec(n: u32) -> String {
        serde_json::json!({
            "schema_version": 1, "backend": "test", "operation": "test.op",
            "arguments": {"n": n},
        })
        .to_string()
    }

    /// Issue #92: a call that writes nothing (a submission with a known key, a
    /// cancellation of a finished job) is answered from the committed state
    /// even if another call writes while it runs, before or after its own
    /// change, and that write is held back. A key whose job is not committed
    /// yet still waits for the commit, and another job under a known key is
    /// still refused.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_call_that_writes_nothing_does_not_wait_for_writes_made_meanwhile() {
        let node = one_scheduler().await;
        let service = ClientService(node.clone());
        let known = service
            .write(|s| s.submit_changed(&spec(1), 0, "known"))
            .await
            .unwrap();
        let queued = service
            .write(|s| s.submit_changed(&spec(2), 0, ""))
            .await
            .unwrap();
        let finished = service
            .write(|s| s.cancel_changed(&queued.job_id))
            .await
            .unwrap();
        assert_eq!(finished.state, "CANCELLED");

        // Other calls' writes are held back from now on.
        node.hold_proposals(true);
        let quick = Duration::from_secs(3);
        let other = |s: &Scheduler, key: &str, n: u32| {
            assert!(s.submit_changed(&spec(n), 0, key).unwrap().1);
        };
        // Another call writes after this one began, before its change.
        let again = tokio::time::timeout(
            quick,
            service.write(|s| {
                other(s, "b1", 3);
                s.submit_changed(&spec(1), 0, "known")
            }),
        )
        .await
        .expect("the known key waited for another call's commit")
        .unwrap();
        assert_eq!(again, known);
        // ... and after its change.
        let again = tokio::time::timeout(
            quick,
            service.write(|s| {
                let reply = s.submit_changed(&spec(1), 0, "known");
                other(s, "b2", 4);
                reply
            }),
        )
        .await
        .expect("the known key waited for another call's commit")
        .unwrap();
        assert_eq!(again, known);
        let cancelled = tokio::time::timeout(
            quick,
            service.write(|s| {
                other(s, "b3", 5);
                s.cancel_changed(&queued.job_id)
            }),
        )
        .await
        .expect("the finished job waited for another call's commit")
        .unwrap();
        assert_eq!(cancelled, finished);
        // The same key with another job is refused.
        let conflict = tokio::time::timeout(
            quick,
            service.write(|s| s.submit_changed(&spec(9), 0, "known")),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(conflict.code(), tonic::Code::AlreadyExists);

        // A key whose job is not committed is not answered before the commit,
        // whether the call wrote it or found it.
        let first = {
            let service = service.clone();
            tokio::spawn(async move {
                service
                    .write(|s| s.submit_changed(&spec(6), 0, "pending"))
                    .await
            })
        };
        let duplicate = {
            let service = service.clone();
            tokio::spawn(async move {
                service
                    .write(|s| s.submit_changed(&spec(6), 0, "pending"))
                    .await
            })
        };
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(!first.is_finished() && !duplicate.is_finished());
        node.hold_proposals(false);
        let first = first.await.unwrap().unwrap();
        assert_eq!(duplicate.await.unwrap().unwrap(), first);
        node.shutdown().await;
    }
}

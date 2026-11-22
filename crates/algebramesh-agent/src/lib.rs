//! Worker agent (design §4.1, §7.2, §11).
//!
//! An agent registers its backends with the scheduler, keeps one polling task per
//! slot (for OpenXM one slot is one session), runs assigned jobs and reports the
//! outcome. A heartbeat task reports running jobs and receives cancellations; if the
//! scheduler no longer knows the agent, it registers again and stops jobs whose
//! results would be rejected.
//!
//! With several scheduler URLs (a Raft cluster, ADR 0013), the agent registers
//! with the first that accepts it and sends its next heartbeat to another when
//! its scheduler answers `UNAVAILABLE`; it registers again where the scheduler
//! does not know it. It never goes back to a scheduler whose epoch is lower than
//! one it has seen, so a deposed leader cannot hold it.

pub mod config;
mod offload;
mod resolve;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use algebramesh_core::{CasBackend, ExecutionContext, JobId, JobSpec};
use algebramesh_rpc::pb::agent_gateway_client::AgentGatewayClient;
use algebramesh_rpc::{MAX_MESSAGE_BYTES, TokenInterceptor, client_tls, pb};
use thiserror::Error;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tonic::service::interceptor::InterceptedService;
use tonic::transport::{Channel, Endpoint, Identity};

/// How long one heartbeat may take before the agent tries another scheduler.
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the agent goes on sending a result the scheduler has not taken.
/// The job stays in the heartbeats meanwhile; afterwards the scheduler declares
/// it `LOST` (Issue #10).
const REPORT_PATIENCE: Duration = Duration::from_secs(120);
/// A result larger than this is replaced by a `RESOURCE_LIMIT` error: it would
/// not fit in a gRPC message.
const MAX_REPORT_BYTES: usize = algebramesh_core::MAX_RESULT_BYTES;
/// How long one call to the scheduler may take (Issue #14). HTTP/2 keepalive
/// only shows that the connection lives; a handler that never answers would
/// otherwise hold the call forever. A poll may take its wait on top.
const RPC_TIMEOUT: Duration = Duration::from_secs(30);

/// `call`, or `UNAVAILABLE` if it does not answer within `limit`. Its outcome is
/// then unknown; every call the agent makes may be sent again (the scheduler
/// ignores a repeated report of a job that already finished).
async fn within<T>(
    limit: Duration,
    call: impl std::future::Future<Output = Result<T, tonic::Status>>,
) -> Result<T, tonic::Status> {
    tokio::time::timeout(limit, call).await.unwrap_or_else(|_| {
        Err(tonic::Status::unavailable(format!(
            "the scheduler did not answer within {limit:?}"
        )))
    })
}

pub use config::AgentConfig;

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("configuration: {0}")]
    Config(String),
    #[error("cannot connect to the scheduler: {0}")]
    Connect(#[from] tonic::transport::Error),
    #[error("scheduler refused the request: {0}")]
    Rpc(#[from] tonic::Status),
}

/// One backend served by this agent.
pub struct BackendSlot {
    pub backend: Arc<dyn CasBackend>,
    pub slots: u32,
    /// Image digest or source commit of the CAS installation.
    pub build: String,
    /// Version reported by the CAS, if known before the first job.
    pub backend_version: String,
}

/// How to reach the scheduler.
pub struct Connection {
    /// `https://host:port`, or several URLs separated by commas.
    pub url: String,
    pub ca_pem: Vec<u8>,
    /// Name expected in the scheduler certificates; empty for the host of each URL.
    pub domain: String,
    pub token: String,
    /// Client certificate and key of this agent, for schedulers that require
    /// mutual TLS (`agent_ca_file`).
    pub client_identity: Option<ClientIdentity>,
}

/// PEM certificate and private key of an agent (`algebramesh certs agent`).
#[derive(Clone)]
pub struct ClientIdentity {
    pub cert_pem: Vec<u8>,
    pub key_pem: Vec<u8>,
}

type Gateway = AgentGatewayClient<InterceptedService<Channel, TokenInterceptor>>;

struct Running {
    attempt: u32,
    cancel: CancellationToken,
}

struct Shared {
    /// One gateway per scheduler URL; `current` is the one registered with.
    gateways: Vec<Gateway>,
    current: AtomicUsize,
    /// Highest scheduler epoch seen.
    epoch: AtomicU64,
    name: String,
    host: String,
    backends: Vec<BackendSlot>,
    agent_id: RwLock<String>,
    running: Mutex<HashMap<String, Running>>,
    /// Features the schedulers have agreed on, as the last registration
    /// reported them (ADR 0020). The agent uses no other.
    features: std::sync::RwLock<Vec<String>>,
    /// The SHA-256 of each backend's driver files, indexed like `backends`,
    /// computed once when the agent starts (feature `backend_provenance`).
    driver_digests: Vec<Option<String>>,
    /// Checks every result after its backend and writes its guarantee
    /// (ADR 0018).
    verifier: Arc<dyn algebramesh_core::guarantee::ResultVerifier>,
}

/// The commit this agent was built from (`ALGEBRAMESH_BUILD_COMMIT` when it
/// was compiled), with `-modified` unless the build said its working tree was
/// clean (`ALGEBRAMESH_BUILD_CLEAN=1`); None if not known (ADR 0020).
pub fn agent_build() -> Option<String> {
    let commit = option_env!("ALGEBRAMESH_BUILD_COMMIT")?.trim();
    if commit.is_empty() {
        return None;
    }
    Some(if option_env!("ALGEBRAMESH_BUILD_CLEAN") == Some("1") {
        commit.to_owned()
    } else {
        format!("{commit}-modified")
    })
}

/// The checks of results: the standard ones and those of the packages this
/// agent was built with (`pkg-gkz`, Issue #100; `pkg-topology`, Issue #103).
pub fn registry() -> algebramesh_verify::Registry {
    #[allow(unused_mut)]
    let mut registry = algebramesh_verify::Registry::standard();
    #[cfg(feature = "pkg-gkz")]
    algebramesh_gkz::verify::register(&mut registry);
    #[cfg(feature = "pkg-topology")]
    algebramesh_topology::verify::register(&mut registry);
    registry
}

pub struct Agent {
    shared: Arc<Shared>,
}

const POLL_WAIT_MS: u32 = 15_000;

impl Agent {
    pub async fn connect(
        connection: Connection,
        name: String,
        host: String,
        backends: Vec<BackendSlot>,
    ) -> Result<Self, AgentError> {
        if backends.is_empty() {
            return Err(AgentError::Config("no backend configured".into()));
        }
        let token = TokenInterceptor::new(&connection.token)
            .map_err(|e| AgentError::Config(e.to_string()))?;
        let mut gateways = Vec::new();
        for url in connection
            .url
            .split(',')
            .map(str::trim)
            .filter(|u| !u.is_empty())
        {
            let domain = if connection.domain.is_empty() {
                config::url_host(url)
                    .ok_or_else(|| AgentError::Config(format!("no host in {url}")))?
            } else {
                connection.domain.clone()
            };
            let mut tls = client_tls(&connection.ca_pem, &domain);
            if let Some(id) = &connection.client_identity {
                tls = tls.identity(Identity::from_pem(&id.cert_pem, &id.key_pem));
            }
            let endpoint = Endpoint::from_shared(url.to_owned())?.tls_config(tls)?;
            let channel = algebramesh_rpc::connect_lazy(&endpoint, Duration::from_secs(10));
            gateways.push(
                AgentGatewayClient::with_interceptor(channel, token.clone())
                    .max_decoding_message_size(MAX_MESSAGE_BYTES)
                    .max_encoding_message_size(MAX_MESSAGE_BYTES),
            );
        }
        if gateways.is_empty() {
            return Err(AgentError::Config("no scheduler URL".into()));
        }
        let driver_digests: Vec<Option<String>> = backends
            .iter()
            .map(|b| {
                algebramesh_core::driver_sha256(&b.backend.driver_files()).unwrap_or_else(|e| {
                    tracing::warn!(
                        backend = b.backend.name(),
                        "cannot read the driver files: {e}"
                    );
                    None
                })
            })
            .collect();
        Ok(Self {
            shared: Arc::new(Shared {
                gateways,
                current: AtomicUsize::new(0),
                epoch: AtomicU64::new(0),
                name,
                host,
                backends,
                driver_digests,
                verifier: Arc::new(registry()),
                agent_id: RwLock::new(String::new()),
                running: Mutex::new(HashMap::new()),
                features: std::sync::RwLock::new(Vec::new()),
            }),
        })
    }

    /// Runs until `shutdown` is cancelled. Running jobs are cancelled on shutdown.
    ///
    /// Before registering, every backend is prepared (for example the OSCAR
    /// process is started); a backend whose preparation fails is not offered.
    pub async fn run(mut self, shutdown: CancellationToken) -> Result<(), AgentError> {
        self.prepare_backends().await?;
        let interval = self.shared.register_with_retry(&shutdown).await?;
        let mut tasks = tokio::task::JoinSet::new();
        {
            let shared = self.shared.clone();
            let stop = shutdown.clone();
            tasks.spawn(async move { shared.heartbeat_loop(interval, stop).await });
        }
        for index in 0..self.shared.backends.len() {
            for _ in 0..self.shared.backends[index].slots.max(1) {
                let shared = self.shared.clone();
                let stop = shutdown.clone();
                tasks.spawn(async move { shared.slot_loop(index, stop).await });
            }
        }
        shutdown.cancelled().await;
        self.shared.cancel_all();
        // Give slots a moment to report cancelled jobs.
        let _ = tokio::time::timeout(Duration::from_secs(10), async {
            while tasks.join_next().await.is_some() {}
        })
        .await;
        Ok(())
    }
}

impl Agent {
    async fn prepare_backends(&mut self) -> Result<(), AgentError> {
        let shared = Arc::get_mut(&mut self.shared)
            .ok_or_else(|| AgentError::Config("agent already running".into()))?;
        // A backend that is not offered takes its driver digest with it, so
        // that `driver_digests` stays indexed like `backends`.
        let mut ready = Vec::new();
        let mut ready_digests = Vec::new();
        let digests = std::mem::take(&mut shared.driver_digests);
        for (mut slot, digest) in std::mem::take(&mut shared.backends)
            .into_iter()
            .zip(digests)
        {
            let name = slot.backend.name();
            match tokio::time::timeout(Duration::from_secs(600), slot.backend.prepare()).await {
                Ok(Ok(version)) => {
                    if let Some(v) = version {
                        slot.backend_version = v;
                    }
                    tracing::info!(backend = name, version = %slot.backend_version, "backend ready");
                    ready.push(slot);
                    ready_digests.push(digest);
                }
                Ok(Err(e)) => tracing::error!(backend = name, "backend not offered: {e}"),
                Err(_) => {
                    tracing::error!(backend = name, "backend not offered: preparation timed out")
                }
            }
        }
        if ready.is_empty() {
            return Err(AgentError::Config("no backend could be prepared".into()));
        }
        shared.backends = ready;
        shared.driver_digests = ready_digests;
        Ok(())
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Shared {
    /// Whether the schedulers have agreed on `feature` (ADR 0020).
    fn uses(&self, feature: &str) -> bool {
        self.features
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .any(|f| f == feature)
    }

    fn offers(&self) -> Vec<pb::BackendOffer> {
        self.backends
            .iter()
            .zip(&self.driver_digests)
            .map(|(b, digest)| pb::BackendOffer {
                name: b.backend.name().to_owned(),
                capabilities_json: serde_json::to_string(&b.backend.capabilities())
                    .unwrap_or_default(),
                backend_version: b.backend_version.clone(),
                build: b.build.clone(),
                slots: b.slots,
                driver_sha256: digest.clone().unwrap_or_default(),
                agent_build: agent_build().unwrap_or_default(),
                driver_version: b.backend.driver_version(),
            })
            .collect()
    }

    fn gateway(&self) -> Gateway {
        self.gateways[self.current.load(Ordering::Relaxed) % self.gateways.len()].clone()
    }

    /// Registers with the current scheduler or, if it is unavailable or has an
    /// older epoch, with the next one that accepts the agent.
    async fn register(&self) -> Result<Duration, AgentError> {
        let n = self.gateways.len();
        let start = self.current.load(Ordering::Relaxed);
        let mut last = None;
        for i in 0..n {
            let index = (start + i) % n;
            let mut gateway = self.gateways[index].clone();
            let result = within(
                RPC_TIMEOUT,
                gateway.register(pb::RegisterRequest {
                    name: self.name.clone(),
                    host: self.host.clone(),
                    agent_version: env!("CARGO_PKG_VERSION").to_owned(),
                    backends: self.offers(),
                    features: algebramesh_core::features::SUPPORTED
                        .iter()
                        .map(|f| (*f).to_owned())
                        .collect(),
                }),
            )
            .await;
            let response = match result {
                Ok(r) => r.into_inner(),
                Err(s) if s.code() == tonic::Code::Unavailable => {
                    last = Some(s);
                    continue;
                }
                Err(s) => return Err(s.into()),
            };
            let seen = self.epoch.load(Ordering::SeqCst);
            if response.epoch < seen {
                tracing::warn!(
                    epoch = response.epoch,
                    seen,
                    "skipping a scheduler with an older epoch"
                );
                last = Some(tonic::Status::unavailable("scheduler with an older epoch"));
                continue;
            }
            self.epoch.fetch_max(response.epoch, Ordering::SeqCst);
            self.current.store(index, Ordering::Relaxed);
            tracing::info!(agent_id = %response.agent_id, epoch = response.epoch, "registered with the scheduler");
            *self.agent_id.write().await = response.agent_id;
            *self.features.write().unwrap_or_else(|e| e.into_inner()) = response.features;
            return Ok(Duration::from_millis(
                response.heartbeat_interval_ms.max(100).into(),
            ));
        }
        Err(last
            .unwrap_or_else(|| tonic::Status::unavailable("no scheduler"))
            .into())
    }

    async fn register_with_retry(&self, stop: &CancellationToken) -> Result<Duration, AgentError> {
        let mut delay = Duration::from_millis(500);
        loop {
            match self.register().await {
                Ok(interval) => return Ok(interval),
                Err(AgentError::Rpc(s)) if s.code() == tonic::Code::Unauthenticated => {
                    return Err(AgentError::Rpc(s));
                }
                Err(e) => tracing::warn!("registration failed, retrying: {e}"),
            }
            tokio::select! {
                _ = stop.cancelled() => return Err(AgentError::Config("stopped".into())),
                _ = tokio::time::sleep(delay) => {}
            }
            delay = (delay * 2).min(Duration::from_secs(10));
        }
    }

    fn running_ids(&self) -> Vec<String> {
        lock(&self.running).keys().cloned().collect()
    }

    fn cancel(&self, job_id: &str) {
        if let Some(r) = lock(&self.running).get(job_id) {
            r.cancel.cancel();
        }
    }

    fn cancel_all(&self) {
        for r in lock(&self.running).values() {
            r.cancel.cancel();
        }
    }

    async fn heartbeat_loop(self: Arc<Self>, interval: Duration, stop: CancellationToken) {
        let mut tick = tokio::time::interval(interval);
        loop {
            tokio::select! {
                _ = stop.cancelled() => return,
                _ = tick.tick() => {}
            }
            let agent_id = self.agent_id.read().await.clone();
            let request = pb::HeartbeatRequest {
                agent_id,
                running_job_ids: self.running_ids(),
            };
            match self.heartbeat_any(request).await {
                Ok(response) => {
                    let response = response.into_inner();
                    let stale = response.epoch < self.epoch.load(Ordering::SeqCst);
                    if response.known && !stale {
                        *self.features.write().unwrap_or_else(|e| e.into_inner()) =
                            response.features.clone();
                    }
                    for id in &response.cancel_job_ids {
                        tracing::info!(job_id = %id, "cancellation requested by the scheduler");
                        self.cancel(id);
                    }
                    if !response.known || stale {
                        tracing::warn!(
                            stale,
                            "the scheduler does not know this agent; registering again"
                        );
                        self.cancel_all();
                        if stale {
                            self.current.fetch_add(1, Ordering::Relaxed);
                        }
                        if let Err(e) = self.register().await {
                            tracing::warn!("re-registration failed: {e}");
                        }
                    }
                }
                Err(e) if e.code() == tonic::Code::Unavailable => {
                    // No scheduler serves now (an election, or no majority). The
                    // next heartbeat tries them again. A scheduler that does not
                    // know this agent (a new leader) says so, and the agent stops
                    // its jobs, which are LOST there, and registers again; the same
                    // leader serving again still knows the agent and its jobs.
                    tracing::warn!("no scheduler is available: {e}");
                }
                Err(e) => tracing::warn!("heartbeat failed: {e}"),
            }
        }
    }

    /// Sends a heartbeat to the current scheduler and, while they answer
    /// `UNAVAILABLE` (down, or not the leader), to the others in turn, so that a
    /// round through the URLs takes one heartbeat, not one per URL.
    async fn heartbeat_any(
        &self,
        request: pb::HeartbeatRequest,
    ) -> Result<tonic::Response<pb::HeartbeatResponse>, tonic::Status> {
        let mut last = None;
        for _ in 0..self.gateways.len() {
            let mut gateway = self.gateway();
            let result =
                tokio::time::timeout(HEARTBEAT_TIMEOUT, gateway.heartbeat(request.clone()))
                    .await
                    .unwrap_or_else(|_| Err(tonic::Status::unavailable("the heartbeat timed out")));
            match result {
                Err(e) if e.code() == tonic::Code::Unavailable && self.gateways.len() > 1 => {
                    tracing::debug!("scheduler unavailable ({e}); trying another");
                    self.current.fetch_add(1, Ordering::Relaxed);
                    last = Some(e);
                }
                other => return other,
            }
        }
        Err(last.unwrap_or_else(|| tonic::Status::unavailable("no scheduler")))
    }

    async fn slot_loop(self: Arc<Self>, index: usize, stop: CancellationToken) {
        let backend_name = self.backends[index].backend.name().to_owned();
        loop {
            if stop.is_cancelled() {
                return;
            }
            let agent_id = self.agent_id.read().await.clone();
            let mut gateway = self.gateway();
            // An assignment lost with a call that timed out is declared LOST by
            // the scheduler when it does not appear in the heartbeats.
            let poll = within(
                Duration::from_millis(POLL_WAIT_MS.into()) + RPC_TIMEOUT,
                gateway.poll_assignment(pb::PollRequest {
                    agent_id,
                    backends: vec![backend_name.clone()],
                    wait_ms: POLL_WAIT_MS,
                }),
            );
            let response = tokio::select! {
                _ = stop.cancelled() => return,
                r = poll => r,
            };
            match response {
                Ok(r) => {
                    if let Some(assignment) = r.into_inner().assignment {
                        self.run_assignment(index, assignment).await;
                    }
                }
                Err(e) => {
                    tracing::debug!("poll failed: {e}");
                    tokio::select! {
                        _ = stop.cancelled() => return,
                        _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                    }
                }
            }
        }
    }

    async fn run_assignment(&self, index: usize, assignment: pb::Assignment) {
        let agent_id = self.agent_id.read().await.clone();
        let job_id = assignment.job_id.clone();
        let cancel = CancellationToken::new();
        lock(&self.running).insert(
            job_id.clone(),
            Running {
                attempt: assignment.attempt,
                cancel: cancel.clone(),
            },
        );
        // However this function ends, the job leaves the heartbeats (Issue
        // #31), so that the scheduler declares it LOST if it was not reported.
        let _listed = Listed {
            running: &self.running,
            job_id: job_id.clone(),
        };
        let event = pb::JobEvent {
            agent_id: agent_id.clone(),
            job_id: job_id.clone(),
            attempt: assignment.attempt,
        };
        let mut gateway = self.gateway();
        if let Err(e) = within(RPC_TIMEOUT, gateway.report_started(event)).await {
            tracing::warn!(%job_id, "could not report start: {e}");
        }

        let mut outcome = match (
            JobSpec::from_json(&assignment.spec_json),
            job_id.parse::<uuid::Uuid>(),
        ) {
            (Ok(mut spec), Ok(uuid)) => 'run: {
                // Arguments in blobs are read before the backend runs, and
                // before its deadline is fixed (ADR 0021, B2). A blob that
                // cannot be read fails the job without calling the backend.
                if self.uses(algebramesh_core::features::BLOB_ARGUMENTS) {
                    let bytes = resolve::blob_bytes(&spec);
                    if bytes > 0 {
                        let deadline = tokio::time::Instant::now()
                            + REPORT_PATIENCE
                            + offload::extra_patience(bytes);
                        let gateway = || self.gateway();
                        if let Err(e) =
                            resolve::resolve_arguments(&mut spec, &gateway, deadline).await
                        {
                            tracing::error!(%job_id, "cannot read the arguments: {e}");
                            break 'run Err(e);
                        }
                    }
                }
                // References to values of other jobs' results (W2) are read
                // from their producers' records. Only an agent that names
                // `workflow_refs` is given such a job, and only once the
                // cluster has enabled it; this agent may not have heard yet.
                if !resolve::record_refs(&spec).is_empty() {
                    let deadline = tokio::time::Instant::now() + REPORT_PATIENCE;
                    let gateway = || self.gateway();
                    if let Err(e) = resolve::resolve_references(&mut spec, &gateway, deadline).await
                    {
                        tracing::error!(%job_id, "cannot read the referenced values: {e}");
                        break 'run Err(e);
                    }
                }
                tracing::info!(%job_id, operation = %spec.operation, "running job");
                // The runner fixes the deadline from the spec now, as the job starts.
                let ctx = ExecutionContext {
                    timeout: None,
                    cancel,
                    deadline: None,
                };
                // In a task of its own, so that a backend that panics fails
                // the job instead of stopping this slot (Issue #31).
                // In order: the backend runs, the verifier checks its result
                // and writes the guarantee, and the agent reports it; an
                // upload of large values (#36) will come between the last two.
                let backend = self.backends[index].backend.clone();
                let verifier = self.verifier.clone();
                let run = tokio::spawn(async move {
                    algebramesh_core::execute(backend.as_ref(), JobId(uuid), &spec, ctx, &verifier)
                        .await
                });
                let mut outcome = match run.await {
                    Ok(outcome) => outcome,
                    Err(e) => {
                        let what = if e.is_panic() {
                            let payload = e.into_panic();
                            payload
                                .downcast_ref::<&str>()
                                .map(|s| (*s).to_owned())
                                .or_else(|| payload.downcast_ref::<String>().cloned())
                                .unwrap_or_else(|| "a panic".into())
                        } else {
                            e.to_string()
                        };
                        tracing::error!(%job_id, "the backend failed unexpectedly: {what}");
                        Err(algebramesh_core::CasError::Internal(format!(
                            "the backend failed unexpectedly: {what}"
                        )))
                    }
                };
                if let Ok(result) = &mut outcome {
                    result
                        .log
                        .push(format!("executed by agent {} on {}", self.name, self.host));
                    if result.execution_host.is_none() {
                        result.execution_host = Some(self.host.clone());
                    }
                    // Only once every scheduler can keep these fields (ADR 0020).
                    if self.uses(algebramesh_core::features::BACKEND_PROVENANCE) {
                        result.backend.driver_sha256 = self.driver_digests[index].clone();
                        result.backend.agent_build = agent_build();
                    }
                    if !self.uses(algebramesh_core::features::RESULT_GUARANTEE) {
                        result.guarantee = None;
                    }
                }
                outcome
            }
            (Err(e), _) => Err(algebramesh_core::CasError::InvalidArguments(e.to_string())),
            (_, Err(e)) => Err(algebramesh_core::CasError::InvalidArguments(e.to_string())),
        };
        // Large values go to blobs before the report, once every scheduler
        // keeps them (ADR 0021). The job stays in the heartbeats meanwhile, and
        // the time it takes is not counted in its `wall_seconds`.
        let mut patience = REPORT_PATIENCE;
        if self.uses(algebramesh_core::features::BLOB_REFS)
            && let Ok(result) = &mut outcome
        {
            let bytes = offload::planned_bytes(&result.value);
            if bytes > 0 {
                patience += offload::extra_patience(bytes);
                let deadline = tokio::time::Instant::now() + patience;
                let gateway = || self.gateway();
                if let Err(e) = offload::offload(&mut result.value, &gateway, deadline).await {
                    tracing::error!(%job_id, "cannot store the result in blobs: {e}");
                    outcome = Err(e);
                }
            }
        }
        let outcome_json = serde_json::to_string(&outcome).unwrap_or_else(|e| {
            format!(r#"{{"Err":{{"code":"UNSUPPORTED_RESULT","message":"{e}"}}}}"#)
        });
        let attempt = lock(&self.running)
            .get(&job_id)
            .map_or(assignment.attempt, |r| r.attempt);
        let report = pb::JobFinished {
            agent_id,
            job_id: job_id.clone(),
            attempt,
            outcome_json,
        };
        // The job stays in `running`, and so in the heartbeats, until the report
        // is delivered or the agent gives up.
        let delivery = deliver(report, patience, |report| {
            let mut gateway = self.gateway();
            async move {
                gateway
                    .report_finished(report)
                    .await
                    .map(tonic::Response::into_inner)
            }
        })
        .await;
        match delivery {
            Delivery::Accepted => {}
            Delivery::Refused(reason) => {
                tracing::warn!(%job_id, %reason, "result not accepted");
            }
            Delivery::GaveUp(e) => tracing::error!(
                %job_id,
                "giving up reporting the result: {e}; the scheduler will declare the job LOST"
            ),
        }
    }
}

/// A job listed in the agent's heartbeats; removed from them when dropped.
struct Listed<'a> {
    running: &'a Mutex<HashMap<String, Running>>,
    job_id: String,
}

impl Drop for Listed<'_> {
    fn drop(&mut self) {
        lock(self.running).remove(&self.job_id);
    }
}

/// How a report ended.
#[derive(Debug)]
enum Delivery {
    Accepted,
    /// The scheduler answered and did not take it (a late or stale report).
    Refused(String),
    /// Every attempt failed for `patience`.
    GaveUp(tonic::Status),
}

/// The outcome of a job whose result could not be sent.
fn undeliverable(detail: String) -> String {
    let outcome: Result<algebramesh_core::CasResult, _> =
        Err(algebramesh_core::CasError::ResourceLimit(detail));
    serde_json::to_string(&outcome).unwrap_or_default()
}

/// Sends `report` until the scheduler answers it, retrying failures with a
/// growing delay for up to `patience`. `patience` is a deadline for the whole
/// delivery: each attempt waits at most [`RPC_TIMEOUT`] and never past it, so a
/// call that never returns cannot hold the job (Issue #14).
///
/// A result that cannot be sent at all (larger than a message, or refused by
/// the scheduler as too large or malformed) is replaced once by a small
/// `RESOURCE_LIMIT` error, so that the job ends `FAILED` instead of staying
/// active.
async fn deliver<F, Fut>(mut report: pb::JobFinished, patience: Duration, mut send: F) -> Delivery
where
    F: FnMut(pb::JobFinished) -> Fut,
    Fut: std::future::Future<Output = Result<pb::Ack, tonic::Status>>,
{
    let mut replaced = false;
    if report.outcome_json.len() > MAX_REPORT_BYTES {
        report.outcome_json = undeliverable(format!(
            "the result of {} bytes is larger than a message to the scheduler ({MAX_REPORT_BYTES} \
             bytes)",
            report.outcome_json.len()
        ));
        replaced = true;
    }
    let deadline = tokio::time::Instant::now() + patience;
    let mut delay = Duration::from_millis(200);
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            return Delivery::GaveUp(tonic::Status::unavailable(
                "no attempt was answered before the deadline",
            ));
        }
        match within(left.min(RPC_TIMEOUT), send(report.clone())).await {
            Ok(ack) if ack.accepted => return Delivery::Accepted,
            Ok(ack) => return Delivery::Refused(ack.reason),
            Err(e)
                if !replaced
                    && matches!(
                        e.code(),
                        tonic::Code::ResourceExhausted
                            | tonic::Code::InvalidArgument
                            | tonic::Code::OutOfRange
                    ) =>
            {
                tracing::warn!(job_id = %report.job_id, "the result cannot be sent ({e}); reporting an error instead");
                report.outcome_json = undeliverable(format!(
                    "the result could not be sent to the scheduler: {e}"
                ));
                replaced = true;
            }
            Err(e) => {
                if tokio::time::Instant::now() + delay >= deadline {
                    return Delivery::GaveUp(e);
                }
                tracing::warn!(job_id = %report.job_id, "reporting the result failed, retrying: {e}");
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(5));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::sync::Mutex;

    use super::*;

    /// Issues #100, #103: the agent checks the operations of exactly the
    /// packages it was built with.
    #[test]
    fn the_registry_checks_the_packages_built_in() {
        let registry = registry();
        assert_eq!(registry.checks("gkz.series"), cfg!(feature = "pkg-gkz"));
        assert_eq!(
            registry.checks("simplicial.homology"),
            cfg!(feature = "pkg-topology")
        );
        assert!(registry.checks("matrix.smith_normal_form"));
    }

    fn report(outcome_json: String) -> pb::JobFinished {
        pb::JobFinished {
            agent_id: "a".into(),
            job_id: "j".into(),
            attempt: 1,
            outcome_json,
        }
    }

    fn accepted() -> Result<pb::Ack, tonic::Status> {
        Ok(pb::Ack {
            accepted: true,
            reason: String::new(),
        })
    }

    /// A gateway that answers the reports with `answers`, in turn, then accepts;
    /// it records what it was sent.
    fn gateway(
        answers: Vec<Result<pb::Ack, tonic::Status>>,
        sent: &Mutex<Vec<String>>,
    ) -> impl FnMut(pb::JobFinished) -> std::future::Ready<Result<pb::Ack, tonic::Status>> + '_
    {
        let mut answers = answers.into_iter();
        move |r| {
            sent.lock().unwrap().push(r.outcome_json);
            std::future::ready(answers.next().unwrap_or_else(accepted))
        }
    }

    #[tokio::test(start_paused = true)]
    async fn unavailable_reports_are_retried_until_taken() {
        let sent = Mutex::new(Vec::new());
        let answers = (0..20)
            .map(|_| Err(tonic::Status::unavailable("down")))
            .collect();
        let delivery = deliver(
            report("{}".into()),
            REPORT_PATIENCE,
            gateway(answers, &sent),
        )
        .await;
        assert!(matches!(delivery, Delivery::Accepted), "{delivery:?}");
        // More than the eight attempts of before, all with the same outcome.
        let sent = sent.into_inner().unwrap();
        assert_eq!(sent.len(), 21);
        assert!(sent.iter().all(|s| s == "{}"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_report_that_never_goes_through_gives_up_after_the_patience() {
        let sent = Mutex::new(Vec::new());
        let answers = (0..1000)
            .map(|_| Err(tonic::Status::unavailable("down")))
            .collect();
        let started = tokio::time::Instant::now();
        let delivery = deliver(
            report("{}".into()),
            REPORT_PATIENCE,
            gateway(answers, &sent),
        )
        .await;
        assert!(matches!(delivery, Delivery::GaveUp(_)), "{delivery:?}");
        assert!(started.elapsed() <= REPORT_PATIENCE);
        assert!(started.elapsed() > REPORT_PATIENCE - Duration::from_secs(6));
    }

    #[tokio::test(start_paused = true)]
    async fn a_result_the_scheduler_cannot_take_becomes_an_error() {
        let sent = Mutex::new(Vec::new());
        let answers = vec![Err(tonic::Status::resource_exhausted("message too large"))];
        let delivery = deliver(
            report("{\"Ok\":1}".into()),
            REPORT_PATIENCE,
            gateway(answers, &sent),
        )
        .await;
        assert!(matches!(delivery, Delivery::Accepted), "{delivery:?}");
        let sent = sent.into_inner().unwrap();
        assert_eq!(sent.len(), 2);
        let outcome: Result<algebramesh_core::CasResult, algebramesh_core::CasError> =
            serde_json::from_str(&sent[1]).unwrap();
        assert!(
            matches!(&outcome, Err(algebramesh_core::CasError::ResourceLimit(m)) if m.contains("message too large")),
            "{outcome:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_result_larger_than_a_message_is_not_sent() {
        let sent = Mutex::new(Vec::new());
        let big = "x".repeat(MAX_REPORT_BYTES + 1);
        let delivery = deliver(report(big), REPORT_PATIENCE, gateway(vec![], &sent)).await;
        assert!(matches!(delivery, Delivery::Accepted), "{delivery:?}");
        let sent = sent.into_inner().unwrap();
        assert_eq!(sent.len(), 1);
        assert!(sent[0].len() < 1000, "{}", sent[0].len());
        assert!(sent[0].contains("RESOURCE_LIMIT"), "{}", sent[0]);
    }

    #[tokio::test(start_paused = true)]
    async fn calls_that_never_answer_give_up_by_the_deadline() {
        // Every attempt hangs (a handler that never answers on a live
        // connection): each one is cut at RPC_TIMEOUT, and the delivery gives up
        // by the patience.
        let attempts = std::sync::atomic::AtomicUsize::new(0);
        let started = tokio::time::Instant::now();
        let delivery = deliver(report("{}".into()), REPORT_PATIENCE, |_| {
            attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            std::future::pending::<Result<pb::Ack, tonic::Status>>()
        })
        .await;
        assert!(matches!(delivery, Delivery::GaveUp(_)), "{delivery:?}");
        assert!(
            started.elapsed() <= REPORT_PATIENCE,
            "{:?}",
            started.elapsed()
        );
        let n = attempts.into_inner();
        assert!(n >= 3, "{n} attempts");
        // A short patience is a deadline too, even inside one attempt.
        let started = tokio::time::Instant::now();
        let delivery = deliver(report("{}".into()), Duration::from_secs(7), |_| {
            std::future::pending::<Result<pb::Ack, tonic::Status>>()
        })
        .await;
        assert!(matches!(delivery, Delivery::GaveUp(_)), "{delivery:?}");
        assert_eq!(started.elapsed(), Duration::from_secs(7));
    }

    #[tokio::test(start_paused = true)]
    async fn a_hung_call_is_followed_by_an_answered_one() {
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let delivery = deliver(report("{}".into()), REPORT_PATIENCE, |_| {
            let first = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0;
            async move {
                if first {
                    std::future::pending::<()>().await;
                }
                accepted()
            }
        })
        .await;
        assert!(matches!(delivery, Delivery::Accepted), "{delivery:?}");
        assert_eq!(calls.into_inner(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn within_cuts_a_call_that_does_not_answer() {
        let started = tokio::time::Instant::now();
        let r = within(
            Duration::from_secs(3),
            std::future::pending::<Result<(), tonic::Status>>(),
        )
        .await;
        assert_eq!(r.unwrap_err().code(), tonic::Code::Unavailable);
        assert_eq!(started.elapsed(), Duration::from_secs(3));
    }

    #[tokio::test(start_paused = true)]
    async fn a_refused_report_is_not_sent_again() {
        let sent = Mutex::new(Vec::new());
        let answers = vec![Ok(pb::Ack {
            accepted: false,
            reason: "the job was declared LOST".into(),
        })];
        let delivery = deliver(
            report("{}".into()),
            REPORT_PATIENCE,
            gateway(answers, &sent),
        )
        .await;
        assert!(matches!(delivery, Delivery::Refused(r) if r.contains("LOST")));
        assert_eq!(sent.into_inner().unwrap().len(), 1);
    }

    /// A backend whose preparation succeeds or fails, with at most one driver file.
    struct Prepared {
        name: &'static str,
        ready: bool,
        driver: Option<std::path::PathBuf>,
    }

    #[async_trait::async_trait]
    impl CasBackend for Prepared {
        fn name(&self) -> &'static str {
            self.name
        }

        fn capabilities(&self) -> algebramesh_core::Capabilities {
            algebramesh_core::Capabilities::default()
        }

        async fn execute(
            &self,
            _: algebramesh_core::CasRequest,
            _: ExecutionContext,
        ) -> Result<algebramesh_core::CasResult, algebramesh_core::CasError> {
            Err(algebramesh_core::CasError::Cancelled)
        }

        async fn prepare(&self) -> Result<Option<String>, algebramesh_core::CasError> {
            if self.ready {
                Ok(None)
            } else {
                Err(algebramesh_core::CasError::Unavailable(
                    "no executable".into(),
                ))
            }
        }

        fn driver_files(&self) -> Vec<std::path::PathBuf> {
            self.driver.iter().cloned().collect()
        }
    }

    /// The backends offered after preparation, each with the digest of its
    /// offer and the digest its results are given.
    async fn digests_after_preparation(
        backends: Vec<Prepared>,
    ) -> Vec<(&'static str, String, Option<String>)> {
        let slots = backends
            .into_iter()
            .map(|b| BackendSlot {
                backend: Arc::new(b),
                slots: 1,
                build: String::new(),
                backend_version: String::new(),
            })
            .collect();
        let connection = Connection {
            url: "https://127.0.0.1:1".into(),
            ca_pem: Vec::new(),
            domain: String::new(),
            token: "t".into(),
            client_identity: None,
        };
        let mut agent = Agent::connect(connection, "a".into(), "h".into(), slots)
            .await
            .unwrap();
        agent.prepare_backends().await.unwrap();
        let shared = &agent.shared;
        shared
            .offers()
            .into_iter()
            .enumerate()
            .map(|(index, offer)| {
                assert_eq!(offer.name, shared.backends[index].backend.name());
                (
                    shared.backends[index].backend.name(),
                    offer.driver_sha256,
                    shared.driver_digests[index].clone(),
                )
            })
            .collect()
    }

    #[tokio::test]
    async fn driver_digests_follow_the_backends_that_were_prepared() {
        let temp = tempfile::Builder::new()
            .prefix("am-agent-driver-")
            .tempdir()
            .unwrap();
        let dir = temp.path().to_path_buf();
        std::fs::create_dir_all(&dir).unwrap();
        let (m2, jl, echo) = (
            dir.join("algebramesh.m2"),
            dir.join("algebramesh.jl"),
            dir.join("echo.txt"),
        );
        std::fs::write(&m2, b"-- macaulay2 driver\n").unwrap();
        std::fs::write(&jl, b"# oscar driver\n").unwrap();
        std::fs::write(&echo, b"echo driver\n").unwrap();
        let digest = |path: &std::path::PathBuf| {
            algebramesh_core::driver_sha256(std::slice::from_ref(path))
                .unwrap()
                .unwrap()
        };
        let backend = |name, ready, driver: Option<&std::path::PathBuf>| Prepared {
            name,
            ready,
            driver: driver.cloned(),
        };

        // The first backend fails.
        let offered = digests_after_preparation(vec![
            backend("macaulay2", false, Some(&m2)),
            backend("oscar", true, Some(&jl)),
        ])
        .await;
        assert_eq!(offered, vec![("oscar", digest(&jl), Some(digest(&jl)))]);

        // The first backend, without driver files, and a middle one fail.
        let offered = digests_after_preparation(vec![
            backend("asir", false, None),
            backend("macaulay2", true, Some(&m2)),
            backend("oscar", false, Some(&jl)),
            backend("echo", true, Some(&echo)),
        ])
        .await;
        assert_eq!(
            offered,
            vec![
                ("macaulay2", digest(&m2), Some(digest(&m2))),
                ("echo", digest(&echo), Some(digest(&echo))),
            ]
        );

        // Every backend is prepared.
        let offered = digests_after_preparation(vec![
            backend("asir", true, None),
            backend("macaulay2", true, Some(&m2)),
            backend("oscar", true, Some(&jl)),
        ])
        .await;
        assert_eq!(
            offered,
            vec![
                ("asir", String::new(), None),
                ("macaulay2", digest(&m2), Some(digest(&m2))),
                ("oscar", digest(&jl), Some(digest(&jl))),
            ]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

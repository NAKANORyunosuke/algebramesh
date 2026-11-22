//! The Raft cluster of the integration tests (`raft.rs`, `raft_faults.rs`):
//! schedulers, a client and an agent in one process, with helpers that wait
//! for what the tests rely on.

#![allow(dead_code, clippy::unwrap_used)]

pub use std::collections::BTreeMap;
pub use std::net::SocketAddr;
pub use std::path::PathBuf;
pub use std::sync::Arc;
pub use std::time::Duration;

pub use algebramesh_agent::{Agent, BackendSlot, Connection};
pub use algebramesh_client::{Client, ClientConfig, ClientError};
pub use algebramesh_core::{
    BackendInfo, Capabilities, CasBackend, CasError, CasRequest, CasResult, ExecutionContext,
    JobState,
};
pub use algebramesh_rpc::{DevPki, generate_dev_pki};
pub use algebramesh_server::{
    ClusterConfig, Member, Node, SchedulerOptions, Security, ServeError, Store, serve_node,
};
pub use tokio::net::{TcpListener, TcpSocket};
pub use tokio_util::sync::CancellationToken;

pub const CLIENT_TOKEN: &str = "client-token";
/// A second client token every scheduler accepts.
pub const OTHER_CLIENT_TOKEN: &str = "other-client-token";
pub const AGENT_TOKEN: &str = "agent-token";
pub const PEER_TOKEN: &str = "peer-token";

pub struct EchoBackend;

#[async_trait::async_trait]
impl CasBackend for EchoBackend {
    fn name(&self) -> &'static str {
        "test"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            operations: vec!["test.sleep".into()],
            ..Capabilities::default()
        }
    }

    async fn execute(&self, job: CasRequest, _: ExecutionContext) -> Result<CasResult, CasError> {
        Ok(CasResult {
            value: job.arguments,
            backend: BackendInfo {
                name: "test".into(),
                backend_version: Some("1".into()),
                driver_version: "test".into(),
                build: None,
                ..BackendInfo::default()
            },
            execution_host: None,
            log: vec![],
            guarantee: None,
        })
    }
}

/// [`EchoBackend`] with a driver file, for the provenance of results.
pub struct DriverBackend {
    pub driver: PathBuf,
}

#[async_trait::async_trait]
impl CasBackend for DriverBackend {
    fn name(&self) -> &'static str {
        "test"
    }

    fn capabilities(&self) -> Capabilities {
        EchoBackend.capabilities()
    }

    async fn execute(&self, job: CasRequest, ctx: ExecutionContext) -> Result<CasResult, CasError> {
        EchoBackend.execute(job, ctx).await
    }

    fn driver_version(&self) -> String {
        "test-driver 1".into()
    }

    fn driver_files(&self) -> Vec<PathBuf> {
        vec![self.driver.clone()]
    }
}

pub fn options() -> SchedulerOptions {
    SchedulerOptions {
        heartbeat_interval: Duration::from_millis(200),
        heartbeat_timeout: Duration::from_secs(5),
        ..SchedulerOptions::default()
    }
}

pub fn bind(addr: SocketAddr) -> TcpListener {
    let socket = TcpSocket::new_v4().unwrap();
    socket.set_reuseaddr(true).unwrap();
    socket.bind(addr).unwrap();
    socket.listen(64).unwrap()
}

pub fn url(addr: SocketAddr) -> String {
    format!("https://localhost:{}", addr.port())
}

pub struct Scheduler {
    pub node: Arc<Node>,
    pub stop: CancellationToken,
    pub served: tokio::task::JoinHandle<Result<(), ServeError>>,
}

impl Scheduler {
    pub async fn stop(self) {
        self.stop.cancel();
        self.served.await.unwrap().unwrap();
    }

    /// Waits until the scheduler stops by itself after its Raft stopped (an
    /// injected crash, a full disk), and returns the error it ended with.
    pub async fn stopped_by_itself(self) -> String {
        let budget = (election_timeout() * 100).max(Duration::from_secs(30));
        match tokio::time::timeout(budget, self.served).await {
            Ok(Ok(Err(e))) => e.to_string(),
            other => panic!("the scheduler did not stop with an error: {other:?}"),
        }
    }
}

pub struct Cluster {
    pub pki: DevPki,
    pub dir: PathBuf,
    pub addrs: Vec<SocketAddr>,
    /// The first `initial` schedulers form the cluster; the others join later.
    pub initial: usize,
    pub election_timeout: Duration,
    pub snapshot_chunk_bytes: usize,
    /// Data directories elsewhere than under `dir` (a small file system).
    pub data_dirs: BTreeMap<usize, PathBuf>,
    /// Addresses at which the others reach a scheduler, instead of its own
    /// (a proxy in front of it).
    pub reached_at: BTreeMap<usize, SocketAddr>,
    /// The features a scheduler can use, fewer than its build's to stand for
    /// an older scheduler (ADR 0020).
    pub features: BTreeMap<usize, Vec<String>>,
    /// `blob_upload_bytes_per_token_per_hour` of every scheduler; 0: none.
    pub upload_allowance: u64,
}

impl Cluster {
    /// Reserves a port for each of `n` schedulers, all in the first cluster.
    pub fn new(n: usize) -> (Self, Vec<TcpListener>) {
        Self::with_initial(n, n)
    }

    /// Reserves a port for each of `n` schedulers; the first `initial` form the
    /// cluster.
    pub fn with_initial(n: usize, initial: usize) -> (Self, Vec<TcpListener>) {
        test_log();
        let listeners: Vec<TcpListener> = (0..n)
            .map(|_| bind("127.0.0.1:0".parse().unwrap()))
            .collect();
        let addrs = listeners.iter().map(|l| l.local_addr().unwrap()).collect();
        let cluster = Self {
            pki: generate_dev_pki(&["localhost".into()]).unwrap(),
            dir: std::env::temp_dir().join(format!("am-raft-{}", uuid::Uuid::new_v4())),
            addrs,
            initial,
            election_timeout: election_timeout(),
            // Small chunks, so that a snapshot goes in several.
            snapshot_chunk_bytes: 64 * 1024,
            data_dirs: BTreeMap::new(),
            reached_at: BTreeMap::new(),
            features: BTreeMap::new(),
            upload_allowance: 0,
        };
        (cluster, listeners)
    }

    /// Keeps the arguments of every job inline: no scheduler takes blobs in
    /// arguments (ADR 0021, B2), so that large arguments make the log and
    /// the snapshots large, as tests of them need.
    pub fn without_blob_arguments(&mut self) {
        let features: Vec<String> = algebramesh_server::cluster::supported_features()
            .into_iter()
            .filter(|f| f != algebramesh_core::features::BLOB_ARGUMENTS)
            .collect();
        for i in 0..self.addrs.len() {
            self.features.insert(i, features.clone());
        }
    }

    pub fn member(&self, i: usize) -> Member {
        Member {
            name: format!("s{}", i + 1),
            url: url(*self.reached_at.get(&i).unwrap_or(&self.addrs[i])),
            domain: "localhost".into(),
        }
    }

    pub fn members(&self) -> BTreeMap<u64, Member> {
        (0..self.initial)
            .map(|i| (i as u64 + 1, self.member(i)))
            .collect()
    }

    /// Starts scheduler `i` (0-based) on `listener`, over its data directory.
    pub fn data_dir(&self, i: usize) -> PathBuf {
        self.data_dirs
            .get(&i)
            .cloned()
            .unwrap_or_else(|| self.dir.join(format!("s{}", i + 1)))
    }

    pub async fn start(&self, i: usize, listener: TcpListener) -> Scheduler {
        let data = self.data_dir(i);
        std::fs::create_dir_all(&data).unwrap();
        let store = Store::open(
            Some(&data.join("metadata.sqlite3")),
            Some(data.join("artifacts")),
        )
        .unwrap();
        let join = i >= self.initial;
        let mut options = options();
        options.blobs.upload_bytes_per_token_per_hour = self.upload_allowance;
        let node = Node::start_cluster(
            store,
            options,
            ClusterConfig {
                node_id: i as u64 + 1,
                members: if join {
                    BTreeMap::from([(i as u64 + 1, self.member(i))])
                } else {
                    self.members()
                },
                join,
                ca_pem: self.pki.ca_cert_pem.clone().into_bytes(),
                peer_token: PEER_TOKEN.into(),
                election_timeout: self.election_timeout,
                data_dir: Some(data),
                snapshot_chunk_bytes: self.snapshot_chunk_bytes,
                features: self
                    .features
                    .get(&i)
                    .cloned()
                    .unwrap_or_else(algebramesh_server::cluster::supported_features),
            },
        )
        .await
        .unwrap();
        let security = Security {
            cert_pem: self.pki.server_cert_pem.clone().into_bytes(),
            key_pem: self.pki.server_key_pem.clone().into_bytes(),
            client_tokens: vec![CLIENT_TOKEN.into(), OTHER_CLIENT_TOKEN.into()],
            agent_tokens: vec![AGENT_TOKEN.into()],
            agent_ca_pem: None,
            revoked_agent_serials: vec![],
            peer_tokens: vec![PEER_TOKEN.into()],
        };
        let stop = CancellationToken::new();
        let token = stop.clone();
        let served = tokio::spawn(serve_node(node.clone(), security, listener, async move {
            token.cancelled().await
        }));
        Scheduler { node, stop, served }
    }

    /// Starts scheduler `i` again on its own address.
    pub async fn restart(&self, i: usize) -> Scheduler {
        self.start(i, bind(self.addrs[i])).await
    }

    pub fn urls(&self) -> String {
        self.addrs
            .iter()
            .map(|a| url(*a))
            .collect::<Vec<_>>()
            .join(",")
    }

    pub async fn client(&self) -> Client {
        self.client_with_token(CLIENT_TOKEN).await
    }

    pub async fn client_with_token(&self, token: &str) -> Client {
        let mut last = None;
        // The first connection needs a scheduler that accepts connections.
        for _ in 0..50 {
            match Client::connect(ClientConfig {
                url: self.urls(),
                ca_pem: self.pki.ca_cert_pem.clone().into_bytes(),
                domain: None,
                token: token.into(),
            })
            .await
            {
                Ok(c) => return c,
                Err(e) => last = Some(e),
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("cannot connect: {last:?}");
    }

    pub async fn agent(&self) -> CancellationToken {
        self.agent_with(Arc::new(EchoBackend)).await
    }

    /// An agent with `backend` in its one slot.
    pub async fn agent_with(&self, backend: Arc<dyn CasBackend>) -> CancellationToken {
        let agent = Agent::connect(
            Connection {
                url: self.urls(),
                ca_pem: self.pki.ca_cert_pem.clone().into_bytes(),
                domain: String::new(),
                token: AGENT_TOKEN.into(),
                client_identity: None,
            },
            "worker".into(),
            "worker.test".into(),
            vec![BackendSlot {
                backend,
                slots: 1,
                build: "b".into(),
                backend_version: "1".into(),
            }],
        )
        .await
        .unwrap();
        let stop = CancellationToken::new();
        let token = stop.clone();
        tokio::spawn(async move {
            let _ = agent.run(token).await;
        });
        stop
    }
}

impl Drop for Cluster {
    /// Removes the data directories, or leaves them to [`LEFTOVERS`] when
    /// they cannot be removed yet: the schedulers of the test still run until
    /// its runtime stops, and Windows does not remove a file they hold open.
    fn drop(&mut self) {
        let dirs = std::iter::once(&self.dir).chain(self.data_dirs.values());
        for dir in dirs {
            if std::fs::remove_dir_all(dir).is_err() && dir.exists() {
                LEFTOVERS.with(|l| l.0.borrow_mut().push(dir.clone()));
            }
        }
    }
}

/// Directories of clusters that could not be removed when the cluster was
/// dropped. libtest runs each test on a thread of its own, and the runtime
/// of `#[tokio::test]` stops before the thread ends, closing the files of
/// its schedulers: the directories are removed then, when the thread's
/// locals are dropped.
struct Leftovers(std::cell::RefCell<Vec<PathBuf>>);

impl Drop for Leftovers {
    fn drop(&mut self) {
        for dir in self.0.take() {
            // A blocking task of the runtime may still be closing a file.
            for _ in 0..50 {
                if std::fs::remove_dir_all(&dir).is_ok() || !dir.exists() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

thread_local! {
    static LEFTOVERS: Leftovers = const { Leftovers(std::cell::RefCell::new(Vec::new())) };
}

pub fn spec() -> String {
    serde_json::json!({
        "schema_version": 1, "backend": "test", "operation": "test.sleep",
        "arguments": {"ms": 1},
    })
    .to_string()
}

/// The election timeout of the test clusters: 300 ms, or
/// `ALGEBRAMESH_TEST_ELECTION_TIMEOUT_MS` on a host too slow for it (Issue
/// #41), where heartbeats come late and the followers start elections that
/// the tests do not expect.
pub fn election_timeout() -> Duration {
    std::env::var("ALGEBRAMESH_TEST_ELECTION_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .map_or(Duration::from_millis(300), Duration::from_millis)
}

/// Logs of the schedulers on standard error, with `ALGEBRAMESH_TEST_LOG` as
/// the filter (for example `info` or `algebramesh_server=debug`).
pub fn test_log() {
    if let Ok(filter) = std::env::var("ALGEBRAMESH_TEST_LOG") {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new(filter))
            .with_test_writer()
            .try_init();
    }
}

/// Waits until every running scheduler has enabled every feature its build
/// can use (ADR 0020). The leader asks the others over new connections until
/// then, which a test that counts connections must not see.
pub async fn features_agreed(schedulers: &[Option<Scheduler>]) {
    let mut all = algebramesh_server::cluster::supported_features();
    all.sort();
    eventually("the cluster has enabled its features", || {
        schedulers
            .iter()
            .flatten()
            .all(|s| s.node.status().enabled_features == all)
    })
    .await;
}

pub async fn eventually(what: &str, mut check: impl FnMut() -> bool) {
    // 15 s with the default election timeout, longer with a longer one.
    let budget = (election_timeout() * 50).max(Duration::from_secs(15));
    let started = std::time::Instant::now();
    while started.elapsed() < budget {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting until {what}");
}

/// Starts `call` until its change is in the leader's working store, whose
/// changes the test holds back from Raft: the call then waits for a commit
/// that does not come. On a slow host the leader may refuse a call before
/// writing it (`UNAVAILABLE`, when a majority has not confirmed it lately);
/// such a call is started again (Issue #41).
pub async fn held_call<T, F, Fut>(
    node: &Node,
    mut call: F,
) -> tokio::task::JoinHandle<Result<T, ClientError>>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, ClientError>> + Send + 'static,
    T: Send + std::fmt::Debug + 'static,
{
    let written = || {
        node.scheduler()
            .ok()
            .map(|s| s.with_store(|st| st.last_seq().unwrap()))
    };
    for _ in 0..20 {
        let mut before = None;
        eventually("the leader serves", || {
            before = written();
            before.is_some()
        })
        .await;
        let before = before.unwrap();
        let task = tokio::spawn(call());
        let mut refused = false;
        eventually("the call is written or refused", || {
            refused = task.is_finished();
            refused || written().is_some_and(|s| s > before)
        })
        .await;
        if !refused {
            return task;
        }
        match task.await.unwrap() {
            Err(e) if e.to_string().contains("Unavailable") => {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            other => panic!("the held call ended: {other:?}"),
        }
    }
    panic!("the leader refused the call every time");
}

/// Watches the term of the cluster while a test relies on one leader. If the
/// test fails while the watch is alive and the term has changed, it says so:
/// an election the test did not cause broke its premise, which on a slow host
/// is no defect of what the test checks (Issue #41).
pub struct ElectionWatch {
    nodes: Vec<Arc<Node>>,
    term: u64,
}

impl ElectionWatch {
    pub fn new(schedulers: &[Option<&Scheduler>]) -> Self {
        let nodes: Vec<Arc<Node>> = schedulers
            .iter()
            .flatten()
            .map(|s| s.node.clone())
            .collect();
        let term = Self::term_of(&nodes);
        Self { nodes, term }
    }

    pub fn term_of(nodes: &[Arc<Node>]) -> u64 {
        nodes.iter().map(|n| n.status().epoch).max().unwrap_or(0)
    }
}

impl Drop for ElectionWatch {
    fn drop(&mut self) {
        let term = Self::term_of(&self.nodes);
        if std::thread::panicking() && term != self.term {
            eprintln!(
                "note: the term changed from {} to {term} while the test relied on one \
                 leader: an election the test did not cause broke its premise. On a slow \
                 host, set ALGEBRAMESH_TEST_ELECTION_TIMEOUT_MS above {} (Issue #41).",
                self.term,
                election_timeout().as_millis()
            );
        }
    }
}

/// Runs a job to its end. A submission whose outcome is unknown (a change of
/// leader in between) is sent again with the same key, as clients are told to.
pub async fn run_job(client: &Client) -> String {
    let key = uuid::Uuid::new_v4().to_string();
    let mut tries = 0;
    let id = loop {
        match client.submit_json_with_request_id(&spec(), 0, &key).await {
            Ok(status) => break status.job_id,
            Err(e)
                if tries < 20
                    && (e.to_string().contains("Unavailable")
                        || e.to_string().contains("Aborted")) =>
            {
                tries += 1;
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            Err(e) => panic!("{e}"),
        }
    };
    let record = client
        .wait(&id, Some(Duration::from_secs(30)), false)
        .await
        .unwrap();
    assert_eq!(record.state, JobState::Succeeded, "{:?}", record.error);
    id
}

/// Index of the scheduler that serves calls, once there is exactly one and
/// every other scheduler given knows it as the leader of the same term: right
/// after an election, another node may still be a candidate of a later term
/// and depose the new leader (Issue #41).
pub async fn serving(schedulers: &[Option<&Scheduler>]) -> usize {
    let mut found = None;
    eventually(
        "one scheduler serves calls and the others follow it",
        || {
            let statuses: Vec<(usize, algebramesh_rpc::pb::NodeStatus)> = schedulers
                .iter()
                .enumerate()
                .filter_map(|(i, s)| s.map(|s| (i, s.node.status())))
                .collect();
            let serving: Vec<&(usize, algebramesh_rpc::pb::NodeStatus)> =
                statuses.iter().filter(|(_, s)| s.serving).collect();
            let [(index, leader)] = serving.as_slice() else {
                return false;
            };
            found = Some(*index);
            statuses
                .iter()
                .all(|(_, s)| s.epoch == leader.epoch && s.leader == leader.node_id)
        },
    )
    .await;
    found.unwrap()
}

pub fn all(schedulers: &[Option<Scheduler>]) -> Vec<Option<&Scheduler>> {
    schedulers.iter().map(Option::as_ref).collect()
}

/// A job of about `bytes` bytes, queued (no agent takes it).
pub fn padded(bytes: usize) -> String {
    serde_json::json!({
        "schema_version": 1, "backend": "test", "operation": "test.sleep",
        "arguments": {"ms": 1, "blob": "x".repeat(bytes)},
    })
    .to_string()
}

/// Submits `n` jobs of `bytes` bytes with keys `{prefix}-{k}`, sending again
/// a call whose outcome is unknown.
pub async fn submit_padded(client: &Client, prefix: &str, n: usize, bytes: usize) {
    for k in 0..n {
        let key = format!("{prefix}-{k}");
        let mut tries = 0;
        while let Err(e) = client
            .submit_json_with_request_id(&padded(bytes), 0, &key)
            .await
        {
            tries += 1;
            assert!(tries < 20, "{key}: {e}");
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}

pub const ALL_STATES: [JobState; 9] = [
    JobState::Queued,
    JobState::Assigned,
    JobState::Running,
    JobState::Succeeded,
    JobState::Failed,
    JobState::TimedOut,
    JobState::Cancelling,
    JobState::Cancelled,
    JobState::Lost,
];

/// What a node has applied: its jobs with their states, the last change, the
/// log position and the membership of its state machine.
pub type Applied = (Vec<(String, JobState)>, i64, Option<String>, Option<String>);

pub fn applied(node: &Node) -> Applied {
    node.with_applied_state(|s| {
        let mut jobs: Vec<(String, JobState)> = s
            .jobs_in_states(&ALL_STATES)
            .unwrap()
            .into_iter()
            .map(|r| (r.record.job_id.to_string(), r.record.state))
            .collect();
        jobs.sort_by(|a, b| a.0.cmp(&b.0));
        (
            jobs,
            s.last_seq().unwrap(),
            s.meta("raft_applied").unwrap(),
            s.meta("raft_membership").unwrap(),
        )
    })
    .unwrap()
}

/// Files that an interrupted snapshot or restore leaves behind.
pub fn stale_files(dir: &std::path::Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| {
            (n.starts_with("snapshot-") && n.ends_with(".sqlite3")) || n.ends_with(".restore")
        })
        .collect()
}

/// Waits until `node` has applied what the leader has, and checks that they
/// agree: the same jobs in the same states, the same last change, log
/// position and membership.
pub async fn agrees_with_leader(schedulers: &[Option<Scheduler>], node: &Node) {
    // Catching up may take a snapshot transfer, sent again after failures.
    let budget = (election_timeout() * 100).max(Duration::from_secs(60));
    let started = std::time::Instant::now();
    loop {
        let leader = schedulers
            .iter()
            .flatten()
            .find(|s| s.node.status().serving);
        if let Some(leader) = leader {
            let (a, b) = (applied(&leader.node), applied(node));
            if a == b {
                assert!(!a.0.is_empty());
                return;
            }
            if started.elapsed() > budget {
                panic!(
                    "the scheduler does not agree with the leader: {:?} has {} jobs, last change {}, applied {:?}; it has {} jobs, last change {}, applied {:?}",
                    leader.node.status(),
                    a.0.len(),
                    a.1,
                    a.2,
                    b.0.len(),
                    b.1,
                    b.2,
                );
            }
        } else if started.elapsed() > budget {
            panic!("no scheduler serves");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Makes scheduler `victim` the leader (`want`) or a follower: the leader
/// hands over, and the nodes that are not to lead hold their elections.
pub async fn lead_or_follow(schedulers: &[Option<Scheduler>], victim: usize, want: bool) {
    for _ in 0..10 {
        let leader = serving(&all(schedulers)).await;
        if (leader == victim) == want {
            return;
        }
        for (i, s) in schedulers.iter().enumerate() {
            let Some(s) = s else { continue };
            if i != leader && (i == victim) != want {
                s.node.hold_elections(election_timeout() * 20);
            }
        }
        schedulers[leader].as_ref().unwrap().node.hand_over();
        eventually("another scheduler leads", || {
            schedulers
                .iter()
                .enumerate()
                .any(|(i, s)| i != leader && s.as_ref().is_some_and(|s| s.node.status().serving))
        })
        .await;
    }
    panic!("could not make scheduler {victim} the leader ({want}) or a follower");
}

// ------------------------------------------------------- a proxy (#37, #74)

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// A TCP proxy in front of a scheduler: it can cut connections after some
/// bytes, slow them down, or pause them. It sees only TLS records.
pub struct Proxy {
    pub addr: SocketAddr,
    /// Cut a connection after this many bytes towards the scheduler (0: never),
    /// for the next `cuts_left` connections.
    pub cut_after: AtomicU64,
    pub cuts_left: AtomicUsize,
    /// Cut a connection after this many bytes from the scheduler (0: never),
    /// for the next `cuts_left` connections.
    pub cut_answers_after: AtomicU64,
    /// Connections cut so far.
    pub cuts: AtomicUsize,
    /// Delay per 16 KiB forwarded, in milliseconds.
    pub delay_ms: AtomicU64,
    /// Forwarding waits while this is set.
    pub paused: tokio::sync::watch::Sender<bool>,
}

impl Proxy {
    /// Takes one of the cuts left, if any. (Not `fetch_update`, deprecated
    /// since Rust 1.99, nor `try_update`, newer than the minimum version.)
    fn take_cut(&self) -> bool {
        let mut left = self.cuts_left.load(Ordering::SeqCst);
        while left > 0 {
            match self.cuts_left.compare_exchange(
                left,
                left - 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return true,
                Err(now) => left = now,
            }
        }
        false
    }

    pub async fn start(target: SocketAddr) -> Arc<Self> {
        let listener = bind("127.0.0.1:0".parse().unwrap());
        let proxy = Arc::new(Self {
            addr: listener.local_addr().unwrap(),
            cut_after: AtomicU64::new(0),
            cuts_left: AtomicUsize::new(0),
            cut_answers_after: AtomicU64::new(0),
            cuts: AtomicUsize::new(0),
            delay_ms: AtomicU64::new(0),
            paused: tokio::sync::watch::channel(false).0,
        });
        let p = proxy.clone();
        tokio::spawn(async move {
            while let Ok((inbound, _)) = listener.accept().await {
                let p = p.clone();
                tokio::spawn(async move {
                    let Ok(outbound) = tokio::net::TcpStream::connect(target).await else {
                        return;
                    };
                    let (cut, cut_answers) = if p.take_cut() {
                        (
                            p.cut_after.load(Ordering::SeqCst),
                            p.cut_answers_after.load(Ordering::SeqCst),
                        )
                    } else {
                        (0, 0)
                    };
                    let (ri, wi) = inbound.into_split();
                    let (ro, wo) = outbound.into_split();
                    let mut up = tokio::spawn(Self::forward(p.clone(), ri, wo, cut));
                    let mut down = tokio::spawn(Self::forward(p.clone(), ro, wi, cut_answers));
                    // Either direction ending ends the connection.
                    tokio::select! {
                        _ = &mut up => down.abort(),
                        _ = &mut down => up.abort(),
                    }
                });
            }
        });
        proxy
    }

    async fn forward(
        p: Arc<Self>,
        mut from: tokio::net::tcp::OwnedReadHalf,
        mut to: tokio::net::tcp::OwnedWriteHalf,
        cut: u64,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut buf = vec![0u8; 16 * 1024];
        let mut sent = 0u64;
        let mut paused = p.paused.subscribe();
        loop {
            let Ok(n) = from.read(&mut buf).await else {
                return;
            };
            if n == 0 {
                return;
            }
            let _ = paused.wait_for(|p| !*p).await;
            let delay = p.delay_ms.load(Ordering::SeqCst);
            if delay > 0 {
                tokio::time::sleep(Duration::from_millis(delay)).await;
            }
            if cut > 0 && sent + n as u64 > cut {
                p.cuts.fetch_add(1, Ordering::SeqCst);
                return;
            }
            if to.write_all(&buf[..n]).await.is_err() {
                return;
            }
            sent += n as u64;
        }
    }
}

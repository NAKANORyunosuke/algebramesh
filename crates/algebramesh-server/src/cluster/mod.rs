//! A Raft cluster of schedulers (ADR 0013).
//!
//! Every scheduler applies the committed Raft log to its own metadata store (the
//! state machine). The leader also keeps a working copy of that store for its
//! [`Scheduler`]: it is made when the node becomes leader, after the entries of
//! the earlier terms are applied, and the scheduler's changes to it are proposed
//! to the Raft log in the order in which they were written. A client or agent call
//! is answered only after the entries that carry the changes it made are
//! committed by a majority and applied (Issue #5): the proposer of the leader's
//! term reports which changes its own entries carried, and the call waits for
//! that, not for a sequence number, which another leader may have used for other
//! changes. When the node stops being the leader, the calls still waiting are
//! answered with `ABORTED` (the outcome is unknown), and the scheduler and its
//! working copy are dropped; changes that were not committed go with them. An
//! entry proposed for a working copy of an earlier term, or out of order, is
//! applied as no change on every node.
//!
//! A node that is not the leader, or a leader that has not heard from a majority
//! for a while, answers client and agent calls with `UNAVAILABLE`, so that they
//! try the next scheduler URL.
//!
//! The members (number, name, URL) are part of the Raft membership. The first
//! start initializes it from the configuration; afterwards the leader adds a
//! scheduler, as a learner until it has caught up and then as a voter
//! ([`Node::add_member`]), and removes one ([`Node::remove_member`]).

pub mod blob_sync;
mod log_store;
mod network;
mod snapshot;
mod state_machine;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::{Duration, Instant};

use algebramesh_rpc::pb;
use algebramesh_rpc::pb::replication_server::Replication;
use openraft::error::{ClientWriteError, InitializeError, RaftError};
use openraft::raft::{AppendEntriesRequest, VoteRequest};
use openraft::storage::{Snapshot, SnapshotMeta};
use openraft::{ChangeMembers, Config, Raft, RaftMetrics, ServerState, SnapshotPolicy, Vote};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch};
use tonic::{Request, Response, Status};

use crate::ServeError;
use crate::scheduler::{Scheduler, SchedulerOptions};
use crate::store::Store;

pub use log_store::LogStore;
pub use network::REPLICATION_MAX_MESSAGE_BYTES;
pub use state_machine::StateMachine;

openraft::declare_raft_types!(
    /// Raft types of the scheduler cluster.
    pub TypeConfig:
        D = ChangeBatch,
        R = BatchReply,
        NodeId = u64,
        Node = Member,
        Entry = openraft::Entry<TypeConfig>,
        SnapshotData = Vec<u8>,
        AsyncRuntime = openraft::TokioRuntime,
);

/// One Raft log entry: changes of the leader's working store, in order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeBatch {
    /// Sequence number and JSON of each [`Change`](crate::store::Change).
    pub changes: Vec<(i64, String)>,
    /// The term of the working store the changes were written to. The state
    /// machine applies the batch only in an entry of that term; `None` in entries
    /// written before the field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub term: Option<u64>,
}

/// What the state machine did with an entry.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchReply {
    /// Whether the changes of the entry were applied. An entry of another term
    /// than its batch, or whose changes do not follow the last applied one, is
    /// applied as no change, the same way on every node.
    pub applied: bool,
}

/// A scheduler of the cluster, as recorded in the Raft membership.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    pub name: String,
    pub url: String,
    /// Name expected in the member's server certificate.
    pub domain: String,
}

#[derive(Clone)]
pub struct ClusterConfig {
    /// This scheduler's number in `members`.
    pub node_id: u64,
    /// The members the first start initializes the cluster with. Later changes
    /// are made with [`Node::add_member`] and [`Node::remove_member`].
    pub members: BTreeMap<u64, Member>,
    /// Joins an existing cluster instead of initializing one: the scheduler
    /// waits until the leader adds it.
    pub join: bool,
    /// CA that signed the other schedulers' certificates.
    pub ca_pem: Vec<u8>,
    /// Token this scheduler presents to the others.
    pub peer_token: String,
    /// Elections start after this long, up to twice as long, without a leader.
    pub election_timeout: Duration,
    /// Directory of `raft.sqlite3` and the working store; `None` keeps them in
    /// memory, for tests.
    pub data_dir: Option<PathBuf>,
    /// Snapshots are sent to other schedulers in chunks of this size
    /// ([`SNAPSHOT_CHUNK_BYTES`] by default).
    pub snapshot_chunk_bytes: usize,
    /// The features this scheduler can use (ADR 0020):
    /// [`supported_features`], or fewer to stand for an older scheduler in
    /// tests.
    pub features: Vec<String>,
}

/// The version of this scheduler binary.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The features of this build, as [`ClusterConfig::features`] takes them.
pub fn supported_features() -> Vec<String> {
    algebramesh_core::features::SUPPORTED
        .iter()
        .map(|f| (*f).to_owned())
        .collect()
}

/// How often the leader looks for features every member can use.
const FEATURE_CHECK: Duration = Duration::from_secs(2);

/// How long the leader waits for a member to say what it can use.
const FEATURE_QUERY: Duration = Duration::from_secs(5);

/// How long `add_member` asks a scheduler what it can use before it gives up;
/// one started just before may not listen yet.
const JOIN_QUERY: Duration = Duration::from_secs(30);

/// The default size of a snapshot chunk.
pub const SNAPSHOT_CHUNK_BYTES: usize = 16 * 1024 * 1024;

/// Changes proposed in one Raft entry at most.
const MAX_BATCH: usize = 500;
/// Bytes of changes proposed in one Raft entry at most, unless a single change
/// is larger (Issue #20).
const MAX_BATCH_BYTES: usize = 4 * 1024 * 1024;
/// How long a call waits for its changes to be committed.
const COMMIT_WAIT: Duration = Duration::from_secs(10);
/// How long a leader whose scheduler could not start waits before it tries
/// again (Issue #37).
const LEAD_RETRY: Duration = Duration::from_secs(1);
/// A new snapshot after this many log entries; the log before it is dropped.
const SNAPSHOT_EVERY: u64 = 10_000;
/// How long a new member may take to catch up with the log.
const CATCH_UP: Duration = Duration::from_secs(60);

enum Role {
    Leader {
        term: u64,
        scheduler: Arc<Scheduler>,
        /// The last change of the working store that an entry of this term has
        /// carried into the state machine. Closed when the proposer stops.
        committed: watch::Receiver<i64>,
        /// Set when the node stops serving as leader in this term.
        deposed: Arc<watch::Sender<bool>>,
    },
    /// Not the leader. `abandoned` is a term in which this node dropped its
    /// working store while still leader; it does not lead again in that term.
    Other { abandoned: Option<u64> },
}

struct Cluster {
    /// This node, for the tasks it starts.
    me: Weak<Node>,
    config: ClusterConfig,
    raft: Raft<TypeConfig>,
    state: Arc<Mutex<Store>>,
    applied_seq: watch::Receiver<i64>,
    role: RwLock<Role>,
    /// Last time a majority confirmed this node as leader.
    confirmed: Mutex<Option<Instant>>,
    /// Whether this node may start elections: only while it is a voter.
    electing: AtomicBool,
    /// Whether this node, still leader in a term it cannot serve, has stopped
    /// its heartbeats and elections so that another node leads (Issue #37).
    aside: AtomicBool,
    /// When a leader whose scheduler could not start (a full disk) tries again.
    lead_retry: Mutex<Option<Instant>>,
    /// A node that stepped aside starts no election before this.
    no_elections_until: Mutex<Option<Instant>>,
    /// Orders enabling features and changing the members (ADR 0020), so that
    /// no member without a feature joins while it is being enabled.
    membership: tokio::sync::Mutex<()>,
    /// Set by [`Node::shutdown`]: Raft stops because it was asked to.
    stopping: AtomicBool,
    /// While true, the proposer waits before proposing (tests only).
    hold: watch::Sender<bool>,
    /// Snapshots received in chunks (Issue #20).
    inbox: snapshot::SnapshotInbox,
}

/// Leaves the leader role, if this is it, waking the calls that wait for their
/// changes. The node does not lead again in the term it leaves.
fn depose(role: &mut Role, next: Option<u64>) {
    if let Role::Leader { term, deposed, .. } = role {
        deposed.send_replace(true);
        *role = Role::Other {
            abandoned: next.or(Some(*term)),
        };
    }
}

enum Kind {
    Single(Arc<Scheduler>),
    Cluster(Box<Cluster>),
}

pub struct Node {
    name: String,
    options: SchedulerOptions,
    kind: Kind,
    /// The blobs of this node (ADR 0021).
    blobs: blob_sync::Blobs,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn read<T>(m: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    m.read().unwrap_or_else(|e| e.into_inner())
}

fn write<T>(m: &RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    m.write().unwrap_or_else(|e| e.into_inner())
}

impl Node {
    /// A single scheduler without a cluster.
    pub fn single(scheduler: Arc<Scheduler>) -> Arc<Self> {
        // Alone, no older scheduler can follow the changes: every feature of
        // this build is enabled (ADR 0020).
        let enabled = scheduler.with_store(|s| {
            let missing: Vec<String> = supported_features()
                .into_iter()
                .filter(|f| !s.enabled_features().unwrap_or_default().contains(f))
                .collect();
            if missing.is_empty() {
                Ok(())
            } else {
                s.enable_features(&missing, VERSION)
            }
        });
        if let Err(e) = enabled {
            tracing::error!("cannot enable the features of this build: {e}");
        }
        let options = scheduler.options.clone();
        let dir = blob_sync::blob_dir(&options.blobs, None);
        let blobs = blob_sync::Blobs::new(dir, options.blobs.clone());
        let every = options.blobs.collect_every;
        let node = Arc::new(Self {
            name: String::new(),
            options,
            kind: Kind::Single(scheduler),
            blobs,
        });
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(blob_sync::collect_garbage(Arc::downgrade(&node), every));
            handle.spawn(blob_sync::repair(Arc::downgrade(&node)));
        }
        node
    }

    /// Starts Raft over `store` (the state machine) and joins the cluster. The
    /// first start initializes the cluster with the configured members.
    pub async fn start_cluster(
        store: Store,
        options: SchedulerOptions,
        config: ClusterConfig,
    ) -> Result<Arc<Self>, ServeError> {
        let me = config
            .members
            .get(&config.node_id)
            .ok_or_else(|| {
                ServeError::Config(format!(
                    "raft.node_id {} is not among raft.members",
                    config.node_id
                ))
            })?
            .clone();
        if let Some(dir) = &config.data_dir {
            crate::store::remove_stale_files(dir);
        }
        check_version(&store)?;
        let state = Arc::new(Mutex::new(store));
        let state_machine = StateMachine::new(state.clone())?;
        let log = LogStore::open(
            config
                .data_dir
                .as_ref()
                .map(|d| d.join("raft.sqlite3"))
                .as_deref(),
        )
        .map_err(|e| ServeError::Config(format!("raft log: {e}")))?
        .with_applied(state_machine.applied_log());
        let applied_seq = state_machine.applied_seq();
        let election = config.election_timeout.as_millis().max(100) as u64;
        let raft_config = Config {
            cluster_name: "algebramesh".into(),
            election_timeout_min: election,
            election_timeout_max: election * 2,
            // An AppendEntries must be sent within one heartbeat interval
            // (openraft): a third of the election timeout leaves room for
            // entries of several MiB (Issue #20).
            heartbeat_interval: (election / 3).max(20),
            install_snapshot_timeout: 60_000,
            snapshot_policy: SnapshotPolicy::LogsSinceLast(SNAPSHOT_EVERY),
            max_in_snapshot_log_to_keep: 1_000,
            ..Default::default()
        }
        .validate()
        .map_err(|e| ServeError::Config(format!("raft: {e}")))?;
        let raft = Raft::new(
            config.node_id,
            Arc::new(raft_config),
            network::Network::new(
                config.ca_pem.clone(),
                config.peer_token.clone(),
                config.snapshot_chunk_bytes.max(1),
            ),
            log,
            state_machine,
        )
        .await
        .map_err(|e| ServeError::Config(format!("raft: {e}")))?;
        if config.join {
            tracing::info!(node = %me.name, "waiting to be added to the cluster");
        } else {
            match raft.initialize(config.members.clone()).await {
                Ok(()) => tracing::info!(node = %me.name, "initialized the cluster"),
                Err(RaftError::APIError(InitializeError::NotAllowed(_))) => {}
                Err(e) => return Err(ServeError::Config(format!("raft: {e}"))),
            }
        }
        let inbox = snapshot::SnapshotInbox::new(
            config
                .data_dir
                .clone()
                .unwrap_or_else(|| {
                    std::env::temp_dir()
                        .join(format!("algebramesh-snapshots-{}", uuid::Uuid::new_v4()))
                })
                .join("snapshots"),
        );
        let blobs = blob_sync::Blobs::new(
            blob_sync::blob_dir(&options.blobs, config.data_dir.as_deref()),
            options.blobs.clone(),
        );
        let collect_every = options.blobs.collect_every;
        let node = Arc::new_cyclic(|weak| Self {
            name: me.name,
            options,
            blobs,
            kind: Kind::Cluster(Box::new(Cluster {
                me: weak.clone(),
                config,
                raft: raft.clone(),
                state,
                applied_seq,
                role: RwLock::new(Role::Other { abandoned: None }),
                confirmed: Mutex::new(None),
                electing: AtomicBool::new(true),
                aside: AtomicBool::new(false),
                lead_retry: Mutex::new(None),
                no_elections_until: Mutex::new(None),
                membership: tokio::sync::Mutex::new(()),
                stopping: AtomicBool::new(false),
                hold: watch::channel(false).0,
                inbox,
            })),
        });
        tokio::spawn(follow_metrics(Arc::downgrade(&node), raft.clone()));
        tokio::spawn(agree_on_features(Arc::downgrade(&node)));
        tokio::spawn(blob_sync::collect_garbage(
            Arc::downgrade(&node),
            collect_every,
        ));
        tokio::spawn(blob_sync::repair(Arc::downgrade(&node)));
        if let Kind::Cluster(c) = &node.kind {
            tokio::spawn(blob_sync::follow_references(
                Arc::downgrade(&node),
                c.applied_seq.clone(),
            ));
        }
        tokio::spawn(confirm_leadership(
            Arc::downgrade(&node),
            raft,
            Duration::from_millis(election),
        ));
        Ok(node)
    }

    fn cluster(&self) -> Option<&Cluster> {
        match &self.kind {
            Kind::Cluster(c) => Some(c),
            Kind::Single(_) => None,
        }
    }

    /// The member `id` as the Raft membership records it.
    fn member(&self, id: u64) -> Option<Member> {
        let cluster = self.cluster()?;
        let metrics = cluster.raft.metrics();
        let m = metrics.borrow();
        m.membership_config.membership().get_node(&id).cloned()
    }

    fn member_name(&self, id: u64) -> String {
        self.member(id).map_or_else(|| id.to_string(), |m| m.name)
    }

    /// The scheduler, if this node serves client and agent calls.
    pub fn scheduler(&self) -> Result<Arc<Scheduler>, Status> {
        let cluster = match &self.kind {
            Kind::Single(s) => return Ok(s.clone()),
            Kind::Cluster(c) => c,
        };
        if let Role::Leader { scheduler, .. } = &*read(&cluster.role) {
            if cluster.confirmed_recently() {
                return Ok(scheduler.clone());
            }
            return Err(Status::unavailable(format!(
                "scheduler {} has not heard from a majority of the cluster",
                self.name
            )));
        }
        let leader = self.leader_hint();
        Err(Status::unavailable(format!(
            "scheduler {} is not the leader{leader}",
            self.name
        )))
    }

    /// "; the leader is <name> (<url>)", if the leader is known.
    fn leader_hint(&self) -> String {
        let leader = self
            .cluster()
            .and_then(|c| c.raft.metrics().borrow().current_leader);
        leader
            .and_then(|id| self.member(id))
            .map(|m| format!("; the leader is {} ({})", m.name, m.url))
            .unwrap_or_default()
    }

    /// Waits until the changes `scheduler` has written so far are committed by a
    /// majority and applied, so that they survive a change of leader.
    ///
    /// The wait is for the entries this leader proposed in its term (Issue #5):
    /// a sequence number alone does not identify a change, since a later leader
    /// may commit other changes under the same numbers.
    ///
    /// * If the node stops being the leader of that term first, or the proposal
    ///   fails, the call is answered with `ABORTED`: its changes may or may not be
    ///   committed. The client does not repeat it by itself; it may send it again
    ///   (with the same `request_id` for a submission, which then returns the job
    ///   if it was committed).
    /// * If nothing is committed within [`COMMIT_WAIT`] (a leader cut off from the
    ///   majority), the call is answered with `UNAVAILABLE`, later than the
    ///   client's own retry window.
    pub async fn committed(&self, scheduler: &Scheduler) -> Result<(), Status> {
        let Some(cluster) = self.cluster() else {
            return Ok(());
        };
        let aborted = || {
            Status::aborted(format!(
                "scheduler {} stopped leading before the change was committed; it may or may \
                 not have been committed: send the call again, with the same request_id for a \
                 submission",
                self.name
            ))
        };
        let (mut committed, mut deposed) = match &*read(&cluster.role) {
            Role::Leader {
                scheduler: s,
                committed,
                deposed,
                ..
            } if std::ptr::eq(Arc::as_ptr(s), scheduler) => {
                (committed.clone(), deposed.subscribe())
            }
            _ => return Err(aborted()),
        };
        let target = scheduler
            .with_store(|s| s.last_seq())
            .map_err(|e| Status::internal(format!("metadata store error: {e}")))?;
        let wait = async {
            // Biased: changes committed before the node stopped leading count.
            tokio::select! {
                biased;
                r = committed.wait_for(|seq| *seq >= target) => r.is_ok(),
                _ = deposed.wait_for(|d| *d) => false,
            }
        };
        match tokio::time::timeout(COMMIT_WAIT, wait).await {
            Ok(true) => Ok(()),
            Ok(false) => Err(aborted()),
            Err(_) => Err(Status::unavailable(format!(
                "scheduler {}: the change was not committed by a majority of the cluster",
                self.name
            ))),
        }
    }

    /// Runs `read` on the committed state, for calls that only read (Issue #13).
    ///
    /// The leader's scheduler works on a copy that also holds changes not yet
    /// committed, which a change of leader may drop; a client must not see them
    /// as done. In a cluster, `read` gets the state machine: what a majority has
    /// committed, as applied on this node. Before reading, the leader confirms
    /// with a majority that it still leads and waits until its state machine has
    /// applied everything committed so far (`ensure_linearizable`), so the
    /// answer is not older than any acknowledged change, its own included.
    pub async fn read_committed<R>(&self, read: impl FnOnce(&Store) -> R) -> Result<R, Status> {
        let cluster = match &self.kind {
            Kind::Single(s) => return Ok(s.with_store(read)),
            Kind::Cluster(c) => c,
        };
        // Only the leader that serves answers, as for every client call.
        self.scheduler()?;
        let barrier = cluster.config.election_timeout * 2;
        match tokio::time::timeout(barrier, cluster.raft.ensure_linearizable()).await {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                return Err(Status::unavailable(format!(
                    "scheduler {} cannot confirm its leadership: {e}{}",
                    self.name,
                    self.leader_hint()
                )));
            }
            Err(_) => {
                return Err(Status::unavailable(format!(
                    "scheduler {} did not hear from a majority within {barrier:?}",
                    self.name
                )));
            }
        }
        Ok(read(&lock(&cluster.state)))
    }

    /// The inbox of chunked snapshots, shared with blocking tasks.
    fn inbox(&self) -> Result<Arc<snapshot::SnapshotInbox>, Status> {
        let cluster = self
            .cluster()
            .ok_or_else(|| Status::failed_precondition("not in a cluster"))?;
        Ok(Arc::new(snapshot::SnapshotInbox::new(
            cluster.inbox.dir().to_path_buf(),
        )))
    }

    /// Builds a snapshot now and drops the log up to it, so that a member that
    /// is behind is sent the snapshot. For tests; Raft does this by itself every
    /// [`SNAPSHOT_EVERY`] entries.
    #[doc(hidden)]
    pub async fn compact_log(&self) -> Result<(), String> {
        let Some(cluster) = self.cluster() else {
            return Ok(());
        };
        let applied = cluster
            .raft
            .metrics()
            .borrow()
            .last_applied
            .ok_or("nothing is applied")?;
        cluster
            .raft
            .trigger()
            .snapshot()
            .await
            .map_err(|e| e.to_string())?;
        // At or after the position: entries applied meanwhile are in it too.
        cluster
            .raft
            .wait(Some(Duration::from_secs(30)))
            .metrics(
                |m| m.snapshot.is_some_and(|s| s.index >= applied.index),
                "snapshot",
            )
            .await
            .map_err(|e| e.to_string())?;
        cluster
            .raft
            .trigger()
            .purge_log(applied.index)
            .await
            .map_err(|e| e.to_string())?;
        cluster
            .raft
            .wait(Some(Duration::from_secs(30)))
            .metrics(
                |m| m.purged.is_some_and(|p| p.index >= applied.index),
                "purged",
            )
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Makes the proposer of this node wait before proposing (`true`) or go on
    /// (`false`). For tests that need changes written but not yet proposed.
    #[doc(hidden)]
    pub fn hold_proposals(&self, hold: bool) {
        if let Some(c) = self.cluster() {
            c.hold.send_replace(hold);
        }
    }

    pub fn status(&self) -> pb::NodeStatus {
        let cluster = match &self.kind {
            Kind::Single(s) => {
                let blobs = self.blob_status();
                return pb::NodeStatus {
                    node_id: self.name.clone(),
                    role: "single".into(),
                    epoch: s.epoch(),
                    last_seq: s.with_store(|st| st.last_seq()).unwrap_or(0),
                    leader: String::new(),
                    serving: true,
                    version: VERSION.into(),
                    features: supported_features(),
                    enabled_features: s.with_store(|st| st.enabled_features()).unwrap_or_default(),
                    blobs_under_replicated: 0,
                    blobs_missing: blobs.missing,
                    blobs_quarantined: blobs.quarantined,
                    blob_free_bytes: blobs.free_bytes,
                    blobs_stored: blobs.stored,
                    blob_bytes_stored: blobs.bytes_stored,
                    blob_uploads_refused: blobs.uploads_refused,
                };
            }
            Kind::Cluster(c) => c,
        };
        let metrics = cluster.raft.metrics().borrow().clone();
        let role = match metrics.state {
            ServerState::Leader => "leader",
            ServerState::Follower => "follower",
            ServerState::Candidate => "candidate",
            ServerState::Learner => "learner",
            ServerState::Shutdown => "shutdown",
        };
        // One lock at a time: the temporaries of a struct expression live to
        // its end, and the state machine takes `state` and then sends on
        // `applied_seq`, so holding the borrow of `applied_seq` while taking
        // `state` would deadlock with it.
        let last_seq = *cluster.applied_seq.borrow();
        let enabled_features = lock(&cluster.state).enabled_features().unwrap_or_default();
        let serving = self.scheduler().is_ok();
        let blobs = self.blob_status();
        pb::NodeStatus {
            node_id: self.name.clone(),
            role: role.into(),
            epoch: metrics.current_term,
            last_seq,
            leader: metrics
                .current_leader
                .map(|id| self.member_name(id))
                .unwrap_or_default(),
            serving,
            version: VERSION.into(),
            features: cluster.config.features.clone(),
            enabled_features,
            blobs_under_replicated: if serving { blobs.under_replicated } else { 0 },
            blobs_missing: blobs.missing,
            blobs_quarantined: blobs.quarantined,
            blob_free_bytes: blobs.free_bytes,
            blobs_stored: blobs.stored,
            blob_bytes_stored: blobs.bytes_stored,
            blob_uploads_refused: blobs.uploads_refused,
        }
    }

    /// The features the cluster has enabled, as this node has applied them.
    pub fn enabled_features(&self) -> Vec<String> {
        match &self.kind {
            Kind::Single(s) => s.with_store(|st| st.enabled_features()).unwrap_or_default(),
            Kind::Cluster(c) => lock(&c.state).enabled_features().unwrap_or_default(),
        }
    }

    /// On the leader: enables the features every member can use and the
    /// cluster has not enabled yet (ADR 0020). Asks every member (voters and
    /// learners) what it can use; one that does not answer, or is older than
    /// the agreement, holds every feature back.
    async fn agree_on_features(&self) -> Result<Vec<String>, String> {
        let Some(cluster) = self.cluster() else {
            return Ok(Vec::new());
        };
        let scheduler = self.scheduler().map_err(|s| s.message().to_owned())?;
        let enabled = lock(&cluster.state)
            .enabled_features()
            .map_err(|e| e.to_string())?;
        let mut agreed: Vec<String> = cluster
            .config
            .features
            .iter()
            .filter(|f| !enabled.contains(f))
            .cloned()
            .collect();
        if agreed.is_empty() {
            return Ok(Vec::new());
        }
        let _members = cluster.membership.lock().await;
        let others: Vec<(u64, Member)> = {
            let metrics = cluster.raft.metrics();
            let m = metrics.borrow();
            m.membership_config
                .membership()
                .nodes()
                .filter(|(id, _)| **id != cluster.config.node_id)
                .map(|(id, n)| (*id, n.clone()))
                .collect()
        };
        let mut min_version = VERSION.to_owned();
        for (id, member) in others {
            let status = self.ask_status(&member).await.map_err(|e| {
                format!(
                    "member {id} ({}) did not say what it can use: {e}",
                    member.name
                )
            })?;
            agreed.retain(|f| status.features.contains(f));
            if crate::store::version_older(&status.version, &min_version) {
                min_version = status.version.clone();
            }
            if agreed.is_empty() {
                return Ok(Vec::new());
            }
        }
        scheduler
            .with_store(|s| s.enable_features(&agreed, &min_version))
            .map_err(|e| e.to_string())?;
        self.committed(&scheduler)
            .await
            .map_err(|s| s.message().to_owned())?;
        tracing::info!(node = %self.name, features = ?agreed, min_version, "the cluster agreed on features");
        Ok(agreed)
    }

    /// The status of another member, asked over the replication service.
    async fn ask_status(&self, member: &Member) -> Result<pb::NodeStatus, String> {
        let cluster = self.cluster().ok_or("not in a cluster")?;
        let mut client =
            network::peer_client(member, &cluster.config.ca_pem, &cluster.config.peer_token)?;
        match tokio::time::timeout(FEATURE_QUERY, client.status(pb::Empty {})).await {
            Ok(Ok(r)) => Ok(r.into_inner()),
            Ok(Err(s)) => Err(s.message().to_owned()),
            Err(_) => Err(format!("no answer in {} s", FEATURE_QUERY.as_secs())),
        }
    }

    /// [`Self::ask_status`], asked again until `wait` has passed: a scheduler
    /// started just before it is added may not listen yet.
    async fn ask_status_within(
        &self,
        member: &Member,
        wait: Duration,
    ) -> Result<pb::NodeStatus, String> {
        let deadline = Instant::now() + wait;
        loop {
            match self.ask_status(member).await {
                Ok(status) => return Ok(status),
                Err(e) if Instant::now() >= deadline => return Err(e),
                Err(_) => tokio::time::sleep(Duration::from_millis(500)).await,
            }
        }
    }

    /// Starts an election on this node now.
    pub async fn promote(&self) -> Result<(), String> {
        match self.cluster() {
            Some(c) => c.raft.trigger().elect().await.map_err(|e| e.to_string()),
            None => Ok(()),
        }
    }

    /// Stops Raft; the node answers nothing afterwards.
    /// Resolves, with the error, once Raft has stopped after an error of its
    /// storage; never for a single scheduler.
    pub async fn raft_failure(&self) -> String {
        let Some(cluster) = self.cluster() else {
            return std::future::pending().await;
        };
        let mut metrics = cluster.raft.metrics();
        loop {
            if let Err(e) = &metrics.borrow_and_update().running_state {
                return e.to_string();
            }
            if metrics.changed().await.is_err() {
                // Raft is gone without a recorded error: a panic of its core,
                // unless it was shut down.
                if cluster.stopping.load(Ordering::SeqCst) {
                    return std::future::pending().await;
                }
                return "the Raft core stopped".into();
            }
        }
    }

    pub async fn shutdown(&self) {
        self.stop_blobs();
        if let Some(c) = self.cluster() {
            c.stopping.store(true, Ordering::SeqCst);
            depose(&mut write(&c.role), None);
            let _ = c.raft.shutdown().await;
        }
    }

    /// Gives up leading the current term: the node stops serving and stops its
    /// heartbeats, so that another node is elected, and then starts no
    /// election for a while (as after a scheduler that could not start).
    #[doc(hidden)]
    pub fn hand_over(&self) {
        let Some(cluster) = self.cluster() else {
            return;
        };
        let term = cluster.raft.metrics().borrow().current_term;
        self.step_down(term);
        let m = cluster.raft.metrics().borrow().clone();
        self.on_metrics(&m);
    }

    /// The working store of the leader's scheduler failed (Issue #37): the
    /// node gives up the term, so that another node, whose store works,
    /// leads.
    pub fn store_failed(&self, scheduler: &Scheduler) {
        let Some(cluster) = self.cluster() else {
            return;
        };
        let term = match &*read(&cluster.role) {
            Role::Leader {
                term, scheduler: s, ..
            } if std::ptr::eq(Arc::as_ptr(s), scheduler) => *term,
            _ => return,
        };
        tracing::error!(node = %self.name, term, "the working store failed; giving up the term");
        self.step_down(term);
    }

    /// Starts no election for `time`.
    #[doc(hidden)]
    pub fn hold_elections(&self, time: Duration) {
        if let Some(cluster) = self.cluster() {
            *lock(&cluster.no_elections_until) = Some(Instant::now() + time);
            cluster.electing.store(false, Ordering::SeqCst);
            cluster.raft.runtime_config().elect(false);
        }
    }

    /// Runs `read` on this node's state machine as it is, without asking the
    /// leader: what this node has applied. For tests that compare nodes.
    #[doc(hidden)]
    pub fn with_applied_state<R>(&self, read: impl FnOnce(&Store) -> R) -> Option<R> {
        self.cluster().map(|c| read(&lock(&c.state)))
    }

    /// The members as this node knows them.
    pub fn members(&self) -> pb::ClusterMembers {
        let Some(cluster) = self.cluster() else {
            return pb::ClusterMembers::default();
        };
        let m = cluster.raft.metrics().borrow().clone();
        let membership = m.membership_config.membership();
        let voters: BTreeSet<u64> = membership.voter_ids().collect();
        pb::ClusterMembers {
            members: membership
                .nodes()
                .map(|(id, n)| pb::ClusterMember {
                    id: *id,
                    name: n.name.clone(),
                    url: n.url.clone(),
                    voter: voters.contains(id),
                })
                .collect(),
            leader: m
                .current_leader
                .and_then(|id| membership.get_node(&id))
                .map(|n| n.name.clone())
                .unwrap_or_default(),
            epoch: m.current_term,
        }
    }

    /// On the leader: adds `member` as a learner, waits until it has caught up
    /// with the log, and makes it a voter. While it lacks blobs the records
    /// refer to, it stays a learner, the leader sends them, and the call
    /// returns; the same call made again once it holds them makes it a voter
    /// (ADR 0021 Q24). Refused unless `member` can use
    /// every feature the cluster has enabled or this leader is enabling
    /// (ADR 0020), and while this node, leader in Raft, has not started its
    /// scheduler for the term.
    pub async fn add_member(&self, id: u64, member: Member) -> Result<pb::ClusterMembers, Status> {
        let cluster = self.cluster_for_changes()?;
        let _members = cluster.membership.lock().await;
        // A member must use every feature the cluster has enabled (ADR 0020):
        // an older one would stop on the changes, or drop fields of records.
        // Features this leader is still enabling count too (Issue #85): an
        // agreement whose wait for the commit ran out leaves its change in
        // the working store and on the way to the log, where it may still be
        // committed. The working store is a copy of the state machine made
        // once every entry of earlier terms was applied, so it also holds
        // features enabled by an earlier leader. Before that copy exists, the
        // state machine may lack them: the call is refused.
        let scheduler = match &*read(&cluster.role) {
            Role::Leader { scheduler, .. } => scheduler.clone(),
            Role::Other { .. } => {
                return Err(Status::unavailable(format!(
                    "scheduler {} does not serve as leader yet; try again",
                    self.name
                )));
            }
        };
        let mut enabled = scheduler
            .with_store(|s| s.enabled_features())
            .map_err(|e| Status::internal(format!("metadata store error: {e}")))?;
        for f in self.enabled_features() {
            if !enabled.contains(&f) {
                enabled.push(f);
            }
        }
        if !enabled.is_empty() {
            let status = self
                .ask_status_within(&member, JOIN_QUERY)
                .await
                .map_err(|e| {
                    Status::failed_precondition(format!(
                        "cannot ask {} which features it can use ({e}); start it with join = \
                         true first",
                        member.name
                    ))
                })?;
            let missing: Vec<&String> = enabled
                .iter()
                .filter(|f| !status.features.contains(f))
                .collect();
            if !missing.is_empty() {
                let version = if status.version.is_empty() {
                    "from before the agreement on features"
                } else {
                    status.version.as_str()
                };
                return Err(Status::failed_precondition(format!(
                    "{} (version {version}) cannot use the features {missing:?} the cluster has \
                     enabled or is enabling; update it first",
                    member.name
                )));
            }
        }
        {
            let metrics = cluster.raft.metrics();
            let m = metrics.borrow();
            let membership = m.membership_config.membership();
            if membership.voter_ids().any(|v| v == id) {
                return Err(Status::already_exists(format!(
                    "member {id} is already a voter"
                )));
            }
            if let Some((other, _)) = membership
                .nodes()
                .find(|(i, n)| **i != id && n.name == member.name)
            {
                return Err(Status::already_exists(format!(
                    "member {other} is already named {}",
                    member.name
                )));
            }
        }
        let name = member.name.clone();
        tracing::info!(node = %self.name, id, member = %name, "adding a member as a learner");
        match tokio::time::timeout(CATCH_UP, cluster.raft.add_learner(id, member, true)).await {
            Err(_) => {
                return Err(Status::deadline_exceeded(format!(
                    "{name} has not caught up with the log in {} s and stays a learner; check \
                     that it runs with `join = true` and that the leader reaches it, then add it \
                     again, or remove it",
                    CATCH_UP.as_secs()
                )));
            }
            Ok(r) => r.map_err(|e| self.write_error(e))?,
        };
        // Blobs take far longer than the log: the call does not wait for them.
        let lacking = self.lacking_on(id).await?;
        if !lacking.is_empty() {
            tracing::info!(
                node = %self.name, id, member = %name, lacking = lacking.len(),
                "the new member stays a learner until it holds the blobs records refer to"
            );
            if let Some(me) = cluster.me.upgrade() {
                tokio::spawn(async move { me.unless_stopped(me.push_all(&lacking)).await });
            }
            return Ok(self.members());
        }
        cluster
            .raft
            .change_membership(ChangeMembers::AddVoterIds(BTreeSet::from([id])), false)
            .await
            .map_err(|e| self.write_error(e))?;
        tracing::info!(node = %self.name, id, member = %name, "added a voter");
        Ok(self.members())
    }

    /// On the leader: removes member `id` (a voter or a learner).
    pub async fn remove_member(&self, id: u64) -> Result<pb::ClusterMembers, Status> {
        let cluster = self.cluster_for_changes()?;
        let _members = cluster.membership.lock().await;
        let (known, voters) = {
            let metrics = cluster.raft.metrics();
            let m = metrics.borrow();
            let membership = m.membership_config.membership();
            let voters: BTreeSet<u64> = membership.voter_ids().collect();
            (membership.get_node(&id).is_some(), voters)
        };
        if !known {
            return Err(Status::not_found(format!("no member {id}")));
        }
        let change = if voters.contains(&id) {
            if voters.len() == 1 {
                return Err(Status::failed_precondition(
                    "cannot remove the last voter of the cluster",
                ));
            }
            // The blobs records refer to must stay on a majority of the
            // voters that remain (ADR 0021 §9).
            let remaining: BTreeSet<u64> = voters.iter().copied().filter(|v| *v != id).collect();
            let short = self.short_of(&remaining).await?;
            if !short.is_empty() {
                let count = short.len();
                if let Some(me) = cluster.me.upgrade() {
                    tokio::spawn(async move { me.unless_stopped(me.push_all(&short)).await });
                }
                return Err(Status::failed_precondition(format!(
                    "without member {id}, {count} blobs that records refer to would be held by \
                     fewer than a majority of the voters; they are being copied, try again \
                     when `server status` shows none under-replicated"
                )));
            }
            ChangeMembers::RemoveVoters(BTreeSet::from([id]))
        } else {
            ChangeMembers::RemoveNodes(BTreeSet::from([id]))
        };
        tracing::info!(node = %self.name, id, "removing a member");
        cluster
            .raft
            .change_membership(change, false)
            .await
            .map_err(|e| self.write_error(e))?;
        Ok(self.members())
    }

    /// The cluster, if this node is its leader and may change the membership.
    fn cluster_for_changes(&self) -> Result<&Cluster, Status> {
        let cluster = self
            .cluster()
            .ok_or_else(|| Status::failed_precondition("this scheduler is not in a cluster"))?;
        let leader = cluster.raft.metrics().borrow().current_leader;
        if leader != Some(cluster.config.node_id) {
            return Err(Status::failed_precondition(format!(
                "scheduler {} is not the leader{}",
                self.name,
                self.leader_hint()
            )));
        }
        Ok(cluster)
    }

    fn write_error(&self, e: RaftError<u64, ClientWriteError<u64, Member>>) -> Status {
        match e {
            RaftError::APIError(ClientWriteError::ForwardToLeader(_)) => {
                Status::failed_precondition(format!(
                    "scheduler {} is not the leader{}",
                    self.name,
                    self.leader_hint()
                ))
            }
            RaftError::APIError(ClientWriteError::ChangeMembershipError(e)) => {
                Status::failed_precondition(e.to_string())
            }
            RaftError::Fatal(e) => Status::internal(e.to_string()),
        }
    }

    fn raft(&self) -> Result<&Raft<TypeConfig>, Status> {
        self.cluster()
            .map(|c| &c.raft)
            .ok_or_else(|| Status::failed_precondition("this scheduler is not in a cluster"))
    }

    /// Takes or gives up the leader's role after a change of the Raft metrics.
    fn on_metrics(&self, m: &RaftMetrics<u64, Member>) {
        let Some(cluster) = self.cluster() else {
            return;
        };
        // A scheduler that is not a voter (one removed from the cluster, or one
        // waiting to be added) starts no elections.
        let voter = m
            .membership_config
            .membership()
            .voter_ids()
            .any(|v| v == m.id);
        let leading = m.state == ServerState::Leader && m.current_leader == Some(m.id);
        let term = m.current_term;
        if !leading && cluster.aside.swap(false, Ordering::SeqCst) {
            // Not at once a candidate again: it would likely win and step
            // aside again; the new leader settles first.
            tracing::info!(node = %self.name, "another node leads; heartbeats again");
            cluster.raft.runtime_config().heartbeat(true);
            *lock(&cluster.no_elections_until) =
                Some(Instant::now() + cluster.config.election_timeout * 10);
        }
        let held_off = lock(&cluster.no_elections_until).is_some_and(|t| Instant::now() < t);
        let elect = voter && !held_off && !cluster.aside.load(Ordering::SeqCst);
        if cluster.electing.swap(elect, Ordering::SeqCst) != elect {
            cluster.raft.runtime_config().elect(elect);
            if !voter {
                tracing::warn!(node = %self.name, "not a voter of the cluster; no elections");
            }
        }
        let mut role = write(&cluster.role);
        if let Role::Leader { term: t, .. } = &*role
            && (!leading || *t != term)
        {
            tracing::warn!(node = %self.name, term = *t, "no longer the leader");
            depose(&mut role, None);
        }
        let Role::Other { abandoned } = &*role else {
            return;
        };
        // Lead once the entries of the earlier terms are applied: the state machine
        // has applied an entry of this term (the leader's first, blank entry).
        let ready = m.last_applied.as_ref().map(|l| l.leader_id.term) == Some(term);
        if leading && *abandoned == Some(term) {
            let alone = m.membership_config.membership().voter_ids().count() <= 1;
            if alone {
                // No other node can lead: a new term, in which this node leads
                // again from the state machine, once in a while.
                let due = lock(&cluster.lead_retry).is_none_or(|t| Instant::now() >= t);
                if due {
                    *lock(&cluster.lead_retry) = Some(Instant::now() + LEAD_RETRY);
                    let raft = cluster.raft.clone();
                    tokio::spawn(async move {
                        let _ = raft.trigger().elect().await;
                    });
                }
                return;
            }
            // A leader that gave up its term would otherwise hold it without
            // serving: without its heartbeats the others elect a new leader.
            if !cluster.aside.swap(true, Ordering::SeqCst) {
                tracing::warn!(node = %self.name, term, "cannot serve this term; letting another node lead");
                cluster.raft.runtime_config().heartbeat(false);
                cluster.raft.runtime_config().elect(false);
                cluster.electing.store(false, Ordering::SeqCst);
            }
            return;
        }
        let retry_due = lock(&cluster.lead_retry).is_none_or(|t| Instant::now() >= t);
        if leading && ready && retry_due {
            match self.lead(cluster, term) {
                Ok((scheduler, committed)) => {
                    tracing::info!(node = %self.name, term, "leader; serving clients and agents");
                    *lock(&cluster.lead_retry) = None;
                    *lock(&cluster.confirmed) = Some(Instant::now());
                    *role = Role::Leader {
                        term,
                        scheduler,
                        committed,
                        deposed: Arc::new(watch::channel(false).0),
                    };
                }
                Err(e) => {
                    // With other voters, one of them leads instead (Issue #37):
                    // this node gives up the term and steps aside. Alone, it
                    // tries again in the same term, which is safe since no
                    // proposer of the term has started; the cause (a full
                    // disk) may pass.
                    let alone = m.membership_config.membership().voter_ids().count() <= 1;
                    tracing::error!(node = %self.name, term, error = %e, alone, "cannot start the scheduler");
                    if alone {
                        *lock(&cluster.lead_retry) = Some(Instant::now() + LEAD_RETRY);
                    } else {
                        *role = Role::Other {
                            abandoned: Some(term),
                        };
                    }
                }
            }
        }
    }

    /// Copies the state machine into a working store and starts the scheduler on
    /// it, with its changes proposed to the Raft log. Returns the scheduler and
    /// the last change committed by the proposer of `term`.
    fn lead(
        &self,
        cluster: &Cluster,
        term: u64,
    ) -> Result<(Arc<Scheduler>, watch::Receiver<i64>), ServeError> {
        let copy = lock(&cluster.state).snapshot()?;
        let path = cluster.config.data_dir.as_ref().map(|dir| {
            remove_working_stores(dir);
            dir.join(format!("working-{term}.sqlite3"))
        });
        let mut working = Store::open(path.as_deref(), None)?;
        working.restore(&copy)?;
        // Made again from the state machine in every term: no need to survive
        // a power loss (Issue #23).
        working.relax_durability()?;
        working.set_epoch(term)?;
        let (sink, changes) = mpsc::unbounded_channel();
        working.set_change_sink(Some(sink));
        let (committed, committed_rx) = watch::channel(working.last_seq()?);
        let scheduler = Arc::new(Scheduler::new(working, self.options.clone())?);
        tokio::spawn(propose(
            Proposer {
                raft: cluster.raft.clone(),
                node: cluster.me.clone(),
                term,
                committed,
                hold: cluster.hold.subscribe(),
            },
            changes,
        ));
        Ok((scheduler, committed_rx))
    }

    /// Gives up serving as leader in `term` after a proposal failed.
    fn step_down(&self, term: u64) {
        let Some(cluster) = self.cluster() else {
            return;
        };
        let mut role = write(&cluster.role);
        if let Role::Leader { term: t, .. } = &*role
            && *t == term
        {
            tracing::warn!(node = %self.name, term, "stopping as leader of this term");
            depose(&mut role, Some(term));
        }
    }
}

impl Cluster {
    fn confirmed_recently(&self) -> bool {
        let lease = self.config.election_timeout * 4;
        lock(&self.confirmed).is_some_and(|t| t.elapsed() < lease)
    }
}

/// Refuses to start on a store whose cluster has enabled features with members
/// newer than this binary (ADR 0020): it could not follow their changes.
pub fn check_version(store: &Store) -> Result<(), ServeError> {
    if let Some(min) = store.min_binary_version()?
        && crate::store::version_older(VERSION, &min)
    {
        return Err(ServeError::Config(format!(
            "this scheduler is version {VERSION}, but its cluster has enabled features of \
             version {min} or later; start a newer version"
        )));
    }
    Ok(())
}

/// Removes working stores of earlier terms (best effort; one still open by a call
/// in flight stays until the next time).
fn remove_working_stores(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name.to_string_lossy().starts_with("working-") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// The proposer of one leader term.
struct Proposer {
    raft: Raft<TypeConfig>,
    node: Weak<Node>,
    term: u64,
    /// The last change whose entry is committed and applied.
    committed: watch::Sender<i64>,
    hold: watch::Receiver<bool>,
}

/// Proposes the working store's changes in the order in which they were written,
/// and reports each one whose entry is committed and applied. Stops at the first
/// entry that fails or is not applied (the node no longer leads `term`); the calls
/// waiting for later changes are then answered with `ABORTED`.
async fn propose(mut p: Proposer, mut changes: mpsc::UnboundedReceiver<(i64, String)>) {
    // A change that would make the batch too large starts the next one.
    let mut carry: Option<(i64, String)> = None;
    loop {
        let first = match carry.take() {
            Some(c) => c,
            None => match changes.recv().await {
                Some(c) => c,
                None => return,
            },
        };
        let mut bytes = first.1.len();
        let mut batch = vec![first];
        while batch.len() < MAX_BATCH {
            match changes.try_recv() {
                Ok(c) if bytes + c.1.len() > MAX_BATCH_BYTES => {
                    carry = Some(c);
                    break;
                }
                Ok(c) => {
                    bytes += c.1.len();
                    batch.push(c);
                }
                Err(_) => break,
            }
        }
        if p.hold.wait_for(|h| !*h).await.is_err() {
            return;
        }
        let last = batch.last().map_or(0, |(seq, _)| *seq);
        let entry = ChangeBatch {
            changes: batch,
            term: Some(p.term),
        };
        let failure = match p.raft.client_write(entry).await {
            Ok(r) if r.data.applied => {
                p.committed.send_replace(last);
                continue;
            }
            Ok(r) => format!("entry {} was not applied", r.log_id),
            Err(e) => e.to_string(),
        };
        tracing::warn!(term = p.term, error = %failure, "cannot propose changes");
        if let Some(node) = p.node.upgrade() {
            node.step_down(p.term);
        }
        return;
    }
}

/// On the leader, enables the features every member can use (ADR 0020), once
/// in a while; until then they are not used.
async fn agree_on_features(node: Weak<Node>) {
    loop {
        tokio::time::sleep(FEATURE_CHECK).await;
        let Some(n) = node.upgrade() else { return };
        if n.scheduler().is_err() {
            continue;
        }
        if let Err(e) = n.agree_on_features().await {
            tracing::debug!(node = %n.name, "features not agreed on yet: {e}");
        }
    }
}

async fn follow_metrics(node: Weak<Node>, raft: Raft<TypeConfig>) {
    let mut metrics = raft.metrics();
    let mut snapshot = None;
    loop {
        let m = metrics.borrow_and_update().clone();
        match node.upgrade() {
            Some(n) => {
                n.on_metrics(&m);
                // A snapshot installed (or built) may refer to blobs this node
                // lacks: the repair looks now (ADR 0021 §9).
                if m.snapshot != snapshot {
                    snapshot = m.snapshot;
                    n.blobs.wake_repair.notify_one();
                }
            }
            None => return,
        }
        // Also without a change, so that a leader whose scheduler could not
        // start tries again.
        tokio::select! {
            changed = metrics.changed() => if changed.is_err() { return },
            _ = tokio::time::sleep(LEAD_RETRY) => {}
        }
    }
}

/// Records, while this node is leader, that a majority still follows it.
async fn confirm_leadership(node: Weak<Node>, raft: Raft<TypeConfig>, election: Duration) {
    loop {
        tokio::time::sleep(election / 3).await;
        let Some(n) = node.upgrade() else { return };
        let Some(cluster) = n.cluster() else { return };
        if !matches!(&*read(&cluster.role), Role::Leader { .. }) {
            continue;
        }
        if let Ok(Ok(_)) = tokio::time::timeout(election * 2, raft.ensure_linearizable()).await {
            // Coming back after a lapse: restart the agents' heartbeat timeouts
            // before the scheduler serves (and sweeps) again.
            if !cluster.confirmed_recently()
                && let Role::Leader { scheduler, .. } = &*read(&cluster.role)
            {
                tracing::info!(node = %n.name, "a majority answers again; serving");
                scheduler.restart_heartbeat_clocks();
            }
            *lock(&cluster.confirmed) = Some(Instant::now());
        }
    }
}

fn json<T: Serialize>(value: &T) -> Result<Response<pb::RaftMessage>, Status> {
    serde_json::to_vec(value)
        .map(|json| Response::new(pb::RaftMessage { json }))
        .map_err(|e| Status::internal(e.to_string()))
}

fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, Status> {
    serde_json::from_slice(bytes).map_err(|e| Status::invalid_argument(e.to_string()))
}

/// The `Replication` service: Raft messages from the other schedulers, and the
/// operator's `server status` and `server promote`.
#[derive(Clone)]
pub struct ReplicationService(pub Arc<Node>);

#[tonic::async_trait]
impl Replication for ReplicationService {
    async fn status(&self, _: Request<pb::Empty>) -> Result<Response<pb::NodeStatus>, Status> {
        Ok(Response::new(self.0.status()))
    }

    async fn promote(&self, _: Request<pb::Empty>) -> Result<Response<pb::NodeStatus>, Status> {
        self.0.promote().await.map_err(Status::internal)?;
        Ok(Response::new(self.0.status()))
    }

    async fn members(&self, _: Request<pb::Empty>) -> Result<Response<pb::ClusterMembers>, Status> {
        Ok(Response::new(self.0.members()))
    }

    async fn add_member(
        &self,
        r: Request<pb::AddMemberRequest>,
    ) -> Result<Response<pb::ClusterMembers>, Status> {
        let r = r.into_inner();
        if !r.url.starts_with("https://") {
            return Err(Status::invalid_argument("the URL must start with https://"));
        }
        let domain = if r.server_name.is_empty() {
            crate::config::url_host(&r.url)
                .ok_or_else(|| Status::invalid_argument(format!("no host in {}", r.url)))?
        } else {
            r.server_name
        };
        let name = if r.name.is_empty() {
            format!("node-{}", r.id)
        } else {
            r.name
        };
        let member = Member {
            name,
            url: r.url,
            domain,
        };
        self.0.add_member(r.id, member).await.map(Response::new)
    }

    async fn remove_member(
        &self,
        r: Request<pb::RemoveMemberRequest>,
    ) -> Result<Response<pb::ClusterMembers>, Status> {
        self.0
            .remove_member(r.into_inner().id)
            .await
            .map(Response::new)
    }

    async fn append_entries(
        &self,
        r: Request<pb::RaftMessage>,
    ) -> Result<Response<pb::RaftMessage>, Status> {
        let request: AppendEntriesRequest<TypeConfig> = decode(&r.into_inner().json)?;
        json(&self.0.raft()?.append_entries(request).await)
    }

    async fn vote(&self, r: Request<pb::RaftMessage>) -> Result<Response<pb::RaftMessage>, Status> {
        let request: VoteRequest<u64> = decode(&r.into_inner().json)?;
        json(&self.0.raft()?.vote(request).await)
    }

    async fn send_snapshot_chunk(
        &self,
        r: Request<pb::SnapshotChunk>,
    ) -> Result<Response<pb::Empty>, Status> {
        let r = r.into_inner();
        let inbox = self.0.inbox()?;
        tokio::task::spawn_blocking(move || inbox.write(&r.transfer_id, r.offset, &r.data))
            .await
            .map_err(|e| Status::internal(e.to_string()))?
            .map_err(Status::failed_precondition)?;
        Ok(Response::new(pb::Empty {}))
    }

    async fn install_snapshot(
        &self,
        r: Request<pb::RaftSnapshot>,
    ) -> Result<Response<pb::RaftMessage>, Status> {
        let r = r.into_inner();
        let vote: Vote<u64> = decode(&r.vote_json)?;
        let meta: SnapshotMeta<u64, Member> = decode(&r.meta_json)?;
        let database = if r.transfer_id.is_empty() {
            r.database
        } else {
            // Put together from its chunks, checked (Issue #20).
            let inbox = self.0.inbox()?;
            tokio::task::spawn_blocking(move || inbox.take(&r.transfer_id, r.size, &r.sha256))
                .await
                .map_err(|e| Status::internal(e.to_string()))?
                .map_err(Status::failed_precondition)?
        };
        let snapshot = Snapshot {
            meta,
            snapshot: Box::new(database),
        };
        json(&self.0.raft()?.install_full_snapshot(vote, snapshot).await)
    }

    async fn push_blob(
        &self,
        r: Request<tonic::Streaming<pb::BlobUpload>>,
    ) -> Result<Response<pb::BlobRef>, Status> {
        self.0
            .receive_pushed(r.into_inner())
            .await
            .map(Response::new)
    }

    type FetchBlobStream = blob_sync::ChunkStream;

    async fn fetch_blob(
        &self,
        r: Request<pb::BlobRequest>,
    ) -> Result<Response<Self::FetchBlobStream>, Status> {
        let r = r.into_inner();
        self.0.fetch_blob(&r.id, r.offset).await.map(Response::new)
    }

    async fn has_blobs(&self, r: Request<pb::BlobList>) -> Result<Response<pb::BlobList>, Status> {
        Ok(Response::new(self.0.has_blobs(&r.into_inner().ids)))
    }

    async fn blobs_registered(
        &self,
        r: Request<pb::BlobList>,
    ) -> Result<Response<pb::BlobList>, Status> {
        self.0
            .blobs_registered(&r.into_inner().ids)
            .map(Response::new)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use openraft::StorageError;
    use openraft::testing::{StoreBuilder, Suite};

    use super::*;

    struct InMemory;

    impl StoreBuilder<TypeConfig, LogStore, StateMachine> for InMemory {
        async fn build(&self) -> Result<((), LogStore, StateMachine), StorageError<u64>> {
            let store = Store::open(None, None).unwrap();
            Ok((
                (),
                LogStore::open(None).unwrap(),
                StateMachine::new(Arc::new(Mutex::new(store))).unwrap(),
            ))
        }
    }

    /// ADR 0020: a scheduler older than the version the cluster has recorded
    /// with its features does not start on that state.
    #[test]
    fn a_scheduler_older_than_the_enabled_features_does_not_start() {
        let store = Store::open(None, None).unwrap();
        check_version(&store).unwrap();
        store.enable_features(&["a".into()], VERSION).unwrap();
        check_version(&store).unwrap();
        store.enable_features(&["b".into()], "99.0.0").unwrap();
        let e = check_version(&store).unwrap_err().to_string();
        assert!(e.contains("99.0.0") && e.contains("newer version"), "{e}");
        assert_eq!(store.enabled_features().unwrap(), ["a", "b"]);
        // An older version recorded later does not lower it.
        store.enable_features(&["c".into()], "1.0.0").unwrap();
        assert_eq!(
            store.min_binary_version().unwrap().as_deref(),
            Some("99.0.0")
        );
    }

    #[test]
    fn versions_compare_by_their_numbers() {
        use crate::store::version_older;
        assert!(version_older("1.2.0", "1.10.0"));
        assert!(version_older("1.1.9", "1.2.0"));
        assert!(!version_older("1.2.0", "1.2.0"));
        assert!(!version_older("2.0.0", "1.99.99"));
        assert!(version_older("", "1.2.0"));
        assert!(!version_older("1.2.0-rc1", "1.2.0"));
    }

    #[test]
    fn the_log_store_and_the_state_machine_pass_the_openraft_suite() {
        Suite::test_all(InMemory).unwrap();
    }

    /// Issue #37: the log is purged only up to what the state machine has
    /// applied; a purge asked for before (a snapshot being installed) waits.
    #[tokio::test]
    async fn a_purge_waits_for_the_state_machine() {
        use openraft::storage::RaftLogStorage;
        use openraft::{CommittedLeaderId, LogId};
        let log_id = |index| LogId::new(CommittedLeaderId::new(1, 1), index);
        let (applied, rx) = watch::channel(Some(log_id(3)));
        let mut log = LogStore::open(None).unwrap().with_applied(rx);
        // Up to what is applied: at once.
        log.purge(log_id(3)).await.unwrap();
        // Beyond it: only once the state machine gets there.
        let mut purging = log.clone();
        let purge = tokio::spawn(async move { purging.purge(log_id(9)).await });
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!purge.is_finished());
        assert_eq!(
            log.get_log_state().await.unwrap().last_purged_log_id,
            Some(log_id(3))
        );
        applied.send_replace(Some(log_id(9)));
        purge.await.unwrap().unwrap();
        assert_eq!(
            log.get_log_state().await.unwrap().last_purged_log_id,
            Some(log_id(9))
        );
        // A state machine that stops first makes the purge fail, not hang.
        let (applied, rx) = watch::channel(Some(log_id(1)));
        let mut log = LogStore::open(None).unwrap().with_applied(rx);
        let purge = tokio::spawn(async move { log.purge(log_id(5)).await });
        drop(applied);
        assert!(purge.await.unwrap().is_err());
    }
}

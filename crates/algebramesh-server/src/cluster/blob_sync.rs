//! Blobs across the schedulers (ADR 0021 §5, §9, §10).
//!
//! The leader stores a blob on its own disk and pushes it to every other
//! member over `Replication.PushBlob`, on connections of their own, not those
//! of Raft. `PutBlob` answers once a majority of the voters (of each
//! configuration, during a joint consensus) holds the body on disk, the
//! leader included. The leader keeps in memory which members it knows to hold
//! each blob; after a change of leader it asks them again (`HasBlobs`).
//!
//! A follower serves `GetBlob` from its own disk only. The leader fetches a
//! body it lacks from the others, and decides between `NOT_FOUND`,
//! `UNAVAILABLE` and `DATA_LOSS` only after confirming that it leads.
//!
//! Every scheduler removes, from time to time, the bodies untouched for
//! `orphan_grace` that the leader says no record refers to.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::Read;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant, SystemTime};

use algebramesh_core::blob::{BlobId, BlobRef, CHUNK_BYTES, EXPANDED_MAX_BYTES, markers};
use algebramesh_core::features::BLOB_REFS;
use algebramesh_rpc::pb;
use tokio::sync::{Notify, Semaphore, mpsc};
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::{Stream, StreamExt};
use tonic::Status;

use super::network::{self, PeerClient};
use super::{Kind, Member, Node, lock};
use crate::blobs::{BlobDisk, BlobOptions, DiskError, Incoming};
use crate::quota::UploadQuota;

/// A stream of the pieces of a blob, as `GetBlob` and `FetchBlob` return it.
pub type ChunkStream = Pin<Box<dyn Stream<Item = Result<pb::BlobChunk, Status>> + Send>>;

/// How long a member may take to say which blobs it holds.
const HAS_QUERY: Duration = Duration::from_secs(5);
/// A push or a fetch may take this long, plus the time its bytes need at
/// [`ASSUMED_RATE`] (or at the configured bandwidth, if lower).
const TRANSFER_BASE: Duration = Duration::from_secs(60);
const ASSUMED_RATE: u64 = 8 * 1024 * 1024;
/// Records whose markers are expanded for old clients at the same time.
const EXPANSIONS: usize = 2;
/// A transfer whose file was not written for this long is dropped.
const PART_STALE: Duration = Duration::from_secs(3600);
/// Ids asked about in one `HasBlobs`.
const ASK_BATCH: usize = 1000;
/// The first repair after a start, and then how often it runs.
const FIRST_REPAIR: Duration = Duration::from_secs(2);
const REPAIR_EVERY: Duration = Duration::from_secs(30);
/// The least time between two repairs.
const REPAIR_GAP: Duration = Duration::from_secs(1);

/// The blob store of a node and what the leader knows of the others.
pub(crate) struct Blobs {
    pub(crate) disk: Arc<BlobDisk>,
    options: BlobOptions,
    /// Members (by Raft id) known to hold each blob on disk.
    holders: Mutex<HashMap<BlobId, BTreeSet<u64>>>,
    /// Pushes in progress, so that one blob goes to one member once at a time.
    pushing: Mutex<HashSet<(BlobId, u64)>>,
    /// Woken when a push ends.
    pushed: Notify,
    expanding: Semaphore,
    peers: Mutex<BTreeMap<u64, (String, PeerClient)>>,
    /// Wakes the repair before its time (after a snapshot was installed).
    pub(crate) wake_repair: Arc<Notify>,
    /// Set when the node shuts down: the background work on blobs ends, so
    /// that nothing keeps the node (and its database) after it stopped.
    stop: tokio::sync::watch::Sender<bool>,
    /// What the last repair found (`NodeStatus`).
    under_replicated: AtomicU64,
    missing: AtomicU64,
    /// The bytes each client token may still upload (B3).
    quota: UploadQuota,
}

/// The blob store of a node, for `NodeStatus`.
pub(crate) struct BlobStatus {
    pub(crate) under_replicated: u64,
    pub(crate) missing: u64,
    pub(crate) quarantined: u64,
    pub(crate) free_bytes: u64,
    pub(crate) stored: u64,
    pub(crate) bytes_stored: u64,
    pub(crate) uploads_refused: u64,
}

impl Blobs {
    pub(crate) fn new(dir: std::path::PathBuf, options: BlobOptions) -> Self {
        let disk = BlobDisk::new(dir, options.limits());
        // No transfer runs before the node starts: what is left in `incoming`
        // was cut off by a stop or a crash.
        let left = disk.clean_parts(Duration::ZERO);
        if left > 0 {
            tracing::info!(parts = left, "removed blob transfers cut off by a stop");
        }
        Self {
            disk: Arc::new(disk),
            quota: UploadQuota::new(options.upload_bytes_per_token_per_hour),
            options,
            holders: Mutex::new(HashMap::new()),
            pushing: Mutex::new(HashSet::new()),
            pushed: Notify::new(),
            expanding: Semaphore::new(EXPANSIONS),
            peers: Mutex::new(BTreeMap::new()),
            wake_repair: Arc::new(Notify::new()),
            stop: tokio::sync::watch::channel(false).0,
            under_replicated: AtomicU64::new(0),
            missing: AtomicU64::new(0),
        }
    }

    fn transfer_timeout(&self, size: u64) -> Duration {
        let rate = match self.options.replication_bytes_per_second {
            0 => ASSUMED_RATE,
            r => r.min(ASSUMED_RATE),
        };
        TRANSFER_BASE + Duration::from_secs(size / rate.max(1))
    }
}

impl Node {
    /// Runs `work` until it ends or the node shuts down (`None`).
    pub(crate) async fn unless_stopped<T>(
        &self,
        work: impl std::future::Future<Output = T>,
    ) -> Option<T> {
        let mut stop = self.blobs.stop.subscribe();
        tokio::select! {
            biased;
            _ = stop.wait_for(|s| *s) => None,
            out = work => Some(out),
        }
    }

    /// Ends the background work on blobs.
    pub(crate) fn stop_blobs(&self) {
        self.blobs.stop.send_replace(true);
    }
}

/// A push in progress; its end is recorded when this is dropped.
struct PushDone {
    node: Arc<Node>,
    key: (BlobId, u64),
}

impl Drop for PushDone {
    fn drop(&mut self) {
        lock(&self.node.blobs.pushing).remove(&self.key);
        self.node.blobs.pushed.notify_waiters();
    }
}

/// The directory of the blobs of a node: the configured one, or `blobs` in the
/// data directory, or a temporary one.
pub(crate) fn blob_dir(
    options: &BlobOptions,
    data_dir: Option<&std::path::Path>,
) -> std::path::PathBuf {
    options.dir.clone().unwrap_or_else(|| match data_dir {
        Some(d) => d.join("blobs"),
        None => std::env::temp_dir().join(format!("algebramesh-blobs-{}", uuid::Uuid::new_v4())),
    })
}

/// Whether `holders` include a majority of every configuration.
fn majority(configs: &[BTreeSet<u64>], holders: &BTreeSet<u64>) -> bool {
    configs
        .iter()
        .filter(|c| !c.is_empty())
        .all(|c| c.intersection(holders).count() * 2 > c.len())
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, DiskError> + Send + 'static,
) -> Result<T, Status> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| Status::internal(format!("blob store task failed: {e}")))?
        .map_err(|e| e.status())
}

/// Receives a blob from `stream` into `disk`: the first piece names it.
pub(crate) async fn receive<S>(disk: Arc<BlobDisk>, mut stream: S) -> Result<BlobRef, Status>
where
    S: Stream<Item = Result<pb::BlobUpload, Status>> + Unpin + Send,
{
    let first = stream
        .next()
        .await
        .ok_or_else(|| Status::invalid_argument("an empty blob upload"))??;
    let id = BlobId::parse(&first.id).map_err(|e| Status::invalid_argument(e.to_string()))?;
    let size = first.size;
    let d = disk.clone();
    let mut incoming: Incoming = blocking(move || d.begin(&id, size)).await?;
    let mut piece = first.data;
    loop {
        if !piece.is_empty() {
            incoming = blocking(move || {
                incoming.write(&piece)?;
                Ok(incoming)
            })
            .await?;
        }
        match stream.next().await {
            Some(next) => piece = next?.data,
            None => break,
        }
    }
    blocking(move || disk.finish(incoming)).await
}

/// The pieces of the body of `id` from `offset`, read in a blocking task.
fn read_chunks(
    disk: Arc<BlobDisk>,
    id: BlobId,
    offset: u64,
    rate: u64,
) -> mpsc::Receiver<Result<Vec<u8>, Status>> {
    let (tx, rx) = mpsc::channel(2);
    tokio::task::spawn_blocking(move || {
        let mut file = match disk.open_at(&id, offset) {
            Ok(f) => f,
            Err(e) => {
                let _ = tx.blocking_send(Err(e.status()));
                return;
            }
        };
        loop {
            let mut buf = vec![0u8; CHUNK_BYTES];
            let mut filled = 0;
            while filled < buf.len() {
                match file.read(&mut buf[filled..]) {
                    Ok(0) => break,
                    Ok(n) => filled += n,
                    Err(e) => {
                        let _ = tx.blocking_send(Err(Status::internal(e.to_string())));
                        return;
                    }
                }
            }
            if filled == 0 {
                return;
            }
            buf.truncate(filled);
            let started = Instant::now();
            if tx.blocking_send(Ok(buf)).is_err() {
                return;
            }
            if rate > 0 {
                let due = Duration::from_secs_f64(filled as f64 / rate as f64);
                if let Some(rest) = due.checked_sub(started.elapsed()) {
                    std::thread::sleep(rest);
                }
            }
        }
    });
    rx
}

/// The body of `id` as a `GetBlob` answer.
pub(crate) fn chunk_stream(disk: Arc<BlobDisk>, id: BlobId, offset: u64) -> ChunkStream {
    let rx = read_chunks(disk, id, offset, 0);
    Box::pin(ReceiverStream::new(rx).map(|r| r.map(|data| pb::BlobChunk { data })))
}

impl Node {
    /// The disk of this node's blobs.
    pub fn blob_disk(&self) -> Arc<BlobDisk> {
        self.blobs.disk.clone()
    }

    fn node_id(&self) -> u64 {
        match &self.kind {
            Kind::Single(_) => 0,
            Kind::Cluster(c) => c.config.node_id,
        }
    }

    /// The voters of each configuration, and the other members.
    fn blob_membership(&self) -> (Vec<BTreeSet<u64>>, BTreeMap<u64, Member>) {
        match &self.kind {
            Kind::Single(_) => (vec![BTreeSet::from([0])], BTreeMap::new()),
            Kind::Cluster(c) => {
                let metrics = c.raft.metrics();
                let m = metrics.borrow();
                let membership = m.membership_config.membership();
                let others = membership
                    .nodes()
                    .filter(|(id, _)| **id != c.config.node_id)
                    .map(|(id, n)| (*id, n.clone()))
                    .collect();
                (membership.get_joint_config().clone(), others)
            }
        }
    }

    /// `FAILED_PRECONDITION` unless the cluster uses blobs (ADR 0020).
    pub fn require_blobs(&self) -> Result<(), Status> {
        if self.enabled_features().iter().any(|f| f == BLOB_REFS) {
            Ok(())
        } else {
            Err(Status::failed_precondition(format!(
                "the schedulers have not enabled the feature {BLOB_REFS}"
            )))
        }
    }

    fn peer(&self, id: u64, member: &Member) -> Result<PeerClient, Status> {
        let Kind::Cluster(c) = &self.kind else {
            return Err(Status::internal("no peers without a cluster"));
        };
        let mut peers = lock(&self.blobs.peers);
        if let Some((url, client)) = peers.get(&id)
            && *url == member.url
        {
            return Ok(client.clone());
        }
        let client = network::peer_client(member, &c.config.ca_pem, &c.config.peer_token)
            .map_err(Status::internal)?;
        peers.insert(id, (member.url.clone(), client.clone()));
        Ok(client)
    }

    /// The members known to hold `id`, this node included if it does.
    fn holders(&self, id: &BlobId) -> BTreeSet<u64> {
        let mut holders = lock(&self.blobs.holders)
            .get(id)
            .cloned()
            .unwrap_or_default();
        if self.blobs.disk.has(id) {
            holders.insert(self.node_id());
        } else {
            holders.remove(&self.node_id());
        }
        holders
    }

    fn add_holder(&self, id: &BlobId, member: u64) {
        lock(&self.blobs.holders)
            .entry(id.clone())
            .or_default()
            .insert(member);
    }

    /// Asks the other members which of `ids` they hold, and records it.
    /// Returns whether every member answered.
    async fn ask_holders(&self, ids: &[BlobId]) -> bool {
        let (_, others) = self.blob_membership();
        let mut all = true;
        for (id, member) in others {
            let asked = async {
                let mut client = self.peer(id, &member)?;
                let request = pb::BlobList {
                    ids: ids.iter().map(|i| i.as_str().to_owned()).collect(),
                };
                tokio::time::timeout(HAS_QUERY, client.has_blobs(request))
                    .await
                    .map_err(|_| Status::deadline_exceeded("no answer"))?
            };
            match asked.await {
                Ok(r) => {
                    let held: HashSet<String> = r.into_inner().ids.into_iter().collect();
                    let mut holders = lock(&self.blobs.holders);
                    for blob in ids {
                        let entry = holders.entry(blob.clone()).or_default();
                        if held.contains(blob.as_str()) {
                            entry.insert(id);
                        } else {
                            entry.remove(&id);
                        }
                    }
                }
                Err(e) => {
                    tracing::debug!(member = %member.name, "no answer about blobs: {e}");
                    all = false;
                }
            }
        }
        all
    }

    /// Whether a majority of the voters holds `id`, asking the members if
    /// this node does not know.
    pub(crate) async fn durable(&self, id: &BlobId) -> bool {
        let (configs, _) = self.blob_membership();
        if majority(&configs, &self.holders(id)) {
            return true;
        }
        self.ask_holders(std::slice::from_ref(id)).await;
        majority(&configs, &self.holders(id))
    }

    /// Pushes `r` to the members that do not hold it, in the background, and,
    /// if `wait`, waits until a majority holds it. `UNAVAILABLE` if the pushes
    /// end without one.
    pub(crate) async fn replicate(self: &Arc<Self>, r: &BlobRef, wait: bool) -> Result<(), Status> {
        let (configs, others) = self.blob_membership();
        let holders = self.holders(&r.id);
        for (id, member) in others {
            if holders.contains(&id) {
                continue;
            }
            if !lock(&self.blobs.pushing).insert((r.id.clone(), id)) {
                continue;
            }
            let node = self.clone();
            let blob = r.clone();
            tokio::spawn(async move {
                // Ends the push however the task ends, a panic included, so
                // that the blob may be pushed again and no call waits for it.
                let _done = PushDone {
                    node: node.clone(),
                    key: (blob.id.clone(), id),
                };
                match node.unless_stopped(node.push(id, &member, &blob)).await {
                    Some(Ok(())) => {
                        if let Some(dir) = node.data_dir() {
                            crate::fault::point(&dir, crate::fault::BLOB_PUSHED_ONE);
                        }
                        node.add_holder(&blob.id, id);
                    }
                    Some(Err(e)) => {
                        tracing::warn!(blob = %blob.id, member = %member.name, "cannot push a blob: {e}");
                    }
                    None => {}
                }
            });
        }
        if !wait {
            return Ok(());
        }
        loop {
            let pushed = self.blobs.pushed.notified();
            tokio::pin!(pushed);
            pushed.as_mut().enable();
            let holders = self.holders(&r.id);
            if majority(&configs, &holders) {
                return Ok(());
            }
            let pending = lock(&self.blobs.pushing)
                .iter()
                .any(|(blob, _)| *blob == r.id);
            if !pending {
                let voters: BTreeSet<u64> = configs.iter().flatten().copied().collect();
                return Err(Status::unavailable(format!(
                    "blob {} is held by {} of the {} voters, not by a majority; send it again \
                     later",
                    r.id,
                    voters.intersection(&holders).count(),
                    voters.len()
                )));
            }
            pushed.await;
        }
    }

    fn data_dir(&self) -> Option<std::path::PathBuf> {
        Some(self.blobs.disk.root().to_path_buf())
    }

    async fn push(&self, id: u64, member: &Member, r: &BlobRef) -> Result<(), Status> {
        let mut client = self.peer(id, member)?;
        let mut chunks = read_chunks(
            self.blobs.disk.clone(),
            r.id.clone(),
            0,
            self.blobs.options.replication_bytes_per_second,
        );
        let (tx, rx) = mpsc::channel::<pb::BlobUpload>(2);
        let blob = r.clone();
        let feed = tokio::spawn(async move {
            let mut first = true;
            let mut failed = None;
            while let Some(piece) = chunks.recv().await {
                let data = match piece {
                    Ok(d) => d,
                    Err(e) => {
                        failed = Some(e);
                        break;
                    }
                };
                let upload = pb::BlobUpload {
                    id: if first {
                        blob.id.as_str().to_owned()
                    } else {
                        String::new()
                    },
                    size: if first { blob.size } else { 0 },
                    data,
                };
                first = false;
                if tx.send(upload).await.is_err() {
                    break;
                }
            }
            if first && failed.is_none() {
                // An empty blob: one message names it.
                let _ = tx
                    .send(pb::BlobUpload {
                        id: blob.id.as_str().to_owned(),
                        size: 0,
                        data: Vec::new(),
                    })
                    .await;
            }
            failed
        });
        let call = client.push_blob(ReceiverStream::new(rx));
        let answer = tokio::time::timeout(self.blobs.transfer_timeout(r.size), call)
            .await
            .map_err(|_| Status::deadline_exceeded("the push took too long"))?;
        if let Ok(Some(e)) = feed.await {
            return Err(e);
        }
        let got = answer?.into_inner();
        if got.id != r.id.as_str() || got.size != r.size {
            return Err(Status::internal(format!(
                "member {} stored {} of {} bytes for {}",
                member.name, got.id, got.size, r.id
            )));
        }
        Ok(())
    }

    /// `PutBlob`: on the leader, stores the blob and waits for a majority.
    pub async fn put_blob<S>(self: &Arc<Self>, stream: S) -> Result<pb::BlobRef, Status>
    where
        S: Stream<Item = Result<pb::BlobUpload, Status>> + Unpin + Send,
    {
        self.put_blob_as(None, stream).await
    }

    /// `PutBlob` from the client token `caller`: the upload takes its size
    /// from the token's allowance before any byte is stored (ADR 0021, B3),
    /// unless this scheduler holds the blob already; an upload that fails
    /// gives it back.
    pub async fn put_blob_as<S>(
        self: &Arc<Self>,
        caller: Option<&str>,
        mut stream: S,
    ) -> Result<pb::BlobRef, Status>
    where
        S: Stream<Item = Result<pb::BlobUpload, Status>> + Unpin + Send,
    {
        self.scheduler()?;
        self.require_blobs()?;
        let first = stream
            .next()
            .await
            .ok_or_else(|| Status::invalid_argument("an empty blob upload"))??;
        let quota = &self.blobs.quota;
        let charged = match caller {
            Some(token) if quota.is_bounded() => {
                let held = BlobId::parse(&first.id)
                    .ok()
                    .and_then(|id| self.blobs.disk.size(&id))
                    == Some(first.size);
                if held {
                    None
                } else {
                    quota.take(token, first.size).map_err(|message| {
                        tracing::warn!(token = token, size = first.size, "blob upload refused");
                        Status::resource_exhausted(message)
                    })?;
                    Some((token, first.size))
                }
            }
            _ => None,
        };
        let stream = tokio_stream::once(Ok(first)).chain(stream);
        let stored = async {
            let r = receive(self.blobs.disk.clone(), stream).await?;
            self.replicate(&r, true).await?;
            Ok::<_, Status>(r)
        }
        .await;
        let r = match stored {
            Ok(r) => r,
            Err(e) => {
                if let Some((token, size)) = charged {
                    quota.give_back(token, size);
                }
                return Err(e);
            }
        };
        if let Some(dir) = self.data_dir() {
            crate::fault::point(&dir, crate::fault::BLOB_DURABLE);
        }
        Ok(pb::BlobRef {
            id: r.id.as_str().to_owned(),
            size: r.size,
        })
    }

    /// `PushBlob`: stores a blob the leader sends.
    pub async fn receive_pushed<S>(&self, stream: S) -> Result<pb::BlobRef, Status>
    where
        S: Stream<Item = Result<pb::BlobUpload, Status>> + Unpin + Send,
    {
        let r = receive(self.blobs.disk.clone(), stream).await?;
        Ok(pb::BlobRef {
            id: r.id.as_str().to_owned(),
            size: r.size,
        })
    }

    /// `StatBlob`.
    pub async fn stat_blob(&self, id: &str) -> Result<pb::BlobStat, Status> {
        let id = BlobId::parse(id).map_err(|e| Status::invalid_argument(e.to_string()))?;
        let size = self.blobs.disk.size(&id);
        if size.is_some() {
            let disk = self.blobs.disk.clone();
            let touched = id.clone();
            let _ = tokio::task::spawn_blocking(move || disk.touch(&touched)).await;
        }
        let durable = self.scheduler().is_ok() && self.durable(&id).await;
        Ok(pb::BlobStat {
            present: size.is_some(),
            durable,
            size: size.unwrap_or_default(),
        })
    }

    /// Checks, on the leader, that a majority holds each of `refs` before a
    /// record refers to them (ADR 0021 §5 step 4). Transfers nothing large:
    /// a blob held but not by a majority is pushed in the background and the
    /// call answered with `UNAVAILABLE`; one nobody holds gives
    /// `FAILED_PRECONDITION`. Before the cluster enables blobs, nothing.
    pub async fn ensure_blobs(self: &Arc<Self>, refs: &[BlobRef]) -> Result<(), Status> {
        // Without the feature, the scheduler refuses a result with markers
        // itself; nothing is checked here.
        if refs.is_empty() || self.require_blobs().is_err() {
            return Ok(());
        }
        for r in refs {
            let disk = self.blobs.disk.clone();
            let touched = r.id.clone();
            let _ = tokio::task::spawn_blocking(move || disk.touch(&touched)).await;
        }
        // The members are asked afresh: one may have removed a body since
        // this node last heard of it (an upload older than the grace). Asking
        // also marks the bodies as used there.
        let ids: Vec<BlobId> = refs.iter().map(|r| r.id.clone()).collect();
        self.ask_holders(&ids).await;
        let (configs, _) = self.blob_membership();
        let mut missing = Vec::new();
        let mut short = Vec::new();
        for r in refs {
            if let Some(size) = self.blobs.disk.size(&r.id)
                && size != r.size
            {
                return Err(Status::invalid_argument(format!(
                    "blob {} has {size} bytes, not {}",
                    r.id, r.size
                )));
            }
            if majority(&configs, &self.holders(&r.id)) {
                continue;
            }
            if self.holders(&r.id).is_empty() {
                missing.push(r.id.to_string());
            } else {
                short.push(r.clone());
            }
        }
        if !missing.is_empty() {
            return Err(Status::failed_precondition(format!(
                "no scheduler holds the blobs {}; store them again",
                missing.join(", ")
            )));
        }
        if short.is_empty() {
            return Ok(());
        }
        // Held somewhere, but not by a majority: the leader fetches it if it
        // lacks it, and pushes it on.
        let node = self.clone();
        let pending: Vec<String> = short.iter().map(|r| r.id.to_string()).collect();
        tokio::spawn(async move {
            node.unless_stopped(async {
                for r in short {
                    if !node.blobs.disk.has(&r.id) {
                        let _ = node.fetch_from_peers(&r.id, Some(r.size)).await;
                    }
                    if node.blobs.disk.has(&r.id) {
                        let _ = node.replicate(&r, false).await;
                    }
                }
            })
            .await;
        });
        Err(Status::unavailable(format!(
            "the blobs {} are not yet held by a majority of the schedulers; ask StatBlob again \
             later",
            pending.join(", ")
        )))
    }

    /// Fetches `id` from a member that holds it. Returns whether this node
    /// holds it now, and whether every member answered.
    async fn fetch_from_peers(&self, id: &BlobId, size: Option<u64>) -> (bool, bool) {
        let answered = self.ask_holders(std::slice::from_ref(id)).await;
        let (_, others) = self.blob_membership();
        let holders = self.holders(id);
        for (peer, member) in others {
            if !holders.contains(&peer) {
                continue;
            }
            let fetched = async {
                let mut client = self.peer(peer, &member)?;
                let stream = tokio::time::timeout(
                    HAS_QUERY,
                    client.fetch_blob(pb::BlobRequest {
                        id: id.as_str().to_owned(),
                        offset: 0,
                    }),
                )
                .await
                .map_err(|_| Status::deadline_exceeded("no answer"))??
                .into_inner();
                let bound = size.unwrap_or(self.blobs.disk.limits().max_blob_bytes);
                let timeout = self.blobs.transfer_timeout(bound);
                tokio::time::timeout(
                    timeout,
                    receive_unsized(self.blobs.disk.clone(), id.clone(), bound, stream),
                )
                .await
                .map_err(|_| Status::deadline_exceeded("the fetch took too long"))?
            };
            match fetched.await {
                Ok(_) => return (true, answered),
                Err(e) => {
                    tracing::warn!(blob = %id, member = %member.name, "cannot fetch a blob: {e}");
                }
            }
        }
        (self.blobs.disk.has(id), answered)
    }

    /// Makes sure this node holds a good body of `id`: from its disk, or, on
    /// the leader, from another member (ADR 0021 §9).
    pub(crate) async fn ensure_local(&self, id: &BlobId) -> Result<(), Status> {
        if self.blobs.disk.has(id) {
            let disk = self.blobs.disk.clone();
            let checked = id.clone();
            match blocking(move || disk.verify(&checked)).await {
                Ok(true) => {
                    let disk = self.blobs.disk.clone();
                    let touched = id.clone();
                    let _ = tokio::task::spawn_blocking(move || disk.touch(&touched)).await;
                    return Ok(());
                }
                // Moved to quarantine: fetch it again below.
                Ok(false) => {}
                Err(e) if e.code() == tonic::Code::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        // A follower answers from its own disk only.
        self.scheduler()?;
        let registered = !self
            .read_committed(|s| s.registered_blobs(std::slice::from_ref(id)))
            .await?
            .map_err(|e| Status::internal(e.to_string()))?
            .is_empty();
        let (held, all_answered) = match &self.kind {
            Kind::Single(_) => (false, true),
            Kind::Cluster(_) => self.fetch_from_peers(id, None).await,
        };
        if held {
            return Ok(());
        }
        if !registered {
            Err(Status::not_found(format!("no blob {id} is known")))
        } else if all_answered {
            Err(Status::data_loss(format!(
                "no scheduler holds blob {id}, which a job refers to"
            )))
        } else {
            Err(Status::unavailable(format!(
                "no scheduler that answered holds blob {id}; others did not answer"
            )))
        }
    }

    /// `GetBlob`.
    pub async fn get_blob(&self, id: &str, offset: u64) -> Result<ChunkStream, Status> {
        let id = BlobId::parse(id).map_err(|e| Status::invalid_argument(e.to_string()))?;
        self.ensure_local(&id).await?;
        let size = self.blobs.disk.size(&id).unwrap_or_default();
        if offset > size {
            return Err(Status::out_of_range(format!(
                "offset {offset} is past the {size} bytes of blob {id}"
            )));
        }
        Ok(chunk_stream(self.blobs.disk.clone(), id, offset))
    }

    /// `FetchBlob`: this node's own good body, or `NOT_FOUND`.
    pub async fn fetch_blob(&self, id: &str, offset: u64) -> Result<ChunkStream, Status> {
        let id = BlobId::parse(id).map_err(|e| Status::invalid_argument(e.to_string()))?;
        let disk = self.blobs.disk.clone();
        let checked = id.clone();
        if !blocking(move || disk.verify(&checked)).await? {
            return Err(Status::not_found(format!("blob {id} was damaged here")));
        }
        Ok(chunk_stream(self.blobs.disk.clone(), id, offset))
    }

    /// `HasBlobs`. The bodies asked about are marked as used: the leader asks
    /// before a record refers to them.
    pub fn has_blobs(&self, ids: &[String]) -> pb::BlobList {
        let held: Vec<String> = ids
            .iter()
            .filter(|i| BlobId::parse(i).is_ok_and(|id| self.blobs.disk.has(&id)))
            .cloned()
            .collect();
        for id in held.iter().filter_map(|i| BlobId::parse(i).ok()) {
            self.blobs.disk.touch(&id);
        }
        pb::BlobList { ids: held }
    }

    /// `BlobsRegistered`: on the leader, those of `ids` a record refers to,
    /// in the committed state or among the changes not yet committed.
    pub fn blobs_registered(&self, ids: &[String]) -> Result<pb::BlobList, Status> {
        let scheduler = self.scheduler()?;
        let ids: Vec<BlobId> = ids
            .iter()
            .map(|i| BlobId::parse(i).map_err(|e| Status::invalid_argument(e.to_string())))
            .collect::<Result<_, _>>()?;
        let internal = |e: crate::store::StoreError| Status::internal(e.to_string());
        let mut found: BTreeSet<BlobId> = scheduler
            .with_store(|s| s.registered_blobs(&ids))
            .map_err(internal)?
            .into_iter()
            .collect();
        if let Kind::Cluster(c) = &self.kind {
            found.extend(lock(&c.state).registered_blobs(&ids).map_err(internal)?);
        }
        Ok(pb::BlobList {
            ids: found.into_iter().map(String::from).collect(),
        })
    }

    /// The blobs a record refers to among `ids`, asked of the leader.
    async fn registered_at_leader(&self, ids: &[BlobId]) -> Result<HashSet<BlobId>, Status> {
        let as_strings: Vec<String> = ids.iter().map(|i| i.as_str().to_owned()).collect();
        let list = match self.blobs_registered(&as_strings) {
            Ok(list) => list,
            Err(_) => {
                let Kind::Cluster(c) = &self.kind else {
                    return Err(Status::unavailable("no scheduler"));
                };
                let leader = c
                    .raft
                    .metrics()
                    .borrow()
                    .current_leader
                    .ok_or_else(|| Status::unavailable("no leader"))?;
                let member = self
                    .member(leader)
                    .ok_or_else(|| Status::unavailable("the leader is not a member"))?;
                let mut client = self.peer(leader, &member)?;
                tokio::time::timeout(
                    HAS_QUERY,
                    client.blobs_registered(pb::BlobList { ids: as_strings }),
                )
                .await
                .map_err(|_| Status::deadline_exceeded("no answer"))??
                .into_inner()
            }
        };
        Ok(list
            .ids
            .iter()
            .filter_map(|i| BlobId::parse(i).ok())
            .collect())
    }

    /// Removes the bodies here that no record refers to and nothing touched
    /// for `orphan_grace`, and transfers that stopped (ADR 0021 §10).
    /// Returns the number of bodies removed.
    pub async fn collect_blobs(&self) -> Result<usize, Status> {
        let disk = self.blobs.disk.clone();
        let grace = self.blobs.options.orphan_grace;
        let stale = PART_STALE.min(grace);
        let listed = tokio::task::spawn_blocking(move || {
            disk.clean_parts(stale);
            disk.list()
        })
        .await
        .map_err(|e| Status::internal(e.to_string()))?;
        let Some(before) = SystemTime::now().checked_sub(grace) else {
            return Ok(0);
        };
        let old: Vec<BlobId> = listed
            .into_iter()
            .filter(|(_, touched)| *touched <= before)
            .map(|(r, _)| r.id)
            .collect();
        if old.is_empty() {
            return Ok(0);
        }
        let registered = self.registered_at_leader(&old).await?;
        let doomed: Vec<BlobId> = old
            .into_iter()
            .filter(|i| !registered.contains(i))
            .collect();
        let disk = self.blobs.disk.clone();
        let removed = tokio::task::spawn_blocking(move || {
            doomed
                .iter()
                .filter(|id| disk.remove_untouched(id, before))
                .count()
        })
        .await
        .map_err(|e| Status::internal(e.to_string()))?;
        if removed > 0 {
            tracing::info!(node = %self.name, removed, "removed unreferenced blobs");
        }
        Ok(removed)
    }

    /// The blobs the records refer to, as this node has applied them.
    fn referenced(&self) -> Result<Vec<BlobRef>, Status> {
        let internal = |e: crate::store::StoreError| Status::internal(e.to_string());
        match &self.kind {
            Kind::Single(s) => s.with_store(|st| st.referenced_blobs()).map_err(internal),
            Kind::Cluster(c) => lock(&c.state).referenced_blobs().map_err(internal),
        }
    }

    /// Asks the other members about `ids`, in batches.
    async fn ask_all(&self, ids: &[BlobId]) {
        for batch in ids.chunks(ASK_BATCH) {
            self.ask_holders(batch).await;
        }
    }

    /// One round of repair (ADR 0021 §9, Issue #73): this node fetches the
    /// referenced blobs it lacks; the leader also pushes each one to the
    /// members that lack it. The state machine never waits for this.
    pub async fn repair_blobs(self: &Arc<Self>) -> Result<RepairRound, Status> {
        if self.require_blobs().is_err() {
            return Ok(RepairRound::default());
        }
        let refs = self.referenced()?;
        let mut round = RepairRound::default();
        if matches!(self.kind, Kind::Cluster(_)) {
            for r in &refs {
                if self.blobs.disk.has(&r.id) {
                    continue;
                }
                if self.fetch_from_peers(&r.id, Some(r.size)).await.0 {
                    round.fetched += 1;
                } else {
                    round.missing += 1;
                }
            }
        } else {
            round.missing = refs.iter().filter(|r| !self.blobs.disk.has(&r.id)).count() as u64;
        }
        if self.scheduler().is_ok() {
            let ids: Vec<BlobId> = refs.iter().map(|r| r.id.clone()).collect();
            self.ask_all(&ids).await;
            let (configs, others) = self.blob_membership();
            for r in &refs {
                let holders = self.holders(&r.id);
                if !majority(&configs, &holders) {
                    round.under_replicated += 1;
                }
                let lacking = others.keys().any(|m| !holders.contains(m));
                if lacking && self.blobs.disk.has(&r.id) {
                    round.pushing += 1;
                    let _ = self.replicate(r, false).await;
                }
            }
        }
        self.blobs
            .under_replicated
            .store(round.under_replicated, Ordering::Relaxed);
        self.blobs.missing.store(round.missing, Ordering::Relaxed);
        if round.fetched + round.missing + round.under_replicated + round.pushing > 0 {
            tracing::info!(node = %self.name, ?round, "repaired blobs");
        }
        Ok(round)
    }

    /// The blob counts of `NodeStatus`: under-replicated (on the leader),
    /// missing here, in quarantine, and the free bytes of the volume.
    pub(crate) fn blob_status(&self) -> BlobStatus {
        let bodies = self.blobs.disk.list();
        BlobStatus {
            under_replicated: self.blobs.under_replicated.load(Ordering::Relaxed),
            missing: self.blobs.missing.load(Ordering::Relaxed),
            quarantined: self.blobs.disk.quarantined() as u64,
            free_bytes: self.blobs.disk.free_bytes().unwrap_or(0),
            stored: bodies.len() as u64,
            bytes_stored: bodies.iter().map(|(r, _)| r.size).sum(),
            uploads_refused: self.blobs.quota.refused(),
        }
    }

    /// The referenced blobs that fewer than a majority of `voters` hold, the
    /// members asked afresh.
    pub(crate) async fn short_of(&self, voters: &BTreeSet<u64>) -> Result<Vec<BlobRef>, Status> {
        if self.require_blobs().is_err() {
            return Ok(Vec::new());
        }
        let refs = self.referenced()?;
        let ids: Vec<BlobId> = refs.iter().map(|r| r.id.clone()).collect();
        self.ask_all(&ids).await;
        let configs = [voters.clone()];
        Ok(refs
            .into_iter()
            .filter(|r| !majority(&configs, &self.holders(&r.id)))
            .collect())
    }

    /// The referenced blobs member `id` lacks, asked of it now.
    pub(crate) async fn lacking_on(&self, id: u64) -> Result<Vec<BlobRef>, Status> {
        if self.require_blobs().is_err() {
            return Ok(Vec::new());
        }
        let refs = self.referenced()?;
        let ids: Vec<BlobId> = refs.iter().map(|r| r.id.clone()).collect();
        self.ask_all(&ids).await;
        Ok(refs
            .into_iter()
            .filter(|r| !self.holders(&r.id).contains(&id))
            .collect())
    }

    /// Pushes each of `refs` this node holds to the members that lack it, in
    /// the background.
    pub(crate) async fn push_all(self: &Arc<Self>, refs: &[BlobRef]) {
        for r in refs {
            if self.blobs.disk.has(&r.id) {
                let _ = self.replicate(r, false).await;
            }
        }
    }

    /// The record `record_json` with each marker of its result and of its
    /// arguments replaced by the value, for a client that does not resolve
    /// markers (ADR 0021 §8). `input_normalized` and `input_sha256` stay those
    /// of the recorded spec (ADR 0021 §13).
    pub async fn expand_record(&self, record_json: String) -> Result<String, Status> {
        if !record_json.contains("\"$blob\"") {
            return Ok(record_json);
        }
        let mut record: algebramesh_core::JobRecord = serde_json::from_str(&record_json)
            .map_err(|e| Status::internal(format!("a stored record is not valid: {e}")))?;
        // A value recorded before the cluster used blobs may hold `$` keys
        // that are not markers: it is returned as it is.
        let in_result = record
            .result
            .as_ref()
            .map(|r| markers(r).unwrap_or_default())
            .unwrap_or_default();
        let in_arguments = if self
            .enabled_features()
            .iter()
            .any(|f| f == algebramesh_core::features::BLOB_ARGUMENTS)
        {
            algebramesh_core::blob::argument_markers(&record.spec.arguments).unwrap_or_default()
        } else {
            Vec::new()
        };
        if in_result.is_empty() && in_arguments.is_empty() {
            return Ok(record_json);
        }
        let total = record_json.len() as u64
            + in_result.iter().map(|(_, r)| r.size).sum::<u64>()
            + in_arguments.iter().map(|(_, r)| r.size).sum::<u64>();
        if total > EXPANDED_MAX_BYTES {
            return Err(Status::failed_precondition(format!(
                "the record would take {total} bytes with its blobs, more than the {} bytes                  returned to a client that does not resolve blobs; fetch them with GetBlob",
                EXPANDED_MAX_BYTES
            )));
        }
        let _permit = self
            .blobs
            .expanding
            .acquire()
            .await
            .map_err(|_| Status::unavailable("shutting down"))?;
        for (place, r) in in_result {
            let value = self.read_value(&r).await?;
            match place {
                algebramesh_core::blob::MarkerPlace::Whole => record.result = Some(value),
                algebramesh_core::blob::MarkerPlace::Key(k) => {
                    if let Some(serde_json::Value::Object(m)) = &mut record.result {
                        m.insert(k, value);
                    }
                }
            }
        }
        for (k, r) in in_arguments {
            let value = self.read_value(&r).await?;
            if let serde_json::Value::Object(m) = &mut record.spec.arguments {
                m.insert(k, value);
            }
        }
        serde_json::to_string(&record).map_err(|e| Status::internal(e.to_string()))
    }

    /// The JSON value blob `r` holds, read whole and checked.
    async fn read_value(&self, r: &BlobRef) -> Result<serde_json::Value, Status> {
        self.ensure_local(&r.id).await?;
        let disk = self.blobs.disk.clone();
        let id = r.id.clone();
        let bytes = blocking(move || {
            let mut file = disk.open_at(&id, 0)?;
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)?;
            Ok(bytes)
        })
        .await?;
        if BlobId::of(&bytes) != r.id {
            return Err(Status::data_loss(format!(
                "blob {} changed while read",
                r.id
            )));
        }
        serde_json::from_slice(&bytes)
            .map_err(|e| Status::internal(format!("blob {} is not JSON: {e}", r.id)))
    }
}

/// Receives from another scheduler a body of at most `bound` bytes into
/// `disk`, checked by its hash at the end.
async fn receive_unsized<S>(
    disk: Arc<BlobDisk>,
    id: BlobId,
    bound: u64,
    stream: S,
) -> Result<BlobRef, Status>
where
    S: Stream<Item = Result<pb::BlobChunk, Status>> + Send,
{
    let mut stream = Box::pin(stream);
    let d = disk.clone();
    let mut incoming: Incoming = blocking(move || d.begin(&id, bound)).await?;
    let mut written = 0u64;
    while let Some(piece) = stream.next().await {
        let data = piece?.data;
        written += data.len() as u64;
        incoming = blocking(move || {
            incoming.write(&data)?;
            Ok(incoming)
        })
        .await?;
    }
    blocking(move || disk.finish_sized(incoming, written)).await
}

/// What one round of repair did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RepairRound {
    /// Blobs this node lacked and fetched.
    pub fetched: u64,
    /// Blobs this node lacks still.
    pub missing: u64,
    /// On the leader: blobs fewer than a majority of the voters hold.
    pub under_replicated: u64,
    /// On the leader: blobs pushed to members that lacked them.
    pub pushing: u64,
}

/// Repairs the blobs of `node` shortly after it starts, then every
/// [`REPAIR_EVERY`] or when woken, at most once every [`REPAIR_GAP`], until
/// the node is dropped.
pub(crate) async fn repair(node: Weak<Node>) {
    let mut wait = FIRST_REPAIR;
    loop {
        // Only the signals are held while waiting, not the node.
        let (wake, mut stop) = match node.upgrade() {
            Some(n) => (n.blobs.wake_repair.clone(), n.blobs.stop.subscribe()),
            None => return,
        };
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = wake.notified() => {}
            _ = stop.wait_for(|s| *s) => return,
        }
        wait = REPAIR_EVERY;
        let Some(n) = node.upgrade() else {
            return;
        };
        match n.unless_stopped(n.repair_blobs()).await {
            Some(Err(e)) => tracing::debug!(node = %n.name, "blob repair skipped: {e}"),
            Some(Ok(_)) => {}
            None => return,
        }
        drop(n);
        tokio::time::sleep(REPAIR_GAP).await;
    }
}

/// Wakes the repair of `node` when the records it has applied refer to a
/// blob they did not before (ADR 0021 §9): a follower that missed the push
/// fetches it then, not at the next round.
///
/// It holds the node and its state only while it reads them: a task that kept
/// the state would keep its database open after the node stops, and a
/// scheduler started again over the same files could not replace it.
pub(crate) async fn follow_references(
    node: Weak<Node>,
    mut applied: tokio::sync::watch::Receiver<i64>,
) {
    let mut seen = None;
    loop {
        if applied.changed().await.is_err() {
            return;
        }
        let Some(n) = node.upgrade() else {
            return;
        };
        let signature = match &n.kind {
            super::Kind::Cluster(c) => lock(&c.state).blob_signature().ok(),
            _ => return,
        };
        if signature != seen {
            if seen.is_some() || signature.is_some_and(|(count, _)| count > 0) {
                n.blobs.wake_repair.notify_one();
            }
            seen = signature;
        }
    }
}

/// Collects unreferenced blobs on `node` every `collect_every`, until the node
/// is dropped.
pub(crate) async fn collect_garbage(node: Weak<Node>, every: Duration) {
    loop {
        tokio::time::sleep(every).await;
        let Some(node) = node.upgrade() else {
            return;
        };
        match node.unless_stopped(node.collect_blobs()).await {
            Some(Err(e)) => tracing::debug!(node = %node.name, "blob collection skipped: {e}"),
            Some(Ok(_)) => {}
            None => return,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::scheduler::{Scheduler, SchedulerOptions};
    use crate::store::Store;

    fn single() -> (tempfile::TempDir, Arc<Node>) {
        let dir = tempfile::tempdir().unwrap();
        let options = SchedulerOptions {
            blobs: BlobOptions {
                dir: Some(dir.path().join("blobs")),
                min_free_bytes: 0,
                ..Default::default()
            },
            ..Default::default()
        };
        let store = Store::open(None, None).unwrap();
        let scheduler = Arc::new(Scheduler::new(store, options).unwrap());
        (dir, Node::single(scheduler))
    }

    fn upload(body: &[u8]) -> impl Stream<Item = Result<pb::BlobUpload, Status>> + Unpin + Send {
        let id = BlobId::of(body);
        let mut pieces = vec![Ok(pb::BlobUpload {
            id: id.to_string(),
            size: body.len() as u64,
            data: Vec::new(),
        })];
        pieces.extend(body.chunks(3).map(|c| {
            Ok(pb::BlobUpload {
                id: String::new(),
                size: 0,
                data: c.to_vec(),
            })
        }));
        tokio_stream::iter(pieces)
    }

    fn record_with(result: serde_json::Value) -> String {
        let spec = algebramesh_core::JobSpec::from_json(
            r#"{"schema_version": 1, "backend": "test", "operation": "test.op",
                "arguments": {}}"#,
        )
        .unwrap();
        let mut record = algebramesh_core::JobRecord::new(
            algebramesh_core::JobId::new(),
            spec,
            chrono::Utc::now(),
        );
        record.result = Some(result);
        serde_json::to_string(&record).unwrap()
    }

    #[tokio::test]
    async fn a_single_scheduler_stores_and_expands_blobs() {
        let (_dir, node) = single();
        let value = serde_json::json!({"rows": [1, 2, 3]});
        let body = serde_json::to_vec(&value).unwrap();
        let r = node.put_blob(upload(&body)).await.unwrap();
        assert_eq!(r.id, BlobId::of(&body).to_string());
        let stat = node.stat_blob(&r.id).await.unwrap();
        assert!(stat.present && stat.durable, "{stat:?}");
        let blob = BlobRef {
            id: BlobId::parse(&r.id).unwrap(),
            size: r.size,
        };
        node.ensure_blobs(std::slice::from_ref(&blob))
            .await
            .unwrap();
        // A wrong size or wrong bytes are refused.
        let e = node
            .put_blob(tokio_stream::iter(vec![Ok(pb::BlobUpload {
                id: r.id.clone(),
                size: r.size,
                data: b"wrong".repeat(10)[..body.len()].to_vec(),
            })]))
            .await
            .unwrap_err();
        assert_eq!(e.code(), tonic::Code::InvalidArgument, "{e}");

        let whole = node
            .expand_record(record_with(blob.marker()))
            .await
            .unwrap();
        let whole: algebramesh_core::JobRecord = serde_json::from_str(&whole).unwrap();
        assert_eq!(whole.result.unwrap(), value);
        let keyed = node
            .expand_record(record_with(serde_json::json!({"a": blob.marker(), "b": 2})))
            .await
            .unwrap();
        let keyed: algebramesh_core::JobRecord = serde_json::from_str(&keyed).unwrap();
        assert_eq!(
            keyed.result.unwrap(),
            serde_json::json!({"a": value, "b": 2})
        );
        // A record without markers is returned as it is.
        let plain = record_with(serde_json::json!({"a": 1}));
        assert_eq!(node.expand_record(plain.clone()).await.unwrap(), plain);
    }

    /// An old client is given at most 96 MiB; the bound is decided from the
    /// sizes in the markers, before any body is read.
    #[tokio::test]
    async fn expansion_is_bounded() {
        let (_dir, node) = single();
        let huge = BlobRef {
            id: BlobId::of(b"not stored"),
            size: EXPANDED_MAX_BYTES,
        };
        let e = node
            .expand_record(record_with(huge.marker()))
            .await
            .unwrap_err();
        assert_eq!(e.code(), tonic::Code::FailedPrecondition, "{e}");
        assert!(e.message().contains("GetBlob"), "{e}");
    }
}

#[cfg(test)]
mod majority_tests {
    use std::collections::BTreeSet;

    use proptest::prelude::*;

    use super::majority;

    fn set(bits: u8) -> BTreeSet<u64> {
        (0..6).filter(|i| bits & (1 << i) != 0).collect()
    }

    proptest! {
        /// Held by a majority means more than half of the voters of each
        /// configuration (both, during a joint consensus); a repair that
        /// reached every member leaves every blob some member held on a
        /// majority.
        #[test]
        fn a_majority_is_one_in_every_configuration(
            old in 1u8..64, new in 0u8..64, holders in 0u8..64, reached in 0u8..64,
        ) {
            let configs = vec![set(old), set(new)];
            let holders = set(holders);
            let expected = configs
                .iter()
                .filter(|c| !c.is_empty())
                .all(|c| c.iter().filter(|v| holders.contains(v)).count() * 2 > c.len());
            prop_assert_eq!(majority(&configs, &holders), expected);
            // The repair copies a held blob to every member it reaches.
            let reached = set(reached);
            let voters: BTreeSet<u64> = configs.iter().flatten().copied().collect();
            if !holders.is_empty() && voters.is_subset(&reached) {
                let after: BTreeSet<u64> = holders.union(&reached).copied().collect();
                prop_assert!(majority(&configs, &after));
            }
        }
    }
}

//! Raft messages between schedulers, over the `Replication` gRPC service with TLS
//! and the peer token. Requests and responses are the openraft types as JSON.

// `RPCError` is fixed by the openraft traits these functions serve.
#![allow(clippy::result_large_err)]

use std::collections::BTreeMap;
use std::future::Future;
use std::time::Duration;

use algebramesh_rpc::pb::replication_client::ReplicationClient;
use algebramesh_rpc::{TokenInterceptor, client_tls, pb};
use openraft::error::{
    Fatal, InstallSnapshotError, NetworkError, RPCError, RaftError, RemoteError, ReplicationClosed,
    StreamingError, Unreachable,
};
use openraft::network::{RPCOption, RaftNetwork, RaftNetworkFactory};
use openraft::raft::{
    AppendEntriesRequest, AppendEntriesResponse, InstallSnapshotRequest, InstallSnapshotResponse,
    SnapshotResponse, VoteRequest, VoteResponse,
};
use openraft::{OptionalSend, Snapshot, Vote};
use tonic::service::interceptor::InterceptedService;
use tonic::transport::{Channel, Endpoint};

use super::{Member, TypeConfig};

type NodeId = u64;
pub type PeerClient = ReplicationClient<InterceptedService<Channel, TokenInterceptor>>;

/// How long the name lookup and the TCP connection to another scheduler, and
/// then the TLS handshake, may each take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// The largest message between schedulers. Larger than for clients and agents:
/// a change is JSON inside the JSON of a log entry, inside the JSON of an
/// `AppendEntries` request, and each level escapes the quotes of the one
/// inside, so an entry can be several times as large as the record it carries.
/// The peers are other schedulers, authenticated with the peer token.
pub const REPLICATION_MAX_MESSAGE_BYTES: usize = 1024 * 1024 * 1024;

/// Bytes of entries sent in one `AppendEntries` at most; the rest go in the
/// next request (Issue #20). An entry larger than this goes alone. Openraft
/// gives an `AppendEntries` one heartbeat interval: a request that cannot be
/// sent in that time is cut and sent again, forever, so the budget is what a
/// link carries in a fraction of a second.
const APPEND_BUDGET: usize = 4 * 1024 * 1024;

/// A client of another scheduler's `Replication` service.
pub fn peer_client(member: &Member, ca_pem: &[u8], token: &str) -> Result<PeerClient, String> {
    let endpoint = Endpoint::from_shared(member.url.clone())
        .map_err(|e| e.to_string())?
        .tls_config(client_tls(ca_pem, &member.domain))
        .map_err(|e| e.to_string())?;
    let channel = algebramesh_rpc::connect_lazy(&endpoint, CONNECT_TIMEOUT);
    let token = TokenInterceptor::new(token).map_err(|e| format!("peer token: {e}"))?;
    Ok(ReplicationClient::with_interceptor(channel, token)
        .max_decoding_message_size(REPLICATION_MAX_MESSAGE_BYTES)
        .max_encoding_message_size(REPLICATION_MAX_MESSAGE_BYTES))
}

/// Connects to the members the Raft membership names, with the cluster's CA and
/// the peer token. Openraft asks for a client for every vote request, so the
/// channel to each member is kept and shared while its URL stays the same.
pub struct Network {
    ca_pem: Vec<u8>,
    token: String,
    clients: BTreeMap<NodeId, (String, PeerClient)>,
    /// Size of the chunks a snapshot is sent in.
    chunk: usize,
}

impl Network {
    pub fn new(ca_pem: Vec<u8>, token: String, chunk: usize) -> Self {
        Self {
            ca_pem,
            token,
            clients: BTreeMap::new(),
            chunk,
        }
    }
}

impl RaftNetworkFactory<TypeConfig> for Network {
    type Network = Connection;

    async fn new_client(&mut self, target: NodeId, node: &Member) -> Connection {
        if let Some((url, client)) = self.clients.get(&target)
            && *url == node.url
        {
            return Connection {
                target,
                client: Some(client.clone()),
                chunk: self.chunk,
            };
        }
        let client = peer_client(node, &self.ca_pem, &self.token)
            .inspect_err(|e| tracing::error!(target, url = %node.url, "cannot reach member: {e}"))
            .ok();
        if let Some(c) = &client {
            self.clients.insert(target, (node.url.clone(), c.clone()));
        }
        Connection {
            target,
            client,
            chunk: self.chunk,
        }
    }
}

pub struct Connection {
    target: NodeId,
    /// `None` if the member's URL is not usable.
    client: Option<PeerClient>,
    chunk: usize,
}

fn unreachable(message: impl Into<String>) -> Unreachable {
    Unreachable::new(&std::io::Error::other(message.into()))
}

impl Connection {
    fn client(&self) -> Result<PeerClient, Unreachable> {
        self.client
            .clone()
            .ok_or_else(|| unreachable(format!("scheduler {} has no usable URL", self.target)))
    }

    /// Sends `request` with `call` and decodes the `Result<Resp, E>` in the reply.
    async fn call<Req, Resp, E, F, Fut>(
        &self,
        request: &Req,
        option: &RPCOption,
        call: F,
    ) -> Result<Resp, RPCError<NodeId, Member, E>>
    where
        Req: serde::Serialize,
        Resp: serde::de::DeserializeOwned,
        E: std::error::Error + serde::de::DeserializeOwned,
        F: FnOnce(PeerClient, pb::RaftMessage) -> Fut,
        Fut: Future<Output = Result<tonic::Response<pb::RaftMessage>, tonic::Status>>,
    {
        let json = serde_json::to_vec(request).map_err(|e| NetworkError::new(&e))?;
        let reply = tokio::time::timeout(
            option.hard_ttl(),
            call(self.client()?, pb::RaftMessage { json }),
        )
        .await
        .map_err(|_| unreachable("timed out"))?
        .map_err(|s| Unreachable::new(&s))?;
        let result: Result<Resp, E> =
            serde_json::from_slice(&reply.into_inner().json).map_err(|e| NetworkError::new(&e))?;
        result.map_err(|e| RPCError::RemoteError(RemoteError::new(self.target, e)))
    }
}

impl RaftNetwork<TypeConfig> for Connection {
    async fn append_entries(
        &mut self,
        rpc: AppendEntriesRequest<TypeConfig>,
        option: RPCOption,
    ) -> Result<AppendEntriesResponse<NodeId>, RPCError<NodeId, Member, RaftError<NodeId>>> {
        // Send at most APPEND_BUDGET bytes of entries (at least one entry); if
        // the follower takes them, report the part that was sent, and openraft
        // sends the rest next (Issue #20).
        let mut bytes = 0;
        let mut take = 0;
        for entry in &rpc.entries {
            let size = serde_json::to_vec(entry)
                .map_err(|e| NetworkError::new(&e))?
                .len();
            if take > 0 && bytes + size > APPEND_BUDGET {
                break;
            }
            bytes += size;
            take += 1;
        }
        if take == rpc.entries.len() {
            return self
                .call(&rpc, &option, |mut c, m| async move {
                    c.append_entries(m).await
                })
                .await;
        }
        let last = rpc.entries[take - 1].log_id;
        let part = AppendEntriesRequest::<TypeConfig> {
            vote: rpc.vote,
            prev_log_id: rpc.prev_log_id,
            leader_commit: rpc.leader_commit,
            entries: rpc.entries[..take].to_vec(),
        };
        let reply = self
            .call(&part, &option, |mut c, m| async move {
                c.append_entries(m).await
            })
            .await?;
        Ok(match reply {
            AppendEntriesResponse::Success => AppendEntriesResponse::PartialSuccess(Some(last)),
            other => other,
        })
    }

    async fn vote(
        &mut self,
        rpc: VoteRequest<NodeId>,
        option: RPCOption,
    ) -> Result<VoteResponse<NodeId>, RPCError<NodeId, Member, RaftError<NodeId>>> {
        self.call(&rpc, &option, |mut c, m| async move { c.vote(m).await })
            .await
    }

    #[allow(deprecated)]
    async fn install_snapshot(
        &mut self,
        _rpc: InstallSnapshotRequest<TypeConfig>,
        _option: RPCOption,
    ) -> Result<
        InstallSnapshotResponse<NodeId>,
        RPCError<NodeId, Member, RaftError<NodeId, InstallSnapshotError>>,
    > {
        // Snapshots go whole through `full_snapshot`.
        Err(RPCError::Unreachable(unreachable(
            "chunked snapshots are not used",
        )))
    }

    async fn full_snapshot(
        &mut self,
        vote: Vote<NodeId>,
        snapshot: Snapshot<TypeConfig>,
        cancel: impl Future<Output = ReplicationClosed> + OptionalSend + 'static,
        option: RPCOption,
    ) -> Result<SnapshotResponse<NodeId>, StreamingError<TypeConfig, Fatal<NodeId>>> {
        // The database goes in chunks, then the metadata with the size and
        // digest of the whole (Issue #20). The follower installs it only when
        // every chunk arrived and the digest matches.
        let database = *snapshot.snapshot;
        let transfer_id = uuid::Uuid::new_v4().to_string();
        let sha256 = algebramesh_core::sha256_hex(&database);
        let mut client = self.client()?;
        tokio::pin!(cancel);
        for (k, piece) in database.chunks(self.chunk).enumerate() {
            let chunk = pb::SnapshotChunk {
                transfer_id: transfer_id.clone(),
                offset: (k * self.chunk) as u64,
                data: piece.to_vec(),
            };
            let send = tokio::time::timeout(option.hard_ttl(), client.send_snapshot_chunk(chunk));
            tokio::select! {
                closed = &mut cancel => return Err(StreamingError::Closed(closed)),
                r = send => r,
            }
            .map_err(|_| unreachable("timed out sending a snapshot chunk"))?
            .map_err(|s| Unreachable::new(&s))?;
        }
        let request = pb::RaftSnapshot {
            vote_json: serde_json::to_vec(&vote).map_err(|e| NetworkError::new(&e))?,
            meta_json: serde_json::to_vec(&snapshot.meta).map_err(|e| NetworkError::new(&e))?,
            database: Vec::new(),
            transfer_id,
            size: database.len() as u64,
            sha256,
        };
        let send = tokio::time::timeout(option.hard_ttl(), client.install_snapshot(request));
        let reply = tokio::select! {
            closed = &mut cancel => return Err(StreamingError::Closed(closed)),
            r = send => r,
        }
        .map_err(|_| unreachable("timed out"))?
        .map_err(|s| Unreachable::new(&s))?;
        let result: Result<SnapshotResponse<NodeId>, Fatal<NodeId>> =
            serde_json::from_slice(&reply.into_inner().json).map_err(|e| NetworkError::new(&e))?;
        result.map_err(|e| StreamingError::RemoteError(RemoteError::new(self.target, e)))
    }
}

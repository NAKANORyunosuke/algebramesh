//! Scheduler service (design §4.1, §7): the public gRPC API, the job queue and
//! placement, agent heartbeats, and metadata in SQLite plus a file artifact store.

pub mod blobs;
pub mod cluster;
pub mod config;
pub mod fault;
pub mod quota;
pub mod scheduler;
pub mod service;
pub mod store;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use algebramesh_rpc::pb::agent_gateway_server::AgentGatewayServer;
use algebramesh_rpc::pb::replication_server::ReplicationServer;
use algebramesh_rpc::pb::scheduler_server::SchedulerServer;
use algebramesh_rpc::{MAX_MESSAGE_BYTES, TokenCheck, server_tls};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::service::interceptor::InterceptedService;
use tonic::transport::Server;
use tonic::transport::server::{Connected, TcpConnectInfo};

pub use cluster::{ClusterConfig, Member, Node, SNAPSHOT_CHUNK_BYTES};
pub use config::ServerConfig;
pub use scheduler::{Scheduler, SchedulerOptions};
pub use service::AgentAuth;
pub use store::Store;

#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    #[error("configuration: {0}")]
    Config(String),
    #[error(transparent)]
    Scheduler(#[from] scheduler::SchedulerError),
    #[error(transparent)]
    Store(#[from] store::StoreError),
    #[error("transport: {0}")]
    Transport(#[from] tonic::transport::Error),
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    /// Raft stopped after an error of its storage (a full disk, a failed
    /// write): the scheduler must be started again (Issue #37).
    #[error("raft stopped: {0}; start the scheduler again once the cause is removed")]
    RaftStopped(String),
}

/// TLS identity and token sets of a running server.
pub struct Security {
    pub cert_pem: Vec<u8>,
    pub key_pem: Vec<u8>,
    pub client_tokens: Vec<String>,
    pub agent_tokens: Vec<String>,
    /// CA of the agent client certificates. When set, every agent must present a
    /// certificate signed by it, and acts only for the agent registered with it.
    pub agent_ca_pem: Option<Vec<u8>>,
    /// Agent certificates refused despite a valid signature (lowercase hex).
    pub revoked_agent_serials: Vec<String>,
    /// Tokens of the other schedulers of the cluster for the replication API;
    /// empty without a cluster.
    pub peer_tokens: Vec<String>,
}

/// Time open calls get to finish after a stop.
const GRACE: Duration = Duration::from_secs(3);

/// Serves a single scheduler (no peer) on `listener` until `shutdown` resolves.
pub async fn serve(
    scheduler: Arc<Scheduler>,
    security: Security,
    listener: TcpListener,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<(), ServeError> {
    serve_node(Node::single(scheduler), security, listener, shutdown).await
}

/// Serves the client, agent and replication APIs of `node` until `shutdown`
/// resolves, then stops Raft.
pub async fn serve_node(
    node: Arc<Node>,
    security: Security,
    listener: TcpListener,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<(), ServeError> {
    if security.client_tokens.is_empty() || security.agent_tokens.is_empty() {
        return Err(ServeError::Config(
            "client and agent tokens are required".into(),
        ));
    }
    let sweeper = {
        let node = node.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(500));
            loop {
                tick.tick().await;
                if let Ok(scheduler) = node.scheduler()
                    && let Err(e) = scheduler.sweep()
                {
                    tracing::error!("heartbeat sweep failed: {e}");
                    if matches!(e, scheduler::SchedulerError::Store(_)) {
                        node.store_failed(&scheduler);
                    }
                }
            }
        })
    };
    // Stop on `shutdown`. Open calls (agents long-poll for up to 20 s) get GRACE
    // to finish; then the connections are dropped.
    let (cut, connections) = watch::channel(false);
    let incoming = TcpListenerStream::new(listener)
        .map(move |accepted| accepted.map(|io| Accepted::new(io, connections.clone())));
    let (stopping, stopped) = tokio::sync::oneshot::channel::<()>();
    let stop = async move {
        shutdown.await;
        let _ = stopping.send(());
    };
    let grace = async move {
        if stopped.await.is_ok() {
            tokio::time::sleep(GRACE).await;
        } else {
            std::future::pending::<()>().await;
        }
    };
    let server = Server::builder()
        .tls_config(server_tls(
            &security.cert_pem,
            &security.key_pem,
            security.agent_ca_pem.as_deref(),
        ))?
        .add_service(InterceptedService::new(
            SchedulerServer::new(service::ClientService(node.clone()))
                .max_decoding_message_size(MAX_MESSAGE_BYTES)
                .max_encoding_message_size(MAX_MESSAGE_BYTES),
            TokenCheck::new(security.client_tokens),
        ))
        .add_service(InterceptedService::new(
            AgentGatewayServer::new(service::AgentService {
                node: node.clone(),
                auth: Arc::new(AgentAuth {
                    require_certificate: security.agent_ca_pem.is_some(),
                    revoked_serials: security
                        .revoked_agent_serials
                        .iter()
                        .map(|s| s.trim().to_ascii_lowercase().replace(':', ""))
                        .collect(),
                }),
            })
            .max_decoding_message_size(MAX_MESSAGE_BYTES)
            .max_encoding_message_size(MAX_MESSAGE_BYTES),
            TokenCheck::new(security.agent_tokens),
        ))
        .add_service(InterceptedService::new(
            ReplicationServer::new(cluster::ReplicationService(node.clone()))
                .max_decoding_message_size(cluster::REPLICATION_MAX_MESSAGE_BYTES)
                .max_encoding_message_size(cluster::REPLICATION_MAX_MESSAGE_BYTES),
            TokenCheck::new(security.peer_tokens),
        ))
        .serve_with_incoming_shutdown(incoming, stop);
    // Raft stops for good after an error of its storage; the process then
    // ends with that error instead of answering as a scheduler that cannot
    // commit anything (Issue #37).
    let failed = node.clone();
    let result = tokio::select! {
        r = server => r.map_err(ServeError::from),
        _ = grace => Ok(()),
        e = failed.raft_failure() => Err(ServeError::RaftStopped(e)),
    };
    // tonic serves each connection in a task of its own, which outlives the
    // server and holds the services, and through them the node. The
    // connections still open are cut and their tasks waited for, so that once
    // this returns no connection holds the node and its open databases: a
    // scheduler started again over the same files in the same process can
    // then replace them (Windows refuses to replace an open file).
    cut.send_replace(true);
    if tokio::time::timeout(GRACE, cut.closed()).await.is_err() {
        tracing::warn!("connections still open after the server stopped");
    }
    sweeper.abort();
    node.shutdown().await;
    if let Err(ServeError::RaftStopped(e)) = &result {
        tracing::error!(error = %e, "raft stopped after a storage error");
    }
    result
}

/// A connection accepted by [`serve_node`]. Once the server stops, its reads
/// and writes fail, so that the connection ends even if the peer keeps it
/// open or a call on it does not finish. It holds a receiver of `cut` until it
/// is dropped, so that the server can wait for every connection to end.
struct Accepted {
    io: TcpStream,
    cut: Pin<Box<dyn Future<Output = ()> + Send>>,
    is_cut: bool,
    _open: watch::Receiver<bool>,
}

impl Accepted {
    fn new(io: TcpStream, cut: watch::Receiver<bool>) -> Self {
        let mut wait = cut.clone();
        Self {
            io,
            // Also cut if the server is gone.
            cut: Box::pin(async move {
                let _ = wait.wait_for(|cut| *cut).await;
            }),
            is_cut: false,
            _open: cut,
        }
    }

    fn check(&mut self, cx: &mut Context<'_>) -> std::io::Result<()> {
        if !self.is_cut && self.cut.as_mut().poll(cx).is_ready() {
            self.is_cut = true;
        }
        if self.is_cut {
            return Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionAborted,
                "the server stopped",
            ));
        }
        Ok(())
    }
}

impl Connected for Accepted {
    type ConnectInfo = TcpConnectInfo;

    fn connect_info(&self) -> Self::ConnectInfo {
        self.io.connect_info()
    }
}

impl AsyncRead for Accepted {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        if let Err(e) = this.check(cx) {
            return Poll::Ready(Err(e));
        }
        Pin::new(&mut this.io).poll_read(cx, buf)
    }
}

impl AsyncWrite for Accepted {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        if let Err(e) = this.check(cx) {
            return Poll::Ready(Err(e));
        }
        Pin::new(&mut this.io).poll_write(cx, buf)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        if let Err(e) = this.check(cx) {
            return Poll::Ready(Err(e));
        }
        Pin::new(&mut this.io).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.io.is_write_vectored()
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        if let Err(e) = this.check(cx) {
            return Poll::Ready(Err(e));
        }
        Pin::new(&mut this.io).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().io).poll_shutdown(cx)
    }
}

/// Starts the scheduler described by `config` and serves until Ctrl-C.
pub async fn run(config: ServerConfig) -> Result<(), ServeError> {
    let security = config.security()?;
    store::remove_stale_files(&config.data_dir);
    let store = Store::open(
        Some(&config.data_dir.join("metadata.sqlite3")),
        Some(config.data_dir.join("artifacts")),
    )?;
    cluster::check_version(&store)?;
    let node = match config.cluster_config()? {
        Some(cluster) => Node::start_cluster(store, config.scheduler_options(), cluster).await?,
        None => Node::single(Arc::new(Scheduler::new(store, config.scheduler_options())?)),
    };
    let listener = TcpListener::bind(config.listen).await?;
    tracing::info!(
        addr = %listener.local_addr()?,
        data_dir = %config.data_dir.display(),
        role = %node.status().role,
        "scheduler listening"
    );
    serve_node(node, security, listener, shutdown_signal()).await
}

/// Resolves on Ctrl-C and, on Unix, on SIGTERM, which `docker stop` and systemd
/// send. Without the latter a process running as PID 1 in a container ignores
/// SIGTERM and is killed only after the stop timeout.
pub async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = terminate.recv() => {}
                }
                return;
            }
            Err(e) => tracing::warn!(error = %e, "cannot listen for SIGTERM"),
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

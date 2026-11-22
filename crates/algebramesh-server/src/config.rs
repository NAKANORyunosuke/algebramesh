//! `server.toml`:
//!
//! ```toml
//! listen = "0.0.0.0:50051"
//! data_dir = "/var/lib/algebramesh"
//! tls_cert = "/etc/algebramesh/server.pem"
//! tls_key = "/etc/algebramesh/server-key.pem"
//! client_token_file = "/etc/algebramesh/client.token"
//! agent_token_file = "/etc/algebramesh/agent.token"
//! # Optional: agents must present a client certificate signed by this CA
//! # (`algebramesh certs agent`), and may act only for themselves.
//! agent_ca_file = "/etc/algebramesh/ca.pem"
//! revoked_agent_serials = []    # serial numbers printed by `certs agent`
//! heartbeat_interval_ms = 2000
//! heartbeat_timeout_ms = 15000
//! max_attempts = 3
//! max_queued_jobs = 10000       # a submission beyond it: RESOURCE_EXHAUSTED
//!
//! # Optional: the blob store (ADR 0021). The values shown are the defaults.
//! blob_dir = "/var/lib/algebramesh/blobs"   # default: <data_dir>/blobs
//! max_blob_bytes = 268435456              # one blob at most (256 MiB)
//! max_blob_store_bytes = 0                # all blobs at most; 0: no bound
//! blob_min_free_bytes = 1073741824        # free space the store leaves (1 GiB)
//! blob_orphan_grace_secs = 86400          # unreferenced blobs kept this long
//! blob_replication_bytes_per_second = 0   # bandwidth of pushes; 0: no limit
//! blob_upload_bytes_per_token_per_hour = 0  # uploads of a client token; 0: no bound
//!
//! # Optional: a Raft cluster of schedulers (ADR 0013). Every member uses the
//! # same CA, tokens and member list; `node_id` says which member this one is.
//! [raft]
//! node_id = 1
//! peer_token_file = "/etc/algebramesh/peer.token"
//! ca_file = "/etc/algebramesh/ca.pem"
//! election_timeout_ms = 1500
//! join = false                  # true: wait to be added to a running cluster
//! [[raft.members]]
//! id = 1
//! name = "sched-a"
//! url = "https://sched-a.internal:50051"
//! [[raft.members]]
//! id = 2
//! name = "sched-b"
//! url = "https://sched-b.internal:50051"
//! [[raft.members]]
//! id = 3
//! name = "sched-c"
//! url = "https://sched-c.internal:50051"
//! ```
//!
//! Relative paths are resolved against the directory of the configuration file.
//! Token files hold one token per line; empty lines and `#` comments are ignored.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use crate::cluster::{ClusterConfig, Member};
use crate::{SchedulerOptions, Security, ServeError};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    pub listen: SocketAddr,
    pub data_dir: PathBuf,
    pub tls_cert: PathBuf,
    pub tls_key: PathBuf,
    pub client_token_file: PathBuf,
    pub agent_token_file: PathBuf,
    #[serde(default)]
    pub agent_ca_file: Option<PathBuf>,
    #[serde(default)]
    pub revoked_agent_serials: Vec<String>,
    #[serde(default = "default_heartbeat_interval")]
    pub heartbeat_interval_ms: u64,
    #[serde(default = "default_heartbeat_timeout")]
    pub heartbeat_timeout_ms: u64,
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
    #[serde(default = "default_max_queued_jobs")]
    pub max_queued_jobs: u64,
    #[serde(default)]
    pub blob_dir: Option<PathBuf>,
    #[serde(default)]
    pub max_blob_bytes: Option<u64>,
    #[serde(default)]
    pub max_blob_store_bytes: Option<u64>,
    #[serde(default)]
    pub blob_min_free_bytes: Option<u64>,
    #[serde(default)]
    pub blob_orphan_grace_secs: Option<u64>,
    #[serde(default)]
    pub blob_replication_bytes_per_second: Option<u64>,
    #[serde(default)]
    pub blob_upload_bytes_per_token_per_hour: Option<u64>,
    #[serde(default)]
    pub raft: Option<RaftSection>,
    /// The primary/standby pair of ADR 0012, replaced by `[raft]`.
    #[serde(default)]
    pub ha: Option<toml::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RaftSection {
    /// This scheduler's `id` in `members`.
    pub node_id: u64,
    pub members: Vec<MemberSection>,
    pub peer_token_file: PathBuf,
    /// CA that signed the members' server certificates.
    pub ca_file: PathBuf,
    #[serde(default = "default_election_timeout")]
    pub election_timeout_ms: u64,
    /// Join an existing cluster: do not initialize one, and wait until the
    /// leader adds this scheduler (`algebramesh server add-member`).
    #[serde(default)]
    pub join: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemberSection {
    pub id: u64,
    /// Defaults to `node-<id>`.
    #[serde(default)]
    pub name: Option<String>,
    pub url: String,
    /// Name in the member's certificate; defaults to the host of `url`.
    #[serde(default)]
    pub server_name: Option<String>,
}

fn default_election_timeout() -> u64 {
    1500
}

/// The host of `https://host:port/...`.
pub fn url_host(url: &str) -> Option<String> {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = rest.split('/').next()?;
    let host = authority.rsplit_once(':').map_or(authority, |(h, _)| h);
    (!host.is_empty()).then(|| host.trim_matches(['[', ']']).to_owned())
}

fn default_heartbeat_interval() -> u64 {
    2000
}

fn default_heartbeat_timeout() -> u64 {
    15000
}

fn default_max_attempts() -> u32 {
    3
}

fn default_max_queued_jobs() -> u64 {
    crate::scheduler::DEFAULT_MAX_QUEUED_JOBS
}

pub fn read_tokens(path: &Path) -> Result<Vec<String>, ServeError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| ServeError::Config(format!("{}: {e}", path.display())))?;
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_owned)
        .collect())
}

impl ServerConfig {
    pub fn load(path: &Path) -> Result<Self, ServeError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| ServeError::Config(format!("{}: {e}", path.display())))?;
        let mut config: Self = toml::from_str(&text)
            .map_err(|e| ServeError::Config(format!("{}: {e}", path.display())))?;
        let base = path.parent().unwrap_or(Path::new("."));
        for p in [
            &mut config.data_dir,
            &mut config.tls_cert,
            &mut config.tls_key,
            &mut config.client_token_file,
            &mut config.agent_token_file,
        ]
        .into_iter()
        .chain(config.agent_ca_file.as_mut())
        .chain(config.blob_dir.as_mut())
        .chain(
            config
                .raft
                .as_mut()
                .into_iter()
                .flat_map(|r| [&mut r.peer_token_file, &mut r.ca_file]),
        ) {
            if p.is_relative() {
                *p = base.join(&*p);
            }
        }
        if config.ha.is_some() {
            return Err(ServeError::Config(format!(
                "{}: [ha] (a primary and a standby, ADR 0012) was replaced by [raft] (ADR 0013)",
                path.display()
            )));
        }
        if config.max_queued_jobs == 0 {
            return Err(ServeError::Config(
                "max_queued_jobs must be at least 1".into(),
            ));
        }
        if config.max_blob_bytes == Some(0) {
            return Err(ServeError::Config(
                "max_blob_bytes must be at least 1".into(),
            ));
        }
        if config.heartbeat_timeout_ms <= config.heartbeat_interval_ms {
            return Err(ServeError::Config(
                "heartbeat_timeout_ms must exceed heartbeat_interval_ms".into(),
            ));
        }
        Ok(config)
    }

    pub fn security(&self) -> Result<Security, ServeError> {
        let read = |p: &Path| {
            std::fs::read(p).map_err(|e| ServeError::Config(format!("{}: {e}", p.display())))
        };
        Ok(Security {
            cert_pem: read(&self.tls_cert)?,
            key_pem: read(&self.tls_key)?,
            client_tokens: read_tokens(&self.client_token_file)?,
            agent_tokens: read_tokens(&self.agent_token_file)?,
            agent_ca_pem: self.agent_ca_file.as_deref().map(read).transpose()?,
            revoked_agent_serials: self.revoked_agent_serials.clone(),
            peer_tokens: match &self.raft {
                Some(raft) => read_tokens(&raft.peer_token_file)?,
                None => vec![],
            },
        })
    }

    /// The cluster settings, with the files read.
    pub fn cluster_config(&self) -> Result<Option<ClusterConfig>, ServeError> {
        let Some(raft) = &self.raft else {
            return Ok(None);
        };
        let token = read_tokens(&raft.peer_token_file)?
            .into_iter()
            .next()
            .ok_or_else(|| ServeError::Config("raft.peer_token_file is empty".into()))?;
        let ca_pem = std::fs::read(&raft.ca_file)
            .map_err(|e| ServeError::Config(format!("{}: {e}", raft.ca_file.display())))?;
        let mut members = std::collections::BTreeMap::new();
        for m in &raft.members {
            if !m.url.starts_with("https://") {
                return Err(ServeError::Config(format!(
                    "raft member {}: the URL must start with https://",
                    m.id
                )));
            }
            let domain = m
                .server_name
                .clone()
                .or_else(|| url_host(&m.url))
                .ok_or_else(|| ServeError::Config(format!("no host in {}", m.url)))?;
            let member = Member {
                name: m.name.clone().unwrap_or_else(|| format!("node-{}", m.id)),
                url: m.url.clone(),
                domain,
            };
            if members.insert(m.id, member).is_some() {
                return Err(ServeError::Config(format!(
                    "raft member id {} appears twice",
                    m.id
                )));
            }
        }
        if !members.contains_key(&raft.node_id) {
            return Err(ServeError::Config(format!(
                "raft.node_id {} is not among raft.members",
                raft.node_id
            )));
        }
        Ok(Some(ClusterConfig {
            node_id: raft.node_id,
            members,
            ca_pem,
            peer_token: token,
            join: raft.join,
            election_timeout: Duration::from_millis(raft.election_timeout_ms),
            data_dir: Some(self.data_dir.clone()),
            snapshot_chunk_bytes: crate::cluster::SNAPSHOT_CHUNK_BYTES,
            features: crate::cluster::supported_features(),
        }))
    }

    pub fn scheduler_options(&self) -> SchedulerOptions {
        let mut blobs = crate::blobs::BlobOptions {
            dir: Some(
                self.blob_dir
                    .clone()
                    .unwrap_or_else(|| self.data_dir.join("blobs")),
            ),
            ..Default::default()
        };
        if let Some(n) = self.max_blob_bytes {
            blobs.max_blob_bytes = n;
        }
        if let Some(n) = self.max_blob_store_bytes {
            blobs.max_store_bytes = n;
        }
        if let Some(n) = self.blob_min_free_bytes {
            blobs.min_free_bytes = n;
        }
        if let Some(n) = self.blob_orphan_grace_secs {
            blobs.orphan_grace = Duration::from_secs(n);
        }
        if let Some(n) = self.blob_replication_bytes_per_second {
            blobs.replication_bytes_per_second = n;
        }
        if let Some(n) = self.blob_upload_bytes_per_token_per_hour {
            blobs.upload_bytes_per_token_per_hour = n;
        }
        SchedulerOptions {
            heartbeat_interval: Duration::from_millis(self.heartbeat_interval_ms),
            heartbeat_timeout: Duration::from_millis(self.heartbeat_timeout_ms),
            max_attempts: self.max_attempts,
            max_queued_jobs: self.max_queued_jobs,
            blobs,
            ..SchedulerOptions::default()
        }
    }
}

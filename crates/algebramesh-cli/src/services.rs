//! `server start`, `server status`, `server promote`, `server members`,
//! `server add-member`, `server remove-member`, `worker start`, `certs generate`,
//! `certs agent` and `certs server`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use algebramesh_agent::{Agent, AgentConfig};
use algebramesh_server::ServerConfig;
use clap::Subcommand;
use tokio_util::sync::CancellationToken;

#[derive(Subcommand)]
pub enum ServerCommand {
    /// Start the scheduler.
    Start {
        #[arg(long, default_value = "config/server.toml")]
        config: PathBuf,
    },
    /// Show the role, term, leader and last applied change of a scheduler (ADR 0013).
    Status(PeerArgs),
    /// Start an election on a scheduler of a cluster now.
    Promote(PeerArgs),
    /// List the members of a cluster (ADR 0013).
    Members(PeerArgs),
    /// Add a scheduler to a running cluster: it becomes a learner, catches up with
    /// the log, then becomes a voter. Start it first with `join = true`.
    AddMember {
        #[command(flatten)]
        peer: PeerArgs,
        /// Member number; unique in the cluster.
        #[arg(long)]
        id: u64,
        /// Member name, shown by `server status` and `server members`.
        #[arg(long)]
        name: String,
        /// URL at which the other schedulers reach the new one.
        #[arg(long)]
        url: String,
        /// Name in the new scheduler's certificate (defaults to the host of --url).
        #[arg(long)]
        member_server_name: Option<String>,
    },
    /// Remove a scheduler from a running cluster. Stop it afterwards.
    RemoveMember {
        #[command(flatten)]
        peer: PeerArgs,
        /// Member number.
        #[arg(long)]
        id: u64,
    },
}

/// A scheduler reached through the replication API, with the peer token.
#[derive(clap::Args)]
pub struct PeerArgs {
    /// Scheduler URL. `members`, `add-member` and `remove-member` take several,
    /// separated by commas, and send the change to the leader among them.
    #[arg(long)]
    server: String,
    /// PEM file of the CA that signed the scheduler certificate.
    #[arg(long)]
    ca: PathBuf,
    /// File holding the peer token (`peer_token_file` of server.toml).
    #[arg(long)]
    token_file: PathBuf,
    /// Name expected in the scheduler certificate (defaults to the URL host).
    #[arg(long)]
    server_name: Option<String>,
}

type ReplicationClient = algebramesh_rpc::pb::replication_client::ReplicationClient<
    tonic::service::interceptor::InterceptedService<
        tonic::transport::Channel,
        algebramesh_rpc::TokenInterceptor,
    >,
>;

impl PeerArgs {
    fn urls(&self) -> Vec<String> {
        self.server
            .split(',')
            .map(str::trim)
            .filter(|u| !u.is_empty())
            .map(str::to_owned)
            .collect()
    }

    /// Runs `call` on each URL in turn until one is not refused for not being the
    /// leader; returns that answer.
    async fn on_leader<T, F, Fut>(&self, call: F) -> Result<T, String>
    where
        F: Fn(ReplicationClient) -> Fut,
        Fut: std::future::Future<Output = Result<tonic::Response<T>, tonic::Status>>,
    {
        let mut last = String::from("no scheduler URL");
        for url in self.urls() {
            let client = match self.client_for(&url).await {
                Ok(c) => c,
                Err(e) => {
                    last = e;
                    continue;
                }
            };
            match call(client).await {
                Ok(r) => return Ok(r.into_inner()),
                Err(s)
                    if s.code() == tonic::Code::FailedPrecondition
                        && s.message().contains("is not the leader") =>
                {
                    last = format!("{url}: {}", s.message());
                }
                Err(s) => return Err(format!("{url}: {}", s.message())),
            }
        }
        Err(last)
    }

    async fn client(&self) -> Result<ReplicationClient, String> {
        let url = self.urls().into_iter().next().unwrap_or_default();
        self.client_for(&url).await
    }

    async fn client_for(&self, server: &str) -> Result<ReplicationClient, String> {
        let ca = std::fs::read(&self.ca).map_err(|e| format!("{}: {e}", self.ca.display()))?;
        let token = algebramesh_server::config::read_tokens(&self.token_file)
            .map_err(|e| e.to_string())?
            .into_iter()
            .next()
            .ok_or_else(|| format!("{}: no token", self.token_file.display()))?;
        let domain = match &self.server_name {
            Some(d) => d.clone(),
            None => server
                .split_once("://")
                .map_or(server, |(_, r)| r)
                .split(['/', ':'])
                .next()
                .unwrap_or_default()
                .to_owned(),
        };
        let endpoint = tonic::transport::Endpoint::from_shared(server.to_owned())
            .map_err(|e| e.to_string())?
            .tls_config(algebramesh_rpc::client_tls(&ca, &domain))
            .map_err(|e| e.to_string())?;
        let channel = algebramesh_rpc::connect(&endpoint, std::time::Duration::from_secs(10))
            .await
            .map_err(|e| format!("cannot connect to {server}: {e}"))?;
        let token = algebramesh_rpc::TokenInterceptor::new(&token).map_err(|e| e.to_string())?;
        Ok(
            algebramesh_rpc::pb::replication_client::ReplicationClient::with_interceptor(
                channel, token,
            ),
        )
    }
}

fn node_json(s: &algebramesh_rpc::pb::NodeStatus) -> serde_json::Value {
    serde_json::json!({
        "node_id": s.node_id, "role": s.role, "epoch": s.epoch, "last_seq": s.last_seq,
        "leader": s.leader, "serving": s.serving, "version": s.version,
        "features": s.features, "enabled_features": s.enabled_features,
        "blobs_under_replicated": s.blobs_under_replicated,
        "blobs_missing": s.blobs_missing,
        "blobs_quarantined": s.blobs_quarantined,
        "blob_free_bytes": s.blob_free_bytes,
        "blobs_stored": s.blobs_stored,
        "blob_bytes_stored": s.blob_bytes_stored,
        "blob_uploads_refused": s.blob_uploads_refused,
    })
}

fn members_json(m: &algebramesh_rpc::pb::ClusterMembers) -> serde_json::Value {
    serde_json::json!({
        "leader": m.leader,
        "epoch": m.epoch,
        "members": m.members.iter().map(|x| serde_json::json!({
            "id": x.id, "name": x.name, "url": x.url, "voter": x.voter,
        })).collect::<Vec<_>>(),
    })
}

#[derive(Subcommand)]
pub enum WorkerCommand {
    /// Start a worker agent.
    Start {
        #[arg(long, default_value = "config/worker.toml")]
        config: PathBuf,
        /// Client certificate of this agent; overrides `tls_cert` of the file.
        #[arg(long, requires = "tls_key")]
        tls_cert: Option<PathBuf>,
        /// Private key of the client certificate; overrides `tls_key`.
        #[arg(long, requires = "tls_cert")]
        tls_key: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
pub enum CertsCommand {
    /// Write ca.pem, ca-key.pem, server.pem, server-key.pem, client.token and
    /// agent.token.
    Generate {
        /// Output directory.
        #[arg(long)]
        out: PathBuf,
        /// DNS names or IP addresses of the scheduler (repeatable).
        #[arg(long = "host", required = true)]
        hosts: Vec<String>,
        /// Replace existing files.
        #[arg(long)]
        force: bool,
    },
    /// Issue a client certificate for one agent: <name>.pem and <name>-key.pem,
    /// signed with ca.pem and ca-key.pem. Prints the serial number, which
    /// `revoked_agent_serials` of server.toml takes to revoke the certificate.
    Agent {
        /// Directory with ca.pem and ca-key.pem (from `certs generate`).
        #[arg(long)]
        ca_dir: PathBuf,
        /// Agent name; the scheduler accepts this agent only under this name.
        #[arg(long)]
        name: String,
        /// Output directory.
        #[arg(long)]
        out: PathBuf,
        /// Validity in days.
        #[arg(long, default_value_t = 365)]
        days: u32,
        /// Replace existing files.
        #[arg(long)]
        force: bool,
    },
    /// Issue a server certificate for one more scheduler (`<name>.pem`,
    /// `<name>-key.pem`), signed with ca.pem and ca-key.pem: for a scheduler added
    /// to a cluster with `server add-member`.
    Server {
        /// Directory with ca.pem and ca-key.pem (from `certs generate`).
        #[arg(long)]
        ca_dir: PathBuf,
        /// DNS names or IP addresses of the scheduler (repeatable).
        #[arg(long = "host", required = true)]
        hosts: Vec<String>,
        /// Output directory.
        #[arg(long)]
        out: PathBuf,
        /// Base name of the files.
        #[arg(long, default_value = "server")]
        name: String,
        /// Validity in days.
        #[arg(long, default_value_t = 3650)]
        days: u32,
        /// Replace existing files.
        #[arg(long)]
        force: bool,
    },
}

pub async fn server(command: ServerCommand) -> Result<ExitCode, String> {
    match command {
        ServerCommand::Start { config } => {
            let config = ServerConfig::load(&config).map_err(|e| e.to_string())?;
            algebramesh_server::run(config)
                .await
                .map_err(|e| e.to_string())?;
        }
        ServerCommand::Status(peer) => {
            let status = peer
                .client()
                .await?
                .status(algebramesh_rpc::pb::Empty {})
                .await
                .map_err(|e| e.message().to_owned())?
                .into_inner();
            println!("{}", node_json(&status));
        }
        ServerCommand::Promote(peer) => {
            let status = peer
                .client()
                .await?
                .promote(algebramesh_rpc::pb::Empty {})
                .await
                .map_err(|e| e.message().to_owned())?
                .into_inner();
            println!("{}", node_json(&status));
        }
        ServerCommand::Members(peer) => {
            let members = peer
                .client()
                .await?
                .members(algebramesh_rpc::pb::Empty {})
                .await
                .map_err(|e| e.message().to_owned())?
                .into_inner();
            println!("{}", members_json(&members));
        }
        ServerCommand::AddMember {
            peer,
            id,
            name,
            url,
            member_server_name,
        } => {
            let request = algebramesh_rpc::pb::AddMemberRequest {
                id,
                name,
                url,
                server_name: member_server_name.unwrap_or_default(),
            };
            let members = peer
                .on_leader(|mut c| {
                    let request = request.clone();
                    async move { c.add_member(request).await }
                })
                .await?;
            println!("{}", members_json(&members));
        }
        ServerCommand::RemoveMember { peer, id } => {
            let members = peer
                .on_leader(|mut c| async move {
                    c.remove_member(algebramesh_rpc::pb::RemoveMemberRequest { id })
                        .await
                })
                .await?;
            println!("{}", members_json(&members));
        }
    }
    Ok(ExitCode::SUCCESS)
}

pub async fn worker(command: WorkerCommand) -> Result<ExitCode, String> {
    let WorkerCommand::Start {
        config,
        tls_cert,
        tls_key,
    } = command;
    let mut config = AgentConfig::load(&config).map_err(|e| e.to_string())?;
    if let (Some(cert), Some(key)) = (tls_cert, tls_key) {
        config.set_client_certificate(cert, key);
    }
    let agent = Agent::connect(
        config.connection().map_err(|e| e.to_string())?,
        config.name(),
        config.host(),
        config.backend_slots(),
    )
    .await
    .map_err(|e| e.to_string())?;
    let stop = CancellationToken::new();
    let trigger = stop.clone();
    tokio::spawn(async move {
        algebramesh_server::shutdown_signal().await;
        tracing::info!("stopping the agent");
        trigger.cancel();
    });
    agent.run(stop).await.map_err(|e| e.to_string())?;
    Ok(ExitCode::SUCCESS)
}

fn write_new(path: &Path, contents: &str, force: bool) -> Result<(), String> {
    if path.exists() && !force {
        return Err(format!(
            "{} exists; pass --force to replace it",
            path.display()
        ));
    }
    std::fs::write(path, contents).map_err(|e| format!("{}: {e}", path.display()))?;
    // Private keys and tokens: readable by the owner only.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.ends_with("-key.pem") || name.ends_with(".token") {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| format!("{}: {e}", path.display()))?;
        }
    }
    Ok(())
}

/// Writes all `files` into `out`, or none if one exists and `force` is not set.
fn write_all(out: &Path, files: &[(String, String)], force: bool) -> Result<(), String> {
    std::fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;
    for (name, _) in files {
        let path = out.join(name);
        if path.exists() && !force {
            return Err(format!(
                "{} exists; pass --force to replace it",
                path.display()
            ));
        }
    }
    for (name, contents) in files {
        write_new(&out.join(name), contents, force)?;
    }
    Ok(())
}

pub fn certs(command: CertsCommand) -> Result<ExitCode, String> {
    match command {
        CertsCommand::Generate { out, hosts, force } => {
            let pki = algebramesh_rpc::generate_dev_pki(&hosts).map_err(|e| e.to_string())?;
            let token = || algebramesh_rpc::random_token().map_err(|e| e.to_string());
            let files = [
                ("ca.pem", pki.ca_cert_pem),
                ("ca-key.pem", pki.ca_key_pem),
                ("server.pem", pki.server_cert_pem),
                ("server-key.pem", pki.server_key_pem),
                ("client.token", token()? + "\n"),
                ("agent.token", token()? + "\n"),
            ]
            .map(|(n, c)| (n.to_owned(), c));
            write_all(&out, &files, force)?;
            eprintln!(
                "algebramesh: wrote {} for {}; keep ca-key.pem offline and server-key.pem and the tokens private",
                files.map(|(n, _)| n).join(", "),
                hosts.join(", ")
            );
        }
        CertsCommand::Agent {
            ca_dir,
            name,
            out,
            days,
            force,
        } => {
            let read = |file: &str| {
                let path = ca_dir.join(file);
                std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))
            };
            let cert = algebramesh_rpc::issue_agent_certificate(
                &read("ca.pem")?,
                &read("ca-key.pem")?,
                &name,
                days,
            )
            .map_err(|e| e.to_string())?;
            let files = [
                (format!("{name}.pem"), cert.cert_pem),
                (format!("{name}-key.pem"), cert.key_pem),
            ];
            write_all(&out, &files, force)?;
            eprintln!(
                "algebramesh: wrote {name}.pem and {name}-key.pem, valid for {days} days; keep {name}-key.pem private"
            );
            println!("{}", cert.serial);
        }
        CertsCommand::Server {
            ca_dir,
            hosts,
            out,
            name,
            days,
            force,
        } => {
            if !algebramesh_rpc::valid_agent_name(&name) {
                return Err(format!("invalid file name {name}"));
            }
            let read = |file: &str| {
                let path = ca_dir.join(file);
                std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))
            };
            let (cert, key) = algebramesh_rpc::issue_server_certificate(
                &read("ca.pem")?,
                &read("ca-key.pem")?,
                &hosts,
                days,
            )
            .map_err(|e| e.to_string())?;
            let files = [
                (format!("{name}.pem"), cert),
                (format!("{name}-key.pem"), key),
            ];
            write_all(&out, &files, force)?;
            eprintln!(
                "algebramesh: wrote {name}.pem and {name}-key.pem for {}, valid for {days} days; keep {name}-key.pem private",
                hosts.join(", ")
            );
        }
    }
    Ok(ExitCode::SUCCESS)
}

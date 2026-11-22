//! `worker.toml`:
//!
//! ```toml
//! server = "https://scheduler.internal:50051"   # or "https://a:50051,https://b:50051"
//! ca_file = "ca.pem"
//! token_file = "agent.token"
//! name = "worker-a"          # defaults to the certificate name, else the host name
//! # Client certificate for mutual TLS (`algebramesh certs agent --name worker-a`).
//! # The scheduler then accepts only the name in the certificate.
//! tls_cert = "worker-a.pem"
//! tls_key = "worker-a-key.pem"
//!
//! [[backends]]
//! kind = "asir"
//! slots = 2                  # one OpenXM session per slot
//! ox_launch = "/OpenXM/lib/asir/ox_launch"
//! ox_asir = "/OpenXM/bin/ox_asir"
//! build = "ghcr.io/nakanoryunosuke/risa-asir-container@sha256:..."
//!
//! [[backends]]
//! kind = "macaulay2"
//! slots = 1
//! m2 = "M2"
//! driver = "/opt/algebramesh/drivers/macaulay2/algebramesh.m2"
//! build = "debian:trixie macaulay2 1.24.11+ds-5+b1"
//!
//! [[backends]]
//! kind = "oscar"
//! slots = 1                  # one Julia process per slot, kept running
//! julia = "julia"
//! driver = "/opt/algebramesh/drivers/oscar/algebramesh.jl"
//! project = "/opt/algebramesh-oscar"
//! ```
//!
//! Relative paths are resolved against the directory of the configuration file.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use algebramesh_backend_asir::{AsirBackend, AsirConfig, AsirEndpoint};
use algebramesh_backend_macaulay2::{Macaulay2Backend, Macaulay2Config};
use algebramesh_backend_oscar::{OscarBackend, OscarConfig};
use algebramesh_openxm_client::{LocalLauncher, SessionOptions};
use serde::Deserialize;

use crate::{AgentError, BackendSlot, ClientIdentity, Connection};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    pub server: String,
    pub ca_file: PathBuf,
    pub token_file: PathBuf,
    /// Name expected in the scheduler certificate; defaults to the URL host.
    #[serde(default)]
    pub server_name: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub tls_cert: Option<PathBuf>,
    #[serde(default)]
    pub tls_key: Option<PathBuf>,
    pub backends: Vec<BackendConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum BackendConfig {
    Asir {
        #[serde(default = "one")]
        slots: u32,
        ox_launch: PathBuf,
        ox_asir: PathBuf,
        #[serde(default)]
        build: String,
        #[serde(default = "default_accept_timeout")]
        accept_timeout_secs: u64,
    },
    Macaulay2 {
        #[serde(default = "one")]
        slots: u32,
        #[serde(default = "default_m2")]
        m2: PathBuf,
        driver: PathBuf,
        #[serde(default)]
        build: String,
    },
    Oscar {
        #[serde(default = "one")]
        slots: u32,
        #[serde(default = "default_julia")]
        julia: PathBuf,
        driver: PathBuf,
        #[serde(default)]
        project: Option<PathBuf>,
        #[serde(default = "default_startup")]
        startup_timeout_secs: u64,
        #[serde(default)]
        build: String,
    },
}

fn default_julia() -> PathBuf {
    "julia".into()
}

fn default_startup() -> u64 {
    300
}

fn default_m2() -> PathBuf {
    "M2".into()
}

fn one() -> u32 {
    1
}

fn default_accept_timeout() -> u64 {
    60
}

fn host_name() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .ok()
        .filter(|h| !h.is_empty())
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|s| s.trim().to_owned())
        })
        .unwrap_or_else(|| "unknown-host".into())
}

pub(crate) fn url_host(url: &str) -> Option<String> {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = rest.split('/').next()?;
    let host = authority.rsplit_once(':').map_or(authority, |(h, _)| h);
    (!host.is_empty()).then(|| host.trim_matches(['[', ']']).to_owned())
}

impl AgentConfig {
    pub fn load(path: &Path) -> Result<Self, AgentError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| AgentError::Config(format!("{}: {e}", path.display())))?;
        let mut config: Self = toml::from_str(&text)
            .map_err(|e| AgentError::Config(format!("{}: {e}", path.display())))?;
        let base = path.parent().unwrap_or(Path::new("."));
        for p in [&mut config.ca_file, &mut config.token_file]
            .into_iter()
            .chain(config.tls_cert.as_mut())
            .chain(config.tls_key.as_mut())
        {
            if p.is_relative() {
                *p = base.join(&*p);
            }
        }
        if config.tls_cert.is_some() != config.tls_key.is_some() {
            return Err(AgentError::Config(
                "tls_cert and tls_key must be given together".into(),
            ));
        }
        Ok(config)
    }

    /// Replaces the client certificate of the file, for example to share one
    /// configuration file between agents with different certificates.
    pub fn set_client_certificate(&mut self, cert: PathBuf, key: PathBuf) {
        self.tls_cert = Some(cert);
        self.tls_key = Some(key);
    }

    fn client_identity(&self) -> Result<Option<ClientIdentity>, AgentError> {
        let (Some(cert), Some(key)) = (&self.tls_cert, &self.tls_key) else {
            return Ok(None);
        };
        let read = |p: &Path| {
            std::fs::read(p).map_err(|e| AgentError::Config(format!("{}: {e}", p.display())))
        };
        Ok(Some(ClientIdentity {
            cert_pem: read(cert)?,
            key_pem: read(key)?,
        }))
    }

    pub fn connection(&self) -> Result<Connection, AgentError> {
        let read = |p: &Path| {
            std::fs::read(p).map_err(|e| AgentError::Config(format!("{}: {e}", p.display())))
        };
        let token = String::from_utf8(read(&self.token_file)?)
            .map_err(|_| AgentError::Config("token file is not UTF-8".into()))?
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with('#'))
            .ok_or_else(|| AgentError::Config("token file is empty".into()))?
            .to_owned();
        Ok(Connection {
            url: self.server.clone(),
            ca_pem: read(&self.ca_file)?,
            // Empty: the host of each URL in `server`.
            domain: self.server_name.clone().unwrap_or_default(),
            token,
            client_identity: self.client_identity()?,
        })
    }

    /// The configured name, else the name in the client certificate, else the
    /// host name.
    pub fn name(&self) -> String {
        self.name
            .clone()
            .or_else(|| {
                let pem = std::fs::read(self.tls_cert.as_ref()?).ok()?;
                algebramesh_rpc::certificate_name(&pem)
            })
            .unwrap_or_else(host_name)
    }

    pub fn host(&self) -> String {
        self.host.clone().unwrap_or_else(host_name)
    }

    pub fn backend_slots(&self) -> Vec<BackendSlot> {
        self.backends
            .iter()
            .map(|b| match b {
                BackendConfig::Asir {
                    slots,
                    ox_launch,
                    ox_asir,
                    build,
                    accept_timeout_secs,
                } => BackendSlot {
                    backend: Arc::new(AsirBackend::new(AsirConfig {
                        endpoint: AsirEndpoint::SpawnLocal(LocalLauncher {
                            ox_launch: ox_launch.clone(),
                            server: ox_asir.clone(),
                        }),
                        accept_timeout: Duration::from_secs(*accept_timeout_secs),
                        build: Some(build.clone()).filter(|b| !b.is_empty()),
                        session: SessionOptions::default(),
                    })),
                    slots: *slots,
                    build: build.clone(),
                    backend_version: String::new(),
                },
                BackendConfig::Macaulay2 {
                    slots,
                    m2,
                    driver,
                    build,
                } => BackendSlot {
                    backend: Arc::new(Macaulay2Backend::new(Macaulay2Config {
                        m2: m2.clone(),
                        driver: driver.clone(),
                        work_dir: std::env::temp_dir(),
                        build: Some(build.clone()).filter(|b| !b.is_empty()),
                    })),
                    slots: *slots,
                    build: build.clone(),
                    backend_version: String::new(),
                },
                BackendConfig::Oscar {
                    slots,
                    julia,
                    driver,
                    project,
                    startup_timeout_secs,
                    build,
                } => BackendSlot {
                    backend: Arc::new(OscarBackend::new(OscarConfig {
                        julia: julia.clone(),
                        driver: driver.clone(),
                        project: project.clone(),
                        startup_timeout: Duration::from_secs(*startup_timeout_secs),
                        build: Some(build.clone()).filter(|b| !b.is_empty()),
                        ..OscarConfig::default()
                    })),
                    slots: *slots,
                    build: build.clone(),
                    backend_version: String::new(),
                },
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_example() {
        let config: AgentConfig = toml::from_str(
            r#"
            server = "https://scheduler:50051"
            ca_file = "ca.pem"
            token_file = "agent.token"
            name = "worker-a"
            [[backends]]
            kind = "asir"
            slots = 2
            ox_launch = "/OpenXM/lib/asir/ox_launch"
            ox_asir = "/OpenXM/bin/ox_asir"
            "#,
        )
        .unwrap();
        assert_eq!(config.name(), "worker-a");
        let slots = config.backend_slots();
        assert_eq!(slots.len(), 1);
        assert_eq!(slots[0].slots, 2);
        assert_eq!(slots[0].backend.name(), "asir");
        assert_eq!(url_host(&config.server).as_deref(), Some("scheduler"));
    }

    #[test]
    fn name_defaults_to_the_certificate_name() {
        let temp = tempfile::Builder::new()
            .prefix("am-agent-")
            .tempdir()
            .unwrap();
        let dir = temp.path().to_path_buf();
        std::fs::create_dir_all(&dir).unwrap();
        let pki = algebramesh_rpc::generate_dev_pki(&["localhost".into()]).unwrap();
        let cert = algebramesh_rpc::issue_agent_certificate(
            &pki.ca_cert_pem,
            &pki.ca_key_pem,
            "worker-x",
            1,
        )
        .unwrap();
        std::fs::write(dir.join("w.pem"), &cert.cert_pem).unwrap();
        std::fs::write(dir.join("w-key.pem"), &cert.key_pem).unwrap();
        std::fs::write(dir.join("ca.pem"), &pki.ca_cert_pem).unwrap();
        std::fs::write(dir.join("agent.token"), "t\n").unwrap();
        let file = dir.join("worker.toml");
        std::fs::write(
            &file,
            r#"
            server = "https://scheduler:50051"
            ca_file = "ca.pem"
            token_file = "agent.token"
            tls_cert = "w.pem"
            tls_key = "w-key.pem"
            [[backends]]
            kind = "asir"
            ox_launch = "a"
            ox_asir = "b"
            "#,
        )
        .unwrap();
        let config = AgentConfig::load(&file).unwrap();
        assert_eq!(config.name(), "worker-x");
        assert!(config.connection().unwrap().client_identity.is_some());

        std::fs::write(
            &file,
            r#"
            server = "https://scheduler:50051"
            ca_file = "ca.pem"
            token_file = "agent.token"
            tls_cert = "w.pem"
            [[backends]]
            kind = "asir"
            ox_launch = "a"
            ox_asir = "b"
            "#,
        )
        .unwrap();
        assert!(AgentConfig::load(&file).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rejects_unknown_backend_kind() {
        assert!(
            toml::from_str::<AgentConfig>(
                r#"
                server = "https://s:1"
                ca_file = "a"
                token_file = "b"
                [[backends]]
                kind = "shell"
                "#
            )
            .is_err()
        );
    }
}

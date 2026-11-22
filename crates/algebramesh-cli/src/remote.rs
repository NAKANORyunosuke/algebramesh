//! Commands that talk to the scheduler.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use algebramesh_client::{Client, ClientConfig, backends_json, status_json, workers_json};
use clap::Args;

use crate::local::{exit_for, print_json, read_spec};

#[derive(Args, Clone)]
pub struct RemoteArgs {
    /// Scheduler URL, e.g. https://scheduler.internal:50051.
    #[arg(long, env = "ALGEBRAMESH_SERVER")]
    server: Option<String>,
    /// PEM file of the CA that signed the scheduler certificate.
    #[arg(long, env = "ALGEBRAMESH_CA")]
    ca: Option<PathBuf>,
    /// File holding the client token.
    #[arg(long, env = "ALGEBRAMESH_TOKEN_FILE")]
    token_file: Option<PathBuf>,
    /// Name expected in the scheduler certificate (defaults to the URL host).
    #[arg(long, env = "ALGEBRAMESH_SERVER_NAME")]
    server_name: Option<String>,
}

impl RemoteArgs {
    pub fn is_set(&self) -> bool {
        self.server.is_some()
    }

    pub async fn connect(&self) -> Result<Client, String> {
        let server = self
            .server
            .clone()
            .ok_or("--server (or ALGEBRAMESH_SERVER) is required")?;
        let ca = self
            .ca
            .as_ref()
            .ok_or("--ca (or ALGEBRAMESH_CA) is required")?;
        let token_file = self
            .token_file
            .as_ref()
            .ok_or("--token-file (or ALGEBRAMESH_TOKEN_FILE) is required")?;
        let ca_pem = std::fs::read(ca).map_err(|e| format!("{}: {e}", ca.display()))?;
        let token = std::fs::read_to_string(token_file)
            .map_err(|e| format!("{}: {e}", token_file.display()))?
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with('#'))
            .ok_or_else(|| format!("{}: no token", token_file.display()))?
            .to_owned();
        Client::connect(ClientConfig {
            url: server,
            ca_pem,
            domain: self.server_name.clone(),
            token,
        })
        .await
        .map_err(|e| e.to_string())
    }
}

#[derive(clap::Subcommand)]
pub enum BlobCommand {
    /// Store the bytes of a file as a blob and print its marker,
    /// {"$blob": "sha256:...", "size": n}. A blob that no job refers to is
    /// removed after the scheduler's blob_orphan_grace_secs (24 hours by
    /// default).
    Put {
        file: PathBuf,
        /// Store the canonical JSON of the value in the file instead of its
        /// bytes, so that the marker can stand for the value in the
        /// arguments of a job.
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        remote: RemoteArgs,
    },
    /// Write the bytes of a blob, checked against its SHA-256, to the
    /// standard output or a file.
    Get {
        /// The blob: its id (sha256:...) or its marker.
        blob: String,
        /// Write to this file (through <file>.part) instead.
        #[arg(long, short)]
        output: Option<PathBuf>,
        /// The size of the blob, if the scheduler asked does not hold it.
        #[arg(long)]
        size: Option<u64>,
        #[command(flatten)]
        remote: RemoteArgs,
    },
    /// Whether a scheduler holds a blob, its size, and whether a majority of
    /// the schedulers holds it (durable).
    Stat {
        id: String,
        #[command(flatten)]
        remote: RemoteArgs,
    },
}

pub async fn blob(command: BlobCommand) -> Result<ExitCode, String> {
    use algebramesh_core::blob::{BlobId, BlobRef};
    match command {
        BlobCommand::Put { file, json, remote } => {
            let bytes = std::fs::read(&file).map_err(|e| format!("{}: {e}", file.display()))?;
            let bytes = if json {
                let value: serde_json::Value = serde_json::from_slice(&bytes)
                    .map_err(|e| format!("{}: not JSON: {e}", file.display()))?;
                algebramesh_core::canonical_json(&value).into_bytes()
            } else {
                bytes
            };
            let r = remote
                .connect()
                .await?
                .put_bytes(bytes)
                .await
                .map_err(|e| e.to_string())?;
            print_json(&r.marker(), false)?;
        }
        BlobCommand::Get {
            blob,
            output,
            size,
            remote,
        } => {
            let client = remote.connect().await?;
            let r = match serde_json::from_str::<serde_json::Value>(&blob) {
                Ok(marker @ serde_json::Value::Object(_)) => BlobRef::from_marker(&marker)
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| format!("{blob} is not a blob marker"))?,
                _ => {
                    let id = BlobId::parse(&blob).map_err(|e| e.to_string())?;
                    let size = match size {
                        Some(size) => size,
                        None => {
                            let stat = client.stat_blob(&id).await.map_err(|e| e.to_string())?;
                            if !stat.present {
                                return Err(format!(
                                    "the scheduler does not hold {id}; give its size with --size \
                                     to fetch it from another"
                                ));
                            }
                            stat.size
                        }
                    };
                    BlobRef { id, size }
                }
            };
            match output {
                Some(path) => client
                    .download_blob(&r, &path)
                    .await
                    .map_err(|e| e.to_string())?,
                None => {
                    let bytes = client.blob(&r).await.map_err(|e| e.to_string())?;
                    use std::io::Write;
                    let mut out = std::io::stdout().lock();
                    out.write_all(&bytes)
                        .and_then(|()| out.flush())
                        .map_err(|e| e.to_string())?;
                }
            }
        }
        BlobCommand::Stat { id, remote } => {
            let id = BlobId::parse(&id).map_err(|e| e.to_string())?;
            let stat = remote
                .connect()
                .await?
                .stat_blob(&id)
                .await
                .map_err(|e| e.to_string())?;
            print_json(
                &serde_json::json!({
                    "id": id.as_str(), "present": stat.present,
                    "durable": stat.durable, "size": stat.size,
                }),
                false,
            )?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

#[derive(Args)]
pub struct SubmitArgs {
    /// Job specification (JSON).
    job: PathBuf,
    /// Larger values run first.
    #[arg(long, default_value_t = 0)]
    priority: i32,
    /// Wait for the job to finish and print its record.
    #[arg(long)]
    wait: bool,
    /// With --wait: give up after this many seconds.
    #[arg(long)]
    timeout: Option<u64>,
    /// Idempotency key: running the command again with the same key returns the
    /// job the first run created instead of a new one (a fresh key otherwise).
    #[arg(long)]
    request_id: Option<String>,
    #[command(flatten)]
    remote: RemoteArgs,
}

pub async fn submit(args: SubmitArgs) -> Result<ExitCode, String> {
    let spec = read_spec(&args.job)?;
    let client = args.remote.connect().await?;
    let status = match &args.request_id {
        Some(key) => {
            let spec_json = serde_json::to_string(&spec).map_err(|e| e.to_string())?;
            client
                .submit_json_with_request_id(&spec_json, args.priority, key)
                .await
        }
        None => client.submit(&spec, args.priority).await,
    }
    .map_err(|e| e.to_string())?;
    if !args.wait {
        println!("{}", status.job_id);
        return Ok(ExitCode::SUCCESS);
    }
    eprintln!("algebramesh: submitted {}", status.job_id);
    let record = client
        .wait(&status.job_id, args.timeout.map(Duration::from_secs), true)
        .await
        .map_err(|e| e.to_string())?;
    print_json(&record, true)?;
    Ok(exit_for(&record))
}

pub async fn status(remote: RemoteArgs, job_id: &str) -> Result<ExitCode, String> {
    let status = remote
        .connect()
        .await?
        .status(job_id)
        .await
        .map_err(|e| e.to_string())?;
    print_json(&status_json(&status), true)?;
    Ok(ExitCode::SUCCESS)
}

pub async fn result(remote: RemoteArgs, job_id: &str, claims: bool) -> Result<ExitCode, String> {
    let (status, record) = remote
        .connect()
        .await?
        .result(job_id)
        .await
        .map_err(|e| e.to_string())?;
    match record {
        Some(record) if claims => match &record.guarantee {
            Some(guarantee) => {
                print!("{}", claims_table(guarantee));
                Ok(exit_for(&record))
            }
            None => Err(format!(
                "job {job_id} records no guarantee: it is {:?}, or its cluster has not \
                 enabled the feature result_guarantee",
                record.state
            )),
        },
        Some(record) => {
            print_json(&record, true)?;
            Ok(exit_for(&record))
        }
        None => Err(format!("job {job_id} is {}; no result yet", status.state)),
    }
}

/// The summary of `guarantee` and a table of its claims, for people.
pub fn claims_table(guarantee: &algebramesh_core::guarantee::Guarantee) -> String {
    let mut out = format!(
        "summary: {} ({} {})\n",
        name(&guarantee.summary),
        guarantee.verifier.name,
        guarantee.verifier.version
    );
    let rows: Vec<[String; 4]> = guarantee
        .claims
        .iter()
        .map(|c| {
            let mut level = name(&c.level);
            let effective = guarantee.effective_level(&c.id);
            if effective != c.level {
                level = format!("{level} (effectively {})", name(&effective));
            }
            [
                c.id.clone(),
                level,
                c.method.as_ref().map_or_else(|| "-".to_owned(), name),
                c.by.clone(),
            ]
        })
        .collect();
    let header = ["claim", "level", "method", "by"].map(String::from);
    let widths: Vec<usize> = (0..4)
        .map(|i| {
            rows.iter()
                .chain(std::iter::once(&header))
                .map(|r| r[i].chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    for row in std::iter::once(&header).chain(&rows) {
        let line: Vec<String> = row
            .iter()
            .zip(&widths)
            .map(|(cell, w)| format!("{cell:<w$}"))
            .collect();
        out.push_str(line.join("  ").trim_end());
        out.push('\n');
    }
    for p in &guarantee.not_claimed {
        out.push_str(&format!("not claimed: {p}\n"));
    }
    out
}

/// The snake_case name serde gives a level, a method or a summary.
fn name<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

pub async fn wait(
    remote: RemoteArgs,
    job_id: &str,
    timeout: Option<u64>,
) -> Result<ExitCode, String> {
    let record = remote
        .connect()
        .await?
        .wait(job_id, timeout.map(Duration::from_secs), true)
        .await
        .map_err(|e| e.to_string())?;
    print_json(&record, true)?;
    Ok(exit_for(&record))
}

pub async fn cancel(remote: RemoteArgs, job_id: &str) -> Result<ExitCode, String> {
    let status = remote
        .connect()
        .await?
        .cancel(job_id)
        .await
        .map_err(|e| e.to_string())?;
    print_json(&status_json(&status), true)?;
    Ok(ExitCode::SUCCESS)
}

pub async fn backends(remote: RemoteArgs) -> Result<ExitCode, String> {
    let list = remote
        .connect()
        .await?
        .backends()
        .await
        .map_err(|e| e.to_string())?;
    print_json(&backends_json(&list), true)?;
    Ok(ExitCode::SUCCESS)
}

pub async fn workers(remote: RemoteArgs) -> Result<ExitCode, String> {
    let list = remote
        .connect()
        .await?
        .workers()
        .await
        .map_err(|e| e.to_string())?;
    print_json(&workers_json(&list), true)?;
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use algebramesh_core::guarantee::{
        Claim, EvidenceLevel, Guarantee, Method, Scope, VerifierInfo,
    };

    use super::*;

    #[test]
    fn the_claims_show_their_levels_and_who_decided_them() {
        let claim = |id: &str, level, method, depends_on: &[&str]| Claim {
            id: id.into(),
            level,
            method,
            by: "someone".into(),
            scope: Scope::Exact,
            depends_on: depends_on.iter().map(|d| (*d).to_owned()).collect(),
            reason: None,
        };
        let g = Guarantee::new(
            VerifierInfo {
                name: "algebramesh-verify".into(),
                version: "1.1.0".into(),
                commit: None,
            },
            None,
            vec![
                claim(
                    "factor.domain",
                    EvidenceLevel::Checked,
                    Some(Method::SelfCertifying),
                    &[],
                ),
                claim("factor.irreducible", EvidenceLevel::Asserted, None, &[]),
                claim(
                    "factor.leaning",
                    EvidenceLevel::Checked,
                    Some(Method::Theorem),
                    &["factor.irreducible"],
                ),
            ],
            vec!["convergence".into()],
        );
        let table = claims_table(&g);
        let lines: Vec<&str> = table.lines().collect();
        assert_eq!(
            lines[0],
            "summary: partially_verified (algebramesh-verify 1.1.0)"
        );
        assert!(lines[1].starts_with("claim"), "{table}");
        assert!(
            lines[2].contains("checked") && lines[2].contains("self_certifying"),
            "{table}"
        );
        assert!(
            lines[3].contains("asserted") && lines[3].contains(" - "),
            "{table}"
        );
        assert!(
            lines[4].contains("checked (effectively asserted)"),
            "{table}"
        );
        assert_eq!(lines[5], "not claimed: convergence");
    }
}

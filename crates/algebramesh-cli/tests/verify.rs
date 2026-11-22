//! `algebramesh verify <job-id>` against a scheduler and an agent in this
//! process (V4, Issue #108, ADR 0031): the record and its blobs are read and
//! checked again, a blob without a body is `DATA_LOSS`, and the record is
//! not changed.

#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use algebramesh_agent::{Agent, BackendSlot, Connection};
use algebramesh_client::{Client, ClientConfig};
use algebramesh_core::blob::{BlobRef, markers};
use algebramesh_core::{BackendInfo, Capabilities, CasBackend, CasError, CasRequest, CasResult};
use algebramesh_core::{ExecutionContext, JobRecord};
use algebramesh_rpc::generate_dev_pki;
use algebramesh_server::{Scheduler, SchedulerOptions, Security, Store, serve};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

const CLIENT_TOKEN: &str = "client-token";
const AGENT_TOKEN: &str = "agent-token";

fn ring() -> Value {
    json!({"domain": "QQ", "variables": ["x", "y"], "order": "grevlex"})
}

fn poly(terms: &[(&str, [u32; 2])]) -> Value {
    json!({"type": "polynomial", "terms": terms.iter()
        .map(|(c, e)| json!({"coefficient": c, "exponents": e})).collect::<Vec<_>>()})
}

/// A backend that factors 2 x^2 - 2 y^2 and echoes the arguments of
/// `test.sleep`.
struct TestBackend;

#[async_trait::async_trait]
impl CasBackend for TestBackend {
    fn name(&self) -> &'static str {
        "test"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            operations: vec!["test.sleep".into(), "polynomial.factor".into()],
            ..Capabilities::default()
        }
    }

    async fn execute(&self, job: CasRequest, _: ExecutionContext) -> Result<CasResult, CasError> {
        let value = if job.operation == "polynomial.factor" {
            json!({
                "type": "factorization",
                "ring": ring(),
                "constant": "2",
                "factors": [
                    {"factor": poly(&[("1", [1, 0]), ("1", [0, 1])]), "multiplicity": 1, "display": "x+y"},
                    {"factor": poly(&[("1", [1, 0]), ("-1", [0, 1])]), "multiplicity": 1, "display": "x-y"},
                ],
                "verified_product": true,
            })
        } else {
            job.arguments
        };
        Ok(CasResult {
            value,
            backend: BackendInfo {
                name: "test".into(),
                backend_version: Some("1".into()),
                driver_version: "test-driver 1".into(),
                ..BackendInfo::default()
            },
            execution_host: None,
            log: vec![],
            guarantee: None,
        })
    }
}

struct Cluster {
    dir: PathBuf,
    url: String,
    ca_pem: String,
    stop: tokio::sync::oneshot::Sender<()>,
    served: tokio::task::JoinHandle<()>,
    agent: CancellationToken,
}

async fn cluster() -> Cluster {
    let dir = std::env::temp_dir().join(format!(
        "am-cli-verify-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let pki = generate_dev_pki(&["localhost".into()]).unwrap();
    std::fs::write(dir.join("ca.pem"), &pki.ca_cert_pem).unwrap();
    std::fs::write(dir.join("client.token"), format!("{CLIENT_TOKEN}\n")).unwrap();
    let mut options = SchedulerOptions {
        heartbeat_interval: Duration::from_millis(200),
        ..SchedulerOptions::default()
    };
    options.blobs.dir = Some(dir.join("blobs"));
    let scheduler = Arc::new(Scheduler::new(Store::open(None, None).unwrap(), options).unwrap());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "https://localhost:{}",
        listener.local_addr().unwrap().port()
    );
    let security = Security {
        cert_pem: pki.server_cert_pem.into_bytes(),
        key_pem: pki.server_key_pem.into_bytes(),
        client_tokens: vec![CLIENT_TOKEN.into()],
        agent_tokens: vec![AGENT_TOKEN.into()],
        agent_ca_pem: None,
        revoked_agent_serials: vec![],
        peer_tokens: vec![],
    };
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let served = tokio::spawn(async move {
        serve(scheduler, security, listener, async move {
            let _ = stopped.await;
        })
        .await
        .unwrap();
    });
    let agent = Agent::connect(
        Connection {
            url: url.clone(),
            ca_pem: pki.ca_cert_pem.clone().into_bytes(),
            domain: String::new(),
            token: AGENT_TOKEN.into(),
            client_identity: None,
        },
        "worker".into(),
        "worker.test".into(),
        vec![BackendSlot {
            backend: Arc::new(TestBackend),
            slots: 1,
            build: "b".into(),
            backend_version: "1".into(),
        }],
    )
    .await
    .unwrap();
    let token = CancellationToken::new();
    let running = token.clone();
    tokio::spawn(async move {
        let _ = agent.run(running).await;
    });
    Cluster {
        dir,
        url,
        ca_pem: pki.ca_cert_pem,
        stop,
        served,
        agent: token,
    }
}

impl Cluster {
    async fn client(&self) -> Client {
        Client::connect(ClientConfig {
            url: self.url.clone(),
            ca_pem: self.ca_pem.clone().into_bytes(),
            domain: None,
            token: CLIENT_TOKEN.into(),
        })
        .await
        .unwrap()
    }

    /// Runs the job `spec` to its end; its id.
    async fn run(&self, spec: Value) -> String {
        let client = self.client().await;
        let status = client.submit_json(&spec.to_string(), 0).await.unwrap();
        let record = client
            .wait(&status.job_id, Some(Duration::from_secs(60)), false)
            .await
            .unwrap();
        assert_eq!(
            record.state,
            algebramesh_core::JobState::Succeeded,
            "{record:?}"
        );
        status.job_id
    }

    /// The record as the scheduler keeps it, markers included.
    async fn stored(&self, job_id: &str) -> String {
        let (_, record) = self.client().await.result_refs(job_id).await.unwrap();
        serde_json::to_string(&record.unwrap()).unwrap()
    }

    /// Runs `algebramesh verify <args>`: its exit code and standard output.
    async fn verify(&self, args: &[&str]) -> (i32, String, String) {
        let mut command = Command::new(env!("CARGO_BIN_EXE_algebramesh"));
        command
            .arg("verify")
            .args(args)
            .env("ALGEBRAMESH_SERVER", &self.url)
            .env("ALGEBRAMESH_CA", self.dir.join("ca.pem"))
            .env("ALGEBRAMESH_TOKEN_FILE", self.dir.join("client.token"))
            .env("RUST_LOG", "warn");
        let out = tokio::task::spawn_blocking(move || command.output().unwrap())
            .await
            .unwrap();
        (
            out.status.code().unwrap(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    async fn stop(self) {
        self.agent.cancel();
        let _ = self.stop.send(());
        self.served.await.unwrap();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A factorization recorded with its guarantee is checked again to the same
/// levels; the record is not changed.
#[tokio::test(flavor = "multi_thread")]
async fn a_factorization_is_checked_again_to_its_recorded_levels() {
    let cluster = cluster().await;
    let job = cluster
        .run(json!({
            "schema_version": 1, "backend": "test", "operation": "polynomial.factor",
            "arguments": {"ring": ring(), "expression": poly(&[("2", [2, 0]), ("-2", [0, 2])])},
        }))
        .await;
    let before = cluster.stored(&job).await;

    let (code, out, err) = cluster.verify(&[&job]).await;
    assert_eq!(code, 0, "{out}{err}");
    assert!(out.contains("input: sha256 matches"), "{out}");
    assert!(out.contains("levels: as recorded"), "{out}");
    let product = out
        .lines()
        .find(|l| l.starts_with("factor.product"))
        .unwrap();
    assert_eq!(product.matches("checked").count(), 2, "{out}");

    let (code, out, err) = cluster.verify(&[&job, "--format", "json"]).await;
    assert_eq!(code, 0, "{err}");
    let answer: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(answer["recorded"]["source"], "record");
    assert_eq!(answer["recheck"]["status"], "rechecked");
    assert_eq!(
        answer["recorded"]["guarantee"]["claims"],
        answer["recheck"]["guarantee"]["claims"]
    );
    assert_eq!(cluster.stored(&job).await, before);
    cluster.stop().await;
}

/// A result kept in blobs is read from them and checked; once no scheduler
/// holds a body, the answer is DATA_LOSS and no value. The record is not
/// changed.
#[tokio::test(flavor = "multi_thread")]
async fn a_result_in_blobs_is_read_and_a_lost_body_is_data_loss() {
    let cluster = cluster().await;
    let pad = "a".repeat(300 * 1024);
    let job = cluster
        .run(json!({
            "schema_version": 1, "backend": "test", "operation": "test.sleep",
            "arguments": {"ms": 1, "pad": pad},
        }))
        .await;
    let before = cluster.stored(&job).await;
    let record: JobRecord = serde_json::from_str(&before).unwrap();
    let found: Vec<BlobRef> = markers(record.result.as_ref().unwrap())
        .unwrap()
        .into_iter()
        .map(|(_, r)| r)
        .collect();
    assert!(
        !found.is_empty(),
        "the result is kept in a blob: {before:.300}"
    );

    let (code, out, err) = cluster.verify(&[&job, "--format", "json"]).await;
    assert_eq!(code, 0, "{out}{err}");
    let answer: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(answer["recheck"]["status"], "rechecked", "{out:.2000}");
    assert!(
        answer["blobs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|b| b["status"] == "ok"),
        "{out:.2000}"
    );

    // The body is gone from the only scheduler.
    for r in &found {
        let hex = r.id.hex();
        std::fs::remove_file(
            cluster
                .dir
                .join("blobs/sha256")
                .join(&hex[..2])
                .join(&hex[2..]),
        )
        .unwrap();
    }
    let (code, out, err) = cluster.verify(&[&job, "--format", "json"]).await;
    assert_eq!(code, 1, "{out}{err}");
    let answer: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(answer["recheck"]["status"], "not_checked", "{out:.2000}");
    let lost: Vec<&Value> = answer["blobs"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|b| b["place"].as_str().unwrap().starts_with("result"))
        .collect();
    assert!(!lost.is_empty());
    assert!(
        lost.iter().all(|b| b["status"] == "data_loss"),
        "{out:.2000}"
    );
    assert!(!out.contains(&pad[..1024]), "no value is printed");

    let (code, out, _) = cluster.verify(&[&job]).await;
    assert_eq!(code, 1);
    assert!(out.contains("data_loss"), "{out}");
    assert!(out.contains("recheck: not done"), "{out}");

    assert_eq!(cluster.stored(&job).await, before);
    cluster.stop().await;
}

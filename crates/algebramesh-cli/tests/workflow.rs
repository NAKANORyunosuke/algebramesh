//! `algebramesh workflow run|resume|status|cancel` against a scheduler in
//! this process (W2, ADR 0032, Issue #104). No worker runs: the node's job
//! stays QUEUED until the workflow is cancelled.

#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::time::Duration;

use algebramesh_rpc::generate_dev_pki;
use algebramesh_server::{Scheduler, SchedulerOptions, Security, Store, serve};
use serde_json::{Value, json};

const CLIENT_TOKEN: &str = "client-token";

struct Server {
    dir: PathBuf,
    url: String,
    stop: tokio::sync::oneshot::Sender<()>,
    served: tokio::task::JoinHandle<()>,
}

async fn server() -> Server {
    let dir = std::env::temp_dir().join(format!(
        "am-cli-workflow-{}-{}",
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
    let scheduler = Arc::new(
        Scheduler::new(
            Store::open(None, None).unwrap(),
            SchedulerOptions::default(),
        )
        .unwrap(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "https://localhost:{}",
        listener.local_addr().unwrap().port()
    );
    let security = Security {
        cert_pem: pki.server_cert_pem.into_bytes(),
        key_pem: pki.server_key_pem.into_bytes(),
        client_tokens: vec![CLIENT_TOKEN.into()],
        agent_tokens: vec!["agent-token".into()],
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
    Server {
        dir,
        url,
        stop,
        served,
    }
}

impl Server {
    /// `algebramesh workflow <args>` against this scheduler.
    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_algebramesh"));
        command
            .arg("workflow")
            .args(args)
            .env("ALGEBRAMESH_SERVER", &self.url)
            .env("ALGEBRAMESH_CA", self.dir.join("ca.pem"))
            .env("ALGEBRAMESH_TOKEN_FILE", self.dir.join("client.token"))
            .env("RUST_LOG", "warn");
        command
    }

    /// Runs `algebramesh workflow <args>`; its exit code and the report it
    /// printed.
    async fn workflow(&self, args: &[&str]) -> (Option<i32>, Value, String) {
        let mut command = self.command(args);
        let out = tokio::task::spawn_blocking(move || command.output().unwrap())
            .await
            .unwrap();
        parsed(out)
    }

    fn path(&self, name: &str) -> String {
        self.dir.join(name).display().to_string()
    }

    async fn stop(self) {
        let _ = self.stop.send(());
        self.served.await.unwrap();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn parsed(out: Output) -> (Option<i32>, Value, String) {
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let printed = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
    (out.status.code(), printed, stderr)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_workflow_is_looked_at_run_and_cancelled() {
    let server = server().await;
    let file = server.path("w.json");
    std::fs::write(
        &file,
        json!({
            "workflow": "cli",
            "nodes": {
                "only": {"spec": {
                    "schema_version": 1, "backend": "none", "operation": "x.y",
                    "arguments": {}
                }}
            }
        })
        .to_string(),
    )
    .unwrap();

    let (code, report, err) = server.workflow(&["status", &file]).await;
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(report["progress"], "RUNNING");
    assert_eq!(report["nodes"]["only"]["state"], "NOT_SUBMITTED");

    // A run waits for the job, which no worker takes.
    let mut command = server.command(&["run", &file, "--poll-secs", "1"]);
    let running = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut submitted = false;
    for _ in 0..100 {
        let (_, report, _) = server.workflow(&["status", &file]).await;
        if report["nodes"]["only"]["state"] == "QUEUED" {
            assert_eq!(report["nodes"]["only"]["request_id"], "wf/cli/only/1");
            submitted = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(submitted, "the run submitted nothing");
    // Another orchestrator does not run it at the same time.
    let (code, _, err) = server.workflow(&["resume", &file]).await;
    assert_eq!(code, Some(2), "{err}");
    assert!(err.contains("another orchestrator"), "{err}");

    let (code, report, err) = server.workflow(&["cancel", &file]).await;
    assert_eq!(code, Some(0), "{err}");
    assert_eq!(report["progress"], "CANCELLED");
    assert_eq!(report["nodes"]["only"]["state"], "CANCELLED");
    let out = tokio::task::spawn_blocking(move || running.wait_with_output().unwrap())
        .await
        .unwrap();
    let (code, report, err) = parsed(out);
    assert_eq!(code, Some(1), "{err}");
    assert_eq!(report["progress"], "CANCELLED");
    // The state file keeps the cancellation.
    let state: Value =
        serde_json::from_str(&std::fs::read_to_string(format!("{file}.state.json")).unwrap())
            .unwrap();
    assert_eq!(state["cancelled"], true);

    // A definition that is not one is refused before anything is sent.
    let bad = server.path("bad.json");
    std::fs::write(&bad, r#"{"workflow": "a b", "nodes": {}}"#).unwrap();
    let (code, _, err) = server.workflow(&["run", &bad]).await;
    assert_eq!(code, Some(2), "{err}");
    server.stop().await;
}

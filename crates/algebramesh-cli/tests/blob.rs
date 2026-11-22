//! `algebramesh blob put|get|stat` against a scheduler in this process (ADR
//! 0021, B3, Issue #96).

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use algebramesh_core::blob::{BlobId, BlobRef, canonical_ref};
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
        "am-cli-blob-{}-{}",
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
    /// Runs `algebramesh blob <args>` against this scheduler; its exit status,
    /// standard output and standard error.
    async fn blob(&self, args: &[&str]) -> (bool, Vec<u8>, String) {
        let mut command = Command::new(env!("CARGO_BIN_EXE_algebramesh"));
        command
            .arg("blob")
            .args(args)
            .env("ALGEBRAMESH_SERVER", &self.url)
            .env("ALGEBRAMESH_CA", self.dir.join("ca.pem"))
            .env("ALGEBRAMESH_TOKEN_FILE", self.dir.join("client.token"))
            .env("RUST_LOG", "warn");
        let out = tokio::task::spawn_blocking(move || command.output().unwrap())
            .await
            .unwrap();
        (
            out.status.success(),
            out.stdout,
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    async fn stop(self) {
        let _ = self.stop.send(());
        self.served.await.unwrap();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn marker_of(stdout: &[u8]) -> BlobRef {
    let printed: Value = serde_json::from_slice(stdout).unwrap();
    BlobRef::from_marker(&printed).unwrap().unwrap()
}

fn write(path: &Path, bytes: &[u8]) -> String {
    std::fs::write(path, bytes).unwrap();
    path.display().to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn put_stat_and_get_a_file() {
    let server = server().await;
    // Not JSON, and larger than one chunk of a transfer.
    let bytes: Vec<u8> = (0..(5u32 << 20)).map(|i| (i % 251) as u8).collect();
    let file = write(&server.path("body.bin"), &bytes);

    let (ok, out, err) = server.blob(&["put", &file]).await;
    assert!(ok, "{err}");
    let r = marker_of(&out);
    assert_eq!(r.id, BlobId::of(&bytes));
    assert_eq!(r.size, bytes.len() as u64);

    let (ok, out, err) = server.blob(&["stat", r.id.as_str()]).await;
    assert!(ok, "{err}");
    let stat: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(
        stat,
        json!({"id": r.id.as_str(), "present": true, "durable": true, "size": bytes.len()})
    );

    // To a file, by its id.
    let copy = server.path("copy.bin");
    let (ok, _, err) = server
        .blob(&[
            "get",
            r.id.as_str(),
            "--output",
            &copy.display().to_string(),
        ])
        .await;
    assert!(ok, "{err}");
    assert_eq!(std::fs::read(&copy).unwrap(), bytes);
    // To the standard output, by its marker.
    let marker = r.marker().to_string();
    let (ok, out, err) = server.blob(&["get", &marker]).await;
    assert!(ok, "{err}");
    assert_eq!(out, bytes);

    // A blob no scheduler holds.
    let unknown = BlobId::of(b"never stored");
    let (ok, _, err) = server.blob(&["get", unknown.as_str()]).await;
    assert!(!ok);
    assert!(err.contains(unknown.as_str()), "{err}");
    let (ok, out, err) = server.blob(&["stat", unknown.as_str()]).await;
    assert!(ok, "{err}");
    let stat: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(stat["present"], json!(false));
    server.stop().await;
}

/// `put --json` stores the canonical JSON of the value, so that its marker
/// can stand for the value in the arguments of a job.
#[tokio::test(flavor = "multi_thread")]
async fn put_json_stores_the_canonical_form() {
    let server = server().await;
    let value = json!({"b": [1, 2, 3], "a": "x"});
    let file = write(
        &server.path("value.json"),
        serde_json::to_string_pretty(&value).unwrap().as_bytes(),
    );
    let (ok, out, err) = server.blob(&["put", "--json", &file]).await;
    assert!(ok, "{err}");
    let r = marker_of(&out);
    assert_eq!(r, canonical_ref(&value));
    let (ok, out, err) = server.blob(&["get", r.id.as_str()]).await;
    assert!(ok, "{err}");
    assert_eq!(out, algebramesh_core::canonical_json(&value).into_bytes());

    // A file that is not JSON.
    let bad = write(&server.path("bad.json"), b"{not json");
    let (ok, _, err) = server.blob(&["put", "--json", &bad]).await;
    assert!(!ok);
    assert!(err.contains("JSON"), "{err}");
    server.stop().await;
}

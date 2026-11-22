//! Scheduler, agents and client in one process, talking gRPC over TLS with tokens,
//! and with agent client certificates (mutual TLS).

#![allow(clippy::unwrap_used)]

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use algebramesh_agent::{Agent, BackendSlot, ClientIdentity, Connection};
use algebramesh_client::{Client, ClientConfig, ClientError};
use algebramesh_core::{
    BackendInfo, Capabilities, CasBackend, CasError, CasRequest, CasResult, ExecutionContext,
    JobState,
};
use algebramesh_rpc::pb::agent_gateway_client::AgentGatewayClient;
use algebramesh_rpc::{
    DevPki, TokenInterceptor, client_tls, generate_dev_pki, issue_agent_certificate, pb,
};
use algebramesh_server::{Scheduler, SchedulerOptions, Security, Store, serve};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tonic::transport::{Endpoint, Identity};

const CLIENT_TOKEN: &str = "client-token";
const AGENT_TOKEN: &str = "agent-token";

/// `test.sleep` sleeps `ms` milliseconds (cancellable) and echoes its arguments.
struct SleepBackend;

#[async_trait::async_trait]
impl CasBackend for SleepBackend {
    fn name(&self) -> &'static str {
        "test"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            operations: vec!["test.sleep".into(), "test.panic".into()],
            ..Capabilities::default()
        }
    }

    async fn execute(&self, job: CasRequest, ctx: ExecutionContext) -> Result<CasResult, CasError> {
        if job.operation == "test.panic" {
            panic!("a deliberate panic of the test backend");
        }
        let ms = job.arguments["ms"].as_u64().unwrap_or(0);
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(ms)) => {}
            _ = ctx.cancel.cancelled() => return Err(CasError::Cancelled),
        }
        Ok(CasResult {
            value: job.arguments,
            backend: BackendInfo {
                name: "test".into(),
                backend_version: Some("1".into()),
                driver_version: "test".into(),
                build: None,
                ..BackendInfo::default()
            },
            execution_host: None,
            log: vec![],
            guarantee: None,
        })
    }
}

struct Cluster {
    url: String,
    ca_pem: Vec<u8>,
    pki: DevPki,
    stop: CancellationToken,
}

impl Cluster {
    async fn start(options: SchedulerOptions) -> Self {
        Self::start_with(options, false, vec![]).await
    }

    /// With `mtls`, agents need a client certificate from the cluster CA;
    /// `revoked` lists refused certificate serials.
    async fn start_with(options: SchedulerOptions, mtls: bool, revoked: Vec<String>) -> Self {
        let pki = generate_dev_pki(&["localhost".into()]).unwrap();
        Self::start_with_pki(options, pki, mtls, revoked).await
    }

    async fn start_with_pki(
        options: SchedulerOptions,
        pki: DevPki,
        mtls: bool,
        revoked: Vec<String>,
    ) -> Self {
        let scheduler =
            Arc::new(Scheduler::new(Store::open(None, None).unwrap(), options).unwrap());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let stop = CancellationToken::new();
        let security = Security {
            cert_pem: pki.server_cert_pem.clone().into_bytes(),
            key_pem: pki.server_key_pem.clone().into_bytes(),
            client_tokens: vec![CLIENT_TOKEN.into()],
            agent_tokens: vec![AGENT_TOKEN.into()],
            agent_ca_pem: mtls.then(|| pki.ca_cert_pem.clone().into_bytes()),
            revoked_agent_serials: revoked,
            peer_tokens: vec![],
        };
        let shutdown = stop.clone();
        tokio::spawn(async move {
            serve(scheduler, security, listener, async move {
                shutdown.cancelled().await
            })
            .await
            .unwrap();
        });
        Self {
            url: format!("https://localhost:{port}"),
            ca_pem: pki.ca_cert_pem.clone().into_bytes(),
            pki,
            stop,
        }
    }

    fn certificate(&self, name: &str) -> (ClientIdentity, String) {
        let cert =
            issue_agent_certificate(&self.pki.ca_cert_pem, &self.pki.ca_key_pem, name, 1).unwrap();
        (
            ClientIdentity {
                cert_pem: cert.cert_pem.into_bytes(),
                key_pem: cert.key_pem.into_bytes(),
            },
            cert.serial,
        )
    }

    /// A raw agent gateway client, for calls a well-behaved agent never makes.
    async fn gateway(
        &self,
        identity: Option<&ClientIdentity>,
    ) -> AgentGatewayClient<
        tonic::service::interceptor::InterceptedService<
            tonic::transport::Channel,
            TokenInterceptor,
        >,
    > {
        let mut tls = client_tls(&self.ca_pem, "localhost");
        if let Some(id) = identity {
            tls = tls.identity(Identity::from_pem(&id.cert_pem, &id.key_pem));
        }
        let channel = Endpoint::from_shared(self.url.clone())
            .unwrap()
            .tls_config(tls)
            .unwrap()
            .connect_lazy();
        AgentGatewayClient::with_interceptor(channel, TokenInterceptor::new(AGENT_TOKEN).unwrap())
    }

    async fn client(&self, token: &str) -> Client {
        Client::connect(ClientConfig {
            url: self.url.clone(),
            ca_pem: self.ca_pem.clone(),
            domain: None,
            token: token.into(),
        })
        .await
        .unwrap()
    }

    /// Starts an agent; aborting the returned handle simulates a crashed host.
    async fn agent(
        &self,
        name: &str,
        slots: u32,
    ) -> (tokio::task::JoinHandle<()>, CancellationToken) {
        self.agent_with(name, slots, None).await
    }

    async fn agent_with(
        &self,
        name: &str,
        slots: u32,
        client_identity: Option<ClientIdentity>,
    ) -> (tokio::task::JoinHandle<()>, CancellationToken) {
        let agent = Agent::connect(
            Connection {
                url: self.url.clone(),
                ca_pem: self.ca_pem.clone(),
                domain: "localhost".into(),
                token: AGENT_TOKEN.into(),
                client_identity,
            },
            name.into(),
            format!("{name}.test"),
            vec![BackendSlot {
                backend: Arc::new(SleepBackend),
                slots,
                build: "test-build".into(),
                backend_version: "1".into(),
            }],
        )
        .await
        .unwrap();
        let stop = CancellationToken::new();
        let token = stop.clone();
        let handle = tokio::spawn(async move {
            let _ = agent.run(token).await;
        });
        (handle, stop)
    }
}

impl Drop for Cluster {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

fn spec(ms: u64, retry: &str) -> String {
    serde_json::json!({
        "schema_version": 1, "backend": "test", "operation": "test.sleep",
        "arguments": {"ms": ms}, "retry_policy": retry,
    })
    .to_string()
}

async fn wait_for_workers(client: &Client, n: usize) {
    for _ in 0..100 {
        if client
            .workers()
            .await
            .unwrap()
            .workers
            .iter()
            .filter(|w| w.online)
            .count()
            >= n
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("agents did not register");
}

#[tokio::test(flavor = "multi_thread")]
async fn jobs_are_spread_over_two_agents() {
    let cluster = Cluster::start(SchedulerOptions::default()).await;
    let client = cluster.client(CLIENT_TOKEN).await;
    let _a = cluster.agent("worker-a", 1).await;
    let _b = cluster.agent("worker-b", 1).await;
    wait_for_workers(&client, 2).await;

    let backends = client.backends().await.unwrap().backends;
    assert_eq!(backends.len(), 1);
    assert_eq!(backends[0].agent_ids.len(), 2);

    let mut ids = Vec::new();
    for _ in 0..6 {
        ids.push(
            client
                .submit_json(&spec(300, "never"), 0)
                .await
                .unwrap()
                .job_id,
        );
    }
    let mut agents = HashSet::new();
    for id in &ids {
        let record = client
            .wait(id, Some(Duration::from_secs(30)), false)
            .await
            .unwrap();
        assert_eq!(record.state, JobState::Succeeded);
        assert_eq!(record.backend.build.as_deref(), Some("test-build"));
        assert!(
            record
                .log
                .iter()
                .any(|l| l.starts_with("executed by agent"))
        );
        agents.insert(record.agent_id.unwrap());
    }
    assert_eq!(agents.len(), 2, "both agents should have run jobs");
}

#[tokio::test(flavor = "multi_thread")]
async fn running_job_can_be_cancelled() {
    let cluster = Cluster::start(SchedulerOptions::default()).await;
    let client = cluster.client(CLIENT_TOKEN).await;
    let _a = cluster.agent("worker-a", 1).await;
    wait_for_workers(&client, 1).await;

    let id = client
        .submit_json(&spec(60_000, "never"), 0)
        .await
        .unwrap()
        .job_id;
    for _ in 0..100 {
        if client.status(&id).await.unwrap().state == "RUNNING" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let status = client.cancel(&id).await.unwrap();
    assert_eq!(status.state, "CANCELLING");
    let record = client
        .wait(&id, Some(Duration::from_secs(20)), false)
        .await
        .unwrap();
    assert_eq!(record.state, JobState::Cancelled);
    let states: Vec<_> = record.history.iter().map(|c| c.state).collect();
    assert!(states.contains(&JobState::Cancelling));

    // A queued job is cancelled immediately.
    let _busy = client.submit_json(&spec(60_000, "never"), 0).await.unwrap();
    let queued = client
        .submit_json(&spec(10, "never"), -1)
        .await
        .unwrap()
        .job_id;
    assert_eq!(client.cancel(&queued).await.unwrap().state, "CANCELLED");
}

#[tokio::test(flavor = "multi_thread")]
async fn crashed_agent_makes_job_lost_and_deterministic_job_is_retried() {
    let options = SchedulerOptions {
        heartbeat_interval: Duration::from_millis(100),
        heartbeat_timeout: Duration::from_millis(800),
        ..SchedulerOptions::default()
    };
    let cluster = Cluster::start(options).await;
    let client = cluster.client(CLIENT_TOKEN).await;
    let (crashing, _) = cluster.agent("worker-a", 1).await;
    wait_for_workers(&client, 1).await;

    let never = client
        .submit_json(&spec(1_500, "never"), 0)
        .await
        .unwrap()
        .job_id;
    let retry = client
        .submit_json(&spec(1_500, "if_deterministic"), 0)
        .await
        .unwrap()
        .job_id;
    // Wait until worker-a runs the first job, then crash it.
    for _ in 0..100 {
        if client.status(&never).await.unwrap().state == "RUNNING" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    crashing.abort();

    let record = client
        .wait(&never, Some(Duration::from_secs(10)), false)
        .await
        .unwrap();
    assert_eq!(record.state, JobState::Lost);
    assert!(matches!(record.error, Some(CasError::Lost(_))));
    assert!(client.status(&never).await.unwrap().retried_as.is_empty());

    // The second job was queued behind the first; with no agent online it stays
    // queued until worker-b arrives.
    let _b = cluster.agent("worker-b", 1).await;
    let done = client
        .wait(&retry, Some(Duration::from_secs(20)), true)
        .await
        .unwrap();
    assert_eq!(done.state, JobState::Succeeded);
}

#[tokio::test(flavor = "multi_thread")]
async fn lost_running_job_with_retry_policy_is_resubmitted() {
    let options = SchedulerOptions {
        heartbeat_interval: Duration::from_millis(100),
        heartbeat_timeout: Duration::from_millis(800),
        ..SchedulerOptions::default()
    };
    let cluster = Cluster::start(options).await;
    let client = cluster.client(CLIENT_TOKEN).await;
    let (crashing, _) = cluster.agent("worker-a", 1).await;
    wait_for_workers(&client, 1).await;
    let id = client
        .submit_json(&spec(1_000, "if_deterministic"), 0)
        .await
        .unwrap()
        .job_id;
    for _ in 0..100 {
        if client.status(&id).await.unwrap().state == "RUNNING" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    crashing.abort();
    let _b = cluster.agent("worker-b", 1).await;
    let record = client
        .wait(&id, Some(Duration::from_secs(20)), true)
        .await
        .unwrap();
    assert_eq!(record.state, JobState::Succeeded);
    assert_eq!(record.attempt, 2);
    assert_eq!(record.retry_of.map(|j| j.to_string()), Some(id.clone()));
    let first = client.status(&id).await.unwrap();
    assert_eq!(first.state, "LOST");
    assert_eq!(first.retried_as, record.job_id.to_string());
}

#[tokio::test(flavor = "multi_thread")]
async fn tokens_and_tls_are_required() {
    let cluster = Cluster::start(SchedulerOptions::default()).await;
    let wrong = cluster.client("agent-token").await;
    let err = wrong.submit_json(&spec(1, "never"), 0).await.unwrap_err();
    assert!(
        matches!(
            err,
            ClientError::Rpc {
                code: tonic::Code::Unauthenticated,
                ..
            }
        ),
        "{err}"
    );
    let insecure = Client::connect(ClientConfig {
        url: cluster.url.replace("https://", "http://"),
        ca_pem: cluster.ca_pem.clone(),
        domain: None,
        token: CLIENT_TOKEN.into(),
    })
    .await;
    assert!(matches!(insecure, Err(ClientError::Config(_))));
    let other_ca = generate_dev_pki(&["localhost".into()]).unwrap();
    let untrusted = Client::connect(ClientConfig {
        url: cluster.url.clone(),
        ca_pem: other_ca.ca_cert_pem.into_bytes(),
        domain: None,
        token: CLIENT_TOKEN.into(),
    })
    .await;
    match untrusted {
        Err(_) => {}
        Ok(c) => assert!(c.submit_json(&spec(1, "never"), 0).await.is_err()),
    }
}

fn register_request(name: &str) -> pb::RegisterRequest {
    pb::RegisterRequest {
        name: name.into(),
        host: format!("{name}.test"),
        agent_version: "test".into(),
        backends: vec![],
        ..Default::default()
    }
}

fn code<T: std::fmt::Debug>(r: Result<T, tonic::Status>) -> tonic::Code {
    r.unwrap_err().code()
}

#[tokio::test(flavor = "multi_thread")]
async fn agents_with_client_certificates_run_jobs() {
    let cluster = Cluster::start_with(SchedulerOptions::default(), true, vec![]).await;
    // Clients of the public API still connect without a certificate.
    let client = cluster.client(CLIENT_TOKEN).await;
    let (identity, _) = cluster.certificate("worker-a");
    let _a = cluster.agent_with("worker-a", 1, Some(identity)).await;
    wait_for_workers(&client, 1).await;
    let id = client
        .submit_json(&spec(1, "never"), 0)
        .await
        .unwrap()
        .job_id;
    let record = client
        .wait(&id, Some(Duration::from_secs(30)), false)
        .await
        .unwrap();
    assert_eq!(record.state, JobState::Succeeded);
}

#[tokio::test(flavor = "multi_thread")]
async fn agent_certificates_are_enforced() {
    let cluster = Cluster::start_with(SchedulerOptions::default(), true, vec![]).await;
    let (a, _) = cluster.certificate("worker-a");
    let (b, _) = cluster.certificate("worker-b");

    // No certificate: the token alone is not enough.
    let mut anonymous = cluster.gateway(None).await;
    assert_eq!(
        code(anonymous.register(register_request("worker-a")).await),
        tonic::Code::Unauthenticated
    );

    // A certificate only registers its own name.
    let mut gateway_a = cluster.gateway(Some(&a)).await;
    assert_eq!(
        code(gateway_a.register(register_request("worker-b")).await),
        tonic::Code::PermissionDenied
    );
    let agent_a = gateway_a
        .register(register_request("worker-a"))
        .await
        .unwrap()
        .into_inner()
        .agent_id;

    // Another agent cannot act for worker-a.
    let mut gateway_b = cluster.gateway(Some(&b)).await;
    let heartbeat = pb::HeartbeatRequest {
        agent_id: agent_a.clone(),
        running_job_ids: vec![],
    };
    assert_eq!(
        code(gateway_b.heartbeat(heartbeat.clone()).await),
        tonic::Code::PermissionDenied
    );
    let poll = pb::PollRequest {
        agent_id: agent_a.clone(),
        backends: vec![],
        wait_ms: 0,
    };
    assert_eq!(
        code(gateway_b.poll_assignment(poll).await),
        tonic::Code::PermissionDenied
    );
    let finished = pb::JobFinished {
        agent_id: agent_a.clone(),
        job_id: "j".into(),
        attempt: 1,
        outcome_json: "{}".into(),
    };
    assert_eq!(
        code(gateway_b.report_finished(finished).await),
        tonic::Code::PermissionDenied
    );
    assert!(
        gateway_a
            .heartbeat(heartbeat)
            .await
            .unwrap()
            .into_inner()
            .known
    );

    // A certificate from another CA fails the TLS handshake.
    let other = generate_dev_pki(&["localhost".into()]).unwrap();
    let foreign =
        issue_agent_certificate(&other.ca_cert_pem, &other.ca_key_pem, "worker-a", 1).unwrap();
    let mut gateway_foreign = cluster
        .gateway(Some(&ClientIdentity {
            cert_pem: foreign.cert_pem.into_bytes(),
            key_pem: foreign.key_pem.into_bytes(),
        }))
        .await;
    assert!(
        gateway_foreign
            .register(register_request("worker-a"))
            .await
            .is_err()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn revoked_agent_certificates_are_refused() {
    let pki = generate_dev_pki(&["localhost".into()]).unwrap();
    let revoked =
        issue_agent_certificate(&pki.ca_cert_pem, &pki.ca_key_pem, "worker-a", 1).unwrap();
    // Serials are matched without case and colons.
    let listed = revoked
        .serial
        .to_uppercase()
        .as_bytes()
        .chunks(2)
        .map(|c| std::str::from_utf8(c).unwrap())
        .collect::<Vec<_>>()
        .join(":");
    let cluster =
        Cluster::start_with_pki(SchedulerOptions::default(), pki, true, vec![listed]).await;
    let mut gateway = cluster
        .gateway(Some(&ClientIdentity {
            cert_pem: revoked.cert_pem.into_bytes(),
            key_pem: revoked.key_pem.into_bytes(),
        }))
        .await;
    assert_eq!(
        code(gateway.register(register_request("worker-a")).await),
        tonic::Code::PermissionDenied
    );
    // A new certificate for the same name is accepted.
    let (fresh, _) = cluster.certificate("worker-a");
    let mut gateway = cluster.gateway(Some(&fresh)).await;
    assert!(gateway.register(register_request("worker-a")).await.is_ok());
}

#[tokio::test(flavor = "multi_thread")]
async fn large_specifications_and_records_pass() {
    let cluster = Cluster::start(SchedulerOptions::default()).await;
    let client = cluster.client(CLIENT_TOKEN).await;
    let _a = cluster.agent("worker-a", 1).await;
    wait_for_workers(&client, 1).await;
    // 6 MiB: above tonic's 4 MiB default, below the scheduler's 8 MiB limit for
    // specifications and results; echoed back in the record.
    let padding = "x".repeat(6 * 1024 * 1024);
    let spec = serde_json::json!({
        "schema_version": 1, "backend": "test", "operation": "test.sleep",
        "arguments": {"ms": 1, "padding": padding},
    })
    .to_string();
    let id = client.submit_json(&spec, 0).await.unwrap().job_id;
    let record = client
        .wait(&id, Some(Duration::from_secs(60)), false)
        .await
        .unwrap();
    assert_eq!(record.state, JobState::Succeeded);
    assert_eq!(
        record.result.unwrap()["padding"].as_str().map(str::len),
        Some(6 * 1024 * 1024)
    );
}

/// Issue #31: a backend that panics fails its job, and the slot goes on to the
/// next job; a limit that cannot be represented is refused when submitted.
#[tokio::test(flavor = "multi_thread")]
async fn a_panicking_backend_fails_its_job_and_the_slot_goes_on() {
    let cluster = Cluster::start(SchedulerOptions::default()).await;
    let client = cluster.client(CLIENT_TOKEN).await;
    let _a = cluster.agent("worker-a", 1).await;
    wait_for_workers(&client, 1).await;
    let panic = serde_json::json!({
        "schema_version": 1, "backend": "test", "operation": "test.panic", "arguments": {},
    })
    .to_string();
    let id = client.submit_json(&panic, 0).await.unwrap().job_id;
    let record = client
        .wait(&id, Some(Duration::from_secs(30)), false)
        .await
        .unwrap();
    assert_eq!(record.state, JobState::Failed);
    let error = serde_json::to_string(&record.error).unwrap();
    assert!(
        error.contains("INTERNAL") && error.contains("deliberate panic"),
        "{error}"
    );
    // The one slot runs the next job.
    let id = client
        .submit_json(&spec(1, "never"), 0)
        .await
        .unwrap()
        .job_id;
    let record = client
        .wait(&id, Some(Duration::from_secs(30)), false)
        .await
        .unwrap();
    assert_eq!(record.state, JobState::Succeeded);
    // wall_seconds = u64::MAX cannot be a deadline: refused, nothing runs.
    let extreme = serde_json::json!({
        "schema_version": 1, "backend": "test", "operation": "test.sleep",
        "arguments": {"ms": 1}, "limits": {"wall_seconds": u64::MAX},
    })
    .to_string();
    let refused = client.submit_json(&extreme, 0).await.unwrap_err();
    assert!(refused.to_string().contains("wall_seconds"), "{refused}");
}

/// Issue #40: a full queue answers a new submission with RESOURCE_EXHAUSTED,
/// and `ListBackends` reports the count and the limit.
#[tokio::test(flavor = "multi_thread")]
async fn a_full_queue_answers_resource_exhausted() {
    let options = SchedulerOptions {
        max_queued_jobs: 1,
        ..SchedulerOptions::default()
    };
    let cluster = Cluster::start(options).await;
    let client = cluster.client(CLIENT_TOKEN).await;
    let first = client
        .submit_json_with_request_id(&spec(10, "never"), 0, "first")
        .await
        .unwrap();
    let err = client.submit_json(&spec(10, "never"), 0).await.unwrap_err();
    assert!(
        matches!(
            err,
            ClientError::Rpc {
                code: tonic::Code::ResourceExhausted,
                ..
            }
        ),
        "{err:?}"
    );
    // The known key still answers with its job.
    let again = client
        .submit_json_with_request_id(&spec(10, "never"), 0, "first")
        .await
        .unwrap();
    assert_eq!(again.job_id, first.job_id);
    let list = client.backends().await.unwrap();
    assert_eq!((list.queued_jobs, list.max_queued_jobs), (1, 1));
}

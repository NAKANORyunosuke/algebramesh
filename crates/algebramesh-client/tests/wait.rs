//! `Client::wait` keeps its timeout while a call is pending (Issue #17), against
//! a scheduler whose handlers can be made to hang or to fail. The scheduler
//! does not offer the calls added after it (`LookupRequest` and `ListJobs`,
//! Issue #63), as one of an older version.

#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use algebramesh_client::{Client, ClientConfig, ClientError};
use algebramesh_rpc::{generate_dev_pki, pb, server_tls};
use tonic::{Request, Response, Status};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Behaviour {
    /// Status never answers.
    StatusHangs,
    /// Status says SUCCEEDED; Result never answers.
    ResultHangs,
    /// Status answers UNAVAILABLE three times, then SUCCEEDED; Result answers.
    UnavailableThenDone,
    /// Status says SUCCEEDED; Result answers UNAVAILABLE twice, then answers
    /// (a failover between the two calls, Issue #93).
    ResultUnavailableThenDone,
    /// Status says SUCCEEDED; Result always answers UNAVAILABLE.
    ResultUnavailable,
    /// Status says SUCCEEDED; Result answers NOT_FOUND.
    ResultNotFound,
}

#[derive(Clone)]
struct Fake {
    behaviour: Behaviour,
    status_calls: Arc<AtomicUsize>,
    result_calls: Arc<AtomicUsize>,
}

fn status(state: &str) -> pb::JobStatus {
    pb::JobStatus {
        job_id: "j".into(),
        state: state.into(),
        backend: "test".into(),
        operation: "test.op".into(),
        attempt: 1,
        ..Default::default()
    }
}

fn record() -> String {
    let spec = algebramesh_core::JobSpec::from_json(
        r#"{"schema_version": 1, "backend": "test", "operation": "test.op", "arguments": {}}"#,
    )
    .unwrap();
    let mut record =
        algebramesh_core::JobRecord::new(algebramesh_core::JobId::new(), spec, chrono_now());
    record.state = algebramesh_core::JobState::Succeeded;
    serde_json::to_string(&record).unwrap()
}

fn chrono_now() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now()
}

#[tonic::async_trait]
impl pb::scheduler_server::Scheduler for Fake {
    async fn submit(
        &self,
        _: Request<pb::SubmitRequest>,
    ) -> Result<Response<pb::JobStatus>, Status> {
        Err(Status::unimplemented(""))
    }
    async fn status(&self, _: Request<pb::JobRef>) -> Result<Response<pb::JobStatus>, Status> {
        let n = self.status_calls.fetch_add(1, Ordering::SeqCst);
        match self.behaviour {
            Behaviour::StatusHangs => std::future::pending().await,
            Behaviour::UnavailableThenDone if n < 3 => Err(Status::unavailable("an election")),
            _ => Ok(Response::new(status("SUCCEEDED"))),
        }
    }
    async fn result(&self, _: Request<pb::JobRef>) -> Result<Response<pb::JobResult>, Status> {
        let n = self.result_calls.fetch_add(1, Ordering::SeqCst);
        match self.behaviour {
            Behaviour::ResultHangs => std::future::pending::<()>().await,
            Behaviour::ResultUnavailableThenDone if n < 2 => {
                return Err(Status::unavailable("an election"));
            }
            Behaviour::ResultUnavailable => return Err(Status::unavailable("an election")),
            Behaviour::ResultNotFound => return Err(Status::not_found("no such job")),
            _ => {}
        }
        Ok(Response::new(pb::JobResult {
            status: Some(status("SUCCEEDED")),
            record_json: record(),
        }))
    }
    async fn cancel(&self, _: Request<pb::JobRef>) -> Result<Response<pb::JobStatus>, Status> {
        Err(Status::unimplemented(""))
    }
    async fn list_backends(
        &self,
        _: Request<pb::Empty>,
    ) -> Result<Response<pb::ListBackendsResponse>, Status> {
        Err(Status::unimplemented(""))
    }
    async fn list_workers(
        &self,
        _: Request<pb::Empty>,
    ) -> Result<Response<pb::ListWorkersResponse>, Status> {
        Err(Status::unimplemented(""))
    }
    async fn lookup_request(
        &self,
        _: Request<pb::RequestRef>,
    ) -> Result<Response<pb::JobStatus>, Status> {
        Err(Status::unimplemented(""))
    }
    async fn list_jobs(
        &self,
        _: Request<pb::ListJobsRequest>,
    ) -> Result<Response<pb::ListJobsResponse>, Status> {
        Err(Status::unimplemented(""))
    }

    async fn put_blob(
        &self,
        _: Request<tonic::Streaming<pb::BlobUpload>>,
    ) -> Result<Response<pb::BlobRef>, Status> {
        Err(Status::unimplemented(""))
    }

    type GetBlobStream = tokio_stream::Empty<Result<pb::BlobChunk, Status>>;

    async fn get_blob(
        &self,
        _: Request<pb::BlobRequest>,
    ) -> Result<Response<Self::GetBlobStream>, Status> {
        Err(Status::unimplemented(""))
    }

    async fn stat_blob(
        &self,
        _: Request<pb::BlobRequest>,
    ) -> Result<Response<pb::BlobStat>, Status> {
        Err(Status::unimplemented(""))
    }
}

/// A client of a fake scheduler, and the counts of its Status and Result calls.
async fn client(behaviour: Behaviour) -> (Client, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let pki = generate_dev_pki(&["localhost".to_string()]).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let calls = Arc::new(AtomicUsize::new(0));
    let result_calls = Arc::new(AtomicUsize::new(0));
    let fake = Fake {
        behaviour,
        status_calls: calls.clone(),
        result_calls: result_calls.clone(),
    };
    let server = tonic::transport::Server::builder()
        .tls_config(server_tls(
            pki.server_cert_pem.as_bytes(),
            pki.server_key_pem.as_bytes(),
            None,
        ))
        .unwrap()
        .add_service(pb::scheduler_server::SchedulerServer::new(fake))
        .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener));
    tokio::spawn(server);
    let client = Client::connect(ClientConfig {
        url: format!("https://localhost:{port}"),
        ca_pem: pki.ca_cert_pem.into_bytes(),
        domain: None,
        token: "t".into(),
    })
    .await
    .unwrap();
    (client, calls, result_calls)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_status_call_that_never_answers_does_not_outlast_the_timeout() {
    let (client, _, _) = client(Behaviour::StatusHangs).await;
    let started = Instant::now();
    let err = client
        .wait("j", Some(Duration::from_secs(3)), false)
        .await
        .unwrap_err();
    assert!(matches!(err, ClientError::WaitTimeout(..)), "{err}");
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_secs(3) && elapsed < Duration::from_secs(5),
        "{elapsed:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_result_call_that_never_answers_does_not_outlast_the_timeout() {
    let (client, _, _) = client(Behaviour::ResultHangs).await;
    let started = Instant::now();
    let err = client
        .wait("j", Some(Duration::from_secs(3)), false)
        .await
        .unwrap_err();
    assert!(matches!(err, ClientError::WaitTimeout(..)), "{err}");
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_secs(3) && elapsed < Duration::from_secs(5),
        "{elapsed:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wait_rides_out_unavailable_answers() {
    let (client, calls, _) = client(Behaviour::UnavailableThenDone).await;
    let record = client
        .wait("j", Some(Duration::from_secs(20)), false)
        .await
        .unwrap();
    assert_eq!(record.state, algebramesh_core::JobState::Succeeded);
    assert_eq!(calls.load(Ordering::SeqCst), 4);
}

/// Issue #93: a failover between the terminal Status and the Result does not
/// end the wait; the Result is asked for again, with or without a timeout.
#[tokio::test(flavor = "multi_thread")]
async fn a_wait_rides_out_unavailable_result_answers() {
    for timeout in [Some(Duration::from_secs(20)), None] {
        let (client, _, results) = client(Behaviour::ResultUnavailableThenDone).await;
        let record = client.wait("j", timeout, false).await.unwrap();
        assert_eq!(record.state, algebramesh_core::JobState::Succeeded);
        assert_eq!(results.load(Ordering::SeqCst), 3);
    }
}

/// Issue #93: a Result that stays UNAVAILABLE ends the wait at its timeout,
/// counted from the start of the wait.
#[tokio::test(flavor = "multi_thread")]
async fn a_result_that_stays_unavailable_does_not_outlast_the_timeout() {
    let (client, _, results) = client(Behaviour::ResultUnavailable).await;
    let started = Instant::now();
    let err = client
        .wait("j", Some(Duration::from_secs(3)), false)
        .await
        .unwrap_err();
    assert!(matches!(err, ClientError::WaitTimeout(..)), "{err}");
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_secs(3) && elapsed < Duration::from_secs(5),
        "{elapsed:?}"
    );
    assert!(results.load(Ordering::SeqCst) > 1);
}

/// Issue #93: an error of Result other than UNAVAILABLE still ends the wait.
#[tokio::test(flavor = "multi_thread")]
async fn a_result_that_fails_ends_the_wait() {
    let (client, _, results) = client(Behaviour::ResultNotFound).await;
    let err = client
        .wait("j", Some(Duration::from_secs(20)), false)
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            ClientError::Rpc {
                code: tonic::Code::NotFound,
                ..
            }
        ),
        "{err}"
    );
    assert_eq!(results.load(Ordering::SeqCst), 1);
}

/// Issue #63: a scheduler older than `LookupRequest` and `ListJobs` answers
/// them `UNIMPLEMENTED`, which the client reports as a call the scheduler does
/// not offer, not as a missing job or an empty listing.
#[tokio::test(flavor = "multi_thread")]
async fn an_older_scheduler_is_reported_as_lacking_the_lookups() {
    let (client, _, _) = client(Behaviour::UnavailableThenDone).await;
    let err = client.lookup_request("k").await.unwrap_err();
    assert!(
        matches!(err, ClientError::Unsupported("LookupRequest")),
        "{err}"
    );
    let err = client
        .list_jobs(&pb::ListJobsRequest::default())
        .await
        .unwrap_err();
    assert!(matches!(err, ClientError::Unsupported("ListJobs")), "{err}");
}

//! In a cluster of mixed versions, a scheduler older than `LookupRequest` and
//! `ListJobs` answers them `UNIMPLEMENTED` (Issue #63). The client tries the
//! other schedulers, and reports the calls as unsupported only when none of
//! them served the call: at once when every scheduler lacks them, and after
//! the retries of an election when one was unavailable (Issue #83).

#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use algebramesh_client::{Client, ClientConfig, ClientError, ListJobsRequest};
use algebramesh_rpc::{DevPki, generate_dev_pki, pb, server_tls};
use tonic::{Request, Response, Status};

#[derive(Clone)]
enum Node {
    /// Older than the two calls, and not the leader.
    Older,
    /// Older than the two calls, and the leader.
    OlderLeader,
    /// Of this version, and the leader.
    Leader,
    /// Of this version, and not the leader.
    Follower,
    /// Of this version, and the leader, which cannot confirm its leadership
    /// for its first calls: it answers `UNAVAILABLE` while the count is above
    /// zero.
    Recovering(Arc<AtomicUsize>),
}

fn not_the_leader() -> Status {
    Status::unavailable("not the leader")
}

impl Node {
    /// The answer to one of the two calls, `served` by the leader of this
    /// version.
    fn later<T>(&self, served: T) -> Result<Response<T>, Status> {
        match self {
            Node::Older | Node::OlderLeader => Err(Status::unimplemented("")),
            Node::Leader => Ok(Response::new(served)),
            Node::Follower => Err(not_the_leader()),
            Node::Recovering(left) => {
                let before = left
                    .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                    .unwrap_or(0);
                if before > 0 {
                    Err(Status::unavailable("cannot confirm its leadership"))
                } else {
                    Ok(Response::new(served))
                }
            }
        }
    }
}

#[tonic::async_trait]
impl pb::scheduler_server::Scheduler for Node {
    async fn submit(
        &self,
        _: Request<pb::SubmitRequest>,
    ) -> Result<Response<pb::JobStatus>, Status> {
        Err(not_the_leader())
    }
    async fn status(&self, _: Request<pb::JobRef>) -> Result<Response<pb::JobStatus>, Status> {
        Err(not_the_leader())
    }
    async fn result(&self, _: Request<pb::JobRef>) -> Result<Response<pb::JobResult>, Status> {
        Err(not_the_leader())
    }
    async fn cancel(&self, _: Request<pb::JobRef>) -> Result<Response<pb::JobStatus>, Status> {
        Err(not_the_leader())
    }
    async fn list_backends(
        &self,
        _: Request<pb::Empty>,
    ) -> Result<Response<pb::ListBackendsResponse>, Status> {
        Err(not_the_leader())
    }
    async fn list_workers(
        &self,
        _: Request<pb::Empty>,
    ) -> Result<Response<pb::ListWorkersResponse>, Status> {
        Err(not_the_leader())
    }
    async fn lookup_request(
        &self,
        _: Request<pb::RequestRef>,
    ) -> Result<Response<pb::JobStatus>, Status> {
        self.later(pb::JobStatus {
            job_id: "j".into(),
            state: "QUEUED".into(),
            ..Default::default()
        })
    }
    async fn list_jobs(
        &self,
        _: Request<pb::ListJobsRequest>,
    ) -> Result<Response<pb::ListJobsResponse>, Status> {
        self.later(pb::ListJobsResponse {
            jobs: vec![pb::JobSummary {
                job_id: "j".into(),
                ..Default::default()
            }],
            next_page_token: String::new(),
        })
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

async fn serve(pki: &DevPki, node: Node) -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tonic::transport::Server::builder()
        .tls_config(server_tls(
            pki.server_cert_pem.as_bytes(),
            pki.server_key_pem.as_bytes(),
            None,
        ))
        .unwrap()
        .add_service(pb::scheduler_server::SchedulerServer::new(node))
        .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener));
    tokio::spawn(server);
    port
}

/// A client of a cluster of `nodes`, which reaches the first one first.
async fn client(nodes: &[Node]) -> Client {
    let pki = generate_dev_pki(&["localhost".to_string()]).unwrap();
    let mut urls = Vec::new();
    for node in nodes {
        urls.push(format!(
            "https://localhost:{}",
            serve(&pki, node.clone()).await
        ));
    }
    Client::connect(ClientConfig {
        url: urls.join(","),
        ca_pem: pki.ca_cert_pem.into_bytes(),
        domain: None,
        token: "t".into(),
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_older_scheduler_is_passed_over_for_the_leader() {
    let client = client(&[Node::Older, Node::Leader, Node::Follower]).await;
    let status = client.lookup_request("k").await.unwrap().unwrap();
    assert_eq!(status.job_id, "j");
    let page = client.list_jobs(&ListJobsRequest::default()).await.unwrap();
    assert_eq!(page.jobs.len(), 1);
}

/// While an older scheduler leads, no scheduler serves the calls. The
/// followers of this version answer `UNAVAILABLE`, as a leader of this version
/// would during an election, so the client tries again for as long, then says
/// that the calls are unsupported.
#[tokio::test(flavor = "multi_thread")]
async fn the_calls_are_unsupported_while_an_older_scheduler_leads() {
    let client = client(&[Node::Follower, Node::OlderLeader, Node::Follower]).await;
    let started = Instant::now();
    let err = client.lookup_request("k").await.unwrap_err();
    assert!(
        matches!(err, ClientError::Unsupported("LookupRequest")),
        "{err}"
    );
    assert!(
        started.elapsed() >= Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    let err = client
        .list_jobs(&ListJobsRequest::default())
        .await
        .unwrap_err();
    assert!(matches!(err, ClientError::Unsupported("ListJobs")), "{err}");
}

/// When every scheduler is older, no retry helps: the client says so at once.
#[tokio::test(flavor = "multi_thread")]
async fn the_calls_are_unsupported_at_once_when_every_scheduler_is_older() {
    let client = client(&[Node::Older, Node::OlderLeader, Node::Older]).await;
    let started = Instant::now();
    let err = client.lookup_request("k").await.unwrap_err();
    assert!(
        matches!(err, ClientError::Unsupported("LookupRequest")),
        "{err}"
    );
    let err = client
        .list_jobs(&ListJobsRequest::default())
        .await
        .unwrap_err();
    assert!(matches!(err, ClientError::Unsupported("ListJobs")), "{err}");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
}

/// Issue #83: an older follower, a leader of this version that is briefly
/// unavailable (a read barrier that fails), and a follower of this version.
/// The first round has no answer, but the leader serves the call when the
/// client tries again.
#[tokio::test(flavor = "multi_thread")]
async fn a_leader_that_is_briefly_unavailable_serves_the_call_later() {
    let left = Arc::new(AtomicUsize::new(2));
    let client = client(&[Node::Older, Node::Recovering(left.clone()), Node::Follower]).await;
    let status = client.lookup_request("k").await.unwrap().unwrap();
    assert_eq!(status.job_id, "j");
    assert_eq!(
        left.load(Ordering::SeqCst),
        0,
        "the leader was unavailable twice"
    );
    let page = client.list_jobs(&ListJobsRequest::default()).await.unwrap();
    assert_eq!(page.jobs.len(), 1);
}

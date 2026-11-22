//! Client library for the scheduler API, shared by the CLI and the Python binding.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

pub mod ops;
pub mod workflow;

use algebramesh_core::{JobRecord, JobSpec, blob};
use algebramesh_rpc::pb::scheduler_client::SchedulerClient;
use algebramesh_rpc::{MAX_MESSAGE_BYTES, TokenInterceptor, client_tls, pb};
use thiserror::Error;
use tonic::service::interceptor::InterceptedService;
use tonic::transport::{Channel, Endpoint};

pub use algebramesh_rpc::pb::{
    BackendSummary, JobStatus, JobSummary, ListBackendsResponse, ListJobsRequest, ListJobsResponse,
    ListWorkersResponse, WorkerSummary,
};

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("invalid configuration: {0}")]
    Config(String),
    #[error("cannot connect to the scheduler: {0}")]
    Connect(#[from] tonic::transport::Error),
    #[error("scheduler error ({code:?}): {message}")]
    Rpc { code: tonic::Code, message: String },
    #[error("malformed data from the scheduler: {0}")]
    Data(String),
    #[error("timed out after {0:?} waiting for job {1}")]
    WaitTimeout(Duration, String),
    /// No scheduler served the call, and one that was asked does not offer it:
    /// it is older than this client (it answered `UNIMPLEMENTED`). In a
    /// cluster of mixed versions this can pass: the call may succeed later,
    /// once a scheduler of this version leads, or the older ones are upgraded.
    /// It comes at once when every scheduler answered `UNIMPLEMENTED`, and
    /// otherwise only after the others were tried as for an election (Issue
    /// #83).
    #[error("the scheduler does not offer {0}; it is older than this client")]
    Unsupported(&'static str),
}

/// `error`, or [`ClientError::Unsupported`] when it says that the scheduler
/// does not offer `call`.
fn unsupported(error: ClientError, call: &'static str) -> ClientError {
    match error {
        ClientError::Rpc {
            code: tonic::Code::Unimplemented,
            ..
        } => ClientError::Unsupported(call),
        e => e,
    }
}

impl From<tonic::Status> for ClientError {
    fn from(s: tonic::Status) -> Self {
        Self::Rpc {
            code: s.code(),
            message: s.message().to_owned(),
        }
    }
}

/// Where and how to reach the scheduler.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// `https://host:port`, or several such URLs separated by commas (the
    /// schedulers of a cluster); calls go to the first that answers.
    pub url: String,
    /// PEM of the CA that signed the scheduler certificate.
    pub ca_pem: Vec<u8>,
    /// Name expected in the scheduler certificate; defaults to the URL host.
    pub domain: Option<String>,
    pub token: String,
}

fn url_host(url: &str) -> Option<String> {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = rest.split('/').next()?;
    let host = if let Some(stripped) = authority.strip_prefix('[') {
        stripped.split(']').next()?
    } else {
        authority.rsplit_once(':').map_or(authority, |(h, _)| h)
    };
    (!host.is_empty()).then(|| host.to_owned())
}

type Inner = SchedulerClient<InterceptedService<Channel, TokenInterceptor>>;

#[derive(Clone)]
pub struct Client {
    /// One client per scheduler URL.
    inner: Arc<Vec<Inner>>,
    /// The URL that answered last.
    current: Arc<AtomicUsize>,
}

/// The URLs in a comma-separated list.
pub fn split_urls(urls: &str) -> Vec<String> {
    urls.split(',')
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .map(str::to_owned)
        .collect()
}

/// A job state name as returned by the scheduler.
pub fn is_terminal(state: &str) -> bool {
    matches!(
        state,
        "SUCCEEDED" | "FAILED" | "TIMED_OUT" | "CANCELLED" | "LOST"
    )
}

/// How long the name lookup and the TCP connection, and then the TLS
/// handshake, may each take before the client tries the next scheduler.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a call goes on trying when every scheduler answers `UNAVAILABLE`.
/// Shorter than the scheduler's wait for a commit (10 s), so that only calls
/// refused before they had an effect are tried again.
const RETRY_UNAVAILABLE: Duration = Duration::from_secs(5);

/// How long one attempt of a call may wait for its answer (Issue #17). HTTP/2
/// keepalive only shows that the connection lives, not that a handler answers.
/// Longer than the scheduler's own waits (a commit, 10 s; a read barrier).
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// What a call may do, which decides how an attempt without an answer is treated.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// It only reads: an attempt without an answer counts as `UNAVAILABLE`, and
    /// the next scheduler is tried.
    Read,
    /// It may change the store: an attempt without an answer may have taken
    /// effect, so it is not repeated; the caller gets `DEADLINE_EXCEEDED`.
    Write,
    /// It only reads, and schedulers older than the call answer
    /// `UNIMPLEMENTED`: in a cluster of mixed versions the next scheduler is
    /// tried, and `UNIMPLEMENTED` is the answer only when no scheduler served
    /// the call, at once when none was unavailable and after the retries of an
    /// election otherwise (Issues #63, #83).
    LaterRead,
}

impl Client {
    /// Connects to the first URL that accepts a connection; the others are tried
    /// when a call fails with `UNAVAILABLE` (not the leader, or a scheduler that is
    /// down).
    pub async fn connect(config: ClientConfig) -> Result<Self, ClientError> {
        let urls = split_urls(&config.url);
        if urls.is_empty() {
            return Err(ClientError::Config("no scheduler URL".into()));
        }
        let token =
            TokenInterceptor::new(&config.token).map_err(|e| ClientError::Config(e.to_string()))?;
        let mut clients = Vec::new();
        let mut current = None;
        let mut first_error = None;
        for (i, url) in urls.iter().enumerate() {
            if !url.starts_with("https://") {
                return Err(ClientError::Config(
                    "the scheduler URL must start with https:// (TLS is required)".into(),
                ));
            }
            let domain = config
                .domain
                .clone()
                .or_else(|| url_host(url))
                .ok_or_else(|| ClientError::Config(format!("no host in {url}")))?;
            let endpoint = Endpoint::from_shared(url.clone())?
                .tls_config(client_tls(&config.ca_pem, &domain))?;
            let channel = if current.is_none() {
                match algebramesh_rpc::connect(&endpoint, CONNECT_TIMEOUT).await {
                    Ok(channel) => {
                        current = Some(i);
                        channel
                    }
                    Err(e) => {
                        first_error.get_or_insert(e);
                        algebramesh_rpc::connect_lazy(&endpoint, CONNECT_TIMEOUT)
                    }
                }
            } else {
                algebramesh_rpc::connect_lazy(&endpoint, CONNECT_TIMEOUT)
            };
            clients.push(
                SchedulerClient::with_interceptor(channel, token.clone())
                    .max_decoding_message_size(MAX_MESSAGE_BYTES)
                    .max_encoding_message_size(MAX_MESSAGE_BYTES),
            );
        }
        let Some(current) = current else {
            return Err(first_error.map_or_else(
                || ClientError::Config("no scheduler URL".into()),
                ClientError::Connect,
            ));
        };
        Ok(Self {
            inner: Arc::new(clients),
            current: Arc::new(AtomicUsize::new(current)),
        })
    }

    /// Runs `call` on the scheduler that answered last, then on the others in
    /// turn while they answer `UNAVAILABLE` (or, for [`Kind::LaterRead`],
    /// `UNIMPLEMENTED`). When one was unavailable and none served the call, as
    /// during an election, the round is tried again until [`RETRY_UNAVAILABLE`]
    /// has passed since the call began; a scheduler without the call then makes
    /// the answer `UNIMPLEMENTED`. Each attempt waits at most [`CALL_TIMEOUT`].
    async fn call<T, F, Fut>(&self, kind: Kind, call: F) -> Result<T, ClientError>
    where
        F: Fn(Inner) -> Fut,
        Fut: std::future::Future<Output = Result<tonic::Response<T>, tonic::Status>>,
    {
        self.call_within(kind, CALL_TIMEOUT, call).await
    }

    /// [`Self::call`] with attempts of at most `limit` each.
    async fn call_within<T, F, Fut>(
        &self,
        kind: Kind,
        limit: Duration,
        call: F,
    ) -> Result<T, ClientError>
    where
        F: Fn(Inner) -> Fut,
        Fut: std::future::Future<Output = Result<tonic::Response<T>, tonic::Status>>,
    {
        let n = self.inner.len();
        let began = std::time::Instant::now();
        let mut delay = Duration::from_millis(200);
        loop {
            let start = self.current.load(Ordering::Relaxed);
            let mut last = None;
            let mut unimplemented = None;
            for i in 0..n {
                let index = (start + i) % n;
                let attempt = tokio::time::timeout(limit, call(self.inner[index].clone()));
                let answer = attempt.await.unwrap_or_else(|_| {
                    Err(match kind {
                        Kind::Read | Kind::LaterRead => tonic::Status::unavailable(format!(
                            "the scheduler did not answer within {limit:?}"
                        )),
                        Kind::Write => tonic::Status::deadline_exceeded(format!(
                            "the scheduler did not answer within {limit:?}; the call may \
                             or may not have taken effect"
                        )),
                    })
                });
                match answer {
                    Ok(r) => {
                        self.current.store(index, Ordering::Relaxed);
                        return Ok(r.into_inner());
                    }
                    Err(s) if s.code() == tonic::Code::Unavailable && n > 1 => last = Some(s),
                    Err(s)
                        if matches!(kind, Kind::LaterRead)
                            && s.code() == tonic::Code::Unimplemented =>
                    {
                        unimplemented = Some(s);
                    }
                    Err(s) => return Err(s.into()),
                }
            }
            // No scheduler served the call. When every one that answered does
            // not offer it, no retry helps. When some were unavailable, one of
            // them may be a leader of this version that is about to serve it
            // (Issue #83): the round is tried again, as during an election.
            let Some(last) = last else {
                return Err(unimplemented.map_or_else(
                    || ClientError::Config("no scheduler URL".into()),
                    Into::into,
                ));
            };
            // A refusal that came after the scheduler had taken the call (a change
            // that was not committed) arrives later than this, so a call that
            // may have had an effect is not repeated.
            if began.elapsed() + delay >= RETRY_UNAVAILABLE {
                return Err(unimplemented.map_or_else(|| last.into(), Into::into));
            }
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(Duration::from_secs(1));
        }
    }

    pub async fn submit(&self, spec: &JobSpec, priority: i32) -> Result<JobStatus, ClientError> {
        let spec_json =
            serde_json::to_string(spec).map_err(|e| ClientError::Data(e.to_string()))?;
        self.submit_json(&spec_json, priority).await
    }

    /// Submits a JSON job specification; the scheduler validates it. The call
    /// carries a fresh idempotency key, so that its retries after a lost answer
    /// (another scheduler, a change of leader) create one job, not several.
    pub async fn submit_json(
        &self,
        spec_json: &str,
        priority: i32,
    ) -> Result<JobStatus, ClientError> {
        let request_id = uuid::Uuid::new_v4().to_string();
        self.submit_json_with_request_id(spec_json, priority, &request_id)
            .await
    }

    /// Like [`Self::submit_json`] with the caller's idempotency key: sending the
    /// same key again, from this or another process, returns the same job.
    ///
    /// A specification larger than [`blob::INLINE_MAX_BYTES`] has its largest
    /// arguments sent first as blobs and replaced by markers, once the cluster
    /// takes blobs in arguments (ADR 0021, B2); before, it goes whole, within
    /// the scheduler's limit on its size.
    pub async fn submit_json_with_request_id(
        &self,
        spec_json: &str,
        priority: i32,
        request_id: &str,
    ) -> Result<JobStatus, ClientError> {
        let spec_json = self.offload_arguments(spec_json).await?;
        let request = pb::SubmitRequest {
            spec_json,
            priority,
            request_id: request_id.to_owned(),
        };
        self.call(Kind::Write, |mut c| {
            let request = request.clone();
            async move { c.submit(request).await }
        })
        .await
    }

    /// Submits `spec_json` as it is, with the caller's idempotency key: its
    /// arguments are not moved to blobs. A workflow submits so (W2), having
    /// put references and markers in place itself.
    pub async fn submit_json_as_is(
        &self,
        spec_json: &str,
        priority: i32,
        request_id: &str,
    ) -> Result<JobStatus, ClientError> {
        let request = pb::SubmitRequest {
            spec_json: spec_json.to_owned(),
            priority,
            request_id: request_id.to_owned(),
        };
        self.call(Kind::Write, |mut c| {
            let request = request.clone();
            async move { c.submit(request).await }
        })
        .await
    }

    /// `spec_json` with its largest top-level arguments in blobs, when it is
    /// larger than [`blob::INLINE_MAX_BYTES`] and the cluster takes blobs in
    /// arguments; otherwise as it is. A specification that is not valid goes
    /// as it is, for the scheduler to say why.
    async fn offload_arguments(&self, spec_json: &str) -> Result<String, ClientError> {
        if spec_json.len() <= blob::INLINE_MAX_BYTES || JobSpec::from_json(spec_json).is_err() {
            return Ok(spec_json.to_owned());
        }
        let Ok(mut spec) = serde_json::from_str::<serde_json::Value>(spec_json) else {
            return Ok(spec_json.to_owned());
        };
        let keys = blob::plan_argument_offload(
            &spec["arguments"],
            spec_json.len() as u64,
            blob::INLINE_MAX_BYTES as u64,
        )
        .map_err(|e| ClientError::Data(e.to_string()))?;
        if keys.is_empty() {
            return Ok(spec_json.to_owned());
        }
        let enabled = self.backends().await?.server_features;
        if !enabled
            .iter()
            .any(|f| f == algebramesh_core::features::BLOB_ARGUMENTS)
        {
            return Ok(spec_json.to_owned());
        }
        for key in keys {
            let r = self.put_value(&spec["arguments"][&key]).await?;
            spec["arguments"][&key] = r.marker();
        }
        serde_json::to_string(&spec).map_err(|e| ClientError::Data(e.to_string()))
    }

    /// Stores `value` (its canonical JSON) as a blob, held by a majority of
    /// the schedulers once this returns (ADR 0021), and returns its reference.
    /// Nothing is sent if a majority holds it already.
    pub async fn put_value(&self, value: &serde_json::Value) -> Result<blob::BlobRef, ClientError> {
        self.put_bytes(algebramesh_core::canonical_json(value).into_bytes())
            .await
    }

    /// Stores `bytes` as a blob, held by a majority of the schedulers once
    /// this returns (ADR 0021), and returns its reference. Nothing is sent if
    /// a majority holds it already. A blob no record refers to is removed
    /// after the scheduler's `blob_orphan_grace_secs`.
    pub async fn put_bytes(&self, bytes: Vec<u8>) -> Result<blob::BlobRef, ClientError> {
        let bytes = Arc::new(bytes);
        let r = blob::BlobRef {
            id: blob::BlobId::of(&bytes),
            size: bytes.len() as u64,
        };
        let stat = self.stat_blob(&r.id).await?;
        if stat.present && stat.durable && stat.size == r.size {
            return Ok(r);
        }
        // Sending a blob again has no other effect: an attempt without an
        // answer is tried again, given time for the transfer.
        let limit = CALL_TIMEOUT + Duration::from_secs(r.size / (4 << 20));
        let got = self
            .call_within(Kind::Read, limit, |mut c| {
                let pieces: Vec<pb::BlobUpload> = bytes
                    .chunks(blob::CHUNK_BYTES)
                    .enumerate()
                    .map(|(i, data)| pb::BlobUpload {
                        id: if i == 0 {
                            r.id.to_string()
                        } else {
                            String::new()
                        },
                        size: if i == 0 { r.size } else { 0 },
                        data: data.to_vec(),
                    })
                    .collect();
                async move { c.put_blob(tokio_stream::iter(pieces)).await }
            })
            .await
            .map_err(|e| unsupported(e, "PutBlob"))?;
        if got.id != r.id.as_str() || got.size != r.size {
            return Err(ClientError::Data(format!(
                "the scheduler stored {} ({} bytes) for {} ({} bytes)",
                got.id, got.size, r.id, r.size
            )));
        }
        Ok(r)
    }

    pub async fn status(&self, job_id: &str) -> Result<JobStatus, ClientError> {
        self.call(Kind::Read, |mut c| async move {
            c.status(job_ref(job_id)).await
        })
        .await
    }

    /// The status and, once the job is terminal, its full record, with every
    /// blob its result refers to read and put in place (ADR 0021): the value
    /// is whole, however large.
    pub async fn result(
        &self,
        job_id: &str,
    ) -> Result<(JobStatus, Option<JobRecord>), ClientError> {
        let (status, mut record) = self.result_refs(job_id).await?;
        if let Some(record) = &mut record
            && let Some(result) = &mut record.result
        {
            self.resolve(result).await?;
        }
        Ok((status, record))
    }

    /// Like [`Self::result`], but the values the scheduler keeps as blobs
    /// stay markers `{"$blob": "sha256:...", "size": n}`, to be read with
    /// [`Self::blob`] or [`Self::download_blob`]. A scheduler older than the
    /// blob store returns the values themselves.
    pub async fn result_refs(
        &self,
        job_id: &str,
    ) -> Result<(JobStatus, Option<JobRecord>), ClientError> {
        let r = self
            .call(Kind::Read, |mut c| async move {
                c.result(pb::JobRef {
                    job_id: job_id.to_owned(),
                    accept_blob_refs: true,
                })
                .await
            })
            .await?;
        let status = r
            .status
            .ok_or_else(|| ClientError::Data("result without status".into()))?;
        let record = if r.record_json.is_empty() {
            None
        } else {
            Some(
                serde_json::from_str(&r.record_json)
                    .map_err(|e| ClientError::Data(e.to_string()))?,
            )
        };
        Ok((status, record))
    }

    /// Replaces the markers of `value` by the values of their blobs.
    pub async fn resolve(&self, value: &mut serde_json::Value) -> Result<(), ClientError> {
        let found = blob::markers(value).map_err(|e| ClientError::Data(e.to_string()))?;
        for (place, r) in found {
            let bytes = self.blob(&r).await?;
            let part: serde_json::Value = serde_json::from_slice(&bytes)
                .map_err(|e| ClientError::Data(format!("blob {} is not JSON: {e}", r.id)))?;
            match place {
                blob::MarkerPlace::Whole => *value = part,
                blob::MarkerPlace::Key(k) => {
                    if let serde_json::Value::Object(m) = value {
                        m.insert(k, part);
                    }
                }
            }
        }
        Ok(())
    }

    /// The SHA-256 of the canonical JSON of `value` once its markers are
    /// replaced by the values of their blobs, as a blob id. The blobs are
    /// read through the hash, not kept: the canonical JSON of an object is
    /// that of its keys and values in order, whether a value is inline or in
    /// a blob.
    pub async fn value_sha256(
        &self,
        value: &serde_json::Value,
    ) -> Result<blob::BlobId, ClientError> {
        let found = blob::markers(value).map_err(|e| ClientError::Data(e.to_string()))?;
        if found.is_empty() {
            return Ok(blob::canonical_ref(value).id);
        }
        let serde_json::Value::Object(m) = value else {
            // The only marker of a value that is no object is the value.
            return Ok(found[0].1.id.clone());
        };
        if let [(blob::MarkerPlace::Whole, r)] = found.as_slice() {
            return Ok(r.id.clone());
        }
        let io = |e: std::io::Error| ClientError::Data(e.to_string());
        let mut out = blob::HashingWriter::new(std::io::sink());
        std::io::Write::write_all(&mut out, b"{").map_err(io)?;
        for (i, (k, v)) in m.iter().enumerate() {
            if i > 0 {
                std::io::Write::write_all(&mut out, b",").map_err(io)?;
            }
            blob::write_canonical(&serde_json::Value::String(k.clone()), &mut out).map_err(io)?;
            std::io::Write::write_all(&mut out, b":").map_err(io)?;
            match blob::BlobRef::from_marker(v) {
                Ok(Some(r)) => self.read_blob(&r, &mut out).await?,
                _ => blob::write_canonical(v, &mut out).map_err(io)?,
            }
        }
        std::io::Write::write_all(&mut out, b"}").map_err(io)?;
        Ok(out.finish().1.id)
    }

    /// Whether a scheduler holds the blob `id`, its size there, and whether
    /// a majority of the voters holds it (`durable`).
    pub async fn stat_blob(&self, id: &blob::BlobId) -> Result<pb::BlobStat, ClientError> {
        let request = pb::BlobRequest {
            id: id.to_string(),
            offset: 0,
        };
        self.call(Kind::LaterRead, |mut c| {
            let request = request.clone();
            async move { c.stat_blob(request).await }
        })
        .await
        .map_err(|e| unsupported(e, "StatBlob"))
    }

    /// The bytes of the blob `r`, checked against its name.
    pub async fn blob(&self, r: &blob::BlobRef) -> Result<Vec<u8>, ClientError> {
        let capacity = usize::try_from(r.size).unwrap_or(0).min(1 << 30);
        let mut bytes = Vec::with_capacity(capacity);
        self.read_blob(r, &mut bytes).await?;
        Ok(bytes)
    }

    /// Writes the blob `r` to the file `path`, checked against its name. The
    /// bytes go to `<path>.part` first, which is renamed once they are all
    /// there and checked.
    pub async fn download_blob(
        &self,
        r: &blob::BlobRef,
        path: &std::path::Path,
    ) -> Result<(), ClientError> {
        let mut part = path.as_os_str().to_owned();
        part.push(".part");
        let part = std::path::PathBuf::from(part);
        let io = |e: std::io::Error| ClientError::Data(format!("{}: {e}", part.display()));
        let file = std::fs::File::create(&part).map_err(io)?;
        let mut writer = std::io::BufWriter::with_capacity(1 << 20, file);
        let read = self.read_blob(r, &mut writer).await;
        let flushed = read.and_then(|()| {
            let file = writer.into_inner().map_err(|e| io(e.into_error()))?;
            file.sync_all().map_err(io)
        });
        if let Err(e) = flushed {
            let _ = std::fs::remove_file(&part);
            return Err(e);
        }
        std::fs::rename(&part, path).map_err(io)
    }

    /// Reads the blob `r` into `out`. A transfer cut off is taken up again at
    /// the byte it stopped at, from this or another scheduler, for as long as
    /// each attempt brings bytes; the whole is checked against the name.
    async fn read_blob<W: std::io::Write>(
        &self,
        r: &blob::BlobRef,
        out: W,
    ) -> Result<(), ClientError> {
        let mut out = blob::HashingWriter::new(out);
        let mut offset = 0u64;
        let mut stalled = 0;
        loop {
            let request = pb::BlobRequest {
                id: r.id.to_string(),
                offset,
            };
            let mut stream = self
                .call(Kind::LaterRead, |mut c| {
                    let request = request.clone();
                    async move { c.get_blob(request).await }
                })
                .await
                .map_err(|e| unsupported(e, "GetBlob"))?;
            let before = offset;
            let cut = loop {
                match tokio::time::timeout(CALL_TIMEOUT, stream.message()).await {
                    Ok(Ok(Some(chunk))) => {
                        offset += chunk.data.len() as u64;
                        if offset > r.size {
                            return Err(ClientError::Data(format!(
                                "blob {} has more than its {} bytes",
                                r.id, r.size
                            )));
                        }
                        std::io::Write::write_all(&mut out, &chunk.data)
                            .map_err(|e| ClientError::Data(e.to_string()))?;
                    }
                    Ok(Ok(None)) => break None,
                    Ok(Err(e)) => break Some(ClientError::from(e)),
                    Err(_) => {
                        break Some(ClientError::Rpc {
                            code: tonic::Code::Unavailable,
                            message: format!("no part of the blob came within {CALL_TIMEOUT:?}"),
                        });
                    }
                }
            };
            match cut {
                None if offset == r.size => break,
                None => {
                    return Err(ClientError::Data(format!(
                        "blob {} ended after {offset} of its {} bytes",
                        r.id, r.size
                    )));
                }
                Some(e) => {
                    stalled = if offset > before { 0 } else { stalled + 1 };
                    if stalled >= 3 {
                        return Err(e);
                    }
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
        }
        let (_, got) = out.finish();
        if got.id != r.id {
            return Err(ClientError::Data(format!(
                "blob {} has the SHA-256 {}; it was damaged on the way",
                r.id, got.id
            )));
        }
        Ok(())
    }

    /// The job that a submission with `request_id` created, as the committed
    /// state records it, or `None` when there is none (Issue #63). It creates
    /// nothing, unlike sending the submission again.
    pub async fn lookup_request(&self, request_id: &str) -> Result<Option<JobStatus>, ClientError> {
        let request = pb::RequestRef {
            request_id: request_id.to_owned(),
        };
        let answer = self
            .call(Kind::LaterRead, |mut c| {
                let request = request.clone();
                async move { c.lookup_request(request).await }
            })
            .await;
        match answer {
            Ok(status) => Ok(Some(status)),
            Err(ClientError::Rpc {
                code: tonic::Code::NotFound,
                ..
            }) => Ok(None),
            Err(e) => Err(unsupported(e, "LookupRequest")),
        }
    }

    /// A page of the jobs that pass the filters of `request`, from the
    /// committed state (Issue #63). Pass `next_page_token` of the answer as
    /// `page_token` for the next page, until it is empty; a page can hold fewer
    /// jobs than asked for, none even, before the end.
    pub async fn list_jobs(
        &self,
        request: &ListJobsRequest,
    ) -> Result<ListJobsResponse, ClientError> {
        self.call(Kind::LaterRead, |mut c| {
            let request = request.clone();
            async move { c.list_jobs(request).await }
        })
        .await
        .map_err(|e| unsupported(e, "ListJobs"))
    }

    pub async fn cancel(&self, job_id: &str) -> Result<JobStatus, ClientError> {
        self.call(Kind::Write, |mut c| async move {
            c.cancel(job_ref(job_id)).await
        })
        .await
    }

    pub async fn backends(&self) -> Result<ListBackendsResponse, ClientError> {
        self.call(Kind::Read, |mut c| async move {
            c.list_backends(pb::Empty {}).await
        })
        .await
    }

    pub async fn workers(&self) -> Result<ListWorkersResponse, ClientError> {
        self.call(Kind::Read, |mut c| async move {
            c.list_workers(pb::Empty {}).await
        })
        .await
    }

    /// Waits until the job is terminal and returns its record. If the job was LOST
    /// and resubmitted under its retry policy, the retry is followed when
    /// `follow_retries` is set.
    ///
    /// With a `timeout`, it returns [`ClientError::WaitTimeout`] by then
    /// (Issue #17): the deadline is fixed when the wait begins and bounds the
    /// `Status` and `Result` calls as well as the pauses between them. While no
    /// scheduler serves (`UNAVAILABLE`, as during an election), the wait goes on
    /// asking, for the `Result` of a terminal job as for its `Status` (Issue
    /// #93); without a timeout, it waits for as long as that lasts.
    pub async fn wait(
        &self,
        job_id: &str,
        timeout: Option<Duration>,
        follow_retries: bool,
    ) -> Result<JobRecord, ClientError> {
        let mut record = self.wait_refs(job_id, timeout, follow_retries).await?;
        if let Some(result) = &mut record.result {
            self.resolve(result).await?;
        }
        Ok(record)
    }

    /// Like [`Self::wait`], but the values kept as blobs stay markers, as
    /// [`Self::result_refs`] returns them.
    pub async fn wait_refs(
        &self,
        job_id: &str,
        timeout: Option<Duration>,
        follow_retries: bool,
    ) -> Result<JobRecord, ClientError> {
        let deadline = timeout.map(|t| tokio::time::Instant::now() + t);
        let mut current = job_id.to_owned();
        let expired = |job: &str| ClientError::WaitTimeout(timeout.unwrap_or_default(), job.into());
        let mut delay = Duration::from_millis(100);
        loop {
            match before(deadline, self.status(&current)).await {
                None => return Err(expired(&current)),
                Some(Err(ClientError::Rpc {
                    code: tonic::Code::Unavailable,
                    ..
                })) => {}
                Some(Err(e)) => return Err(e),
                Some(Ok(status)) if is_terminal(&status.state) => {
                    if follow_retries && status.state == "LOST" && !status.retried_as.is_empty() {
                        current = status.retried_as;
                        continue;
                    }
                    match before(deadline, self.result_refs(&current)).await {
                        None => return Err(expired(&current)),
                        // A failover between Status and Result (Issue #93):
                        // asked again after the pause, like Status.
                        Some(Err(ClientError::Rpc {
                            code: tonic::Code::Unavailable,
                            ..
                        })) => {}
                        Some(answer) => {
                            return answer?.1.ok_or_else(|| {
                                ClientError::Data("terminal job without record".into())
                            });
                        }
                    }
                }
                Some(Ok(_)) => {}
            }
            let pause = tokio::time::Instant::now() + delay;
            match deadline {
                Some(d) if d <= pause => {
                    tokio::time::sleep_until(d).await;
                    return Err(expired(&current));
                }
                _ => tokio::time::sleep_until(pause).await,
            }
            delay = (delay * 2).min(Duration::from_secs(2));
        }
    }
}

/// `work`, or `None` if `deadline` passes first.
async fn before<T>(
    deadline: Option<tokio::time::Instant>,
    work: impl std::future::Future<Output = T>,
) -> Option<T> {
    match deadline {
        Some(d) => tokio::time::timeout_at(d, work).await.ok(),
        None => Some(work.await),
    }
}

/// A job status as JSON, with empty strings shown as null.
pub fn status_json(s: &JobStatus) -> serde_json::Value {
    serde_json::json!({
        "job_id": s.job_id,
        "state": s.state,
        "backend": s.backend,
        "operation": s.operation,
        "attempt": s.attempt,
        "agent_id": (!s.agent_id.is_empty()).then_some(&s.agent_id),
        "error": serde_json::from_str::<serde_json::Value>(&s.error_json).ok(),
        "retried_as": (!s.retried_as.is_empty()).then_some(&s.retried_as),
        "cancel_requested": s.cancel_requested,
    })
}

pub fn backends_json(list: &ListBackendsResponse) -> serde_json::Value {
    list.backends
        .iter()
        .map(|b| {
            serde_json::json!({
                "name": b.name,
                "capabilities": serde_json::from_str::<serde_json::Value>(&b.capabilities_json).ok(),
                "available_limits": b.available_limits,
                "agent_ids": b.agent_ids,
            })
        })
        .collect()
}

pub fn workers_json(list: &ListWorkersResponse) -> serde_json::Value {
    list.workers
        .iter()
        .map(|w| {
            serde_json::json!({
                "agent_id": w.agent_id,
                "name": w.name,
                "host": w.host,
                "online": w.online,
                "last_heartbeat_unix_ms": w.last_heartbeat_unix_ms,
                "backends": w.backends.iter().map(|o| serde_json::json!({
                    "name": o.name,
                    "slots": o.slots,
                    "build": o.build,
                    "backend_version": o.backend_version,
                    "driver_version": o.driver_version,
                    "driver_sha256": o.driver_sha256,
                    "agent_build": o.agent_build,
                    "capabilities": serde_json::from_str::<serde_json::Value>(&o.capabilities_json).ok(),
                })).collect::<Vec<_>>(),
                "running_job_ids": w.running_job_ids,
            })
        })
        .collect()
}

fn job_ref(job_id: &str) -> pb::JobRef {
    pb::JobRef {
        job_id: job_id.to_owned(),
        accept_blob_refs: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_from_url() {
        assert_eq!(
            url_host("https://scheduler:50051").as_deref(),
            Some("scheduler")
        );
        assert_eq!(
            url_host("https://127.0.0.1:1/x").as_deref(),
            Some("127.0.0.1")
        );
        assert_eq!(url_host("https://[::1]:50051").as_deref(), Some("::1"));
        assert_eq!(
            url_host("https://example.org").as_deref(),
            Some("example.org")
        );
        assert!(is_terminal("LOST") && !is_terminal("RUNNING"));
        assert_eq!(
            split_urls(" https://a:1, https://b:2 ,"),
            ["https://a:1", "https://b:2"]
        );
    }
}

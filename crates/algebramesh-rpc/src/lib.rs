//! gRPC types for the scheduler API (`proto/algebramesh.proto`) and the transport
//! security shared by the scheduler, agents and clients (design §11):
//!
//! * TLS on every connection ([`server_tls`], [`client_tls`]). [`connect`] and
//!   [`connect_lazy`] open client channels whose timeout also covers a server
//!   that accepts the connection and never answers the TLS handshake.
//! * A bearer token in the `authorization` metadata ([`TokenInterceptor`] on the
//!   client side, [`TokenCheck`] on the server side). Clients and agents use
//!   different token sets, so a client token cannot pose as an agent.
//! * Optionally, a client certificate per agent (mutual TLS). [`server_tls`] then
//!   asks for client certificates signed by the agent CA, and [`peer_identity`]
//!   reads the agent name (the common name) and serial number from the verified
//!   certificate.
//! * [`generate_dev_pki`] creates a private CA and a server certificate for test and
//!   closed-network deployments, and [`issue_agent_certificate`] signs agent
//!   certificates with that CA.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioIo;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio::time::Sleep;
use tonic::codegen::Service;
use tonic::codegen::http::Uri;
use tonic::metadata::{AsciiMetadataValue, MetadataValue};
use tonic::service::Interceptor;
use tonic::transport::{
    Certificate, Channel, ClientTlsConfig, Endpoint, Identity, ServerTlsConfig,
};
use tonic::{Request, Status};

#[allow(clippy::all, clippy::unwrap_used)]
pub mod pb {
    tonic::include_proto!("algebramesh.v1");
}

pub const AUTHORIZATION: &str = "authorization";

/// Upper bound for one gRPC message in either direction. Job specifications and
/// records carry whole polynomials, which can exceed tonic's 4 MiB default.
pub const MAX_MESSAGE_BYTES: usize = 128 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum RpcSetupError {
    #[error("token contains characters not allowed in gRPC metadata")]
    InvalidToken,
    #[error("certificate generation failed: {0}")]
    Certificate(#[from] rcgen::Error),
    #[error(
        "invalid agent name {0:?}: use 1 to 64 characters from A-Z, a-z, 0-9, '.', '_' and '-'"
    )]
    InvalidName(String),
    #[error("validity must be between 1 and 3650 days")]
    InvalidValidity,
}

/// Adds `authorization: Bearer <token>` to every request.
#[derive(Clone)]
pub struct TokenInterceptor {
    value: AsciiMetadataValue,
}

impl TokenInterceptor {
    pub fn new(token: &str) -> Result<Self, RpcSetupError> {
        let value = MetadataValue::try_from(format!("Bearer {}", token.trim()))
            .map_err(|_| RpcSetupError::InvalidToken)?;
        Ok(Self { value })
    }
}

impl Interceptor for TokenInterceptor {
    fn call(&mut self, mut request: Request<()>) -> Result<Request<()>, Status> {
        request
            .metadata_mut()
            .insert(AUTHORIZATION, self.value.clone());
        Ok(request)
    }
}

/// The token a call presented, named by the first 8 bytes of its SHA-256 in
/// hexadecimal. [`TokenCheck`] puts it in the extensions of each request it
/// accepts, so that a service can tell the callers apart (the upload quota of
/// ADR 0021, B3) without holding the token itself.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TokenId(pub String);

impl TokenId {
    pub fn of(token: &[u8]) -> Self {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(token);
        Self(digest[..8].iter().map(|b| format!("{b:02x}")).collect())
    }
}

/// Server-side check of the bearer token against an allowed set.
#[derive(Clone)]
pub struct TokenCheck {
    tokens: Arc<Vec<Vec<u8>>>,
}

impl TokenCheck {
    pub fn new(tokens: impl IntoIterator<Item = String>) -> Self {
        Self {
            tokens: Arc::new(
                tokens
                    .into_iter()
                    .map(|t| t.trim().as_bytes().to_vec())
                    .filter(|t| !t.is_empty())
                    .collect(),
            ),
        }
    }

    fn accepts(&self, presented: &[u8]) -> bool {
        // Compare against every token without early exit.
        self.tokens
            .iter()
            .fold(false, |found, t| found | constant_time_eq(t, presented))
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

impl Interceptor for TokenCheck {
    fn call(&mut self, mut request: Request<()>) -> Result<Request<()>, Status> {
        let presented = request
            .metadata()
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(str::as_bytes);
        match presented {
            Some(token) if self.accepts(token) => {
                let id = TokenId::of(token);
                request.extensions_mut().insert(id);
                Ok(request)
            }
            _ => Err(Status::unauthenticated("missing or invalid bearer token")),
        }
    }
}

/// The server identity and, with `client_ca_pem`, verification of client
/// certificates signed by that CA. Client certificates stay optional at the TLS
/// layer, because clients of the public API do not present one; the agent
/// gateway requires them (see [`peer_identity`]).
pub fn server_tls(
    cert_pem: &[u8],
    key_pem: &[u8],
    client_ca_pem: Option<&[u8]>,
) -> ServerTlsConfig {
    let config = ServerTlsConfig::new().identity(Identity::from_pem(cert_pem, key_pem));
    match client_ca_pem {
        Some(ca) => config
            .client_ca_root(Certificate::from_pem(ca))
            .client_auth_optional(true),
        None => config,
    }
}

/// Trusts only `ca_pem` and checks that the server certificate names `domain`.
pub fn client_tls(ca_pem: &[u8], domain: &str) -> ClientTlsConfig {
    ClientTlsConfig::new()
        .ca_certificate(Certificate::from_pem(ca_pem))
        .domain_name(domain)
}

/// Connects to `endpoint` now. `timeout` bounds the name lookup and the TCP
/// connection together and, from their end, the TLS handshake, so connecting
/// takes at most twice `timeout`. A
/// connection that misses either fails as `UNAVAILABLE`, like a refused one, so
/// that callers move to the next scheduler.
///
/// Do not set [`Endpoint::connect_timeout`] on `endpoint`: tonic applies that
/// timeout to the TCP connection only (and, around a custom connector, reports
/// it as `UNKNOWN`). A port forwarded to a host that is down (an SSH tunnel, a
/// proxy) accepts the connection and never answers the handshake, which would
/// hold the caller until the forwarder gives up, often for minutes.
///
/// The connection also sends HTTP/2 pings ([`KEEP_ALIVE`]) and is closed when
/// one goes unanswered, failing the calls on it as `UNAVAILABLE`: a server cut
/// off from the network drops packets silently, and calls on an established
/// connection would otherwise wait for TCP to give up.
pub async fn connect(
    endpoint: &Endpoint,
    timeout: Duration,
) -> Result<Channel, tonic::transport::Error> {
    keep_alive(endpoint)
        .connect_with_connector(FirstByteConnector::new(timeout))
        .await
}

/// Like [`connect`], but connects on first use and reconnects after a failure,
/// each time with the same timeout.
pub fn connect_lazy(endpoint: &Endpoint, timeout: Duration) -> Channel {
    keep_alive(endpoint).connect_with_connector_lazy(FirstByteConnector::new(timeout))
}

/// Interval of the HTTP/2 pings, and how long an answer may take.
pub const KEEP_ALIVE: Duration = Duration::from_secs(5);

fn keep_alive(endpoint: &Endpoint) -> Endpoint {
    endpoint
        .clone()
        .http2_keep_alive_interval(KEEP_ALIVE)
        .keep_alive_timeout(KEEP_ALIVE)
        .keep_alive_while_idle(true)
}

/// TCP connections whose reads fail when the TLS handshake has not finished
/// within `timeout` of the TCP connection (Issues #29 and #87).
#[derive(Clone)]
struct FirstByteConnector {
    tcp: HttpConnector,
    timeout: Duration,
}

impl FirstByteConnector {
    fn new(timeout: Duration) -> Self {
        let mut tcp = HttpConnector::new();
        tcp.enforce_http(false);
        tcp.set_nodelay(true);
        tcp.set_connect_timeout(Some(timeout));
        Self { tcp, timeout }
    }
}

type BoxError = Box<dyn std::error::Error + Send + Sync>;

impl Service<Uri> for FirstByteConnector {
    type Response = TokioIo<FirstByteDeadline>;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, BoxError>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), BoxError>> {
        self.tcp.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, uri: Uri) -> Self::Future {
        let connecting = self.tcp.call(uri);
        let timeout = self.timeout;
        Box::pin(async move {
            // The name lookup and the TCP connection (both address families,
            // tried one after the other) under one timeout: the connector's own
            // covers each TCP attempt only.
            let stream = tokio::time::timeout(timeout, connecting)
                .await
                .map_err(|_| {
                    std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "the name lookup and the TCP connection took too long",
                    )
                })??
                .into_inner();
            // From the TCP connection, not from the start (Issue #87): a lazy
            // channel makes progress on its connection only while a call waits
            // for it (tower's buffer drops a cancelled call and stops polling),
            // so a peer whose calls time out quickly, as Raft's do, connects in
            // short stretches, and a deadline that also covered the TCP
            // connection left the handshake too little of them.
            Ok(TokioIo::new(FirstByteDeadline {
                stream,
                deadline: Some(Box::pin(tokio::time::sleep(timeout))),
                received: false,
                written: WrittenRecords::default(),
            }))
        })
    }
}

/// A TCP stream that fails reads with `TimedOut` once its deadline has passed,
/// until the TLS handshake is over (Issue #29).
///
/// This layer is below TLS and does not see the handshake's messages, but it
/// sees the record headers the client writes (Issue #87). It takes the
/// handshake as over at the first `application_data` record the client writes.
/// In TLS 1.3 every encrypted record carries that type, and the client's first
/// encrypted flight (its `Certificate`, if any, and its `Finished`) follows the
/// server's whole flight; in TLS 1.2 the client sends application data only
/// after the server's `Finished`. Before that the client writes only
/// `handshake` records (a `ClientHello`, a second one after a
/// `HelloRetryRequest`, the TLS 1.2 key exchange) and `change_cipher_spec`
/// records (the one of TLS 1.2, and the one RFC 8446 Appendix D.4 adds for
/// middleboxes, which rustls writes after the `ServerHello` and before a second
/// `ClientHello`), so none of them ends the deadline. Early data, which would
/// be `application_data` before the server's answer, is off in tonic's TLS
/// configuration.
struct FirstByteDeadline {
    stream: TcpStream,
    deadline: Option<Pin<Box<Sleep>>>,
    /// Whether anything was read since the connection was made.
    received: bool,
    written: WrittenRecords,
}

/// Content type of a TLS record that carries application data (RFC 8446 §5.1),
/// and of every encrypted TLS 1.3 record.
const APPLICATION_DATA: u8 = 23;

/// The record headers in the bytes written so far, followed only as far as the
/// first `application_data` record.
#[derive(Default)]
struct WrittenRecords {
    header: [u8; 5],
    /// Bytes of `header` written so far.
    header_len: usize,
    /// Bytes of the current record's body not yet written.
    body_left: usize,
}

impl WrittenRecords {
    /// Follows `bytes`, which the stream accepted; returns whether they
    /// begin an `application_data` record.
    fn application_data(&mut self, mut bytes: &[u8]) -> bool {
        while !bytes.is_empty() {
            if self.body_left > 0 {
                let n = self.body_left.min(bytes.len());
                self.body_left -= n;
                bytes = &bytes[n..];
                continue;
            }
            let n = (self.header.len() - self.header_len).min(bytes.len());
            self.header[self.header_len..self.header_len + n].copy_from_slice(&bytes[..n]);
            self.header_len += n;
            bytes = &bytes[n..];
            if self.header[0] == APPLICATION_DATA {
                return true;
            }
            if self.header_len == self.header.len() {
                self.body_left = u16::from_be_bytes([self.header[3], self.header[4]]).into();
                self.header_len = 0;
            }
        }
        false
    }
}

impl AsyncRead for FirstByteDeadline {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = &mut *self;
        let before = buf.filled().len();
        let polled = Pin::new(&mut this.stream).poll_read(cx, buf);
        if this.deadline.is_none() {
            return polled;
        }
        match polled {
            Poll::Ready(Ok(())) if buf.filled().len() > before => {
                this.received = true;
                Poll::Ready(Ok(()))
            }
            Poll::Pending => match this.deadline.as_mut().map(|d| d.as_mut().poll(cx)) {
                Some(Poll::Ready(())) => Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    if this.received {
                        "the server did not finish the TLS handshake in time"
                    } else {
                        "the server sent nothing after the connection was accepted"
                    },
                ))),
                _ => Poll::Pending,
            },
            other => other,
        }
    }
}

impl FirstByteDeadline {
    /// The stream accepted the first `n` bytes of `bufs`; ends the deadline if
    /// they begin an `application_data` record (the handshake is over).
    fn wrote<'a>(&mut self, bufs: impl IntoIterator<Item = &'a [u8]>, mut n: usize) {
        for buf in bufs {
            if self.deadline.is_none() || n == 0 {
                return;
            }
            let accepted = &buf[..n.min(buf.len())];
            n -= accepted.len();
            if self.written.application_data(accepted) {
                self.deadline = None;
            }
        }
    }
}

impl AsyncWrite for FirstByteDeadline {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let polled = Pin::new(&mut self.stream).poll_write(cx, buf);
        if let Poll::Ready(Ok(n)) = polled {
            self.wrote([buf], n);
        }
        polled
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<std::io::Result<usize>> {
        let polled = Pin::new(&mut self.stream).poll_write_vectored(cx, bufs);
        if let Poll::Ready(Ok(n)) = polled {
            self.wrote(bufs.iter().map(|b| &**b), n);
        }
        polled
    }

    fn is_write_vectored(&self) -> bool {
        self.stream.is_write_vectored()
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

/// PEM files of a private CA and a server certificate signed by it.
pub struct DevPki {
    pub ca_cert_pem: String,
    /// Needed only to issue agent certificates later; keep it offline.
    pub ca_key_pem: String,
    pub server_cert_pem: String,
    pub server_key_pem: String,
}

/// Creates a CA and a server certificate valid for `hosts` (DNS names or IPs).
pub fn generate_dev_pki(hosts: &[String]) -> Result<DevPki, RpcSetupError> {
    use rcgen::{
        BasicConstraints, CertificateParams, CertifiedIssuer, DnType, IsCa, KeyPair,
        KeyUsagePurpose,
    };
    let mut ca_params = CertificateParams::new(Vec::<String>::new())?;
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    ca_params
        .distinguished_name
        .push(DnType::CommonName, "AlgebraMesh private CA");
    ca_params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    let ca_key = KeyPair::generate()?;
    let ca_key_pem = ca_key.serialize_pem();
    let ca = CertifiedIssuer::self_signed(ca_params, ca_key)?;

    let mut server_params = CertificateParams::new(hosts.to_vec())?;
    server_params.distinguished_name.push(
        DnType::CommonName,
        hosts.first().map_or("algebramesh", |h| h),
    );
    let server_key = KeyPair::generate()?;
    let server_cert = server_params.signed_by(&server_key, &ca)?;
    Ok(DevPki {
        ca_cert_pem: ca.pem(),
        ca_key_pem,
        server_cert_pem: server_cert.pem(),
        server_key_pem: server_key.serialize_pem(),
    })
}

/// Signs a server certificate for `hosts` (DNS names or IPs) with an existing CA
/// of [`generate_dev_pki`], valid for `valid_days` days: for a scheduler added to
/// a cluster later. Returns the certificate and its key, as PEM.
pub fn issue_server_certificate(
    ca_cert_pem: &str,
    ca_key_pem: &str,
    hosts: &[String],
    valid_days: u32,
) -> Result<(String, String), RpcSetupError> {
    use rcgen::{CertificateParams, DnType, Issuer, KeyPair};
    if !(1..=3650).contains(&valid_days) {
        return Err(RpcSetupError::InvalidValidity);
    }
    let issuer = Issuer::from_ca_cert_pem(ca_cert_pem, KeyPair::from_pem(ca_key_pem)?)?;
    let mut params = CertificateParams::new(hosts.to_vec())?;
    params.distinguished_name.push(
        DnType::CommonName,
        hosts.first().map_or("algebramesh", |h| h),
    );
    let now = time::OffsetDateTime::now_utc();
    params.not_before = now - time::Duration::minutes(5);
    params.not_after = now + time::Duration::days(valid_days.into());
    let key = KeyPair::generate()?;
    let cert = params.signed_by(&key, &issuer)?;
    Ok((cert.pem(), key.serialize_pem()))
}

/// A client certificate of one agent.
pub struct AgentCertificate {
    pub cert_pem: String,
    pub key_pem: String,
    /// Serial number as lowercase hex digits, as [`peer_identity`] reports it.
    pub serial: String,
}

/// Agent names double as certificate common names, so they are kept to a
/// conservative character set.
pub fn valid_agent_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Signs a client certificate for the agent `name` with the CA of
/// [`generate_dev_pki`], valid for `valid_days` days from now.
pub fn issue_agent_certificate(
    ca_cert_pem: &str,
    ca_key_pem: &str,
    name: &str,
    valid_days: u32,
) -> Result<AgentCertificate, RpcSetupError> {
    use rcgen::{
        CertificateParams, DnType, ExtendedKeyUsagePurpose, Issuer, KeyPair, KeyUsagePurpose,
        SerialNumber,
    };
    if !valid_agent_name(name) {
        return Err(RpcSetupError::InvalidName(name.to_owned()));
    }
    if !(1..=3650).contains(&valid_days) {
        return Err(RpcSetupError::InvalidValidity);
    }
    let issuer = Issuer::from_ca_cert_pem(ca_cert_pem, KeyPair::from_pem(ca_key_pem)?)?;
    let mut params = CertificateParams::new(Vec::<String>::new())?;
    params.distinguished_name.push(DnType::CommonName, name);
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    // 128 random bits; the first byte is in 1..=127 so that the DER integer is
    // positive and minimal, and the hex form is the same everywhere.
    let mut serial = random_bytes()?[..16].to_vec();
    serial[0] = (serial[0] & 0x7f).max(1);
    params.serial_number = Some(SerialNumber::from(serial.clone()));
    let now = time::OffsetDateTime::now_utc();
    params.not_before = now - time::Duration::minutes(5);
    params.not_after = now + time::Duration::days(valid_days.into());
    let key = KeyPair::generate()?;
    let cert = params.signed_by(&key, &issuer)?;
    Ok(AgentCertificate {
        cert_pem: cert.pem(),
        key_pem: key.serialize_pem(),
        serial: hex(&serial),
    })
}

/// Name and serial number of a verified peer certificate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerIdentity {
    pub name: String,
    pub serial: String,
}

/// Reads the common name and serial number of the certificate `der` (the leaf,
/// as rustls reports it first). Pass only certificates the TLS layer verified.
pub fn peer_identity(der: &[u8]) -> Option<PeerIdentity> {
    let (_, cert) = x509_parser::parse_x509_certificate(der).ok()?;
    identity_of(&cert)
}

/// The common name of a PEM certificate, used as the default agent name.
pub fn certificate_name(pem: &[u8]) -> Option<String> {
    let (_, pem) = x509_parser::pem::parse_x509_pem(pem).ok()?;
    let cert = pem.parse_x509().ok()?;
    identity_of(&cert).map(|i| i.name)
}

fn identity_of(cert: &x509_parser::certificate::X509Certificate<'_>) -> Option<PeerIdentity> {
    let name = cert.subject().iter_common_name().next()?.as_str().ok()?;
    Some(PeerIdentity {
        name: name.to_owned(),
        serial: hex(cert.raw_serial()),
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// 32 bytes from the system CSPRNG, by way of a freshly generated private key.
fn random_bytes() -> Result<Vec<u8>, RpcSetupError> {
    use sha2::{Digest, Sha256};
    let key = rcgen::KeyPair::generate()?;
    Ok(Sha256::digest(key.serialize_der()).to_vec())
}

/// A random 256-bit token as 64 hex digits, derived with SHA-256 from a freshly
/// generated private key (which the system CSPRNG provides).
pub fn random_token() -> Result<String, RpcSetupError> {
    Ok(hex(&random_bytes()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn an_accepted_token_is_named_in_the_request() {
        let mut check = TokenCheck::new(["first".to_owned(), "second".to_owned()]);
        let with = |token: &str| {
            let mut r = Request::new(());
            r.metadata_mut()
                .insert(AUTHORIZATION, format!("Bearer {token}").parse().unwrap());
            r
        };
        let first = check.call(with("first")).unwrap();
        let id = first.extensions().get::<TokenId>().unwrap().clone();
        assert_eq!(id, TokenId::of(b"first"));
        assert_eq!(id.0.len(), 16);
        let second = check.call(with("second")).unwrap();
        assert_ne!(second.extensions().get::<TokenId>(), Some(&id));
        assert!(check.call(with("third")).is_err());
    }

    /// A listener that accepts connections and never answers, like a forwarded
    /// port whose target is down.
    async fn silent_listener() -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("https://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((socket, _)) = listener.accept().await {
                held.push(socket);
            }
        });
        (url, task)
    }

    fn silent_endpoint(url: &str) -> Endpoint {
        let pki = generate_dev_pki(&["localhost".to_string()]).unwrap();
        Endpoint::from_shared(url.to_owned())
            .unwrap()
            .tls_config(client_tls(pki.ca_cert_pem.as_bytes(), "localhost"))
            .unwrap()
    }

    const TIMEOUT: Duration = Duration::from_millis(300);

    /// A listener that sends `bytes` on every connection and then stays
    /// silent with the connection open (Issue #29).
    async fn stalling_listener(bytes: &'static [u8]) -> (String, tokio::task::JoinHandle<()>) {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("https://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((mut socket, _)) = listener.accept().await {
                let _ = socket.write_all(bytes).await;
                held.push(socket);
            }
        });
        (url, task)
    }

    #[tokio::test]
    async fn a_handshake_that_stalls_after_some_bytes_still_times_out() {
        // One byte (the record type of a handshake); a part of a record header;
        // a whole header that promises 100 bytes, followed by ten of them.
        let mut partial_body = vec![0x16, 0x03, 0x03, 0x00, 0x64];
        partial_body.extend_from_slice(&[0x02; 10]);
        let partial_body: &'static [u8] = Box::leak(partial_body.into_boxed_slice());
        for bytes in [&[0x16u8][..], &[0x16, 0x03, 0x03][..], partial_body] {
            let (url, task) = stalling_listener(bytes).await;
            let started = Instant::now();
            let result = tokio::time::timeout(
                Duration::from_secs(10),
                connect(&silent_endpoint(&url), TIMEOUT),
            )
            .await
            .expect("connect returned within 10 s");
            assert!(result.is_err(), "{} bytes", bytes.len());
            assert!(
                started.elapsed() < Duration::from_secs(3),
                "{:?}",
                started.elapsed()
            );
            // A lazy channel's call fails as UNAVAILABLE too.
            let mut client = pb::scheduler_client::SchedulerClient::new(connect_lazy(
                &silent_endpoint(&url),
                TIMEOUT,
            ));
            let status =
                tokio::time::timeout(Duration::from_secs(10), client.list_workers(pb::Empty {}))
                    .await
                    .expect("the call returned within 10 s")
                    .unwrap_err();
            assert_eq!(
                status.code(),
                tonic::Code::Unavailable,
                "{} bytes",
                bytes.len()
            );
            task.abort();
        }
    }

    /// A TLS 1.3 server that answers every `ClientHello` with its first record
    /// only (the `ServerHello`, or a `HelloRetryRequest` with `retry`) and then
    /// stays silent with the connection open (Issue #87). Each connection
    /// reports what the client wrote after that record, once the client has
    /// closed it.
    async fn tls13_server_that_stops_after_its_hello(
        retry: bool,
    ) -> (
        String,
        tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
        tokio::task::JoinHandle<()>,
    ) {
        use rustls::crypto::ring;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = rcgen::CertificateParams::new(vec!["localhost".to_string()])
            .unwrap()
            .self_signed(&key)
            .unwrap();
        let mut provider = ring::default_provider();
        if retry {
            // The client's key share is for X25519, so this asks for another.
            provider.kx_groups = vec![ring::kx_group::SECP256R1];
        }
        let config = Arc::new(
            rustls::ServerConfig::builder_with_provider(Arc::new(provider))
                .with_protocol_versions(&[&rustls::version::TLS13])
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(
                    vec![cert.der().clone()],
                    rustls::pki_types::PrivateKeyDer::Pkcs8(key.serialize_der().into()),
                )
                .unwrap(),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("https://{}", listener.local_addr().unwrap());
        let (report, reports) = tokio::sync::mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let config = config.clone();
                let report = report.clone();
                tokio::spawn(async move {
                    let mut tls = rustls::ServerConnection::new(config).unwrap();
                    let mut out = Vec::new();
                    let mut buf = [0u8; 4096];
                    while out.is_empty() {
                        let n = socket.read(&mut buf).await.unwrap();
                        assert!(n > 0, "the client closed before its ClientHello");
                        tls.read_tls(&mut &buf[..n]).unwrap();
                        tls.process_new_packets().unwrap();
                        while tls.wants_write() {
                            tls.write_tls(&mut out).unwrap();
                        }
                    }
                    let first = 5 + usize::from(u16::from_be_bytes([out[3], out[4]]));
                    socket.write_all(&out[..first]).await.unwrap();
                    let mut after = Vec::new();
                    let _ = socket.read_to_end(&mut after).await;
                    let _ = report.send(after);
                });
            }
        });
        (url, reports, task)
    }

    #[tokio::test]
    async fn the_deadline_outlasts_the_compatibility_change_cipher_spec() {
        for retry in [false, true] {
            let (url, mut reports, task) = tls13_server_that_stops_after_its_hello(retry).await;
            let started = Instant::now();
            let result = tokio::time::timeout(
                Duration::from_secs(10),
                connect(&silent_endpoint(&url), TIMEOUT),
            )
            .await
            .expect("connect returned within 10 s");
            assert!(result.is_err(), "retry {retry}");
            assert!(
                started.elapsed() < Duration::from_secs(3),
                "retry {retry}: {:?}",
                started.elapsed()
            );
            // The client did write the change_cipher_spec record of RFC 8446
            // Appendix D.4 after the server's hello (and, after a retry
            // request, its second ClientHello).
            let after = tokio::time::timeout(Duration::from_secs(10), reports.recv())
                .await
                .expect("the client closed the connection")
                .unwrap();
            assert!(
                after.starts_with(&[20, 3, 3, 0, 1, 1]),
                "retry {retry}: {after:?}"
            );
            assert_eq!(after.len() > 6, retry, "retry {retry}: {after:?}");
            // A lazy channel's call fails as UNAVAILABLE too.
            let mut client = pb::scheduler_client::SchedulerClient::new(connect_lazy(
                &silent_endpoint(&url),
                TIMEOUT,
            ));
            let status =
                tokio::time::timeout(Duration::from_secs(10), client.list_workers(pb::Empty {}))
                    .await
                    .expect("the call returned within 10 s")
                    .unwrap_err();
            assert_eq!(status.code(), tonic::Code::Unavailable, "retry {retry}");
            task.abort();
        }
    }

    #[test]
    fn written_records_end_at_the_first_application_data_record() {
        let mut written = WrittenRecords::default();
        // A handshake record of 3 bytes, split inside its header and its body.
        assert!(!written.application_data(&[22, 3]));
        assert!(!written.application_data(&[3, 0, 3, 1]));
        assert!(!written.application_data(&[2, 3]));
        // A change_cipher_spec record, then an application_data record.
        assert!(!written.application_data(&[20, 3, 3, 0, 1, 1]));
        assert!(written.application_data(&[23, 3, 3, 0, 1, 0]));
        // An application_data record behind others in the same write.
        let mut written = WrittenRecords::default();
        assert!(written.application_data(&[22, 3, 1, 0, 1, 9, 20, 3, 3, 0, 1, 1, 23, 3]));
    }

    #[tokio::test]
    async fn a_server_certificate_from_an_existing_ca_is_trusted() {
        let pki = generate_dev_pki(&["sched-a".to_string()]).unwrap();
        let (cert, key) = issue_server_certificate(
            &pki.ca_cert_pem,
            &pki.ca_key_pem,
            &["sched-d".to_string()],
            30,
        )
        .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tonic::transport::Server::builder()
            .tls_config(server_tls(cert.as_bytes(), key.as_bytes(), None))
            .unwrap()
            .add_service(pb::scheduler_server::SchedulerServer::new(Unimplemented(
                Duration::ZERO,
            )))
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener));
        let task = tokio::spawn(server);
        let endpoint = Endpoint::from_shared(format!("https://127.0.0.1:{port}"))
            .unwrap()
            .tls_config(client_tls(pki.ca_cert_pem.as_bytes(), "sched-d"))
            .unwrap();
        // The handshake succeeds; the call reaches the (unimplemented) service.
        let channel = connect(&endpoint, Duration::from_secs(5)).await.unwrap();
        let status = pb::scheduler_client::SchedulerClient::new(channel)
            .list_workers(pb::Empty {})
            .await
            .unwrap_err();
        assert_eq!(status.code(), tonic::Code::Unimplemented);
        task.abort();
    }

    /// A connection that finished its handshake outlives the deadline, idle
    /// or not, on an eager and on a lazy channel, in TLS 1.3 and in TLS 1.2,
    /// where the client's first `application_data` record is the HTTP/2
    /// preface (Issue #87). Each call takes three times the deadline: on a
    /// connection that kept it, the call would fail (a channel connects again
    /// for the next call, so a quick call after an idle time would not show
    /// it).
    #[tokio::test]
    async fn an_established_connection_outlives_the_deadline() {
        use rustls::pki_types::pem::PemObject;
        use rustls::pki_types::{CertificateDer, PrivateKeyDer};
        let pki = generate_dev_pki(&["sched-a".to_string()]).unwrap();
        for version in [&rustls::version::TLS13, &rustls::version::TLS12] {
            let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_protocol_versions(&[version])
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from_pem_slice(pki.server_cert_pem.as_bytes()).unwrap()],
                PrivateKeyDer::from_pem_slice(pki.server_key_pem.as_bytes()).unwrap(),
            )
            .unwrap();
            config.alpn_protocols = vec![b"h2".to_vec()];
            let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let (streams, incoming) = tokio::sync::mpsc::unbounded_channel();
            let accepting = tokio::spawn(async move {
                while let Ok((socket, _)) = listener.accept().await {
                    let acceptor = acceptor.clone();
                    let streams = streams.clone();
                    tokio::spawn(async move {
                        if let Ok(tls) = acceptor.accept(socket).await {
                            let _ = streams.send(Ok::<_, std::io::Error>(tls));
                        }
                    });
                }
            });
            let server = tokio::spawn(
                tonic::transport::Server::builder()
                    .add_service(pb::scheduler_server::SchedulerServer::new(Unimplemented(
                        TIMEOUT * 3,
                    )))
                    .serve_with_incoming(tokio_stream::wrappers::UnboundedReceiverStream::new(
                        incoming,
                    )),
            );
            let endpoint = Endpoint::from_shared(format!("https://127.0.0.1:{port}"))
                .unwrap()
                .tls_config(client_tls(pki.ca_cert_pem.as_bytes(), "sched-a"))
                .unwrap();
            for lazy in [false, true] {
                let channel = if lazy {
                    connect_lazy(&endpoint, TIMEOUT)
                } else {
                    connect(&endpoint, TIMEOUT).await.unwrap()
                };
                let mut client = pb::scheduler_client::SchedulerClient::new(channel);
                for _ in 0..2 {
                    let status = client.list_workers(pb::Empty {}).await.unwrap_err();
                    assert_eq!(
                        status.code(),
                        tonic::Code::Unimplemented,
                        "{version:?}, lazy {lazy}: {status}"
                    );
                    tokio::time::sleep(TIMEOUT * 3).await;
                }
            }
            accepting.abort();
            server.abort();
        }
    }

    /// Refuses every call; `list_workers` only after the given time.
    struct Unimplemented(Duration);

    #[tonic::async_trait]
    impl pb::scheduler_server::Scheduler for Unimplemented {
        async fn submit(
            &self,
            _: Request<pb::SubmitRequest>,
        ) -> Result<tonic::Response<pb::JobStatus>, Status> {
            Err(Status::unimplemented(""))
        }
        async fn status(
            &self,
            _: Request<pb::JobRef>,
        ) -> Result<tonic::Response<pb::JobStatus>, Status> {
            Err(Status::unimplemented(""))
        }
        async fn result(
            &self,
            _: Request<pb::JobRef>,
        ) -> Result<tonic::Response<pb::JobResult>, Status> {
            Err(Status::unimplemented(""))
        }
        async fn cancel(
            &self,
            _: Request<pb::JobRef>,
        ) -> Result<tonic::Response<pb::JobStatus>, Status> {
            Err(Status::unimplemented(""))
        }
        async fn list_backends(
            &self,
            _: Request<pb::Empty>,
        ) -> Result<tonic::Response<pb::ListBackendsResponse>, Status> {
            Err(Status::unimplemented(""))
        }
        async fn list_workers(
            &self,
            _: Request<pb::Empty>,
        ) -> Result<tonic::Response<pb::ListWorkersResponse>, Status> {
            tokio::time::sleep(self.0).await;
            Err(Status::unimplemented(""))
        }
        async fn lookup_request(
            &self,
            _: Request<pb::RequestRef>,
        ) -> Result<tonic::Response<pb::JobStatus>, Status> {
            Err(Status::unimplemented(""))
        }
        async fn list_jobs(
            &self,
            _: Request<pb::ListJobsRequest>,
        ) -> Result<tonic::Response<pb::ListJobsResponse>, Status> {
            Err(Status::unimplemented(""))
        }
        async fn put_blob(
            &self,
            _: Request<tonic::Streaming<pb::BlobUpload>>,
        ) -> Result<tonic::Response<pb::BlobRef>, Status> {
            Err(Status::unimplemented(""))
        }

        type GetBlobStream = tokio_stream::Empty<Result<pb::BlobChunk, Status>>;

        async fn get_blob(
            &self,
            _: Request<pb::BlobRequest>,
        ) -> Result<tonic::Response<Self::GetBlobStream>, Status> {
            Err(Status::unimplemented(""))
        }

        async fn stat_blob(
            &self,
            _: Request<pb::BlobRequest>,
        ) -> Result<tonic::Response<pb::BlobStat>, Status> {
            Err(Status::unimplemented(""))
        }
    }

    #[tokio::test]
    async fn the_timeout_covers_the_tls_handshake() {
        let (url, task) = silent_listener().await;
        let started = Instant::now();
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            connect(&silent_endpoint(&url), TIMEOUT),
        )
        .await
        .expect("connect returned within 10 s");
        assert!(result.is_err());
        assert!(started.elapsed() < Duration::from_secs(5));
        task.abort();
    }

    #[tokio::test]
    async fn a_lazy_channel_fails_unavailable_when_the_handshake_hangs() {
        let (url, task) = silent_listener().await;
        let mut client = pb::scheduler_client::SchedulerClient::new(connect_lazy(
            &silent_endpoint(&url),
            TIMEOUT,
        ));
        let status =
            tokio::time::timeout(Duration::from_secs(10), client.list_workers(pb::Empty {}))
                .await
                .expect("the call returned within 10 s")
                .unwrap_err();
        assert_eq!(status.code(), tonic::Code::Unavailable);
        task.abort();
    }

    #[test]
    fn token_check_accepts_only_known_tokens() {
        let mut check = TokenCheck::new(["secret".to_string()]);
        let mut good = TokenInterceptor::new("secret").unwrap();
        let mut bad = TokenInterceptor::new("other").unwrap();
        assert!(check.call(good.call(Request::new(())).unwrap()).is_ok());
        assert_eq!(
            check
                .call(bad.call(Request::new(())).unwrap())
                .unwrap_err()
                .code(),
            tonic::Code::Unauthenticated
        );
        assert!(check.call(Request::new(())).is_err());
        assert!(
            TokenCheck::new([String::new()])
                .call(Request::new(()))
                .is_err()
        );
    }

    #[test]
    fn dev_pki_and_tokens() {
        let pki = generate_dev_pki(&["localhost".into(), "127.0.0.1".into()]).unwrap();
        assert!(pki.ca_cert_pem.starts_with("-----BEGIN CERTIFICATE-----"));
        assert!(pki.server_key_pem.contains("PRIVATE KEY"));
        let a = random_token().unwrap();
        let b = random_token().unwrap();
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
    }

    #[test]
    fn agent_certificates_carry_name_and_serial() {
        let pki = generate_dev_pki(&["localhost".into()]).unwrap();
        let cert =
            issue_agent_certificate(&pki.ca_cert_pem, &pki.ca_key_pem, "worker-a", 30).unwrap();
        assert_eq!(cert.serial.len(), 32);
        assert_eq!(
            certificate_name(cert.cert_pem.as_bytes()).as_deref(),
            Some("worker-a")
        );
        let (_, pem) = x509_parser::pem::parse_x509_pem(cert.cert_pem.as_bytes()).unwrap();
        assert_eq!(
            peer_identity(&pem.contents),
            Some(PeerIdentity {
                name: "worker-a".into(),
                serial: cert.serial.clone(),
            })
        );
        let x509 = pem.parse_x509().unwrap();
        assert_eq!(
            x509.issuer()
                .iter_common_name()
                .next()
                .unwrap()
                .as_str()
                .unwrap(),
            "AlgebraMesh private CA"
        );
        let other =
            issue_agent_certificate(&pki.ca_cert_pem, &pki.ca_key_pem, "worker-a", 30).unwrap();
        assert_ne!(other.serial, cert.serial);
    }

    #[test]
    fn agent_names_and_validity_are_checked() {
        let pki = generate_dev_pki(&["localhost".into()]).unwrap();
        let long = "n".repeat(65);
        for name in ["", "a b", "x/y", long.as_str()] {
            assert!(matches!(
                issue_agent_certificate(&pki.ca_cert_pem, &pki.ca_key_pem, name, 30),
                Err(RpcSetupError::InvalidName(_))
            ));
        }
        assert!(matches!(
            issue_agent_certificate(&pki.ca_cert_pem, &pki.ca_key_pem, "w", 0),
            Err(RpcSetupError::InvalidValidity)
        ));
    }
}

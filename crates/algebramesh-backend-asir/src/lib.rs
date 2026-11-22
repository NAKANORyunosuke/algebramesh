//! Risa/Asir backend over OpenXM (design §8).
//!
//! Each job uses its own OpenXM session: the backend waits for (or starts) an
//! `ox_launch`, checks the server's mathcap, runs one allow-listed function and shuts
//! the session down. On timeout or cancellation the server is killed and the session
//! is discarded, never reused.

pub mod convert;
pub mod operations;

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use algebramesh_core::check::check_result;
use algebramesh_core::{
    BackendInfo, Capabilities, CasBackend, CasError, CasRequest, CasResult, ExecutionContext,
};
use algebramesh_openxm_client::{
    LocalLauncher, LocalSession, Mathcap, OxError, OxSession, ReverseConnectListener,
    SessionOptions, spawn_local,
};
use algebramesh_openxm_protocol::{Cmo, SmCommand};

use crate::operations::{OPERATIONS, Operation, REQUIRED_CMO_TAGS};

pub const NAME: &str = "asir";
const DRIVER_VERSION: &str = concat!("algebramesh-backend-asir ", env!("CARGO_PKG_VERSION"));

/// Called with the control and data addresses once the listeners are bound, so the
/// caller can tell the worker where to connect.
pub type ListeningHook = Arc<dyn Fn(SocketAddr, SocketAddr) + Send + Sync>;

#[derive(Clone)]
pub enum AsirEndpoint {
    /// Bind listeners and wait for `ox_launch <host> 0 <control> <data> ox_asir 0`.
    ReverseConnect {
        bind: IpAddr,
        control_port: u16,
        data_port: u16,
        on_listening: Option<ListeningHook>,
    },
    /// Start `ox_launch` on this host.
    SpawnLocal(LocalLauncher),
}

#[derive(Clone)]
pub struct AsirConfig {
    pub endpoint: AsirEndpoint,
    /// How long to wait for the worker to connect and the server to start.
    pub accept_timeout: Duration,
    /// Image digest or source commit of the Asir installation, recorded as provenance.
    pub build: Option<String>,
    pub session: SessionOptions,
}

pub struct AsirBackend {
    config: AsirConfig,
}

// One value per job, so the size difference between variants does not matter.
#[allow(clippy::large_enum_variant)]
enum Opened {
    Remote(OxSession),
    Local(LocalSession),
}

impl Opened {
    fn session(&mut self) -> &mut OxSession {
        match self {
            Self::Remote(s) => s,
            Self::Local(l) => &mut l.session,
        }
    }

    async fn close(self, kill: bool) {
        let grace = Duration::from_secs(5);
        let result = match (self, kill) {
            (Self::Remote(s), false) => s.shutdown().await,
            (Self::Remote(s), true) => s.kill().await,
            (Self::Local(l), false) => l.shutdown(grace).await,
            (Self::Local(l), true) => l.kill(grace).await,
        };
        if let Err(e) = result {
            tracing::warn!("closing the OpenXM session failed: {e}");
        }
    }
}

struct Executed {
    raw: Cmo,
    mathcap: Mathcap,
    version: Option<String>,
    cas_time: Duration,
}

enum Outcome<T> {
    Done(Result<T, CasError>),
    Cancelled,
    TimedOut(Duration),
}

/// Runs `work` until it finishes, `ctx` is cancelled or its deadline passes.
async fn bounded<T>(
    work: impl std::future::Future<Output = Result<T, CasError>>,
    ctx: &ExecutionContext,
) -> Outcome<T> {
    tokio::select! {
        r = work => Outcome::Done(r),
        () = ctx.cancel.cancelled() => Outcome::Cancelled,
        () = ctx.expired() => Outcome::TimedOut(ctx.timeout.unwrap_or_default()),
    }
}

impl AsirBackend {
    pub fn new(config: AsirConfig) -> Self {
        Self { config }
    }

    async fn open(&self) -> Result<Opened, OxError> {
        match &self.config.endpoint {
            AsirEndpoint::ReverseConnect {
                bind,
                control_port,
                data_port,
                on_listening,
            } => {
                let listener = ReverseConnectListener::bind(
                    *bind,
                    *control_port,
                    *data_port,
                    self.config.session,
                )
                .await?;
                if let Some(hook) = on_listening {
                    hook(listener.control_addr(), listener.data_addr());
                }
                Ok(Opened::Remote(
                    listener.accept(self.config.accept_timeout).await?,
                ))
            }
            AsirEndpoint::SpawnLocal(launcher) => Ok(Opened::Local(
                spawn_local(launcher, self.config.accept_timeout, self.config.session).await?,
            )),
        }
    }
}

fn ox_to_cas(error: OxError) -> CasError {
    match error {
        OxError::Server(_) => CasError::Cas(error.to_string()),
        OxError::Closed { .. } | OxError::Io { .. } => CasError::Lost(error.to_string()),
        OxError::Decode { .. } => CasError::UnsupportedResult(error.to_string()),
        other => CasError::Unavailable(other.to_string()),
    }
}

async fn run(session: &mut OxSession, op: &Operation) -> Result<Executed, CasError> {
    let mathcap = session.mathcap().await.map_err(ox_to_cas)?;
    for command in [SmCommand::ExecuteFunction, SmCommand::PopCmo] {
        if !mathcap.supports_command(command) {
            return Err(CasError::Unavailable(format!(
                "the server does not list {command:?} in its mathcap"
            )));
        }
    }
    let missing: Vec<u32> = REQUIRED_CMO_TAGS
        .iter()
        .copied()
        .filter(|t| !mathcap.supports_cmo(*t))
        .collect();
    if !missing.is_empty() {
        return Err(CasError::Unavailable(format!(
            "the server mathcap lacks CMO tags {missing:?}"
        )));
    }

    let version = match session.execute_function("version", vec![]).await {
        Ok(Cmo::Zz(v)) => Some(v.to_string()),
        Ok(Cmo::Int32(v)) => Some(v.to_string()),
        Ok(_) | Err(OxError::Server(_)) => None,
        Err(e) => return Err(ox_to_cas(e)),
    };

    let (function, args) = op.function();
    let started = Instant::now();
    let raw = session
        .execute_function(function, args)
        .await
        .map_err(ox_to_cas)?;
    Ok(Executed {
        raw,
        mathcap,
        version,
        cas_time: started.elapsed(),
    })
}

#[async_trait::async_trait]
impl CasBackend for AsirBackend {
    fn name(&self) -> &'static str {
        NAME
    }

    fn driver_version(&self) -> String {
        DRIVER_VERSION.into()
    }

    /// With `SpawnLocal`, starts one session to check the mathcap and read the
    /// Asir version. With `ReverseConnect` there is no worker to ask yet.
    async fn prepare(&self) -> Result<Option<String>, CasError> {
        if !matches!(self.config.endpoint, AsirEndpoint::SpawnLocal(_)) {
            return Ok(None);
        }
        let mut opened = self.open().await.map_err(ox_to_cas)?;
        let mathcap = opened.session().mathcap().await.map_err(ox_to_cas);
        let version = opened
            .session()
            .execute_function("version", vec![])
            .await
            .map_err(ox_to_cas);
        opened.close(false).await;
        let missing: Vec<u32> = REQUIRED_CMO_TAGS
            .iter()
            .copied()
            .filter(|t| !mathcap.as_ref().is_ok_and(|m| m.supports_cmo(*t)))
            .collect();
        if !missing.is_empty() {
            return Err(CasError::Unavailable(format!(
                "the server mathcap lacks CMO tags {missing:?}"
            )));
        }
        Ok(match version? {
            Cmo::Zz(v) => Some(v.to_string()),
            Cmo::Int32(v) => Some(v.to_string()),
            _ => None,
        })
    }

    fn capabilities(&self) -> Capabilities {
        let mut capabilities = Capabilities {
            operations: OPERATIONS.iter().map(|s| s.to_string()).collect(),
            supported_types: vec![
                "integer".into(),
                "rational".into(),
                "polynomial[ZZ]".into(),
                "polynomial[QQ]".into(),
                "factorization".into(),
            ],
            max_precision: None,
            enforced_limits: vec![algebramesh_core::LIMIT_WALL_SECONDS.into()],
            ..Capabilities::default()
        };
        for d in OPERATIONS
            .iter()
            .filter_map(|op| algebramesh_ops::descriptor(op))
        {
            capabilities.declare(d.name, d.arguments, d.result);
        }
        capabilities
    }

    async fn execute(&self, job: CasRequest, ctx: ExecutionContext) -> Result<CasResult, CasError> {
        // Validate before contacting any worker.
        let op = Operation::parse(&job.operation, &job.arguments)?;
        // limits.memory_mb is not enforced (one OpenXM session serves many jobs);
        // the common runner refuses jobs that ask for it.
        let mut log = Vec::new();

        // The deadline of the job covers opening the session, the computation and
        // the check (ADR 0016).
        let mut opened = tokio::select! {
            opened = self.open() => opened.map_err(ox_to_cas)?,
            () = ctx.cancel.cancelled() => return Err(CasError::Cancelled),
            () = ctx.expired() => return Err(ctx.timed_out()),
        };
        let host = opened.session().peer_addr().to_string();
        log.push(format!("OpenXM session established with {host}"));

        let outcome = bounded(run(opened.session(), &op), &ctx).await;
        let executed = match outcome {
            Outcome::Done(result) => {
                // A session that saw a protocol or I/O error is killed, not shut down.
                let poisoned = opened.session().is_poisoned();
                opened.close(poisoned).await;
                result?
            }
            Outcome::Cancelled => {
                opened.session().poison("cancelled");
                opened.close(true).await;
                return Err(CasError::Cancelled);
            }
            Outcome::TimedOut(after) => {
                opened.session().poison("timed out");
                opened.close(true).await;
                return Err(CasError::Timeout(after.as_secs()));
            }
        };
        log.push(format!(
            "server {} (mathcap version {})",
            executed.mathcap.system.as_deref().unwrap_or("unknown"),
            executed
                .mathcap
                .version
                .map_or_else(|| "unknown".into(), |v| v.to_string())
        ));
        log.push(format!(
            "{} returned after {:.3} s",
            op.function().0,
            executed.cas_time.as_secs_f64()
        ));

        // Converting and checking the result is CPU-bound; keep it off the async
        // runtime and under the same deadline. The session is already closed.
        let started = Instant::now();
        let value = {
            let op = op.clone();
            let raw = executed.raw;
            check_result(&ctx, move |interrupt| op.interpret(&raw, interrupt)).await?
        };
        log.push(format!(
            "result converted and checked in {:.3} s",
            started.elapsed().as_secs_f64()
        ));

        Ok(CasResult {
            value,
            backend: BackendInfo {
                name: NAME.into(),
                backend_version: executed.version,
                driver_version: DRIVER_VERSION.into(),
                build: self.config.build.clone(),
                ..BackendInfo::default()
            },
            execution_host: Some(host),
            log,
            guarantee: None,
        })
    }
}

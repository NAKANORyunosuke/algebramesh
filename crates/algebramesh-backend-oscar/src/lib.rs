//! Julia/OSCAR backend (design §8).
//!
//! Loading OSCAR takes tens of seconds, so the backend keeps Julia processes
//! running `drivers/oscar/algebramesh.jl` and sends one JSON request per line.
//! A process is reused only after a clean answer; on timeout, cancellation or a
//! crash its process group is killed and it is discarded. Singular is reached
//! through OSCAR only; no separate Singular integration is made.
//!
//! The output of a process is read with limits (Issue #12): the bytes read from
//! stdout for one request (or for the start-up) count against
//! [`OscarConfig::max_output_bytes`], and a line is never held beyond that, so
//! a driver that writes a huge line cannot exhaust the worker's memory. Past the
//! limit the request fails with `RESOURCE_LIMIT` and the process, whose place in
//! the output is lost, is discarded. Lines kept for the job log are limited to
//! [`MAX_LOG_BYTES`]; stderr is read in pieces and only traced.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;

use algebramesh_core::check::{Interrupt, check_result};
use algebramesh_core::driver::protocol_payload;
use algebramesh_core::{
    BackendInfo, Capabilities, CasBackend, CasError, CasRequest, CasResult, ExecutionContext,
};
use algebramesh_math_types::{
    BaseOrder, Factorization, MAX_WEIGHT, MonomialOrder, Polynomial, PowerBudget, Rational,
    RingSpec,
};
use algebramesh_ops::ideal::{self, IdealRequest};
use algebramesh_ops::matrix::{self, MatrixRequest};
use algebramesh_ops::module::{self, ModuleRequest};
use algebramesh_ops::{FactorArguments, FactorResult};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

pub const NAME: &str = "oscar";
const DRIVER_VERSION: &str = concat!("algebramesh-backend-oscar ", env!("CARGO_PKG_VERSION"));
pub use algebramesh_ops::POLYNOMIAL_FACTOR;

const READY: &str = "ALGEBRAMESH_READY ";
const RESULT: &str = "ALGEBRAMESH_RESULT ";
const ERROR: &str = "ALGEBRAMESH_ERROR ";
/// Bytes of driver output kept in the job log; further lines are counted only.
pub const MAX_LOG_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone)]
pub struct OscarConfig {
    /// The `julia` executable.
    pub julia: PathBuf,
    /// `drivers/oscar/algebramesh.jl`.
    pub driver: PathBuf,
    /// Julia project with OSCAR installed (`--project`).
    pub project: Option<PathBuf>,
    /// Allowed time for Julia to load OSCAR.
    pub startup_timeout: Duration,
    /// Image digest or package versions, recorded as provenance.
    pub build: Option<String>,
    /// Bytes the driver may write to stdout for one request, or during the
    /// start-up, the answer line included.
    pub max_output_bytes: usize,
}

impl Default for OscarConfig {
    fn default() -> Self {
        Self {
            julia: "julia".into(),
            driver: "/opt/algebramesh/drivers/oscar/algebramesh.jl".into(),
            project: Some("/opt/algebramesh-oscar".into()),
            startup_timeout: Duration::from_secs(300),
            build: None,
            max_output_bytes: 256 * 1024 * 1024,
        }
    }
}

/// The process group of a Julia process and the task reading its stderr
/// (Issue #90, as Issue #30 for the OpenXM launcher). Julia, or a fake driver,
/// may start helpers; `kill_on_drop` stops only Julia itself. On Unix Julia
/// leads a group of its own, and the whole group is killed when this is
/// dropped: after a failed or abandoned start-up, when a process is discarded,
/// and when an idle process goes with the backend. The group is killed at most
/// once, before Julia is reaped, so that its ID cannot have been reused.
/// Elsewhere only Julia is stopped; Windows would need a job object, which is
/// not implemented.
struct ProcessGroup {
    #[cfg(unix)]
    pgid: Option<i32>,
    stderr: Option<tokio::task::AbortHandle>,
}

impl ProcessGroup {
    fn of(child: &Child) -> Self {
        #[cfg(unix)]
        {
            Self {
                pgid: child.id().and_then(|p| i32::try_from(p).ok()),
                stderr: None,
            }
        }
        #[cfg(not(unix))]
        {
            let _ = child;
            Self { stderr: None }
        }
    }

    /// Kills the group, the first time only, and stops reading stderr.
    fn kill(&mut self) {
        #[cfg(unix)]
        if let Some(pgid) = self.pgid.take() {
            use nix::sys::signal::{Signal, killpg};
            use nix::unistd::Pid;
            let _ = killpg(Pid::from_raw(pgid), Signal::SIGKILL);
        }
        if let Some(stderr) = self.stderr.take() {
            stderr.abort();
        }
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        self.kill();
    }
}

struct Process {
    /// Declared before `child`, so that it is dropped, and the group killed,
    /// before `child` can be reaped.
    group: ProcessGroup,
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    versions: Versions,
}

/// Reading a line went wrong.
#[derive(Debug)]
enum LineError {
    /// The output passed the budget; the rest of the line was not read.
    OverBudget,
    Io(std::io::Error),
}

/// Reads one line (without its line break), counting its bytes against
/// `budget`. Holds at most `budget` bytes of it: past that, stops with
/// [`LineError::OverBudget`]. `Ok(None)` at the end of the output.
async fn read_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    budget: &mut usize,
) -> Result<Option<String>, LineError> {
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf().await.map_err(LineError::Io)?;
        if available.is_empty() {
            return Ok((!line.is_empty()).then(|| String::from_utf8_lossy(&line).into_owned()));
        }
        let (take, done) = match available.iter().position(|&b| b == b'\n') {
            Some(i) => (i + 1, true),
            None => (available.len(), false),
        };
        if take > *budget {
            return Err(LineError::OverBudget);
        }
        *budget -= take;
        line.extend_from_slice(&available[..take]);
        reader.consume(take);
        if done {
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(Some(String::from_utf8_lossy(&line).into_owned()));
        }
    }
}

/// Lines of driver output for the job log, up to [`MAX_LOG_BYTES`].
#[derive(Default)]
struct OutputLog {
    kept: usize,
    dropped: usize,
}

impl OutputLog {
    fn push(&mut self, log: &mut Vec<String>, line: &str) {
        let entry = format!("stdout: {line}");
        if self.kept + entry.len() <= MAX_LOG_BYTES {
            self.kept += entry.len();
            log.push(entry);
        } else {
            self.dropped += 1;
        }
    }

    fn finish(self, log: &mut Vec<String>) {
        if self.dropped > 0 {
            log.push(format!(
                "{} more stdout lines not kept (log limit {MAX_LOG_BYTES} bytes)",
                self.dropped
            ));
        }
    }
}

fn over_limit(cap: usize) -> CasError {
    CasError::ResourceLimit(format!(
        "the OSCAR driver wrote more than {cap} bytes to stdout for one request"
    ))
}

#[derive(Debug, Clone, Deserialize)]
struct Versions {
    oscar_version: String,
    julia_version: String,
}

impl Process {
    async fn discard(mut self) {
        self.group.kill();
        let _ = self.child.kill().await;
    }
}

#[derive(Default)]
pub struct OscarBackend {
    config: Option<OscarConfig>,
    idle: Mutex<Vec<Process>>,
}

impl OscarBackend {
    pub fn new(config: OscarConfig) -> Self {
        Self {
            config: Some(config),
            idle: Mutex::new(Vec::new()),
        }
    }

    fn take_idle(&self) -> Option<Process> {
        self.idle.lock().unwrap_or_else(|e| e.into_inner()).pop()
    }

    fn put_idle(&self, p: Process) {
        self.idle.lock().unwrap_or_else(|e| e.into_inner()).push(p);
    }

    async fn spawn(config: &OscarConfig, log: &mut Vec<String>) -> Result<Process, CasError> {
        let mut cmd = Command::new(&config.julia);
        if let Some(project) = &config.project {
            cmd.arg(format!("--project={}", project.display()));
        }
        cmd.arg("--startup-file=no")
            .arg(&config.driver)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        cmd.process_group(0);
        let mut child = cmd.spawn().map_err(|e| {
            CasError::Unavailable(format!("cannot start {}: {e}", config.julia.display()))
        })?;
        // From here on, an error or a cancellation (this future dropped) kills
        // the whole group (Issue #90); a ready process takes the guard over.
        let mut group = ProcessGroup::of(&child);
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err(CasError::Unavailable("missing driver pipes".into()));
        };
        // Julia warnings go to stderr; keep them in the agent log only. Read in
        // pieces, so that a long line is never held whole.
        let reader = tokio::spawn(async move {
            let mut stderr = stderr;
            let mut chunk = vec![0u8; 8 * 1024];
            while let Ok(n) = stderr.read(&mut chunk).await {
                if n == 0 {
                    break;
                }
                tracing::debug!(target: "oscar", "{}", String::from_utf8_lossy(&chunk[..n]));
            }
        });
        group.stderr = Some(reader.abort_handle());
        let mut stdout = BufReader::new(stdout);
        let cap = config.max_output_bytes;
        let ready = tokio::time::timeout(config.startup_timeout, async {
            let mut budget = cap;
            let mut kept = OutputLog::default();
            let ready = loop {
                match read_line(&mut stdout, &mut budget).await {
                    Ok(Some(line)) => match protocol_payload(&line, READY) {
                        Some(json) => break Ok(json.to_owned()),
                        None => kept.push(log, &line),
                    },
                    Err(LineError::OverBudget) => break Err(over_limit(cap)),
                    Ok(None) | Err(LineError::Io(_)) => {
                        break Err(CasError::Unavailable(
                            "the OSCAR driver exited during start-up".into(),
                        ));
                    }
                }
            };
            kept.finish(log);
            ready
        })
        .await
        .map_err(|_| {
            CasError::Unavailable(format!(
                "OSCAR did not start within {:?}",
                config.startup_timeout
            ))
        })??;
        let versions: Versions = serde_json::from_str(&ready)
            .map_err(|e| CasError::Unavailable(format!("bad ready line: {e}")))?;
        tracing::info!(oscar = %versions.oscar_version, julia = %versions.julia_version, "OSCAR driver ready");
        Ok(Process {
            group,
            child,
            stdin,
            stdout,
            versions,
        })
    }

    async fn checkout(
        &self,
        config: &OscarConfig,
        log: &mut Vec<String>,
    ) -> Result<Process, CasError> {
        match self.take_idle() {
            Some(p) => Ok(p),
            None => {
                log.push("starting a Julia/OSCAR process".into());
                Self::spawn(config, log).await
            }
        }
    }
}

/// Sends one request and reads lines until the answer, at most `cap` bytes of
/// them. Any error leaves the process at an unknown place in its output; the
/// caller discards it.
async fn ask(
    process: &mut Process,
    request: &serde_json::Value,
    cap: usize,
    log: &mut Vec<String>,
) -> Result<Result<serde_json::Value, String>, CasError> {
    let crashed = |e: std::io::Error| CasError::Cas(format!("the OSCAR driver failed: {e}"));
    let line = format!("{request}\n");
    process
        .stdin
        .write_all(line.as_bytes())
        .await
        .map_err(crashed)?;
    process.stdin.flush().await.map_err(crashed)?;
    let mut budget = cap;
    let mut kept = OutputLog::default();
    let answer = loop {
        let line = match read_line(&mut process.stdout, &mut budget).await {
            Ok(Some(line)) => line,
            Ok(None) => break Err(CasError::Cas("the OSCAR driver exited".into())),
            Err(LineError::OverBudget) => break Err(over_limit(cap)),
            Err(LineError::Io(e)) => break Err(crashed(e)),
        };
        if let Some(json) = protocol_payload(&line, RESULT) {
            break serde_json::from_str(json).map(Ok).map_err(|e| {
                CasError::UnsupportedResult(format!("driver result is not JSON: {e}"))
            });
        }
        if let Some(message) = protocol_payload(&line, ERROR) {
            break Ok(Err(message.to_owned()));
        }
        if !line.trim().is_empty() {
            kept.push(log, &line);
        }
    };
    kept.finish(log);
    answer
}

#[derive(Debug, Deserialize)]
struct DriverTerm {
    coefficient: String,
    exponents: Vec<u32>,
}

#[derive(Debug, Deserialize)]
struct DriverFactor {
    terms: Vec<DriverTerm>,
    multiplicity: u32,
}

#[derive(Debug, Deserialize)]
struct FactorReply {
    factors: Vec<DriverFactor>,
}

fn bad(e: impl std::fmt::Display) -> CasError {
    CasError::UnsupportedResult(e.to_string())
}

/// A job the driver can run.
#[derive(Debug, Clone)]
enum Job {
    Factor(Polynomial),
    /// `matrix.*` (Issue #57).
    Matrix(MatrixRequest),
    /// `ideal.*` (Issues #66, #67), with the items of the ordering of its
    /// ring.
    Ideal(IdealRequest, Vec<OrderItem>),
    /// `module.*`, `complex.*`, `ideal.hilbert_series` (Issues #97, #98),
    /// with the items of the ordering of its ring.
    Module(ModuleRequest, Vec<OrderItem>),
}

/// The operations this backend offers.
fn operations() -> Vec<&'static str> {
    let mut ops = vec![POLYNOMIAL_FACTOR];
    ops.extend(matrix::OPERATIONS);
    ops.extend(ideal::OPERATIONS);
    ops.extend(module::OPERATIONS);
    ops
}

fn parse(job: &CasRequest) -> Result<Job, CasError> {
    if matrix::OPERATIONS.contains(&job.operation.as_str()) {
        return MatrixRequest::parse(&job.operation, &job.arguments)
            .map(Job::Matrix)
            .map_err(CasError::InvalidArguments);
    }
    if ideal::OPERATIONS.contains(&job.operation.as_str()) {
        let request = IdealRequest::parse(&job.operation, &job.arguments)
            .map_err(CasError::InvalidArguments)?;
        let order = order_items(&request.ring().order, request.ring().nvars())?;
        return Ok(Job::Ideal(request, order));
    }
    if module::OPERATIONS.contains(&job.operation.as_str()) {
        let request = ModuleRequest::parse(&job.operation, &job.arguments)
            .map_err(CasError::InvalidArguments)?;
        let order = order_items(&request.ring().order, request.ring().nvars())?;
        return Ok(Job::Module(request, order));
    }
    parse_factor(job).map(Job::Factor)
}

fn parse_factor(job: &CasRequest) -> Result<Polynomial, CasError> {
    if job.operation != POLYNOMIAL_FACTOR {
        return Err(CasError::UnsupportedOperation {
            backend: NAME.into(),
            operation: job.operation.clone(),
        });
    }
    let invalid = |e: String| CasError::InvalidArguments(e);
    let args: FactorArguments =
        serde_json::from_value(job.arguments.clone()).map_err(|e| invalid(e.to_string()))?;
    let p =
        Polynomial::from_data(args.ring, &args.expression).map_err(|e| invalid(e.to_string()))?;
    if p.is_zero() {
        return Err(invalid("cannot factor the zero polynomial".into()));
    }
    Ok(p)
}

/// One item of the monomial ordering the OSCAR driver builds for a ring
/// (Issue #64). The driver receives it as `["weights", [w_1, ..., w_n]]` or as
/// `[name, first, last]`, with `name` one of `"lex"`, `"deglex"` and
/// `"degrevlex"` and `first..last` the variables `v[first:last]` (counted from
/// 1, as Julia does, both ends included). It compares by the items in turn:
/// a `"weights"` item, which comes first, is `weight_ordering(w, o)` with `o`
/// the ordering of the items after it, and the others are multiplied in
/// order, `name(v[first:last]) * ...` (a product compares by its first
/// factor first).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrderItem {
    /// `weight_ordering(w, o)`: by the weighted degree first, one weight for
    /// each variable.
    Weights(Vec<u32>),
    /// `lex`, `deglex` or `degrevlex` (for lex, grlex and grevlex) on the
    /// variables `v[first:last]`.
    Block {
        order: BaseOrder,
        first: usize,
        last: usize,
    },
}

impl Serialize for OrderItem {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Weights(w) => ("weights", w).serialize(serializer),
            Self::Block { order, first, last } => {
                let name = match order {
                    BaseOrder::Lex => "lex",
                    BaseOrder::Grlex => "deglex",
                    BaseOrder::Grevlex => "degrevlex",
                };
                (name, first, last).serialize(serializer)
            }
        }
    }
}

// OSCAR takes a weight only as an `Int32` ("InexactError: trunc(Int32,
// 2147483648)" in OSCAR 1.8.2); every weight a valid order has is one. A
// larger `MAX_WEIGHT` needs a check in `order_items`.
const _: () = assert!(MAX_WEIGHT <= i32::MAX.unsigned_abs());

/// The items of the ordering the OSCAR driver builds for `order` on `nvars`
/// variables (Issue #64): the driver builds the ring of an order-dependent
/// operation with them, and passes the ordering to every function that
/// depends on it, instead of relying on OSCAR's default (design §6, rule 2).
/// An order that is not valid for `nvars` variables, and so cannot be written
/// for OSCAR, is refused as invalid arguments before the driver is asked.
///
/// A ring without variables gets one variable of the driver's own (Issue #35);
/// every order of one variable is the same, and its item is `lex(v[1:1])`.
pub fn order_items(order: &MonomialOrder, nvars: usize) -> Result<Vec<OrderItem>, CasError> {
    order
        .validate(nvars)
        .map_err(|e| CasError::InvalidArguments(e.to_string()))?;
    let all = |order| OrderItem::Block {
        order,
        first: 1,
        last: nvars.max(1),
    };
    Ok(match order {
        _ if nvars == 0 => vec![all(BaseOrder::Lex)],
        MonomialOrder::Lex => vec![all(BaseOrder::Lex)],
        MonomialOrder::Grlex => vec![all(BaseOrder::Grlex)],
        MonomialOrder::Grevlex => vec![all(BaseOrder::Grevlex)],
        MonomialOrder::Weights { weights, then } => {
            vec![OrderItem::Weights(weights.clone()), all(*then)]
        }
        MonomialOrder::Block(blocks) => {
            let mut first = 1;
            blocks
                .iter()
                .map(|b| {
                    let item = OrderItem::Block {
                        order: b.order,
                        first,
                        last: first + b.count - 1,
                    };
                    first += b.count;
                    item
                })
                .collect()
        }
    })
}

fn request(job: &Job) -> serde_json::Value {
    match job {
        Job::Factor(p) => factor_request(p),
        Job::Matrix(m) => m.driver_json(),
        Job::Ideal(r, order) => r.driver_json(order),
        Job::Module(r, order) => r.driver_json(order),
    }
}

fn factor_request(p: &Polynomial) -> serde_json::Value {
    serde_json::json!({
        "operation": POLYNOMIAL_FACTOR,
        "domain": p.ring().domain.name(),
        "nvars": p.ring().nvars(),
        "terms": p.terms().map(|(e, c)| serde_json::json!({
            "coefficient": c.to_string(), "exponents": e,
        })).collect::<Vec<_>>(),
    })
}

/// Converts OSCAR's reply into the value, stopping at the checkpoints once
/// `interrupt` is raised. The value of a matrix or an ideal job is read by
/// the type of its operation; its certificate is checked after the backend.
fn interpret(
    job: &Job,
    reply: serde_json::Value,
    interrupt: &Interrupt,
) -> Result<serde_json::Value, CasError> {
    match job {
        Job::Factor(p) => interpret_factor(p, reply, interrupt),
        Job::Matrix(m) => m.value(&reply).map_err(bad),
        Job::Ideal(r, _) => r.value(&reply).map_err(bad),
        Job::Module(r, _) => r.value(&reply).map_err(bad),
    }
}

/// Converts OSCAR's reply to a factorization, stopping at the checkpoints once
/// `interrupt` is raised.
/// The exponents a driver returned, for `ring` (Issue #35). The driver gives a
/// ring without variables one variable of its own (a polynomial ring needs
/// one), so its terms carry one exponent, which must be 0; it is dropped.
fn ring_exponents(ring: &RingSpec, exponents: &[u32]) -> Result<Vec<u32>, CasError> {
    match exponents {
        [0] if ring.nvars() == 0 => Ok(Vec::new()),
        [_] if ring.nvars() == 0 => Err(bad(format!(
            "exponents {exponents:?} for a ring without variables"
        ))),
        _ => Ok(exponents.to_vec()),
    }
}

fn interpret_factor(
    input: &Polynomial,
    reply: serde_json::Value,
    interrupt: &Interrupt,
) -> Result<serde_json::Value, CasError> {
    let reply: FactorReply = serde_json::from_value(reply).map_err(bad)?;
    let ring = input.ring().clone();
    let mut constant = Rational::one();
    let mut budget = PowerBudget::for_coefficients(input.terms().map(|(_, c)| c));
    let mut factors = Vec::new();
    for f in &reply.factors {
        let terms = f
            .terms
            .iter()
            .map(|t| {
                Ok((
                    ring_exponents(&ring, &t.exponents)?,
                    t.coefficient.parse::<Rational>().map_err(bad)?,
                ))
            })
            .collect::<Result<Vec<_>, CasError>>()?;
        let p = Polynomial::from_terms(ring.clone(), terms).map_err(bad)?;
        if p.is_constant() {
            let c = p
                .terms()
                .next()
                .map(|(_, c)| c.clone())
                .unwrap_or_else(Rational::zero);
            constant = constant
                .mul_pow_with(&c, f.multiplicity, &mut budget, &mut || {
                    interrupt.checkpoint()
                })?
                .map_err(bad)?;
        } else {
            factors.push((p, f.multiplicity));
        }
    }
    interrupt.checkpoint()?;
    // The product is compared with the input after the backend, by
    // algebramesh-verify.
    let factorization = Factorization::new(ring, constant, factors).map_err(bad)?;
    serde_json::to_value(FactorResult::new(factorization)).map_err(bad)
}

enum Outcome {
    Answer(Result<Result<serde_json::Value, String>, CasError>),
    Cancelled,
    TimedOut,
}

#[async_trait::async_trait]
impl CasBackend for OscarBackend {
    fn name(&self) -> &'static str {
        NAME
    }

    fn driver_version(&self) -> String {
        DRIVER_VERSION.into()
    }

    fn driver_files(&self) -> Vec<std::path::PathBuf> {
        self.config.iter().map(|c| c.driver.clone()).collect()
    }

    fn capabilities(&self) -> Capabilities {
        if self.config.is_none() {
            return Capabilities::default();
        }
        let mut capabilities = Capabilities {
            operations: operations().into_iter().map(String::from).collect(),
            supported_types: vec![
                "polynomial[ZZ]".into(),
                "polynomial[QQ]".into(),
                "factorization".into(),
                "matrix[ZZ]".into(),
                "matrix[QQ]".into(),
                "ideal[QQ]".into(),
                "groebner_basis".into(),
                "normal_form".into(),
                "ideal_membership".into(),
                "submodule[QQ]".into(),
                "syzygies".into(),
                "free_resolution".into(),
                "hilbert_series".into(),
            ],
            max_precision: None,
            enforced_limits: vec![algebramesh_core::LIMIT_WALL_SECONDS.into()],
            ..Capabilities::default()
        };
        for d in operations()
            .into_iter()
            .filter_map(algebramesh_ops::descriptor)
        {
            capabilities.declare(d.name, d.arguments, d.result);
        }
        capabilities
    }

    async fn prepare(&self) -> Result<Option<String>, CasError> {
        let Some(config) = &self.config else {
            return Ok(None);
        };
        let mut log = Vec::new();
        let p = self.checkout(config, &mut log).await?;
        let version = format!(
            "OSCAR {} (Julia {})",
            p.versions.oscar_version, p.versions.julia_version
        );
        self.put_idle(p);
        Ok(Some(version))
    }

    async fn execute(&self, job: CasRequest, ctx: ExecutionContext) -> Result<CasResult, CasError> {
        let Some(config) = &self.config else {
            return Err(CasError::UnsupportedOperation {
                backend: NAME.into(),
                operation: job.operation,
            });
        };
        let input = parse(&job)?;
        let mut log = Vec::new();
        // The deadline of the job covers checkout, the answer and the check.
        let expiry = ctx.expired();
        tokio::pin!(expiry);

        let mut process = tokio::select! {
            p = self.checkout(config, &mut log) => p?,
            _ = ctx.cancel.cancelled() => return Err(CasError::Cancelled),
            () = &mut expiry => return Err(ctx.timed_out()),
        };
        let asked = tokio::time::Instant::now();
        let outcome = {
            let request = request(&input);
            tokio::select! {
                r = ask(&mut process, &request, config.max_output_bytes, &mut log) => Outcome::Answer(r),
                _ = ctx.cancel.cancelled() => Outcome::Cancelled,
                () = &mut expiry => Outcome::TimedOut,
            }
        };
        let reply = match outcome {
            Outcome::Answer(Ok(answer)) => {
                let versions = process.versions.clone();
                self.put_idle(process);
                (answer, versions)
            }
            Outcome::Answer(Err(e)) => {
                process.discard().await;
                return Err(e);
            }
            Outcome::Cancelled => {
                process.discard().await;
                return Err(CasError::Cancelled);
            }
            Outcome::TimedOut => {
                process.discard().await;
                return Err(ctx.timed_out());
            }
        };
        let (answer, versions) = reply;
        let value = answer.map_err(CasError::Cas)?;
        log.push(format!(
            "OSCAR answered after {:.3} s",
            asked.elapsed().as_secs_f64()
        ));
        let checked = tokio::time::Instant::now();
        let value =
            check_result(&ctx, move |interrupt| interpret(&input, value, interrupt)).await?;
        log.push(format!(
            "result converted and checked in {:.3} s",
            checked.elapsed().as_secs_f64()
        ));
        Ok(CasResult {
            value,
            backend: BackendInfo {
                name: NAME.into(),
                backend_version: Some(format!(
                    "OSCAR {} (Julia {})",
                    versions.oscar_version, versions.julia_version
                )),
                driver_version: DRIVER_VERSION.into(),
                build: config.build.clone(),
                ..BackendInfo::default()
            },
            execution_host: None,
            log,
            guarantee: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(arguments: serde_json::Value) -> CasRequest {
        CasRequest {
            job_id: algebramesh_core::JobId::new(),
            operation: POLYNOMIAL_FACTOR.into(),
            arguments,
            limits: Default::default(),
        }
    }

    /// Issue #91: a constant factor of multiplicity 2^32 - 1 costs no
    /// multiplication per unit of multiplicity. A constant 1 is absorbed, and
    /// a constant 3, whose power has more than 2^16 bits, is refused; both
    /// within seconds.
    #[test]
    fn a_constant_of_huge_multiplicity_is_absorbed_or_refused_at_once() {
        let input = parse(&job(serde_json::json!({
            "ring": {"domain": "QQ", "variables": ["x"], "order": "lex"},
            "expression": {"type": "polynomial", "terms": [
                {"coefficient": "1", "exponents": [1]}]}
        })))
        .unwrap();
        let reply = |c: &str| {
            serde_json::json!({"factors": [
                {"multiplicity": u32::MAX, "terms": [{"coefficient": c, "exponents": [0]}]},
                {"multiplicity": 1, "terms": [{"coefficient": "1", "exponents": [1]}]}]})
        };
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let one = interpret(&input, reply("1"), &Interrupt::none());
            let three = interpret(&input, reply("3"), &Interrupt::none());
            let _ = tx.send((one, three));
        });
        let (one, three) = rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the constant is not absorbed within 10 s");
        assert_eq!(one.unwrap()["constant"], "1");
        assert!(
            matches!(&three, Err(CasError::UnsupportedResult(m)) if m.contains("too large")),
            "{three:?}"
        );
    }

    #[test]
    fn interprets_reply_with_unit_and_constant_factors() {
        let input = parse(&job(serde_json::json!({
            "ring": {"domain": "QQ", "variables": ["x"], "order": "lex"},
            "expression": {"type": "polynomial", "terms": [
                {"coefficient": "1/2", "exponents": [2]},
                {"coefficient": "-1/2", "exponents": [0]}]}
        })))
        .unwrap();
        assert!(!request(&input).to_string().contains("\"x\""));
        let reply = serde_json::json!({"factors": [
            {"multiplicity": 1, "terms": [{"coefficient": "1/2", "exponents": [0]}]},
            {"multiplicity": 1, "terms": [{"coefficient": "1", "exponents": [1]}, {"coefficient": "1", "exponents": [0]}]},
            {"multiplicity": 1, "terms": [{"coefficient": "1", "exponents": [1]}, {"coefficient": "-1", "exponents": [0]}]}]});
        let value = interpret(&input, reply.clone(), &Interrupt::none()).unwrap();
        assert_eq!(value["constant"], "1/2");
        assert_eq!(value["factors"].as_array().unwrap().len(), 2);

        // A wrong product is written as it is and refused by the check after
        // the backend (Issue #47).
        let mut wrong = reply.clone();
        wrong["factors"][2]["terms"][1]["coefficient"] = "1".into();
        let value = interpret(&input, wrong, &Interrupt::none()).unwrap();
        let spec = algebramesh_core::JobSpec {
            schema_version: 1,
            backend: NAME.into(),
            operation: POLYNOMIAL_FACTOR.into(),
            arguments: serde_json::json!({
                "ring": {"domain": "QQ", "variables": ["x"], "order": "lex"},
                "expression": {"type": "polynomial", "terms": [
                    {"coefficient": "1/2", "exponents": [2]},
                    {"coefficient": "-1/2", "exponents": [0]}]}
            }),
            limits: Default::default(),
            retry_policy: Default::default(),
            environment: Default::default(),
        };
        let result = CasResult {
            value,
            backend: BackendInfo::default(),
            execution_host: None,
            log: vec![],
            guarantee: None,
        };
        use algebramesh_core::guarantee::ResultVerifier;
        assert!(matches!(
            algebramesh_verify::Registry::standard().verify(&spec, result, &Interrupt::none()),
            Err(CasError::UnsupportedResult(m)) if m.contains("multiply to")
        ));

        // Issue #48: a factor of multiplicity 0 keeps the product, and is
        // refused for its multiplicity.
        let mut mutated = reply;
        mutated["factors"].as_array_mut().unwrap().push(serde_json::json!(
            {"multiplicity": 0, "terms": [{"coefficient": "1", "exponents": [1]}, {"coefficient": "3", "exponents": [0]}]}));
        let value = interpret(&input, mutated, &Interrupt::none()).unwrap();
        let result = CasResult {
            value,
            backend: BackendInfo::default(),
            execution_host: None,
            log: vec![],
            guarantee: None,
        };
        assert!(matches!(
            algebramesh_verify::Registry::standard().verify(&spec, result, &Interrupt::none()),
            Err(CasError::UnsupportedResult(m)) if m.contains("multiplicity 0")
        ));
    }

    /// Issue #64: a ring whose order is malformed or does not fit its
    /// variables is refused as invalid arguments when the job is read, before
    /// the driver is asked, also for an operation that does not depend on the
    /// order.
    #[tokio::test]
    async fn a_ring_with_an_invalid_order_is_refused_before_running() {
        let factor = |order: serde_json::Value| {
            job(serde_json::json!({
                "ring": {"domain": "QQ", "variables": ["x", "y"], "order": order},
                "expression": {"type": "polynomial", "terms": [
                    {"coefficient": "1", "exponents": [2, 0]},
                    {"coefficient": "-1", "exponents": [0, 1]}]}
            }))
        };
        // Starting this executable would fail as unavailable.
        let backend = OscarBackend::new(OscarConfig {
            julia: "/nonexistent/julia".into(),
            ..OscarConfig::default()
        });
        for (order, why) in [
            (
                serde_json::json!({"weights": [-1, 0], "then": "lex"}),
                "the weight -1 is negative",
            ),
            (
                serde_json::json!({"weights": [1, 0, 0], "then": "lex"}),
                "3 weights for 2 variables",
            ),
            (
                serde_json::json!({"block": [{"count": 1, "order": "lex"}]}),
                "the blocks do not cover the 2 variables",
            ),
        ] {
            let input = factor(order);
            assert!(
                matches!(parse(&input), Err(CasError::InvalidArguments(ref m)) if m.contains(why)),
                "{why}"
            );
            let e = backend
                .execute(input, ExecutionContext::new(None))
                .await
                .unwrap_err();
            assert!(
                matches!(e, CasError::InvalidArguments(ref m) if m.contains(why)),
                "{e:?}"
            );
        }
        // With a valid order the job is read, and its request does not change:
        // factoring does not depend on the order.
        let valid = factor(serde_json::json!({"weights": [1, 2], "then": "grevlex"}));
        assert_eq!(
            request(&parse(&valid).unwrap()),
            request(&parse(&factor(serde_json::json!("lex"))).unwrap())
        );
    }

    fn block(parts: &[(usize, BaseOrder)]) -> MonomialOrder {
        MonomialOrder::Block(
            parts
                .iter()
                .map(|&(count, order)| algebramesh_math_types::OrderBlock { count, order })
                .collect(),
        )
    }

    fn weights(weights: &[u32], then: BaseOrder) -> MonomialOrder {
        MonomialOrder::Weights {
            weights: weights.to_vec(),
            then,
        }
    }

    /// Issue #64: the items of each kind of order, as the driver receives them.
    #[test]
    fn orders_become_oscar_items() {
        use BaseOrder::{Grevlex, Grlex, Lex};
        let items = |order: MonomialOrder, nvars: usize| {
            serde_json::to_value(order_items(&order, nvars).unwrap()).unwrap()
        };
        assert_eq!(
            items(MonomialOrder::Lex, 3),
            serde_json::json!([["lex", 1, 3]])
        );
        assert_eq!(
            items(MonomialOrder::Grlex, 3),
            serde_json::json!([["deglex", 1, 3]])
        );
        assert_eq!(
            items(MonomialOrder::Grevlex, 3),
            serde_json::json!([["degrevlex", 1, 3]])
        );
        assert_eq!(
            items(weights(&[3, 0, MAX_WEIGHT], Grlex), 3),
            serde_json::json!([["weights", [3, 0, 2147483647]], ["deglex", 1, 3]])
        );
        assert_eq!(
            items(block(&[(1, Grevlex), (2, Grlex), (1, Lex)]), 4),
            serde_json::json!([["degrevlex", 1, 1], ["deglex", 2, 3], ["lex", 4, 4]])
        );
        // A ring without variables: the driver's own variable.
        assert_eq!(
            items(MonomialOrder::Grevlex, 0),
            serde_json::json!([["lex", 1, 1]])
        );
        assert_eq!(
            items(weights(&[], Grlex), 0),
            serde_json::json!([["lex", 1, 1]])
        );
    }

    /// Issue #64: read as the driver builds the ordering from them, the items
    /// order the monomials as `MonomialOrder::compare` does: the weights
    /// first, then each ordering on its own variables.
    #[test]
    fn oscar_items_order_monomials_as_compare_does() {
        use BaseOrder::{Grevlex, Grlex, Lex};
        fn rows(items: &[OrderItem], n: usize) -> Vec<Vec<i128>> {
            let unit = |i: usize, v: i128| {
                let mut row = vec![0i128; n];
                row[i] = v;
                row
            };
            let mut rows = Vec::new();
            let mut covered = 0;
            for item in items {
                match item {
                    OrderItem::Weights(w) => {
                        assert_eq!(w.len(), n);
                        rows.push(w.iter().map(|&w| i128::from(w)).collect());
                    }
                    &OrderItem::Block { order, first, last } => {
                        assert_eq!(first, covered + 1, "the blocks follow each other");
                        let range = first - 1..last;
                        let mut ones = vec![0i128; n];
                        for e in &mut ones[range.clone()] {
                            *e = 1;
                        }
                        match order {
                            Lex => rows.extend(range.map(|i| unit(i, 1))),
                            Grlex => {
                                rows.push(ones);
                                rows.extend(range.map(|i| unit(i, 1)));
                            }
                            Grevlex => {
                                rows.push(ones);
                                rows.extend(range.rev().map(|i| unit(i, -1)));
                            }
                        }
                        covered = last;
                    }
                }
            }
            assert_eq!(covered, n, "the blocks cover the variables");
            rows
        }
        let n = 4;
        // Every monomial with exponents below 3.
        let monomials: Vec<Vec<u32>> = (0..81u32)
            .map(|m| (0..4).map(|i| m / 3u32.pow(i) % 3).collect())
            .collect();
        let orders = [
            MonomialOrder::Lex,
            MonomialOrder::Grlex,
            MonomialOrder::Grevlex,
            weights(&[3, 0, 1, 2], Grevlex),
            weights(&[0, 0, 0, 0], Lex),
            weights(&[0, 1, 0, 1], Grlex),
            weights(&[MAX_WEIGHT, 0, 1, 2], Grlex),
            block(&[(2, Grevlex), (2, Lex)]),
            block(&[(1, Lex), (3, Grlex)]),
            block(&[(2, Grlex), (2, Grlex)]),
            block(&[(1, Grevlex), (2, Grlex), (1, Lex)]),
        ];
        for order in &orders {
            let m = rows(&order_items(order, n).unwrap(), n);
            let images: Vec<Vec<i128>> = monomials
                .iter()
                .map(|e| {
                    m.iter()
                        .map(|row| row.iter().zip(e).map(|(w, &x)| w * i128::from(x)).sum())
                        .collect()
                })
                .collect();
            for (a, image_a) in monomials.iter().zip(&images) {
                for (b, image_b) in monomials.iter().zip(&images) {
                    assert_eq!(
                        image_a.cmp(image_b),
                        order.compare(a, b),
                        "{order:?}: {a:?} and {b:?}"
                    );
                }
            }
        }
    }

    /// Issue #64: an order that is not valid for the variables cannot be
    /// written for OSCAR and is refused as invalid arguments.
    #[test]
    fn orders_oscar_cannot_take_are_refused() {
        use BaseOrder::Lex;
        let refused = |order: MonomialOrder, nvars: usize| {
            matches!(
                order_items(&order, nvars),
                Err(CasError::InvalidArguments(_))
            )
        };
        assert!(refused(weights(&[1, 0], Lex), 3));
        assert!(refused(weights(&[1, 0, 0, 0], Lex), 3));
        // A weight above MAX_WEIGHT, which only Rust can build.
        assert!(refused(weights(&[MAX_WEIGHT + 1, 0], Lex), 2));
        assert!(refused(block(&[(2, Lex)]), 3));
        assert!(refused(block(&[(0, Lex), (2, Lex)]), 2));
        assert!(refused(MonomialOrder::Block(vec![]), 0));
    }

    #[tokio::test]
    async fn a_line_is_read_only_up_to_the_budget() {
        // An endless line: the reader stops at the budget instead of growing.
        let mut endless = BufReader::new(tokio::io::repeat(b'x'));
        let mut budget = 1 << 20;
        assert!(matches!(
            read_line(&mut endless, &mut budget).await,
            Err(LineError::OverBudget)
        ));
        // Lines within the budget, with CR LF, and the end of the output.
        let mut reader = BufReader::new(&b"first\r\nsecond\nlast"[..]);
        let mut budget = 100;
        assert_eq!(
            read_line(&mut reader, &mut budget)
                .await
                .unwrap()
                .as_deref(),
            Some("first")
        );
        assert_eq!(
            read_line(&mut reader, &mut budget)
                .await
                .unwrap()
                .as_deref(),
            Some("second")
        );
        assert_eq!(
            read_line(&mut reader, &mut budget)
                .await
                .unwrap()
                .as_deref(),
            Some("last")
        );
        assert!(read_line(&mut reader, &mut budget).await.unwrap().is_none());
        assert_eq!(budget, 100 - 7 - 7 - 4);
        // The budget counts every byte read, across lines.
        let mut reader = BufReader::new(&b"0123456789\n0123456789\n"[..]);
        let mut budget = 15;
        assert!(read_line(&mut reader, &mut budget).await.unwrap().is_some());
        assert!(matches!(
            read_line(&mut reader, &mut budget).await,
            Err(LineError::OverBudget)
        ));
    }

    #[test]
    fn the_job_log_keeps_a_bounded_part_of_the_output() {
        let mut log = Vec::new();
        let mut kept = OutputLog::default();
        let line = "y".repeat(1000);
        for _ in 0..1000 {
            kept.push(&mut log, &line);
        }
        kept.finish(&mut log);
        let total: usize = log.iter().map(String::len).sum();
        assert!(total <= MAX_LOG_BYTES + 100, "{total}");
        assert!(log.last().unwrap().contains("more stdout lines not kept"));
    }

    /// A fake driver: an executable script used as `julia`, which ignores its
    /// arguments, says it is ready and answers each request with `answer`.
    #[cfg(unix)]
    fn fake_driver(dir: &std::path::Path, answer: &str) -> OscarConfig {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join("fake-julia");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\n\
                 echo 'ALGEBRAMESH_READY {{\"oscar_version\":\"0\",\"julia_version\":\"0\"}}'\n\
                 while read -r request; do\n{answer}\ndone\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        OscarConfig {
            julia: script,
            driver: "unused".into(),
            project: None,
            startup_timeout: Duration::from_secs(10),
            build: None,
            max_output_bytes: 1 << 20,
        }
    }

    #[cfg(unix)]
    fn x2_minus_1() -> CasRequest {
        job(serde_json::json!({
            "ring": {"domain": "QQ", "variables": ["x"], "order": "lex"},
            "expression": {"type": "polynomial", "terms": [
                {"coefficient": "1", "exponents": [2]},
                {"coefficient": "-1", "exponents": [0]}]}
        }))
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_huge_answer_line_is_a_resource_limit_and_the_process_is_replaced() {
        let dir = std::env::temp_dir().join(format!("am-oscar-{}", algebramesh_core::JobId::new()));
        std::fs::create_dir_all(&dir).unwrap();
        // 64 MiB on one line, against a limit of 1 MiB.
        let backend = OscarBackend::new(fake_driver(
            &dir,
            "head -c 67108864 /dev/zero | tr '\\0' x; echo",
        ));
        let started = std::time::Instant::now();
        let outcome = backend
            .execute(
                x2_minus_1(),
                ExecutionContext::new(Some(Duration::from_secs(30))),
            )
            .await;
        assert!(
            matches!(&outcome, Err(CasError::ResourceLimit(m)) if m.contains("more than 1048576 bytes")),
            "{outcome:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(20));
        // The process was discarded, not kept for the next job.
        assert!(backend.take_idle().is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn noise_before_a_normal_answer_is_logged_within_the_limit() {
        let dir = std::env::temp_dir().join(format!("am-oscar-{}", algebramesh_core::JobId::new()));
        std::fs::create_dir_all(&dir).unwrap();
        let answer = r#"i=0; while [ $i -lt 2000 ]; do echo "noise line $i ................................"; i=$((i+1)); done
echo 'ALGEBRAMESH_RESULT {"factors": [{"multiplicity": 1, "terms": [{"coefficient": "1", "exponents": [1]}, {"coefficient": "-1", "exponents": [0]}]}, {"multiplicity": 1, "terms": [{"coefficient": "1", "exponents": [1]}, {"coefficient": "1", "exponents": [0]}]}]}'"#;
        let backend = OscarBackend::new(fake_driver(&dir, answer));
        let result = backend
            .execute(
                x2_minus_1(),
                ExecutionContext::new(Some(Duration::from_secs(30))),
            )
            .await
            .unwrap();
        assert_eq!(result.value["verified_product"], true);
        let logged: usize = result.log.iter().map(String::len).sum();
        // The kept lines, the note and the timing lines.
        assert!(logged <= MAX_LOG_BYTES + 300, "{logged}");
        assert!(
            result
                .log
                .iter()
                .any(|l| l.contains("more stdout lines not kept"))
        );
        // A clean answer keeps the process for the next job.
        assert!(backend.take_idle().is_some());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A fake driver that records its PID, starts a child and a grandchild,
    /// records theirs in `dir/pids`, and then runs `then` (Issue #90).
    #[cfg(unix)]
    fn fake_driver_with_helpers(dir: &std::path::Path, then: &str) -> OscarConfig {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join("fake-julia");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\n\
                 echo $$ >> {pids}\n\
                 sleep 600 &\n\
                 echo $! >> {pids}\n\
                 sh -c 'sleep 600 & echo $! >> {pids}; wait' &\n\
                 while [ \"$(wc -l < {pids})\" -lt 3 ]; do sleep 0.05; done\n\
                 {then}\n",
                pids = dir.join("pids").display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        OscarConfig {
            julia: script,
            driver: "unused".into(),
            project: None,
            startup_timeout: Duration::from_secs(60),
            build: None,
            max_output_bytes: 1 << 20,
        }
    }

    /// The fake driver and its two helpers.
    #[cfg(unix)]
    async fn driver_and_helpers(dir: &std::path::Path) -> Vec<i32> {
        for _ in 0..250 {
            let pids: Vec<i32> = std::fs::read_to_string(dir.join("pids"))
                .unwrap_or_default()
                .lines()
                .filter_map(|l| l.trim().parse().ok())
                .collect();
            if pids.len() == 3 {
                return pids;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("the fake driver did not start its helpers");
    }

    /// Whether `pid` has terminated: it does not exist, or is a zombie not yet
    /// reaped.
    #[cfg(unix)]
    fn terminated(pid: i32) -> bool {
        if nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_err() {
            return true;
        }
        std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
            stat.rsplit_once(')')
                .is_some_and(|(_, rest)| rest.trim_start().starts_with('Z'))
        })
    }

    #[cfg(unix)]
    async fn all_terminated(pids: &[i32]) -> bool {
        for _ in 0..100 {
            if pids.iter().all(|&p| terminated(p)) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        false
    }

    #[cfg(unix)]
    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "am-oscar-{name}-{}",
            algebramesh_core::JobId::new()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Issue #90: a start-up that fails before the ready line stops the
    /// driver and its helpers, not the driver alone.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_failed_start_up_kills_the_drivers_helpers() {
        /// A name, the driver's shell script, and the expected error.
        type Case = (&'static str, &'static str, fn(&CasError) -> bool);
        let cases: [Case; 3] = [
            (
                "timeout",
                "sleep 600",
                |e| matches!(e, CasError::Unavailable(m) if m.contains("did not start")),
            ),
            (
                "bad-ready",
                "echo 'ALGEBRAMESH_READY not json'; sleep 600",
                |e| matches!(e, CasError::Unavailable(m) if m.contains("bad ready line")),
            ),
            (
                "over-limit",
                "head -c 4096 /dev/zero | tr '\\0' x; echo; sleep 600",
                |e| matches!(e, CasError::ResourceLimit(_)),
            ),
        ];
        for (name, then, expected) in cases {
            let dir = scratch(name);
            let config = OscarConfig {
                startup_timeout: Duration::from_secs(2),
                max_output_bytes: 1024,
                ..fake_driver_with_helpers(&dir, then)
            };
            let Err(e) = OscarBackend::spawn(&config, &mut Vec::new()).await else {
                panic!("{name}: the start-up succeeded");
            };
            assert!(expected(&e), "{name}: {e:?}");
            let pids = driver_and_helpers(&dir).await;
            assert!(all_terminated(&pids).await, "{name}: left {pids:?}");
            std::fs::remove_dir_all(&dir).unwrap();
        }
    }

    /// Issue #90: a job cancelled, or past its deadline, while its process
    /// starts stops the driver and its helpers.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_job_abandoned_during_start_up_kills_the_drivers_helpers() {
        // Cancelled once the helpers run.
        let dir = scratch("cancel");
        let backend = OscarBackend::new(fake_driver_with_helpers(&dir, "sleep 600"));
        let ctx = ExecutionContext::new(None);
        let cancel = ctx.cancel.clone();
        let (outcome, pids) = tokio::join!(backend.execute(x2_minus_1(), ctx), async {
            let pids = driver_and_helpers(&dir).await;
            cancel.cancel();
            pids
        });
        assert!(matches!(outcome, Err(CasError::Cancelled)), "{outcome:?}");
        assert!(all_terminated(&pids).await, "left {pids:?}");
        std::fs::remove_dir_all(&dir).unwrap();

        // Past the deadline of the job.
        let dir = scratch("deadline");
        let backend = OscarBackend::new(fake_driver_with_helpers(&dir, "sleep 600"));
        let outcome = backend
            .execute(
                x2_minus_1(),
                ExecutionContext::new(Some(Duration::from_secs(2))),
            )
            .await;
        assert!(matches!(outcome, Err(CasError::Timeout(_))), "{outcome:?}");
        let pids = driver_and_helpers(&dir).await;
        assert!(all_terminated(&pids).await, "left {pids:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Issue #90: an idle process goes, with its helpers, when the backend
    /// is dropped.
    #[cfg(unix)]
    #[tokio::test]
    async fn dropping_the_backend_kills_the_helpers_of_idle_processes() {
        let dir = scratch("idle");
        let backend = OscarBackend::new(fake_driver_with_helpers(
            &dir,
            "echo 'ALGEBRAMESH_READY {\"oscar_version\":\"0\",\"julia_version\":\"0\"}'; sleep 600",
        ));
        assert!(backend.prepare().await.unwrap().is_some());
        let pids = driver_and_helpers(&dir).await;
        assert!(pids.iter().all(|&p| !terminated(p)), "{pids:?}");
        drop(backend);
        assert!(all_terminated(&pids).await, "left {pids:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Issue #35: the OSCAR driver, like Macaulay2's, gives a ring without
    /// variables a variable of its own.
    #[test]
    fn constants_of_a_ring_without_variables_round_trip() {
        let input = parse(&job(serde_json::json!({
            "ring": {"domain": "QQ", "variables": [], "order": "lex"},
            "expression": {"type": "polynomial", "terms": [
                {"coefficient": "-3/2", "exponents": []}]}
        })))
        .unwrap();
        let reply = serde_json::json!({"factors": [
            {"multiplicity": 1, "terms": [{"coefficient": "-3/2", "exponents": [0]}]}]});
        let value = interpret(&input, reply, &Interrupt::none()).unwrap();
        assert_eq!(value["constant"], "-3/2");
        assert_eq!(value["verified_product"], true);
        let bad = serde_json::json!({"factors": [
            {"multiplicity": 1, "terms": [{"coefficient": "1", "exponents": [2]}]}]});
        assert!(interpret(&input, bad, &Interrupt::none()).is_err());
    }

    /// A recorded reply of OSCAR to a matrix or an ideal job, read as the
    /// backend reads it and checked as the runner checks it.
    fn check_reply(
        operation: &str,
        arguments: &serde_json::Value,
        reply: &serde_json::Value,
    ) -> Result<CasResult, CasError> {
        use algebramesh_core::guarantee::ResultVerifier;
        let job = parse(&CasRequest {
            job_id: algebramesh_core::JobId::new(),
            operation: operation.into(),
            arguments: arguments.clone(),
            limits: Default::default(),
        })?;
        let value = interpret(&job, reply.clone(), &Interrupt::none())?;
        let spec = algebramesh_core::JobSpec {
            schema_version: 1,
            backend: NAME.into(),
            operation: operation.into(),
            arguments: arguments.clone(),
            limits: Default::default(),
            retry_policy: Default::default(),
            environment: Default::default(),
        };
        let result = CasResult {
            value,
            backend: BackendInfo::default(),
            execution_host: None,
            log: vec![],
            guarantee: None,
        };
        algebramesh_verify::Registry::standard().verify(&spec, result, &Interrupt::none())
    }

    /// Issues #57, #58: the replies OSCAR gave in its image
    /// (`testdata/matrix.json`) pass every check; each mutation, first shown
    /// to break its claim, is refused.
    #[test]
    fn recorded_matrix_replies_are_checked() {
        use algebramesh_core::guarantee::EvidenceLevel;
        let cases: Vec<serde_json::Value> =
            serde_json::from_str(include_str!("../testdata/matrix.json")).unwrap();
        for case in &cases {
            let op = case["operation"].as_str().unwrap();
            let checked = check_reply(op, &case["arguments"], &case["reply"]).unwrap();
            let claims = checked.guarantee.unwrap().claims;
            assert!(
                claims.iter().all(|c| c.level == EvidenceLevel::Checked),
                "{op}: {claims:?}"
            );
        }
        let mutated = |i: usize, pointer: &str, to: serde_json::Value| {
            let case = &cases[i];
            let mut reply = case["reply"].clone();
            *reply.pointer_mut(pointer).unwrap() = to;
            check_reply(
                case["operation"].as_str().unwrap(),
                &case["arguments"],
                &reply,
            )
        };
        let product = |a: serde_json::Value, b: serde_json::Value| {
            let a: algebramesh_math_types::RationalMatrix = serde_json::from_value(a).unwrap();
            let b: algebramesh_math_types::RationalMatrix = serde_json::from_value(b).unwrap();
            a.mul(&b).unwrap()
        };
        let refused = |r: Result<CasResult, CasError>| {
            assert!(matches!(r, Err(CasError::UnsupportedResult(_))), "{r:?}")
        };
        // SNF: diag(2, 8) keeps 2 | 8, but U A V is diag(2, 4).
        refused(mutated(0, "/diagonal/1", serde_json::json!("8")));
        // HNF: [[2, 0], [0, 8]] is a Hermite form, but U A is [[2, 0], [0, 4]].
        refused(mutated(1, "/form/1/1", serde_json::json!("8")));
        // rank: (1, 1, 1) is not in the kernel.
        assert!(
            !product(
                cases[2]["arguments"]["matrix"].clone(),
                serde_json::json!([["1"], ["1"], ["1"]])
            )
            .is_zero()
        );
        refused(mutated(
            2,
            "/certificate/kernel/2/0",
            serde_json::json!("1"),
        ));
        // kernel over ZZ: (-4, 2) lies in the kernel but spans half the lattice.
        assert!(
            product(
                serde_json::json!([["2", "4"]]),
                serde_json::json!([["-4"], ["2"]])
            )
            .is_zero()
        );
        refused(mutated(4, "/basis", serde_json::json!([["-4"], ["2"]])));
        // solve over ZZ: y = 1 keeps y A integral, but y b = 1 is an integer.
        refused(mutated(5, "/certificate/y/0", serde_json::json!("1")));
        // solve over QQ: y = (1, 1) is not in the left kernel.
        assert!(
            !product(
                serde_json::json!([["1", "1"]]),
                cases[7]["arguments"]["matrix"].clone()
            )
            .is_zero()
        );
        refused(mutated(7, "/certificate/y", serde_json::json!(["1", "1"])));
    }

    fn ideal_cases() -> Vec<serde_json::Value> {
        serde_json::from_str(include_str!("../testdata/ideal.json")).unwrap()
    }

    fn ideal_case<'a>(cases: &'a [serde_json::Value], name: &str) -> &'a serde_json::Value {
        cases.iter().find(|c| c["name"] == name).unwrap()
    }

    /// The polynomials of `ring` with the given terms (coefficient, exponents).
    fn polynomials(ring: &RingSpec, polynomials: &[&[(&str, &[u32])]]) -> Vec<Polynomial> {
        polynomials
            .iter()
            .map(|terms| {
                let terms: Vec<_> = terms
                    .iter()
                    .map(|(c, e)| (e.to_vec(), c.parse::<Rational>().unwrap()))
                    .collect();
                Polynomial::from_terms(ring.clone(), terms).unwrap()
            })
            .collect()
    }

    /// The basis of a value of `ideal.groebner_basis`, read in `ring`.
    fn basis_of(ring: &RingSpec, value: &serde_json::Value) -> Vec<Polynomial> {
        let result: ideal::GroebnerBasisResult = serde_json::from_value(value.clone()).unwrap();
        result
            .basis
            .iter()
            .map(|p| Polynomial::from_data(ring.clone(), p).unwrap())
            .collect()
    }

    /// Whether G = F C and F = G N hold for a reply to a job, its basis and
    /// its matrices read as the backend reads them.
    fn representation_holds(
        arguments: &serde_json::Value,
        reply: &serde_json::Value,
    ) -> (bool, bool) {
        use algebramesh_math_types::PolynomialMatrix;
        let request = IdealRequest::parse(ideal::IDEAL_GROEBNER_BASIS, arguments).unwrap();
        let ring = request.ring().clone();
        let value = request.value(reply).unwrap();
        let g = basis_of(&ring, &value);
        let f = &request.ideal.generators;
        let matrix = |name: &str| {
            let data = serde_json::from_value(value["certificate"][name].clone()).unwrap();
            PolynomialMatrix::from_data(ring.clone(), &data).unwrap()
        };
        let row = |p: &[Polynomial]| PolynomialMatrix::from_row(ring.clone(), p).unwrap();
        let contained = row(f).mul(&matrix("basis_in_generators")).unwrap() == row(&g);
        let contains = row(&g).mul(&matrix("generators_in_basis")).unwrap() == row(f);
        (contained, contains)
    }

    /// Issues #66, #68: the replies OSCAR gave in its image
    /// (`testdata/ideal.json`) to the `ideal.groebner_basis` requests this
    /// backend sends pass every check, with `gb.groebner` checked when the
    /// job asked for the S-pairs and asserted otherwise and, without a
    /// certificate, `gb.contained` and `gb.contains` asserted too. The bases
    /// are the reduced ones computed by hand, and lex and grevlex give
    /// different ones. The reply recorded with a fault (a request the backend
    /// does not send) is left to
    /// `the_grevlex_reply_to_a_lex_job_is_refused_by_its_s_pairs`.
    #[test]
    fn recorded_ideal_replies_are_checked() {
        use algebramesh_core::guarantee::EvidenceLevel::{Asserted, Checked};
        let cases = ideal_cases();
        for case in cases
            .iter()
            .filter(|c| c["operation"] == ideal::IDEAL_GROEBNER_BASIS && c.get("fault").is_none())
        {
            let name = case["name"].as_str().unwrap();
            let op = case["operation"].as_str().unwrap();
            let job = parse(&CasRequest {
                job_id: algebramesh_core::JobId::new(),
                operation: op.into(),
                arguments: case["arguments"].clone(),
                limits: Default::default(),
            })
            .unwrap();
            assert_eq!(request(&job), case["request"], "{name}: the request sent");
            let checked = check_reply(op, &case["arguments"], &case["reply"])
                .unwrap_or_else(|e| panic!("{name}: {e:?}"));
            let claims = checked.guarantee.unwrap().claims;
            let level = |id: &str| claims.iter().find(|c| c.id == id).map(|c| c.level);
            let with_certificate = case["arguments"]["certificate"] != "none";
            let certified = if with_certificate { Checked } else { Asserted };
            let with_s_pairs = case["arguments"]["certificate"] == "s_pairs";
            let groebner = if with_s_pairs { Checked } else { Asserted };
            assert_eq!(level("gb.reduced_form"), Some(Checked), "{name}");
            assert_eq!(level("gb.contained"), Some(certified), "{name}");
            assert_eq!(level("gb.contains"), Some(certified), "{name}");
            assert_eq!(level("gb.groebner"), Some(groebner), "{name}");
        }

        let basis = |name: &str| {
            let case = ideal_case(&cases, name);
            let request =
                IdealRequest::parse(ideal::IDEAL_GROEBNER_BASIS, &case["arguments"]).unwrap();
            let value = request.value(&case["reply"]).unwrap();
            (request.ring().clone(), basis_of(request.ring(), &value))
        };
        // The twisted cubic, as in the contract: z^2 - yw, yz - xw, y^2 - xz.
        let (ring, g) = basis("twisted_cubic_grevlex");
        assert_eq!(
            g,
            polynomials(
                &ring,
                &[
                    &[("1", &[0, 0, 2, 0]), ("-1", &[0, 1, 0, 1])],
                    &[("1", &[0, 1, 1, 0]), ("-1", &[1, 0, 0, 1])],
                    &[("1", &[0, 2, 0, 0]), ("-1", &[1, 0, 1, 0])],
                ]
            )
        );
        // F = {x^2y - z/2, 2xy^2 - x, 3xz - y}: z^7 - z/162, y - 9z^3, x - 3z^2
        // in lex; six elements in grevlex.
        let (ring, g) = basis("small_lex");
        assert_eq!(
            g,
            polynomials(
                &ring,
                &[
                    &[("1", &[0, 0, 7]), ("-1/162", &[0, 0, 1])],
                    &[("1", &[0, 1, 0]), ("-9", &[0, 0, 3])],
                    &[("1", &[1, 0, 0]), ("-3", &[0, 0, 2])],
                ]
            )
        );
        assert_eq!(basis("small_grevlex").1.len(), 6);
        // A zero generator changes neither the basis nor its order.
        assert_eq!(
            basis("small_with_zero_generator_grevlex").1,
            basis("small_grevlex").1
        );
        // F = {-xyz + z^2, -xyz - x^2 + xy}: the grevlex basis of the contract,
        // and a lex basis of four elements.
        let (ring, g) = basis("mixup_grevlex");
        assert_eq!(
            g,
            polynomials(
                &ring,
                &[
                    &[("1", &[2, 0, 0]), ("-1", &[1, 1, 0]), ("1", &[0, 0, 2])],
                    &[("1", &[1, 1, 1]), ("-1", &[0, 0, 2])],
                    &[("1", &[0, 1, 3]), ("1", &[1, 0, 2]), ("-1", &[0, 1, 2])],
                ]
            )
        );
        assert_eq!(basis("mixup_lex").1.len(), 4);
        // x^2 - B y, xy - 1 with B = 2^64 + 1: y^3 - 1/B, x - B y^2 in lex.
        let (ring, g) = basis("big_coefficient_lex");
        assert_eq!(
            g,
            polynomials(
                &ring,
                &[
                    &[("1", &[0, 3]), ("-1/18446744073709551617", &[0, 0])],
                    &[("1", &[1, 0]), ("-18446744073709551617", &[0, 2])],
                ]
            )
        );
        let (ring, g) = basis("unit_grevlex");
        assert_eq!(g, polynomials(&ring, &[&[("1", &[0, 0])]]));
        assert!(basis("zero_grevlex").1.is_empty());
        assert!(basis("no_generators_lex").1.is_empty());
    }

    /// Issue #66: a recorded reply with one entry of C or N changed, first
    /// shown to break its equation, is refused; so is the grevlex basis sent
    /// for a lex job when it is not monic in lex. The grevlex basis of
    /// {-xyz + z^2, -xyz - x^2 + xy}, put in increasing lex order, is monic
    /// and interreduced in lex too, and passes for the lex job with
    /// `representation`: without the S-pairs, `gb.groebner` stays asserted
    /// and nothing shows that it is not a lex Groebner basis. With the
    /// S-pairs (#68) it is refused, as
    /// `the_grevlex_reply_to_a_lex_job_is_refused_by_its_s_pairs` shows.
    #[test]
    fn changed_ideal_replies_are_refused() {
        use algebramesh_core::guarantee::EvidenceLevel::Asserted;
        let cases = ideal_cases();
        let refused = |r: Result<CasResult, CasError>, why: &str| {
            assert!(
                matches!(&r, Err(CasError::UnsupportedResult(m)) if m.contains(why)),
                "{why}: {r:?}"
            )
        };
        let mutated = |name: &str, pointer: &str, to: serde_json::Value| {
            let case = ideal_case(&cases, name);
            let mut reply = case["reply"].clone();
            *reply.pointer_mut(pointer).unwrap() = to;
            (case["arguments"].clone(), reply)
        };

        // C: g_0 = -f_2 for the twisted cubic; C_20 = 1 gives f_2.
        let (arguments, reply) = mutated(
            "twisted_cubic_grevlex",
            "/certificate/basis_in_generators/columns/0/0/terms/0/coefficient",
            serde_json::json!("1"),
        );
        assert_eq!(representation_holds(&arguments, &reply), (false, true));
        refused(
            check_reply(ideal::IDEAL_GROEBNER_BASIS, &arguments, &reply),
            "g_0 is not sum_i f_i C_i0",
        );

        // N: one coefficient of cyclic-4 changed.
        let (arguments, reply) = mutated(
            "cyclic4_grevlex",
            "/certificate/generators_in_basis/columns/3/0/terms/0/coefficient",
            serde_json::json!("12345"),
        );
        assert_eq!(representation_holds(&arguments, &reply), (true, false));
        refused(
            check_reply(ideal::IDEAL_GROEBNER_BASIS, &arguments, &reply),
            "f_3 is not sum_k g_k N_k3",
        );

        // The grevlex basis of F = {x^2y - z/2, 2xy^2 - x, 3xz - y} sent for
        // the lex job: z^2 - x/3 has the lex leading term -x/3.
        let lex = ideal_case(&cases, "small_lex");
        let grevlex = ideal_case(&cases, "small_grevlex");
        assert_eq!(
            representation_holds(&lex["arguments"], &grevlex["reply"]),
            (true, true)
        );
        refused(
            check_reply(
                ideal::IDEAL_GROEBNER_BASIS,
                &lex["arguments"],
                &grevlex["reply"],
            ),
            "g_0 is not monic",
        );

        // The grevlex basis [x^2 - xy + z^2, xyz - z^2, yz^3 + xz^2 - yz^2]
        // in increasing lex order (lex leading monomials xz^2 < xyz < x^2),
        // with the columns of C and the rows of N in the same order.
        let lex = ideal_case(&cases, "mixup_lex");
        let mut reply = ideal_case(&cases, "mixup_grevlex")["reply"].clone();
        let reverse = |v: &mut serde_json::Value| v.as_array_mut().unwrap().reverse();
        reverse(&mut reply["basis"]);
        reverse(&mut reply["certificate"]["basis_in_generators"]["columns"]);
        for column in reply["certificate"]["generators_in_basis"]["columns"]
            .as_array_mut()
            .unwrap()
        {
            for entry in column.as_array_mut().unwrap() {
                entry["row"] = (2 - entry["row"].as_u64().unwrap()).into();
            }
        }
        assert_eq!(
            representation_holds(&lex["arguments"], &reply),
            (true, true)
        );
        let checked = check_reply(ideal::IDEAL_GROEBNER_BASIS, &lex["arguments"], &reply).unwrap();
        assert_eq!(checked.value["basis"].as_array().unwrap().len(), 3);
        assert_eq!(lex["reply"]["basis"].as_array().unwrap().len(), 4);
        let claims = checked.guarantee.unwrap().claims;
        assert!(
            claims
                .iter()
                .any(|c| c.id == "gb.groebner" && c.level == Asserted)
        );
    }

    /// The pairs (i, j), i < j, of elements of `g` whose leading monomials
    /// are not coprime (some variable has a positive exponent in both), in
    /// increasing order.
    fn non_coprime_pairs(g: &[Polynomial]) -> Vec<[usize; 2]> {
        let lm: Vec<&[u32]> = g.iter().map(|p| p.leading_monomial().unwrap()).collect();
        let mut pairs = vec![];
        for i in 0..g.len() {
            for j in i + 1..g.len() {
                if lm[i].iter().zip(lm[j]).any(|(a, b)| *a > 0 && *b > 0) {
                    pairs.push([i, j]);
                }
            }
        }
        pairs
    }

    /// The monomial of `ring` with the exponents `e`.
    fn monomial(ring: &RingSpec, e: Vec<u32>) -> Polynomial {
        Polynomial::from_terms(ring.clone(), vec![(e, "1".parse::<Rational>().unwrap())]).unwrap()
    }

    /// L = lcm(lm(g_i), lm(g_j)) and S(g_i, g_j) = (L / lm(g_i)) g_i -
    /// (L / lm(g_j)) g_j for monic g_i and g_j.
    fn s_polynomial(
        ring: &RingSpec,
        g: &[Polynomial],
        i: usize,
        j: usize,
    ) -> (Vec<u32>, Polynomial) {
        let one: Rational = "1".parse().unwrap();
        assert_eq!(g[i].leading_coefficient(), Some(&one));
        assert_eq!(g[j].leading_coefficient(), Some(&one));
        let (a, b) = (
            g[i].leading_monomial().unwrap(),
            g[j].leading_monomial().unwrap(),
        );
        let l: Vec<u32> = a.iter().zip(b).map(|(x, y)| *x.max(y)).collect();
        let quotient = |m: &[u32]| monomial(ring, l.iter().zip(m).map(|(x, y)| x - y).collect());
        let s = quotient(a)
            .mul(&g[i])
            .unwrap()
            .sub(&quotient(b).mul(&g[j]).unwrap())
            .unwrap();
        (l, s)
    }

    /// The columns of a driver matrix, each as its entries by row, read in
    /// `ring`.
    fn reply_columns(ring: &RingSpec, m: &serde_json::Value) -> Vec<Vec<Polynomial>> {
        let rows = m["rows"].as_u64().unwrap() as usize;
        m["columns"]
            .as_array()
            .unwrap()
            .iter()
            .map(|column| {
                let mut entries = vec![Polynomial::zero(ring.clone()); rows];
                for entry in column.as_array().unwrap() {
                    entries[entry["row"].as_u64().unwrap() as usize] =
                        reply_polynomial(ring, &entry["terms"]);
                }
                entries
            })
            .collect()
    }

    /// The ring of the job of `case`, and the basis and the S-pairs of a
    /// reply to it (for `ideal.membership`, those of its certificate).
    fn basis_and_s_pairs(
        case: &serde_json::Value,
        reply: &serde_json::Value,
    ) -> (RingSpec, Vec<Polynomial>, serde_json::Value) {
        let request =
            IdealRequest::parse(case["operation"].as_str().unwrap(), &case["arguments"]).unwrap();
        let ring = request.ring().clone();
        let basis = if case["operation"] == ideal::IDEAL_MEMBERSHIP {
            &reply["certificate"]["basis"]
        } else {
            &reply["basis"]
        };
        let g = reply_polynomials(&ring, basis);
        (ring, g, reply["certificate"]["s_pairs"].clone())
    }

    /// Whether S(g_i, g_j) = sum_k h_k g_k holds for the pair `p` of a
    /// certificate of the S-pairs, with G and the certificate as the driver
    /// writes them.
    fn s_pair_holds(
        ring: &RingSpec,
        g: &[Polynomial],
        s_pairs: &serde_json::Value,
        p: usize,
    ) -> bool {
        let [i, j]: [usize; 2] = serde_json::from_value(s_pairs["pairs"][p].clone()).unwrap();
        let h = &reply_columns(ring, &s_pairs["representations"])[p];
        s_polynomial(ring, g, i, j).1 == dot(ring, h, g)
    }

    /// Issue #68: the certificates of the S-pairs OSCAR gave in its image
    /// (`s_pairs_json` of the driver: the quotients of
    /// `reduce_with_quotients` on the S-polynomials). Each reply with them is
    /// the reply of the same job asked with `representation` with `s_pairs`
    /// added; a member keeps its coefficients alone. The pairs are those
    /// whose leading monomials are not coprime, in increasing order;
    /// S(g_i, g_j) = sum_k h_k g_k holds, and lm(h_k g_k) <= lm(S(g_i, g_j))
    /// for each h_k != 0 (a standard representation), so that lm(h_k g_k) is
    /// below L = lcm(lm(g_i), lm(g_j)), whose term cancels in S. The twisted
    /// cubic (z^2 - yw, yz - xw, y^2 - xz) has S(g_0, g_1) = -w g_2 and
    /// S(g_1, g_2) = x g_0, with (0, 2) coprime (z^2, y^2); cyclic 4 in
    /// grevlex 13 of its 21 pairs; the lex basis (z^7 - z/162, y - 9z^3,
    /// x - 3z^2), the unit ideal, the zero ideal and the basis (x^2 - y^2) of
    /// one element none; the basis (xy, x^2, y^3) of monomials the pairs
    /// (0, 1) and (0, 2), whose S-polynomials are 0, with empty columns.
    #[test]
    fn recorded_s_pairs_certificates() {
        use std::cmp::Ordering;
        let cases = ideal_cases();
        let mut seen = 0;
        for case in cases.iter().filter(|c| c.get("fault").is_none()) {
            let name = case["name"].as_str().unwrap();
            if case["arguments"]["certificate"] != "s_pairs" {
                continue;
            }
            seen += 1;
            let reply = &case["reply"];
            if let Some(twin) = name.strip_suffix("_s_pairs")
                && let Some(twin) = cases.iter().find(|c| c["name"] == twin)
            {
                let mut without = reply.clone();
                if let Some(c) = without["certificate"].as_object_mut() {
                    c.remove("s_pairs");
                }
                assert_eq!(without, twin["reply"], "{name}");
            }
            if reply["member"] == true {
                assert!(reply["certificate"].get("s_pairs").is_none(), "{name}");
                continue;
            }
            let (ring, g, s_pairs) = basis_and_s_pairs(case, reply);
            let pairs: Vec<[usize; 2]> = serde_json::from_value(s_pairs["pairs"].clone()).unwrap();
            assert_eq!(pairs, non_coprime_pairs(&g), "{name}");
            let h = reply_columns(&ring, &s_pairs["representations"]);
            assert_eq!(s_pairs["representations"]["rows"], g.len(), "{name}");
            assert_eq!(h.len(), pairs.len(), "{name}");
            for (p, &[i, j]) in pairs.iter().enumerate() {
                let (l, s) = s_polynomial(&ring, &g, i, j);
                assert_eq!(s, dot(&ring, &h[p], &g), "{name}: pair {p}");
                let order = &ring.order;
                for (k, hk) in h[p].iter().enumerate().filter(|(_, hk)| !hk.is_zero()) {
                    let product = hk.mul(&g[k]).unwrap();
                    let lm = product.leading_monomial().unwrap();
                    let top = s.leading_monomial().unwrap();
                    assert_ne!(
                        order.compare(lm, top),
                        Ordering::Greater,
                        "{name}: H_{k}{p}"
                    );
                    assert_eq!(order.compare(top, &l), Ordering::Less, "{name}: pair {p}");
                }
            }
        }
        assert_eq!(seen, 20);

        let s_pairs =
            |name: &str| ideal_case(&cases, name)["reply"]["certificate"]["s_pairs"].clone();
        let x = |c: &str, e: [u32; 4]| serde_json::json!({"coefficient": c, "exponents": e});
        assert_eq!(
            s_pairs("twisted_cubic_grevlex_s_pairs"),
            serde_json::json!({"pairs": [[0, 1], [1, 2]], "representations": {"rows": 3, "columns": [
                [{"row": 2, "terms": [x("-1", [0, 0, 0, 1])]}],
                [{"row": 0, "terms": [x("1", [1, 0, 0, 0])]}]
            ]}})
        );
        assert_eq!(
            s_pairs("cyclic4_grevlex_s_pairs")["pairs"]
                .as_array()
                .unwrap()
                .len(),
            13
        );
        assert_eq!(
            ideal_case(&cases, "cyclic4_grevlex_s_pairs")["reply"]["basis"]
                .as_array()
                .unwrap()
                .len(),
            7
        );
        for (name, t) in [
            ("small_lex_s_pairs", 3),
            ("unit_grevlex_s_pairs", 1),
            ("zero_grevlex_s_pairs", 0),
            ("one_element_grevlex_s_pairs", 1),
        ] {
            assert_eq!(
                s_pairs(name),
                serde_json::json!({"pairs": [], "representations": {"rows": t, "columns": []}}),
                "{name}"
            );
        }
        let case = ideal_case(&cases, "one_element_grevlex_s_pairs");
        let (ring, g, _) = basis_and_s_pairs(case, &case["reply"]);
        assert_eq!(g, polynomials(&ring, &[&[("1", &[2, 0]), ("-1", &[0, 2])]]));
        let case = ideal_case(&cases, "monomials_grevlex_s_pairs");
        let (ring, g, _) = basis_and_s_pairs(case, &case["reply"]);
        assert_eq!(
            g,
            polynomials(
                &ring,
                &[&[("1", &[1, 1])], &[("1", &[2, 0])], &[("1", &[0, 3])]]
            )
        );
        assert!(s_polynomial(&ring, &g, 0, 1).1.is_zero());
        assert!(s_polynomial(&ring, &g, 0, 2).1.is_zero());
        assert_eq!(
            s_pairs("monomials_grevlex_s_pairs"),
            serde_json::json!({"pairs": [[0, 1], [0, 2]], "representations": {"rows": 3, "columns": [[], []]}})
        );
    }

    /// Issue #68: a certificate of the S-pairs whose data is wrong is
    /// refused, each change first shown with the test's own arithmetic: with
    /// G = (z^2 - yw, yz - xw, y^2 - xz) and H = (-w e_2 | x e_0), a changed
    /// h breaks S(g_0, g_1) = sum_k g_k H_k0; the representation straight
    /// from the definition, S(g_0, g_1) = y g_0 - z g_1, holds but
    /// lm(y g_0) = yz^2 is L itself; a pair left out, a coprime pair listed,
    /// the pairs reordered; the S-pairs missing where the job asked for them
    /// or given where it did not; and a changed h in a normal form and in a
    /// certificate of f not in I.
    #[test]
    fn changed_s_pair_replies_are_refused() {
        let cases = ideal_cases();
        let refused = |case: &serde_json::Value, reply: &serde_json::Value, why: &str| {
            let operation = case["operation"].as_str().unwrap();
            let r = check_reply(operation, &case["arguments"], reply);
            assert!(
                matches!(&r, Err(CasError::UnsupportedResult(m)) if m.contains(why)),
                "{why}: {r:?}"
            );
        };
        let changed = |reply: &serde_json::Value, changes: &[(&str, serde_json::Value)]| {
            let mut reply = reply.clone();
            for (pointer, to) in changes {
                *reply.pointer_mut(pointer).unwrap() = to.clone();
            }
            reply
        };
        let case = ideal_case(&cases, "twisted_cubic_grevlex_s_pairs");
        let reply = &case["reply"];
        let (ring, g, s_pairs) = basis_and_s_pairs(case, reply);
        let columns = s_pairs["representations"]["columns"].clone();
        let t = |c: &str, e: [u32; 4]| serde_json::json!([{"coefficient": c, "exponents": e}]);
        let first_h = "/certificate/s_pairs/representations/columns/0/0/terms/0/coefficient";
        assert!(s_pair_holds(&ring, &g, &s_pairs, 0));

        // -w made w.
        let changed_h = changed(reply, &[(first_h, serde_json::json!("1"))]);
        let (_, _, changed_s_pairs) = basis_and_s_pairs(case, &changed_h);
        assert!(!s_pair_holds(&ring, &g, &changed_s_pairs, 0));
        refused(case, &changed_h, "S(g_0, g_1) is not sum_k g_k H_k0");

        // h_0 = L / lm(g_0) = y, h_1 = -L / lm(g_1) = -z: the equation holds,
        // and lm(h_0 g_0) = yz^2 = L.
        let definition = changed(
            reply,
            &[(
                "/certificate/s_pairs/representations/columns/0",
                serde_json::json!([
                    {"row": 0, "terms": t("1", [0, 1, 0, 0])},
                    {"row": 1, "terms": t("-1", [0, 0, 1, 0])}
                ]),
            )],
        );
        let (_, _, definition_s_pairs) = basis_and_s_pairs(case, &definition);
        assert!(s_pair_holds(&ring, &g, &definition_s_pairs, 0));
        assert_eq!(s_polynomial(&ring, &g, 0, 1).0, [0, 1, 2, 0]);
        refused(
            case,
            &definition,
            "H_00 gives no lcm representation of S(g_0, g_1)",
        );

        // The pair (1, 2) left out.
        let left_out = changed(
            reply,
            &[
                ("/certificate/s_pairs/pairs", serde_json::json!([[0, 1]])),
                (
                    "/certificate/s_pairs/representations/columns",
                    serde_json::json!([columns[0]]),
                ),
            ],
        );
        refused(case, &left_out, "s_pairs.pairs lacks the pair (1, 2)");

        // (0, 2) listed, z^2 and y^2 coprime, with an empty column: the list
        // is refused before H is read.
        let coprime = changed(
            reply,
            &[
                (
                    "/certificate/s_pairs/pairs",
                    serde_json::json!([[0, 1], [0, 2], [1, 2]]),
                ),
                (
                    "/certificate/s_pairs/representations/columns",
                    serde_json::json!([columns[0], [], columns[1]]),
                ),
            ],
        );
        refused(
            case,
            &coprime,
            "s_pairs.pairs[1] = (0, 2): the leading monomials of g_0 and g_2 are coprime",
        );

        // Both pairs with their columns, in the other order.
        let reordered = changed(
            reply,
            &[
                (
                    "/certificate/s_pairs/pairs",
                    serde_json::json!([[1, 2], [0, 1]]),
                ),
                (
                    "/certificate/s_pairs/representations/columns",
                    serde_json::json!([columns[1], columns[0]]),
                ),
            ],
        );
        refused(case, &reordered, "s_pairs.pairs[0] is (1, 2), not (0, 1)");

        // The S-pairs missing where the job asked for them, and given where
        // it did not.
        let representation = ideal_case(&cases, "twisted_cubic_grevlex");
        refused(case, &representation["reply"], ideal::S_PAIRS_MISSING);
        refused(representation, reply, ideal::S_PAIRS_UNREQUESTED);

        // A normal form and a certificate of f not in I with -w made w: the
        // same G and H as above, so the same equation breaks.
        for name in [
            "normal_form_twisted_cubic_s_pairs",
            "membership_twisted_cubic_not_in_s_pairs",
        ] {
            let case = ideal_case(&cases, name);
            let reply = &case["reply"];
            let (_, other_g, other_s_pairs) = basis_and_s_pairs(case, reply);
            assert_eq!(other_g, g, "{name}");
            assert_eq!(other_s_pairs, s_pairs, "{name}");
            let changed_h = changed(reply, &[(first_h, serde_json::json!("1"))]);
            refused(case, &changed_h, "S(g_0, g_1) is not sum_k g_k H_k0");
        }
    }

    /// Issue #68, acceptance 4: the lex job F = (-xyz + z^2, -xyz - x^2 +
    /// xy) sent to the driver with the grevlex order items (the case
    /// `mixup_lex_answered_in_grevlex`, recorded with that fault). Its reply
    /// is the grevlex one, G = (x^2 - xy + z^2, xyz - z^2, yz^3 + xz^2 -
    /// yz^2) with the grevlex S-pairs (0, 1) and (1, 2). As sent it is
    /// refused (not in increasing lex order). Put in increasing lex order
    /// (lex leading monomials xz^2 < xyz < x^2), with C, N, H and the pairs
    /// renumbered, it passes with `representation`, `gb.groebner` asserted;
    /// with the S-pairs it is refused at `gb.groebner`: every lex leading
    /// monomial has x, so all three pairs are listed in lex and the grevlex
    /// list lacks (0, 2). It is no lex Groebner basis: the lex reply holds an
    /// element of I with the lex leading monomial y^2 z^3, which no lex
    /// leading monomial of G divides.
    #[test]
    fn the_grevlex_reply_to_a_lex_job_is_refused_by_its_s_pairs() {
        use algebramesh_core::guarantee::EvidenceLevel::Asserted;
        let cases = ideal_cases();
        let refused = |arguments: &serde_json::Value, reply: &serde_json::Value, why: &str| {
            let r = check_reply(ideal::IDEAL_GROEBNER_BASIS, arguments, reply);
            assert!(
                matches!(&r, Err(CasError::UnsupportedResult(m)) if m.contains(why)),
                "{why}: {r:?}"
            );
        };
        let case = ideal_case(&cases, "mixup_lex_answered_in_grevlex");
        assert!(case["fault"].is_string());
        let arguments = &case["arguments"];
        let lex = ideal_case(&cases, "mixup_lex_s_pairs");
        let grevlex = ideal_case(&cases, "mixup_grevlex_s_pairs");
        assert_eq!(arguments, &lex["arguments"]);
        assert_eq!(arguments["ideal"]["ring"]["order"], "lex");
        // The request the backend sends for the lex job, with the order items
        // of grevlex: that of the grevlex job.
        let job = parse(&CasRequest {
            job_id: algebramesh_core::JobId::new(),
            operation: ideal::IDEAL_GROEBNER_BASIS.into(),
            arguments: arguments.clone(),
            limits: Default::default(),
        })
        .unwrap();
        let mut sent = request(&job);
        assert_eq!(sent["order"], serde_json::json!([["lex", 1, 3]]));
        sent["order"] = serde_json::json!([["degrevlex", 1, 3]]);
        assert_eq!(sent, case["request"]);
        assert_eq!(case["request"], grevlex["request"]);
        let reply = &case["reply"];
        assert_eq!(reply, &grevlex["reply"]);
        assert_eq!(
            reply["certificate"]["s_pairs"]["pairs"],
            serde_json::json!([[0, 1], [1, 2]])
        );
        refused(
            arguments,
            reply,
            "the leading monomials of g_0 and g_1 are not in strictly increasing order",
        );

        // In increasing lex order: g_k becomes g_(2-k), the pair (0, 1)
        // becomes (1, 2) and (1, 2) becomes (0, 1).
        let mut sorted = reply.clone();
        let reverse = |v: &mut serde_json::Value| v.as_array_mut().unwrap().reverse();
        let renumber_rows = |m: &mut serde_json::Value| {
            for column in m["columns"].as_array_mut().unwrap() {
                for entry in column.as_array_mut().unwrap() {
                    entry["row"] = (2 - entry["row"].as_u64().unwrap()).into();
                }
            }
        };
        reverse(&mut sorted["basis"]);
        reverse(&mut sorted["certificate"]["basis_in_generators"]["columns"]);
        renumber_rows(&mut sorted["certificate"]["generators_in_basis"]);
        let s_pairs = &mut sorted["certificate"]["s_pairs"];
        renumber_rows(&mut s_pairs["representations"]);
        reverse(&mut s_pairs["representations"]["columns"]);
        let (_, g, s_pairs) = basis_and_s_pairs(case, &sorted);
        let leading: Vec<&[u32]> = g.iter().map(|p| p.leading_monomial().unwrap()).collect();
        assert_eq!(leading, [&[1, 0, 2][..], &[1, 1, 1], &[2, 0, 0]]);
        assert_eq!(non_coprime_pairs(&g), [[0, 1], [0, 2], [1, 2]]);
        assert_eq!(s_pairs["pairs"], serde_json::json!([[0, 1], [1, 2]]));

        let mut representation = arguments.clone();
        representation["certificate"] = "representation".into();
        let mut without = sorted.clone();
        without["certificate"]
            .as_object_mut()
            .unwrap()
            .remove("s_pairs");
        let checked = check_reply(ideal::IDEAL_GROEBNER_BASIS, &representation, &without).unwrap();
        let groebner = checked
            .guarantee
            .unwrap()
            .claims
            .into_iter()
            .find(|c| c.id == "gb.groebner")
            .unwrap();
        assert_eq!(groebner.level, Asserted);
        refused(arguments, &sorted, "s_pairs.pairs[1] is (1, 2), not (0, 2)");

        // An element of I with the lex leading monomial y^2 z^3 in the checked
        // lex reply.
        let (_, lex_g, _) = basis_and_s_pairs(lex, &lex["reply"]);
        let y2z3: &[u32] = &[0, 2, 3];
        assert_eq!(lex_g[0].leading_monomial(), Some(y2z3));
        assert!(
            leading
                .iter()
                .all(|m| m.iter().zip(y2z3).any(|(a, b)| a > b))
        );
    }

    /// A polynomial of a driver's reply (its terms as the driver writes
    /// them), read in `ring`.
    fn reply_polynomial(ring: &RingSpec, terms: &serde_json::Value) -> Polynomial {
        let data =
            serde_json::from_value(serde_json::json!({"type": "polynomial", "terms": terms}))
                .unwrap();
        Polynomial::from_data(ring.clone(), &data).unwrap()
    }

    fn reply_polynomials(ring: &RingSpec, list: &serde_json::Value) -> Vec<Polynomial> {
        list.as_array()
            .unwrap()
            .iter()
            .map(|terms| reply_polynomial(ring, terms))
            .collect()
    }

    /// The terms of `p` as a driver writes them.
    fn reply_terms(p: &Polynomial) -> serde_json::Value {
        serde_json::to_value(p.to_data().terms).unwrap()
    }

    /// sum_k a_k b_k in `ring`.
    fn dot(ring: &RingSpec, a: &[Polynomial], b: &[Polynomial]) -> Polynomial {
        assert_eq!(a.len(), b.len());
        a.iter()
            .zip(b)
            .fold(Polynomial::zero(ring.clone()), |sum, (x, y)| {
                sum.add(&x.mul(y).unwrap()).unwrap()
            })
    }

    /// The ring, the generators and the polynomial of a job of `case`.
    fn ideal_job(case: &serde_json::Value) -> (RingSpec, Vec<Polynomial>, Polynomial) {
        let request =
            IdealRequest::parse(case["operation"].as_str().unwrap(), &case["arguments"]).unwrap();
        let ring = request.ring().clone();
        let f = request.polynomial.clone().unwrap();
        (ring, request.ideal.generators, f)
    }

    /// Issues #67, #68: the replies OSCAR gave in its image
    /// (`testdata/ideal.json`) to the `ideal.normal_form` and
    /// `ideal.membership` requests this backend sends pass every check, with
    /// the claims and levels of `docs/claims.md`: `gb.groebner` checked when
    /// the job asked for the S-pairs, so that `normal_form.unique` and
    /// `membership.not_in` are checked in effect, and asserted otherwise, so
    /// that they are asserted in effect; without a certificate, the claims it
    /// supports asserted. A member keeps its coefficients alone when the job
    /// asked for the S-pairs, and `membership.in` is checked. The cases are
    /// the twisted cubic with f in I and f not in I, the normal form of one f
    /// in lex and in grevlex, the zero and the unit ideal, no generators, a
    /// zero generator, f = 0, and coefficients beyond 64 bits; the remainders
    /// and the memberships are those computed by hand. The cases with no
    /// generators, a zero generator and f = 0 are those of the Macaulay2
    /// backend's `testdata/ideal.json`.
    #[test]
    fn recorded_normal_form_and_membership_replies_are_checked() {
        use algebramesh_core::guarantee::EvidenceLevel::{self, Asserted, Checked};
        let cases = ideal_cases();
        let mut seen = 0;
        for case in cases
            .iter()
            .filter(|c| c["operation"] != ideal::IDEAL_GROEBNER_BASIS)
        {
            seen += 1;
            let name = case["name"].as_str().unwrap();
            let op = case["operation"].as_str().unwrap();
            let job = parse(&CasRequest {
                job_id: algebramesh_core::JobId::new(),
                operation: op.into(),
                arguments: case["arguments"].clone(),
                limits: Default::default(),
            })
            .unwrap();
            assert_eq!(request(&job), case["request"], "{name}: the request sent");
            let checked = check_reply(op, &case["arguments"], &case["reply"])
                .unwrap_or_else(|e| panic!("{name}: {e:?}"));
            let guarantee = checked.guarantee.unwrap();
            let claims: Vec<(&str, EvidenceLevel)> = guarantee
                .claims
                .iter()
                .map(|c| (c.id.as_str(), c.level))
                .collect();
            let with_certificate = case["arguments"]["certificate"] != "none";
            let certified = if with_certificate { Checked } else { Asserted };
            let with_s_pairs = case["arguments"]["certificate"] == "s_pairs";
            let groebner = if with_s_pairs { Checked } else { Asserted };
            let expected = if op == ideal::IDEAL_NORMAL_FORM {
                vec![
                    ("gb.reduced_form", Checked),
                    ("gb.contained", certified),
                    ("gb.contains", certified),
                    ("gb.groebner", groebner),
                    ("normal_form.reduced", certified),
                    ("normal_form.unique", Checked),
                ]
            } else if checked.value["member"] == true {
                vec![("membership.in", certified)]
            } else if with_certificate {
                vec![
                    ("gb.reduced_form", Checked),
                    ("gb.contains", Checked),
                    ("gb.groebner", groebner),
                    ("normal_form.reduced", Checked),
                    ("membership.not_in", Checked),
                ]
            } else {
                vec![("membership.not_in", Asserted)]
            };
            assert_eq!(claims, expected, "{name}");
            for (id, _) in &claims {
                if *id == "normal_form.unique" || *id == "membership.not_in" {
                    assert_eq!(guarantee.effective_level(id), groebner, "{name}: {id}");
                }
            }
        }
        assert_eq!(seen, 24);

        let value = |name: &str| {
            let case = ideal_case(&cases, name);
            let request =
                IdealRequest::parse(case["operation"].as_str().unwrap(), &case["arguments"])
                    .unwrap();
            (
                request.ring().clone(),
                request.value(&case["reply"]).unwrap(),
            )
        };
        let remainder = |name: &str| {
            let (ring, value) = value(name);
            let result: ideal::NormalFormResult = serde_json::from_value(value).unwrap();
            let r = Polynomial::from_data(ring.clone(), &result.remainder).unwrap();
            (ring, r)
        };
        let member = |name: &str| value(name).1["member"].as_bool().unwrap();

        // The twisted cubic, grevlex: x^2w + xyz + y^3 + 5 reduces to
        // 3x^2w + 5 (xyz -> x^2w, y^3 -> xyz -> x^2w).
        for name in [
            "normal_form_twisted_cubic",
            "normal_form_twisted_cubic_none",
        ] {
            let (ring, r) = remainder(name);
            assert_eq!(
                r,
                polynomials(&ring, &[&[("3", &[2, 0, 0, 1]), ("5", &[0, 0, 0, 0])]])[0],
                "{name}"
            );
        }
        // x^2z - xy^2 + yzw - z^3 = x f_1 + z f_3 lies in I; x^2w + xyz + y^3 + 5
        // does not.
        assert!(member("membership_twisted_cubic_in"));
        assert!(member("membership_twisted_cubic_in_none"));
        assert!(!member("membership_twisted_cubic_not_in"));
        assert!(!member("membership_twisted_cubic_not_in_none"));
        let case = ideal_case(&cases, "membership_twisted_cubic_in");
        let (ring, _, _) = ideal_job(case);
        assert_eq!(
            reply_polynomials(&ring, &case["reply"]["certificate"]["coefficients"]),
            polynomials(
                &ring,
                &[&[("1", &[1, 0, 0, 0])], &[], &[("1", &[0, 0, 1, 0])]]
            )
        );

        // F = {x^2y - z/2, 2xy^2 - x, 3xz - y}, f = x^3 + y^3 + z^3 - xyz: in
        // lex, x = 3z^2, y = 9z^3 and z^9 = z^3/162 give 11/2 z^3; in grevlex
        // the remainder is 11/18 y, and the two differ by 11/18 (y - 9z^3),
        // an element of I.
        let (ring, lex) = remainder("normal_form_small_lex");
        assert_eq!(lex, polynomials(&ring, &[&[("11/2", &[0, 0, 3])]])[0]);
        let (ring, grevlex) = remainder("normal_form_small_grevlex");
        assert_eq!(grevlex, polynomials(&ring, &[&[("11/18", &[0, 1, 0])]])[0]);
        assert!(!member("membership_small_lex"));
        let case = ideal_case(&cases, "membership_small_lex");
        let (ring, _, _) = ideal_job(case);
        assert_eq!(
            reply_polynomial(&ring, &case["reply"]["certificate"]["remainder"]),
            lex
        );

        // The zero ideal: no basis, r = f; 0 is its only member.
        let (ring, r) = remainder("normal_form_zero_ideal");
        assert_eq!(
            r,
            polynomials(&ring, &[&[("1", &[2, 0]), ("1", &[0, 1])]])[0]
        );
        assert!(!member("membership_zero_ideal"));
        assert!(member("membership_zero_in_zero_ideal"));
        // No generators: r = f.
        let (ring, r) = remainder("normal_form_no_generators");
        assert_eq!(
            r,
            polynomials(
                &ring,
                &[&[("1", &[3, 2]), ("2", &[0, 1]), ("-5/3", &[0, 0])]]
            )[0]
        );
        // The unit ideal: r = 0 and every f is a member.
        assert!(remainder("normal_form_unit_ideal").1.is_zero());
        assert!(remainder("normal_form_zero_polynomial").1.is_zero());
        assert!(member("membership_unit_ideal"));
        // A zero generator, grlex: x^3 - 1 = x (x^2 - y) + (xy - 1), with the
        // coefficient 0 for the zero generator.
        assert!(member("membership_zero_generator"));
        let case = ideal_case(&cases, "membership_zero_generator");
        let (ring, generators, f) = ideal_job(case);
        let c = reply_polynomials(&ring, &case["reply"]["certificate"]["coefficients"]);
        assert_eq!(dot(&ring, &c, &generators), f);

        // B = 2^64 + 1, F = {x^2 - By, xy - 1}, lex: x^2 + x reduces to
        // By + By^2 (x^2 -> By, x -> By^2), and x^3 - B = x f_1 + B f_2.
        let b = "18446744073709551617";
        let (ring, r) = remainder("normal_form_big_coefficient_lex");
        assert_eq!(r, polynomials(&ring, &[&[(b, &[0, 2]), (b, &[0, 1])]])[0]);
        assert!(member("membership_big_coefficient_lex"));
        let case = ideal_case(&cases, "membership_big_coefficient_lex");
        let (ring, _, _) = ideal_job(case);
        assert_eq!(
            reply_polynomials(&ring, &case["reply"]["certificate"]["coefficients"]),
            polynomials(&ring, &[&[("1", &[1, 0])], &[(b, &[0, 0])]])
        );
    }

    /// Issue #67: a recorded reply to `ideal.normal_form` or
    /// `ideal.membership` with a changed quotient, remainder, coefficient or
    /// entry of N, each first shown to break its equation with the test's own
    /// arithmetic, is refused; so are a remainder that is not reduced (with
    /// the quotients changed to keep the representation true), a zero
    /// remainder claimed for f not in I, and a certificate of the wrong kind
    /// for `member`.
    #[test]
    fn changed_normal_form_and_membership_replies_are_refused() {
        let cases = ideal_cases();
        let refused = |r: Result<CasResult, CasError>, why: &str| {
            assert!(
                matches!(&r, Err(CasError::UnsupportedResult(m)) if m.contains(why)),
                "{why}: {r:?}"
            )
        };

        // The twisted cubic, f = x^2w + xyz + y^3 + 5: f - r = sum_k q_k g_k.
        let case = ideal_case(&cases, "normal_form_twisted_cubic");
        let (ring, _, f) = ideal_job(case);
        let reply = &case["reply"];
        let g = reply_polynomials(&ring, &reply["basis"]);
        let q = reply_polynomials(&ring, &reply["certificate"]["quotients"]);
        let r = reply_polynomial(&ring, &reply["remainder"]);
        let one = polynomials(&ring, &[&[("1", &[0, 0, 0, 0])]]).remove(0);
        assert_eq!(f.sub(&r).unwrap(), dot(&ring, &q, &g));
        let check = |reply: &serde_json::Value| {
            check_reply(ideal::IDEAL_NORMAL_FORM, &case["arguments"], reply)
        };

        // q_0 + 1: the sum gains g_0, which is not zero.
        let mut changed_q = q.clone();
        changed_q[0] = q[0].add(&one).unwrap();
        assert_ne!(f.sub(&r).unwrap(), dot(&ring, &changed_q, &g));
        let mut mutated = reply.clone();
        mutated["certificate"]["quotients"][0] = reply_terms(&changed_q[0]);
        refused(check(&mutated), "f - r is not sum_k q_k g_k");

        // r + 1: still reduced, but f - r changes and the quotients do not.
        let changed_r = r.add(&one).unwrap();
        assert_ne!(f.sub(&changed_r).unwrap(), dot(&ring, &q, &g));
        let mut mutated = reply.clone();
        mutated["remainder"] = reply_terms(&changed_r);
        refused(check(&mutated), "f - r is not sum_k q_k g_k");

        // r + g_0 with q_0 - 1: the representation holds, but the remainder
        // has the term lm(g_0) = z^2.
        let unreduced = r.add(&g[0]).unwrap();
        changed_q = q.clone();
        changed_q[0] = q[0].sub(&one).unwrap();
        assert_eq!(f.sub(&unreduced).unwrap(), dot(&ring, &changed_q, &g));
        let lm = g[0].leading_monomial().unwrap();
        assert_eq!(lm, &[0, 0, 2, 0]);
        assert!(unreduced.terms().any(|(e, _)| e == lm));
        let mut mutated = reply.clone();
        mutated["remainder"] = reply_terms(&unreduced);
        mutated["certificate"]["quotients"][0] = reply_terms(&changed_q[0]);
        refused(
            check(&mutated),
            "a term of the remainder is divisible by the leading monomial of g_0",
        );

        // f = x^2z - xy^2 + yzw - z^3 = sum_i c_i f_i with c = (x, 0, z); c_0 + 1
        // adds f_0.
        let in_case = ideal_case(&cases, "membership_twisted_cubic_in");
        let (ring, generators, f) = ideal_job(in_case);
        let in_reply = &in_case["reply"];
        let c = reply_polynomials(&ring, &in_reply["certificate"]["coefficients"]);
        assert_eq!(dot(&ring, &c, &generators), f);
        let mut changed_c = c.clone();
        changed_c[0] = c[0].add(&one).unwrap();
        assert_ne!(dot(&ring, &changed_c, &generators), f);
        let mut mutated = in_reply.clone();
        mutated["certificate"]["coefficients"][0] = reply_terms(&changed_c[0]);
        refused(
            check_reply(ideal::IDEAL_MEMBERSHIP, &in_case["arguments"], &mutated),
            "f is not sum_i c_i f_i",
        );

        // The same f (a member, as c shows) claimed not to be one, with the
        // basis and N of the twisted cubic, the quotients (-z, 0, -x) and
        // r = 0: f = -z g_0 - x g_2 holds, and member false is refused.
        let not_in_case = ideal_case(&cases, "membership_twisted_cubic_not_in");
        let not_in = &not_in_case["reply"];
        let g = reply_polynomials(&ring, &not_in["certificate"]["basis"]);
        let q = polynomials(
            &ring,
            &[&[("-1", &[0, 0, 1, 0])], &[], &[("-1", &[1, 0, 0, 0])]],
        );
        assert_eq!(dot(&ring, &q, &g), f);
        let mut mutated = not_in.clone();
        mutated["certificate"]["quotients"] =
            serde_json::Value::Array(q.iter().map(reply_terms).collect());
        mutated["certificate"]["remainder"] = serde_json::json!([]);
        refused(
            check_reply(ideal::IDEAL_MEMBERSHIP, &in_case["arguments"], &mutated),
            "member is false and the remainder is 0",
        );

        // Certificates of the wrong kind for member.
        let mut mutated = not_in.clone();
        mutated["member"] = true.into();
        refused(
            check_reply(ideal::IDEAL_MEMBERSHIP, &in_case["arguments"], &mutated),
            "member is true and the certificate is not one of coefficients",
        );
        let mut mutated = in_reply.clone();
        mutated["member"] = false.into();
        refused(
            check_reply(ideal::IDEAL_MEMBERSHIP, &not_in_case["arguments"], &mutated),
            "member is false and the certificate is one of coefficients",
        );

        // F = {x^2y - z/2, 2xy^2 - x, 3xz - y} in lex: one coefficient of the
        // first column of N changed, so that f_0 is not sum_k g_k N_k0.
        let case = ideal_case(&cases, "membership_small_lex");
        let (ring, generators, _) = ideal_job(case);
        let pointer = "/certificate/generators_in_basis/columns/0/0/terms/0/coefficient";
        let mut mutated = case["reply"].clone();
        *mutated.pointer_mut(pointer).unwrap() = serde_json::json!("12345");
        let g = reply_polynomials(&ring, &mutated["certificate"]["basis"]);
        let column = |reply: &serde_json::Value| {
            let mut n = vec![Polynomial::zero(ring.clone()); g.len()];
            for entry in reply["certificate"]["generators_in_basis"]["columns"][0]
                .as_array()
                .unwrap()
            {
                n[entry["row"].as_u64().unwrap() as usize] =
                    reply_polynomial(&ring, &entry["terms"]);
            }
            dot(&ring, &g, &n)
        };
        assert_eq!(column(&case["reply"]), generators[0]);
        assert_ne!(column(&mutated), generators[0]);
        refused(
            check_reply(ideal::IDEAL_MEMBERSHIP, &case["arguments"], &mutated),
            "f_0 is not sum_k g_k N_k0",
        );
    }

    /// Issues #66, #67: for each ideal operation, an ideal over ZZ and a ring
    /// without variables are refused as invalid arguments before the driver
    /// is asked (since #68 a request for S-pairs is taken); so are a polynomial for
    /// `ideal.groebner_basis`, and a missing polynomial or one of another
    /// ring for `ideal.normal_form` and `ideal.membership`.
    #[tokio::test]
    async fn ideal_jobs_oscar_cannot_take_are_refused_before_running() {
        // Starting this executable would fail as unavailable.
        let backend = OscarBackend::new(OscarConfig {
            julia: "/nonexistent/julia".into(),
            ..OscarConfig::default()
        });
        let cases = ideal_cases();
        let groebner = ideal_case(&cases, "twisted_cubic_grevlex")["arguments"].clone();
        let with_polynomial =
            ideal_case(&cases, "membership_twisted_cubic_in")["arguments"].clone();
        let no_variables = serde_json::json!({"ideal": {"type": "ideal",
            "ring": {"domain": "QQ", "variables": [], "order": "lex"}, "generators": []}});
        for (op, twisted) in [
            (ideal::IDEAL_GROEBNER_BASIS, &groebner),
            (ideal::IDEAL_NORMAL_FORM, &with_polynomial),
            (ideal::IDEAL_MEMBERSHIP, &with_polynomial),
        ] {
            let mut zz = twisted.clone();
            zz["ideal"]["ring"]["domain"] = "ZZ".into();
            let mut no_variables = no_variables.clone();
            let mut other = twisted.clone();
            if op == ideal::IDEAL_GROEBNER_BASIS {
                other["polynomial"] = with_polynomial["polynomial"].clone();
            } else {
                no_variables["polynomial"] = serde_json::json!({"type": "polynomial", "terms": []});
                other["polynomial"]["terms"][0]["exponents"] = serde_json::json!([1, 0]);
            }
            let mut missing = twisted.clone();
            missing.as_object_mut().unwrap().remove("polynomial");
            let mut jobs = vec![
                (zz, ideal::OVER_QQ_ONLY),
                (no_variables, ideal::OVER_QQ_ONLY),
            ];
            if op == ideal::IDEAL_GROEBNER_BASIS {
                jobs.push((other, "unknown field `polynomial`"));
            } else {
                jobs.push((other, "the polynomial: "));
                jobs.push((missing, "missing field `polynomial`"));
            }
            for (arguments, why) in jobs {
                let e = backend
                    .execute(
                        CasRequest {
                            job_id: algebramesh_core::JobId::new(),
                            operation: op.into(),
                            arguments,
                            limits: Default::default(),
                        },
                        ExecutionContext::new(None),
                    )
                    .await
                    .unwrap_err();
                assert!(
                    matches!(e, CasError::InvalidArguments(ref m) if m.contains(why)),
                    "{op}, {why}: {e:?}"
                );
            }
        }
    }

    /// Issue #54: each operation is declared with its signature from
    /// `algebramesh-ops`.
    #[test]
    fn operations_are_declared_with_their_signatures() {
        let caps = OscarBackend::new(OscarConfig::default()).capabilities();
        assert!(!caps.operations.is_empty());
        for op in ideal::OPERATIONS {
            assert!(caps.operations.iter().any(|o| o == op), "{op}");
        }
        for op in &caps.operations {
            let d = algebramesh_ops::descriptor(op).unwrap();
            assert_eq!(
                caps.signatures[op],
                algebramesh_core::Signature::new(d.arguments, d.result)
            );
        }
        assert_eq!(caps.signatures.len(), caps.operations.len());
    }

    #[test]
    fn unconfigured_backend_offers_nothing() {
        assert!(OscarBackend::default().capabilities().operations.is_empty());
    }
    /// The cases of `testdata/module.json` (Issues #97, #98): replies OSCAR
    /// gave in its image, each with the claims and the levels it must get and
    /// what it must not claim.
    fn module_cases() -> Vec<serde_json::Value> {
        serde_json::from_str(include_str!("../testdata/module.json")).unwrap()
    }

    /// The value of a recorded reply, without the wrapping of this backend.
    fn module_reply(case: &serde_json::Value) -> &serde_json::Value {
        &case["reply"]
    }

    /// Issues #97, #98: the recorded replies pass the checks with the claims
    /// their case lists, and give its not_claimed.
    #[test]
    fn recorded_module_replies_are_checked() {
        let mut operations = std::collections::BTreeSet::new();
        for case in module_cases() {
            let name = case["name"].as_str().unwrap();
            let operation = case["operation"].as_str().unwrap();
            operations.insert(operation.to_owned());
            let checked = check_reply(operation, &case["arguments"], &case["reply"])
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            let guarantee = checked.guarantee.unwrap();
            let claims: Vec<serde_json::Value> = guarantee
                .claims
                .iter()
                .map(|c| serde_json::json!([c.id, c.level]))
                .collect();
            assert_eq!(serde_json::Value::from(claims), case["claims"], "{name}");
            assert_eq!(
                serde_json::json!(guarantee.not_claimed),
                case["not_claimed"],
                "{name}"
            );
        }
        let mut offered = module::OPERATIONS.map(String::from).to_vec();
        offered.sort();
        assert_eq!(operations.into_iter().collect::<Vec<_>>(), offered);
    }

    /// Issues #97, #98: one changed coefficient in a syzygy, in d_2 of the
    /// twisted cubic, and in the numerator of its Hilbert series, is refused:
    /// F S, d_1 d_2 and the Hilbert series with the Betti numbers no longer
    /// agree.
    #[test]
    fn changed_module_replies_are_refused() {
        for (name, pointer) in [
            (
                "syzygies_twisted_cubic",
                "/syzygies/columns/0/0/terms/0/coefficient",
            ),
            (
                "resolution_twisted_cubic",
                "/differentials/1/columns/0/0/terms/0/coefficient",
            ),
            ("hilbert_series_twisted_cubic", "/numerator/0"),
        ] {
            let case = module_cases()
                .into_iter()
                .find(|c| c["name"] == name)
                .unwrap();
            let mut reply = module_reply(&case).clone();
            *reply.pointer_mut(pointer).unwrap() = serde_json::json!("2");
            let operation = case["operation"].as_str().unwrap();
            match check_reply(operation, &case["arguments"], &reply) {
                Err(CasError::UnsupportedResult(_)) => {}
                other => panic!("{name}: {other:?}"),
            }
        }
    }
}

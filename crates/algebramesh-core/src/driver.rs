//! Running a CAS driver as a subprocess (design §8, §11).
//!
//! The driver prints exactly one line `ALGEBRAMESH_RESULT <json>` or
//! `ALGEBRAMESH_ERROR <message>` on stdout. Every other line, on stdout or stderr,
//! is kept as log output and never interpreted as data. On timeout, cancellation or
//! when stdout and stderr together exceed the output limit, the whole process
//! group is killed (Unix), so helper processes started by the CAS do not survive
//! the job.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use crate::backend::{CasError, ExecutionContext};

pub const RESULT_PREFIX: &str = "ALGEBRAMESH_RESULT ";
pub const ERROR_PREFIX: &str = "ALGEBRAMESH_ERROR ";

/// Removes control characters and ANSI CSI sequences (`ESC [ ... final`) at the
/// start of a line. CAS start-up code may leave such bytes (for example a carriage
/// return after a progress message) in front of the next line.
pub fn strip_terminal_noise(line: &str) -> &str {
    let mut s = line;
    loop {
        let trimmed = s.trim_start_matches(|c: char| c.is_control() && c != '\x1b');
        if let Some(rest) = trimmed.strip_prefix("\x1b[") {
            match rest.find(|c: char| ('@'..='~').contains(&c)) {
                Some(end) => s = &rest[end + 1..],
                None => return "",
            }
        } else if let Some(rest) = trimmed.strip_prefix('\x1b') {
            s = rest;
        } else {
            return trimmed;
        }
    }
}

/// The payload after `marker`, if the line (apart from terminal noise) starts with it.
pub fn protocol_payload<'a>(line: &'a str, marker: &str) -> Option<&'a str> {
    strip_terminal_noise(line).strip_prefix(marker)
}

#[derive(Debug, Clone)]
pub struct DriverCommand {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub env: Vec<(OsString, OsString)>,
    /// Written to the driver's stdin, which is then closed.
    pub stdin: Option<Vec<u8>>,
    /// Upper bound for stdout and stderr together.
    pub max_output_bytes: usize,
    /// Log lines kept from the end of stderr.
    pub max_log_lines: usize,
    /// Address-space limit of the driver and the processes it starts, in MiB
    /// (`RLIMIT_AS`, set by `/bin/sh -c 'ulimit -v'` before the driver is
    /// executed; Unix only, see [`MEMORY_LIMIT_SUPPORTED`]).
    pub memory_limit_mb: Option<u64>,
}

/// Whether [`DriverCommand::memory_limit_mb`] is enforced on this platform.
pub const MEMORY_LIMIT_SUPPORTED: bool = cfg!(unix);

impl DriverCommand {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            env: Vec::new(),
            stdin: None,
            max_output_bytes: 256 * 1024 * 1024,
            max_log_lines: 200,
            memory_limit_mb: None,
        }
    }

    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DriverOutput {
    pub result: serde_json::Value,
    pub log: Vec<String>,
    pub elapsed: Duration,
}

/// How long the output of a finished driver may take to reach EOF: a helper that
/// escaped the process group (or any process on a platform without groups) can
/// keep a pipe open.
const DRAIN_AFTER_EXIT: Duration = Duration::from_secs(5);

/// The bytes stdout and stderr have delivered so far, against one limit.
#[derive(Clone)]
struct OutputBudget {
    used: Arc<AtomicUsize>,
    cap: usize,
    /// Cancelled when `used` passes `cap`.
    exceeded: CancellationToken,
}

impl OutputBudget {
    fn new(cap: usize) -> Self {
        Self {
            used: Arc::new(AtomicUsize::new(0)),
            cap,
            exceeded: CancellationToken::new(),
        }
    }

    /// Counts `n` more bytes; false once the total is over the limit.
    fn take(&self, n: usize) -> bool {
        let total = self.used.fetch_add(n, Ordering::SeqCst).saturating_add(n);
        if total > self.cap {
            self.exceeded.cancel();
            return false;
        }
        true
    }
}

/// Reads to EOF, counting against `budget`. Stops at once when the budget is
/// spent (by this stream or the other one), so that the supervisor can stop the
/// driver; a read error is returned, not taken for EOF.
async fn read_capped<R: AsyncRead + Unpin>(
    mut reader: R,
    budget: OutputBudget,
) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        let n = tokio::select! {
            r = reader.read(&mut chunk) => r?,
            _ = budget.exceeded.cancelled() => return Ok(out),
        };
        if n == 0 {
            return Ok(out);
        }
        if !budget.take(n) {
            return Ok(out);
        }
        out.extend_from_slice(&chunk[..n]);
    }
}

#[cfg(unix)]
fn kill_group(pid: Option<u32>) {
    use nix::sys::signal::{Signal, killpg};
    use nix::unistd::Pid;
    if let Some(pid) = pid.and_then(|p| i32::try_from(p).ok()) {
        let _ = killpg(Pid::from_raw(pid), Signal::SIGKILL);
    }
}

#[cfg(not(unix))]
fn kill_group(_pid: Option<u32>) {}

/// Whether the driver's log reads like an allocation failure.
fn out_of_memory(log: &[String]) -> bool {
    const MARKERS: [&str; 6] = [
        "out of memory",
        "cannot allocate",
        "bad_alloc",
        "memory exhausted",
        "outofmemory",
        "not enough memory",
    ];
    log.iter().any(|line| {
        let line = line.to_ascii_lowercase();
        MARKERS.iter().any(|m| line.contains(m))
    })
}

#[cfg(unix)]
fn killed_by_signal(status: &std::process::ExitStatus) -> bool {
    std::os::unix::process::ExitStatusExt::signal(status).is_some()
}

#[cfg(not(unix))]
fn killed_by_signal(_status: &std::process::ExitStatus) -> bool {
    false
}

fn tail(lines: impl Iterator<Item = String>, keep: usize) -> Vec<String> {
    let all: Vec<String> = lines.collect();
    let skip = all.len().saturating_sub(keep);
    all.into_iter().skip(skip).collect()
}

/// Runs `command` under the deadline and cancellation of `ctx`.
pub async fn run_driver(
    command: &DriverCommand,
    ctx: &ExecutionContext,
) -> Result<DriverOutput, CasError> {
    let started = tokio::time::Instant::now();
    let mut cmd = match command.memory_limit_mb {
        None => {
            let mut cmd = Command::new(&command.program);
            cmd.args(&command.args);
            cmd
        }
        // POSIX sh sets RLIMIT_AS (KiB) and replaces itself with the driver, which
        // and whose children inherit the limit.
        #[cfg(unix)]
        Some(mb) => {
            let mut cmd = Command::new("/bin/sh");
            cmd.arg("-c")
                .arg(r#"ulimit -v "$0" && exec "$@""#)
                .arg(mb.saturating_mul(1024).to_string())
                .arg(&command.program)
                .args(&command.args);
            cmd
        }
        #[cfg(not(unix))]
        Some(_) => {
            return Err(CasError::InvalidArguments(
                "a memory limit is not supported on this platform".into(),
            ));
        }
    };
    cmd.envs(command.env.iter().map(|(k, v)| (k, v)))
        .stdin(if command.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = cmd.spawn().map_err(|e| {
        CasError::Unavailable(format!("cannot start {}: {e}", command.program.display()))
    })?;
    let pid = child.id();

    if let (Some(input), Some(mut stdin)) = (&command.stdin, child.stdin.take()) {
        let input = input.clone();
        tokio::spawn(async move {
            let _ = stdin.write_all(&input).await;
            let _ = stdin.shutdown().await;
        });
    }
    let cap = command.max_output_bytes;
    let budget = OutputBudget::new(cap);
    let stdout = child
        .stdout
        .take()
        .map(|s| tokio::spawn(read_capped(s, budget.clone())));
    let stderr = child
        .stderr
        .take()
        .map(|s| tokio::spawn(read_capped(s, budget.clone())));
    let over_limit = || {
        CasError::ResourceLimit(format!(
            "the driver wrote more than {cap} bytes to stdout and stderr together"
        ))
    };

    let deadline = ctx.expired();
    let status = tokio::select! {
        s = child.wait() => s.map_err(|e| CasError::Unavailable(format!("waiting for the driver failed: {e}")))?,
        _ = ctx.cancel.cancelled() => {
            kill_group(pid);
            let _ = child.kill().await;
            return Err(CasError::Cancelled);
        }
        _ = deadline => {
            kill_group(pid);
            let _ = child.kill().await;
            return Err(ctx.timed_out());
        }
        // The readers stopped reading: a driver that goes on writing would block
        // on a full pipe forever, so it is stopped here.
        _ = budget.exceeded.cancelled() => {
            kill_group(pid);
            let _ = child.kill().await;
            return Err(over_limit());
        }
    };
    // Helpers left in the group by the driver are not part of the result.
    kill_group(pid);

    let collect = |h: Option<tokio::task::JoinHandle<std::io::Result<Vec<u8>>>>| async move {
        let Some(h) = h else {
            return Ok(Vec::new());
        };
        match tokio::time::timeout(DRAIN_AFTER_EXIT, h).await {
            Ok(Ok(r)) => r.map_err(|e| {
                CasError::Unavailable(format!("reading the driver's output failed: {e}"))
            }),
            Ok(Err(e)) => Err(CasError::Unavailable(format!(
                "reading the driver's output failed: {e}"
            ))),
            Err(_) => Err(CasError::Unavailable(
                "the driver exited, but a process it started keeps its output open".into(),
            )),
        }
    };
    let out = collect(stdout).await?;
    let err = collect(stderr).await?;
    if budget.exceeded.is_cancelled() {
        return Err(over_limit());
    }
    let out = String::from_utf8_lossy(&out);
    let err = String::from_utf8_lossy(&err);

    let mut result = None;
    let mut error = None;
    let mut log = Vec::new();
    for line in out.lines() {
        if let Some(json) = protocol_payload(line, RESULT_PREFIX) {
            if result.is_some() {
                return Err(CasError::UnsupportedResult(
                    "driver printed more than one result".into(),
                ));
            }
            result = Some(json.to_owned());
        } else if let Some(message) = protocol_payload(line, ERROR_PREFIX) {
            error = Some(message.to_owned());
        } else if !line.trim().is_empty() {
            log.push(format!("stdout: {line}"));
        }
    }
    let mut log = tail(log.into_iter(), command.max_log_lines);
    log.extend(tail(
        err.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| format!("stderr: {l}")),
        command.max_log_lines,
    ));

    if let Some(message) = error {
        return Err(CasError::Cas(message));
    }
    if let Some(mb) = command.memory_limit_mb
        && result.is_none()
        && !status.success()
        && (out_of_memory(&log) || killed_by_signal(&status))
    {
        return Err(CasError::ResourceLimit(format!(
            "the driver failed under the memory limit of {mb} MiB ({status}); last output: {}",
            log.last().cloned().unwrap_or_default()
        )));
    }
    match result {
        Some(json) if status.success() => Ok(DriverOutput {
            result: serde_json::from_str(&json).map_err(|e| {
                CasError::UnsupportedResult(format!("driver result is not JSON: {e}"))
            })?,
            log,
            elapsed: started.elapsed(),
        }),
        _ => Err(CasError::Cas(format!(
            "driver exited with {status} without a result; last output: {}",
            log.last().cloned().unwrap_or_default()
        ))),
    }
}

#[cfg(test)]
mod noise_tests {
    use super::*;

    #[test]
    fn strips_leading_terminal_noise_only() {
        assert_eq!(
            protocol_payload("\rALGEBRAMESH_READY {}", "ALGEBRAMESH_READY "),
            Some("{}")
        );
        assert_eq!(
            protocol_payload("\x1b[?1034h\x1b[2KALGEBRAMESH_RESULT 1", RESULT_PREFIX),
            Some("1")
        );
        assert_eq!(
            protocol_payload("log ALGEBRAMESH_RESULT 1", RESULT_PREFIX),
            None
        );
        assert_eq!(strip_terminal_noise("\x1b["), "");
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn sh(script: &str) -> DriverCommand {
        DriverCommand::new("/bin/sh").arg("-c").arg(script)
    }

    #[tokio::test]
    async fn separates_result_and_log() {
        let out = run_driver(
            &sh("echo hello; echo 'ALGEBRAMESH_RESULT {\"a\": 1}'; echo warn >&2"),
            &ExecutionContext::new(None),
        )
        .await
        .unwrap();
        assert_eq!(out.result, serde_json::json!({"a": 1}));
        assert_eq!(out.log, ["stdout: hello", "stderr: warn"]);
    }

    #[tokio::test]
    async fn reports_driver_errors_and_missing_results() {
        let ctx = ExecutionContext::new(None);
        assert_eq!(
            run_driver(&sh("echo 'ALGEBRAMESH_ERROR bad input'; exit 1"), &ctx).await,
            Err(CasError::Cas("bad input".into()))
        );
        assert!(matches!(
            run_driver(&sh("exit 3"), &ctx).await,
            Err(CasError::Cas(_))
        ));
        assert!(matches!(
            run_driver(&DriverCommand::new("/nonexistent/driver"), &ctx).await,
            Err(CasError::Unavailable(_))
        ));
    }

    fn capped(script: &str, cap: usize) -> DriverCommand {
        let mut command = sh(script);
        command.max_output_bytes = cap;
        command
    }

    /// Runs without a job deadline, under a test deadline that catches a hang.
    async fn run_unbounded(command: &DriverCommand) -> Result<DriverOutput, CasError> {
        tokio::time::timeout(
            Duration::from_secs(20),
            run_driver(command, &ExecutionContext::new(None)),
        )
        .await
        .expect("run_driver hung")
    }

    #[tokio::test]
    async fn a_driver_flooding_stdout_is_stopped_at_the_limit() {
        // Far more than the limit and than a pipe buffer; no job deadline.
        let result = run_unbounded(&capped("yes 0123456789abcdef", 64 * 1024)).await;
        assert!(
            matches!(result, Err(CasError::ResourceLimit(_))),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn a_driver_flooding_stderr_is_stopped_at_the_limit() {
        let result = run_unbounded(&capped("yes 0123456789abcdef >&2", 64 * 1024)).await;
        assert!(
            matches!(result, Err(CasError::ResourceLimit(_))),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn the_limit_covers_stdout_and_stderr_together() {
        // 48 KiB on each stream: within the limit alone, over it together.
        let script = "head -c 49152 /dev/zero; head -c 49152 /dev/zero >&2; \
                      echo 'ALGEBRAMESH_RESULT {}'";
        let result = run_unbounded(&capped(script, 64 * 1024)).await;
        assert!(
            matches!(result, Err(CasError::ResourceLimit(_))),
            "{result:?}"
        );
        // The same output under a larger limit is fine.
        let result = run_unbounded(&capped(script, 256 * 1024)).await;
        assert!(result.is_ok(), "{result:?}");
    }

    #[tokio::test]
    async fn no_helper_survives_an_output_limit() {
        let marker = std::env::temp_dir().join(format!("am-flood-{}", uuid::Uuid::new_v4()));
        let script = format!("(sleep 2; touch {}) & yes flood", marker.display());
        let result = run_unbounded(&capped(&script, 64 * 1024)).await;
        assert!(
            matches!(result, Err(CasError::ResourceLimit(_))),
            "{result:?}"
        );
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert!(
            !marker.exists(),
            "a helper process survived the output limit"
        );
    }

    #[tokio::test]
    async fn the_memory_limit_stops_a_driver_that_grows() {
        // awk doubles a string until allocation fails under a 64 MiB limit.
        let mut grows = sh("awk 'BEGIN { s = \"x\"; while (1) s = s s }'");
        grows.memory_limit_mb = Some(64);
        let result = run_unbounded(&grows).await;
        assert!(
            matches!(result, Err(CasError::ResourceLimit(_))),
            "{result:?}"
        );
        // A driver within the limit is not affected.
        let mut small = sh("echo 'ALGEBRAMESH_RESULT {\"ok\": true}'");
        small.memory_limit_mb = Some(64);
        assert!(run_unbounded(&small).await.is_ok());
    }

    #[tokio::test]
    async fn timeout_kills_the_process_group() {
        let marker = std::env::temp_dir().join(format!("am-driver-{}", uuid::Uuid::new_v4()));
        let script = format!("(sleep 2; touch {}) & sleep 30", marker.display());
        let started = std::time::Instant::now();
        let result = run_driver(
            &sh(&script),
            &ExecutionContext::new(Some(Duration::from_millis(300))),
        )
        .await;
        assert_eq!(result, Err(CasError::Timeout(0)));
        assert!(started.elapsed() < Duration::from_secs(5));
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert!(!marker.exists(), "a helper process survived the timeout");
    }
}

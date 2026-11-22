//! `algebramesh` command-line interface.

mod local;
mod remote;
mod services;
mod verify;
mod workflow;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "algebramesh",
    version,
    about = "Rust-based distributed computer algebra and research computing platform."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run one job in this process (no scheduler) and print its record as JSON.
    Run(local::RunArgs),
    /// Check a job specification without running it.
    Validate { job: PathBuf },
    /// List backends: those built into this binary, or with --server those offered
    /// by online workers.
    Backends {
        #[command(flatten)]
        remote: remote::RemoteArgs,
    },
    /// Scheduler service.
    #[command(subcommand)]
    Server(services::ServerCommand),
    /// Worker agent.
    #[command(subcommand)]
    Worker(services::WorkerCommand),
    /// Submit a job to the scheduler.
    Submit(remote::SubmitArgs),
    /// Show the state of a job.
    Status {
        job_id: String,
        #[command(flatten)]
        remote: remote::RemoteArgs,
    },
    /// Show the record of a finished job, or what its result claims.
    Result {
        job_id: String,
        /// Output format: json (the record) or claims (the summary and the
        /// claims of the result, ADR 0018).
        #[arg(long, default_value = "json")]
        format: String,
        #[command(flatten)]
        remote: remote::RemoteArgs,
    },
    /// Wait for a job to finish and print its record (follows retries of LOST jobs).
    Wait {
        job_id: String,
        /// Give up after this many seconds.
        #[arg(long)]
        timeout: Option<u64>,
        #[command(flatten)]
        remote: remote::RemoteArgs,
    },
    /// Cancel a job.
    Cancel {
        job_id: String,
        #[command(flatten)]
        remote: remote::RemoteArgs,
    },
    /// List registered workers.
    Workers {
        #[command(flatten)]
        remote: remote::RemoteArgs,
    },
    /// Check the record of a finished job again (ADR 0031): its hashes, its
    /// blobs, and its value with this binary's checkers. Prints the claims
    /// as recorded and as rechecked; never a value.
    Verify(verify::VerifyArgs),
    /// Store, read and look up blobs (ADR 0021): values kept by their SHA-256
    /// outside the Raft log.
    #[command(subcommand)]
    Blob(remote::BlobCommand),
    /// Run, resume, look at or cancel a workflow of jobs whose arguments
    /// take values from other jobs' results (ADR 0032).
    #[command(subcommand)]
    Workflow(workflow::WorkflowCommand),
    /// Create a private CA, a scheduler certificate and access tokens.
    #[command(subcommand)]
    Certs(services::CertsCommand),
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let result = match Cli::parse().command {
        Command::Run(args) => local::run(args).await,
        Command::Validate { job } => local::validate(&job),
        Command::Backends { remote } if remote.is_set() => remote::backends(remote).await,
        Command::Backends { .. } => local::backends(),
        Command::Server(c) => services::server(c).await,
        Command::Worker(c) => services::worker(c).await,
        Command::Submit(args) => remote::submit(args).await,
        Command::Status { job_id, remote } => remote::status(remote, &job_id).await,
        Command::Result {
            job_id,
            format,
            remote,
        } => match format.as_str() {
            "json" => remote::result(remote, &job_id, false).await,
            "claims" => remote::result(remote, &job_id, true).await,
            _ => Err(format!("unsupported format {format:?}; use json or claims")),
        },
        Command::Wait {
            job_id,
            timeout,
            remote,
        } => remote::wait(remote, &job_id, timeout).await,
        Command::Cancel { job_id, remote } => remote::cancel(remote, &job_id).await,
        Command::Workers { remote } => remote::workers(remote).await,
        Command::Verify(args) => verify::verify(args).await,
        Command::Blob(c) => remote::blob(c).await,
        Command::Workflow(c) => workflow::workflow(c).await,
        Command::Certs(c) => services::certs(c),
    };
    match result {
        Ok(code) => code,
        Err(message) => {
            eprintln!("algebramesh: {message}");
            ExitCode::from(2)
        }
    }
}

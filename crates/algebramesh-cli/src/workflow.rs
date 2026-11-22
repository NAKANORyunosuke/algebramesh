//! `algebramesh workflow run|resume|status|cancel <file>` (W2, ADR 0032): a
//! workflow of jobs driven from this process.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use algebramesh_client::workflow::{self, Options, Progress, Report, Workflow};
use clap::{Args, Subcommand};

use crate::local::print_json;
use crate::remote::RemoteArgs;

#[derive(Subcommand)]
pub enum WorkflowCommand {
    /// Run a workflow and print its report. A workflow that ran before is
    /// resumed: nodes whose jobs the cluster has are not submitted again.
    Run(RunArgs),
    /// Resume a workflow that stopped, after this process or the scheduler
    /// went away (the same as run).
    Resume(RunArgs),
    /// Print how a workflow stands, without submitting anything.
    Status(LookArgs),
    /// Cancel a workflow: no node is submitted any more, and its jobs that
    /// have not finished are cancelled.
    Cancel(LookArgs),
}

#[derive(Args)]
pub struct LookArgs {
    /// The definition of the workflow (JSON).
    file: PathBuf,
    /// The state file (default: <file>.state.json).
    #[arg(long)]
    state: Option<PathBuf>,
    #[command(flatten)]
    remote: RemoteArgs,
}

#[derive(Args)]
pub struct RunArgs {
    #[command(flatten)]
    look: LookArgs,
    /// Submit the next attempt of a node whose job failed, timed out, was
    /// cancelled or lost (once per run), instead of stopping.
    #[arg(long)]
    retry_failed: bool,
    /// A value taken from a producer's result goes by reference up to this
    /// size and in a blob above it.
    #[arg(long)]
    inline_max_bytes: Option<u64>,
    /// The longest pause, in seconds, between two looks at the jobs that run.
    #[arg(long, default_value_t = 2)]
    poll_secs: u64,
}

fn open(look: &LookArgs) -> Result<(Workflow, Options), String> {
    let text =
        std::fs::read_to_string(&look.file).map_err(|e| format!("{}: {e}", look.file.display()))?;
    let workflow =
        Workflow::from_json(&text).map_err(|e| format!("{}: {e}", look.file.display()))?;
    let state = look
        .state
        .clone()
        .unwrap_or_else(|| workflow::default_state_path(&look.file));
    Ok((workflow, Options::new(state)))
}

/// Succeeds unless the workflow stopped or was cancelled (`status` of one
/// that still runs succeeds).
fn exit_for(report: &Report) -> ExitCode {
    match report.progress {
        Progress::Succeeded | Progress::Running => ExitCode::SUCCESS,
        Progress::Stopped | Progress::Cancelled => ExitCode::FAILURE,
    }
}

pub async fn workflow(command: WorkflowCommand) -> Result<ExitCode, String> {
    match command {
        WorkflowCommand::Run(args) | WorkflowCommand::Resume(args) => {
            let (workflow, mut options) = open(&args.look)?;
            options.retry_failed = args.retry_failed;
            if let Some(max) = args.inline_max_bytes {
                options.inline_max_bytes = max;
            }
            options.poll = Duration::from_secs(args.poll_secs.max(1));
            let client = args.look.remote.connect().await?;
            let report = workflow::run(&client, &workflow, &options)
                .await
                .map_err(|e| e.to_string())?;
            print_json(&report, true)?;
            Ok(exit_for(&report))
        }
        WorkflowCommand::Status(look) => {
            let (workflow, options) = open(&look)?;
            let client = look.remote.connect().await?;
            let report = workflow::status(&client, &workflow, &options)
                .await
                .map_err(|e| e.to_string())?;
            print_json(&report, true)?;
            Ok(exit_for(&report))
        }
        WorkflowCommand::Cancel(look) => {
            let (workflow, options) = open(&look)?;
            let client = look.remote.connect().await?;
            let report = workflow::cancel(&client, &workflow, &options)
                .await
                .map_err(|e| e.to_string())?;
            print_json(&report, true)?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

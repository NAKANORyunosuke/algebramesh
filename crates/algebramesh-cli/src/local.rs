//! Commands that run without a scheduler.

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use algebramesh_backend_asir::{AsirBackend, AsirConfig, AsirEndpoint};
use algebramesh_backend_macaulay2::{Macaulay2Backend, Macaulay2Config};
use algebramesh_backend_oscar::{OscarBackend, OscarConfig};
use algebramesh_core::{CasBackend, ExecutionContext, JobRecord, JobSpec, JobState, run_job};
use algebramesh_openxm_client::{LocalLauncher, SessionOptions};
use clap::Args;

#[derive(Args)]
pub struct RunArgs {
    /// Job specification (JSON).
    job: PathBuf,

    /// Address on which to wait for `ox_launch` (ReverseConnect).
    #[arg(long, default_value = "127.0.0.1")]
    listen: IpAddr,
    /// Control port; 0 picks a free port.
    #[arg(long, default_value_t = 0)]
    control_port: u16,
    /// Data port; 0 picks a free port.
    #[arg(long, default_value_t = 0)]
    data_port: u16,
    /// Host name the worker uses to reach this process (only used in the printed hint).
    #[arg(long, default_value = "<this-host>")]
    advertise_host: String,
    /// Seconds to wait for the worker to connect and the server to start.
    #[arg(long, default_value_t = 120)]
    accept_timeout: u64,

    /// Start `ox_launch` locally instead of waiting for a worker (SpawnLocal).
    #[arg(long, requires_all = ["ox_launch", "ox_asir"])]
    spawn_local: bool,
    #[arg(long)]
    ox_launch: Option<PathBuf>,
    #[arg(long)]
    ox_asir: Option<PathBuf>,

    /// Macaulay2 executable, for jobs with backend "macaulay2".
    #[arg(long, default_value = "M2")]
    m2: PathBuf,
    /// Macaulay2 driver script (drivers/macaulay2/algebramesh.m2).
    #[arg(
        long,
        default_value = "/opt/algebramesh/drivers/macaulay2/algebramesh.m2"
    )]
    m2_driver: PathBuf,

    /// Julia executable, for jobs with backend "oscar".
    #[arg(long, default_value = "julia")]
    julia: PathBuf,
    /// OSCAR driver script (drivers/oscar/algebramesh.jl).
    #[arg(long, default_value = "/opt/algebramesh/drivers/oscar/algebramesh.jl")]
    oscar_driver: PathBuf,
    /// Julia project with OSCAR installed.
    #[arg(long)]
    oscar_project: Option<PathBuf>,

    /// Image digest or source commit of the Asir installation, kept as provenance.
    #[arg(long)]
    asir_build: Option<String>,
    /// Pretty-print the JSON record.
    #[arg(long)]
    pretty: bool,
}

pub fn read_spec(path: &Path) -> Result<JobSpec, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    JobSpec::from_json(&text).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn validate(job: &Path) -> Result<ExitCode, String> {
    let spec = read_spec(job)?;
    println!(
        "ok: schema_version {}, backend {}, operation {}",
        spec.schema_version, spec.backend, spec.operation
    );
    Ok(ExitCode::SUCCESS)
}

fn asir_backend(args: &RunArgs) -> AsirBackend {
    let endpoint = match (&args.ox_launch, &args.ox_asir) {
        (Some(ox_launch), Some(ox_asir)) if args.spawn_local => {
            AsirEndpoint::SpawnLocal(LocalLauncher {
                ox_launch: ox_launch.clone(),
                server: ox_asir.clone(),
            })
        }
        _ => {
            let host = args.advertise_host.clone();
            AsirEndpoint::ReverseConnect {
                bind: args.listen,
                control_port: args.control_port,
                data_port: args.data_port,
                on_listening: Some(Arc::new(move |control, data| {
                    eprintln!(
                        "algebramesh: waiting for ox_launch (control {control}, data {data})\n\
                         algebramesh: on the worker run: ox_launch {host} 0 {} {} /OpenXM/bin/ox_asir 0",
                        control.port(),
                        data.port()
                    );
                })),
            }
        }
    };
    AsirBackend::new(AsirConfig {
        endpoint,
        accept_timeout: Duration::from_secs(args.accept_timeout),
        build: args.asir_build.clone(),
        session: SessionOptions::default(),
    })
}

fn builtin_backends() -> Vec<Box<dyn CasBackend>> {
    vec![
        Box::new(AsirBackend::new(AsirConfig {
            endpoint: AsirEndpoint::ReverseConnect {
                bind: IpAddr::from([127, 0, 0, 1]),
                control_port: 0,
                data_port: 0,
                on_listening: None,
            },
            accept_timeout: Duration::from_secs(1),
            build: None,
            session: SessionOptions::default(),
        })),
        Box::new(Macaulay2Backend::new(Macaulay2Config::default())),
        Box::new(OscarBackend::new(OscarConfig::default())),
    ]
}

pub fn backends() -> Result<ExitCode, String> {
    let list: Vec<_> = builtin_backends()
        .iter()
        .map(|b| serde_json::json!({"name": b.name(), "capabilities": b.capabilities()}))
        .collect();
    print_json(&list, true)?;
    Ok(ExitCode::SUCCESS)
}

pub fn print_json(value: &impl serde::Serialize, pretty: bool) -> Result<(), String> {
    let json = if pretty {
        serde_json::to_string_pretty(value)
    } else {
        serde_json::to_string(value)
    }
    .map_err(|e| e.to_string())?;
    println!("{json}");
    Ok(())
}

pub fn exit_for(record: &JobRecord) -> ExitCode {
    if record.state == JobState::Succeeded {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

pub async fn run(args: RunArgs) -> Result<ExitCode, String> {
    let spec = read_spec(&args.job)?;
    let backend: Box<dyn CasBackend> = match spec.backend.as_str() {
        algebramesh_backend_asir::NAME => Box::new(asir_backend(&args)),
        algebramesh_backend_macaulay2::NAME => Box::new(Macaulay2Backend::new(Macaulay2Config {
            m2: args.m2.clone(),
            driver: args.m2_driver.clone(),
            ..Macaulay2Config::default()
        })),
        algebramesh_backend_oscar::NAME => Box::new(OscarBackend::new(OscarConfig {
            julia: args.julia.clone(),
            driver: args.oscar_driver.clone(),
            project: args.oscar_project.clone(),
            ..OscarConfig::default()
        })),
        other => return Err(format!("unknown backend {other:?}")),
    };

    let ctx = ExecutionContext::new(None);
    let cancel = ctx.cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            eprintln!("algebramesh: cancelling");
            cancel.cancel();
        }
    });

    let verifier: std::sync::Arc<dyn algebramesh_core::guarantee::ResultVerifier> =
        std::sync::Arc::new(algebramesh_agent::registry());
    let record = run_job(backend.as_ref(), spec, ctx, &verifier).await;
    print_json(&record, args.pretty)?;
    Ok(exit_for(&record))
}

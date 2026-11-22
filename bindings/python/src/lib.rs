//! `algebramesh._native`: the Rust client library exposed to Python.
//!
//! This layer only moves JSON text across the boundary; the Python package
//! `algebramesh` wraps it in a typed, documented API. Blocking calls release the
//! GIL while they wait on the network.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use algebramesh_client::workflow::{self, Options, Workflow, WorkflowError};
use algebramesh_client::{Client, ClientConfig, ClientError};
use pyo3::create_exception;
use pyo3::exceptions::{PyException, PyValueError};
use pyo3::prelude::*;
use tokio::runtime::Runtime;

create_exception!(_native, AlgebraMeshError, PyException);
create_exception!(_native, WaitTimeout, AlgebraMeshError);

fn to_py(e: ClientError) -> PyErr {
    match e {
        ClientError::WaitTimeout(..) => WaitTimeout::new_err(e.to_string()),
        ClientError::Config(m) => PyValueError::new_err(m),
        other => AlgebraMeshError::new_err(other.to_string()),
    }
}

fn workflow_to_py(e: WorkflowError) -> PyErr {
    match e {
        WorkflowError::Client(e) => to_py(e),
        WorkflowError::Definition(_) => PyValueError::new_err(e.to_string()),
        other => AlgebraMeshError::new_err(other.to_string()),
    }
}

fn json(value: impl serde::Serialize) -> PyResult<String> {
    serde_json::to_string(&value).map_err(|e| AlgebraMeshError::new_err(e.to_string()))
}

#[pyclass(name = "NativeClient", module = "algebramesh._native", frozen)]
struct NativeClient {
    runtime: Arc<Runtime>,
    client: Client,
}

impl NativeClient {
    fn run<T: Send>(
        &self,
        py: Python<'_>,
        f: impl std::future::Future<Output = Result<T, ClientError>> + Send,
    ) -> PyResult<T> {
        py.detach(|| self.runtime.block_on(f)).map_err(to_py)
    }
}

#[pymethods]
impl NativeClient {
    #[new]
    #[pyo3(signature = (url, ca, token, server_name=None))]
    fn new(
        py: Python<'_>,
        url: String,
        ca: PathBuf,
        token: String,
        server_name: Option<String>,
    ) -> PyResult<Self> {
        let ca_pem = std::fs::read(&ca)
            .map_err(|e| PyValueError::new_err(format!("{}: {e}", ca.display())))?;
        let runtime = Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .map_err(|e| AlgebraMeshError::new_err(e.to_string()))?,
        );
        let config = ClientConfig {
            url,
            ca_pem,
            domain: server_name,
            token,
        };
        let client = py
            .detach(|| runtime.block_on(Client::connect(config)))
            .map_err(to_py)?;
        Ok(Self { runtime, client })
    }

    /// Submits a job specification (JSON) and returns the job id. With a
    /// `request_id`, sending the same submission again returns the same job.
    #[pyo3(signature = (spec_json, priority, request_id=None))]
    fn submit(
        &self,
        py: Python<'_>,
        spec_json: String,
        priority: i32,
        request_id: Option<String>,
    ) -> PyResult<String> {
        let status = match request_id {
            Some(key) => self.run(
                py,
                self.client
                    .submit_json_with_request_id(&spec_json, priority, &key),
            )?,
            None => self.run(py, self.client.submit_json(&spec_json, priority))?,
        };
        Ok(status.job_id)
    }

    fn status(&self, py: Python<'_>, job_id: String) -> PyResult<String> {
        let status = self.run(py, self.client.status(&job_id))?;
        json(algebramesh_client::status_json(&status))
    }

    /// The job record as JSON, or None while the job is not finished.
    fn result(&self, py: Python<'_>, job_id: String) -> PyResult<Option<String>> {
        let (_, record) = self.run(py, self.client.result(&job_id))?;
        record.map(json).transpose()
    }

    #[pyo3(signature = (job_id, timeout=None, follow_retries=true))]
    fn wait(
        &self,
        py: Python<'_>,
        job_id: String,
        timeout: Option<f64>,
        follow_retries: bool,
    ) -> PyResult<String> {
        let timeout = timeout
            .map(|t| {
                Duration::try_from_secs_f64(t)
                    .map_err(|_| PyValueError::new_err("timeout must be a non-negative number"))
            })
            .transpose()?;
        let record = self.run(py, self.client.wait(&job_id, timeout, follow_retries))?;
        json(record)
    }

    /// Waits for the job and, if it succeeded, writes its result value as
    /// JSON to `path`. A value kept as one blob goes from the scheduler to
    /// the file without being held in memory. Returns the record, in which
    /// the values kept as blobs stay markers.
    #[pyo3(signature = (job_id, path, timeout=None, follow_retries=true))]
    fn download(
        &self,
        py: Python<'_>,
        job_id: String,
        path: PathBuf,
        timeout: Option<f64>,
        follow_retries: bool,
    ) -> PyResult<String> {
        let timeout = timeout
            .map(|t| {
                Duration::try_from_secs_f64(t)
                    .map_err(|_| PyValueError::new_err("timeout must be a non-negative number"))
            })
            .transpose()?;
        let client = &self.client;
        let record = self.run(py, async {
            let record = client.wait_refs(&job_id, timeout, follow_retries).await?;
            if record.state != algebramesh_core::JobState::Succeeded {
                return Ok(record);
            }
            let mut value = record.result.clone().unwrap_or_default();
            let whole = algebramesh_core::blob::BlobRef::from_marker(&value)
                .ok()
                .flatten();
            match whole {
                Some(r) => client.download_blob(&r, &path).await?,
                None => {
                    client.resolve(&mut value).await?;
                    let text = algebramesh_core::canonical_json(&value);
                    std::fs::write(&path, text)
                        .map_err(|e| ClientError::Data(format!("{}: {e}", path.display())))?;
                }
            }
            Ok(record)
        })?;
        json(record)
    }

    fn cancel(&self, py: Python<'_>, job_id: String) -> PyResult<String> {
        let status = self.run(py, self.client.cancel(&job_id))?;
        json(algebramesh_client::status_json(&status))
    }

    /// Runs (`command` "run"), looks at ("status") or cancels ("cancel") the
    /// workflow defined in `definition_json` (W2, ADR 0032) and returns its
    /// report as JSON.
    #[pyo3(signature = (command, definition_json, state, retry_failed=false, inline_max_bytes=None, poll=2.0))]
    #[allow(clippy::too_many_arguments)]
    fn workflow(
        &self,
        py: Python<'_>,
        command: &str,
        definition_json: &str,
        state: PathBuf,
        retry_failed: bool,
        inline_max_bytes: Option<u64>,
        poll: f64,
    ) -> PyResult<String> {
        let definition = Workflow::from_json(definition_json).map_err(workflow_to_py)?;
        let mut options = Options::new(state);
        options.retry_failed = retry_failed;
        if let Some(max) = inline_max_bytes {
            options.inline_max_bytes = max;
        }
        options.poll = Duration::try_from_secs_f64(poll)
            .ok()
            .filter(|p| !p.is_zero())
            .ok_or_else(|| PyValueError::new_err("poll must be a positive number"))?;
        let client = &self.client;
        let report = py
            .detach(|| {
                self.runtime.block_on(async {
                    match command {
                        "run" => workflow::run(client, &definition, &options).await,
                        "status" => workflow::status(client, &definition, &options).await,
                        "cancel" => workflow::cancel(client, &definition, &options).await,
                        _ => Err(WorkflowError::Definition(format!(
                            "unknown command {command:?}"
                        ))),
                    }
                })
            })
            .map_err(workflow_to_py)?;
        json(report)
    }

    fn backends(&self, py: Python<'_>) -> PyResult<String> {
        let list = self.run(py, self.client.backends())?;
        json(algebramesh_client::backends_json(&list))
    }

    fn workers(&self, py: Python<'_>) -> PyResult<String> {
        let list = self.run(py, self.client.workers())?;
        json(algebramesh_client::workers_json(&list))
    }
}

#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<NativeClient>()?;
    m.add("AlgebraMeshError", m.py().get_type::<AlgebraMeshError>())?;
    m.add("WaitTimeout", m.py().get_type::<WaitTimeout>())?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}

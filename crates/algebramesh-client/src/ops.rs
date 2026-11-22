//! The operations of `algebramesh-ops`, submitted with their typed arguments
//! and read back as their typed results (Issue #54).
//!
//! ```no_run
//! # async fn f(client: algebramesh_client::Client, arguments: algebramesh_client::ops::FactorArguments)
//! #     -> Result<(), algebramesh_client::ClientError> {
//! use algebramesh_client::ops::{self, PolynomialFactor};
//!
//! let job = client.submit_operation::<PolynomialFactor>("asir", &arguments, 0).await?;
//! let record = client.wait(&job.job_id, None, true).await?;
//! let factorization = ops::result_of::<PolynomialFactor>(&record)?;
//! # Ok(()) }
//! ```

pub use algebramesh_ops::*;

use algebramesh_core::{JobRecord, JobSpec, JobState};

use crate::{Client, ClientError, JobStatus};

/// The specification of a job of the operation `O` on `backend`, without
/// limits; set them on it before submitting it with [`Client::submit`].
pub fn spec<O: Operation>(backend: &str, arguments: &O::Arguments) -> Result<JobSpec, ClientError> {
    Ok(JobSpec {
        schema_version: JobSpec::SCHEMA_VERSION,
        backend: backend.to_owned(),
        operation: O::DESCRIPTOR.name.to_owned(),
        arguments: serde_json::to_value(arguments).map_err(|e| ClientError::Data(e.to_string()))?,
        limits: Default::default(),
        retry_policy: Default::default(),
        environment: Default::default(),
    })
}

/// The typed result of a job of the operation `O`, once it has succeeded.
pub fn result_of<O: Operation>(record: &JobRecord) -> Result<O::Result, ClientError> {
    let name = O::DESCRIPTOR.name;
    if record.spec.operation != name {
        return Err(ClientError::Data(format!(
            "job {} ran {}, not {name}",
            record.job_id, record.spec.operation
        )));
    }
    if record.state != JobState::Succeeded {
        return Err(ClientError::Data(format!(
            "job {} is {:?}, not SUCCEEDED",
            record.job_id, record.state
        )));
    }
    let value = record
        .result
        .clone()
        .ok_or_else(|| ClientError::Data(format!("job {} has no result", record.job_id)))?;
    serde_json::from_value(value).map_err(|e| {
        ClientError::Data(format!(
            "the result of job {} is not a {name} result: {e}",
            record.job_id
        ))
    })
}

impl Client {
    /// Submits the operation `O` on `backend` with `arguments`.
    pub async fn submit_operation<O: Operation>(
        &self,
        backend: &str,
        arguments: &O::Arguments,
        priority: i32,
    ) -> Result<JobStatus, ClientError> {
        self.submit(&spec::<O>(backend, arguments)?, priority).await
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use algebramesh_core::JobId;

    use super::*;

    fn arguments() -> FactorArguments {
        serde_json::from_value(serde_json::json!({
            "ring": {"domain": "QQ", "variables": ["x"], "order": "lex"},
            "expression": {"type": "polynomial", "terms": [
                {"coefficient": "1", "exponents": [2]}, {"coefficient": "-1", "exponents": [0]}]},
        }))
        .unwrap()
    }

    #[test]
    fn a_typed_job_reads_its_typed_result() {
        let spec = spec::<PolynomialFactor>("asir", &arguments()).unwrap();
        assert_eq!(spec.operation, "polynomial.factor");
        assert_eq!(spec.arguments, serde_json::to_value(arguments()).unwrap(),);
        let mut record = JobRecord::new(JobId::new(), spec, chrono::Utc::now());
        assert!(result_of::<PolynomialFactor>(&record).is_err());

        record.state = JobState::Succeeded;
        record.result = Some(serde_json::json!({
            "type": "factorization",
            "ring": {"domain": "QQ", "variables": ["x"], "order": "lex"},
            "constant": "1",
            "factors": [
                {"factor": {"type": "polynomial", "terms": [
                    {"coefficient": "1", "exponents": [1]}, {"coefficient": "-1", "exponents": [0]}]},
                 "multiplicity": 1, "display": "x-1"},
                {"factor": {"type": "polynomial", "terms": [
                    {"coefficient": "1", "exponents": [1]}, {"coefficient": "1", "exponents": [0]}]},
                 "multiplicity": 1, "display": "x+1"},
            ],
            "verified_product": true,
        }));
        let result = result_of::<PolynomialFactor>(&record).unwrap();
        let displays: Vec<&str> = result.factors.iter().map(|f| f.display.as_str()).collect();
        assert_eq!(displays, ["x-1", "x+1"]);
        // Read as another operation, it is refused.
        assert!(result_of::<GkzGenerate>(&record).is_err());
    }
}

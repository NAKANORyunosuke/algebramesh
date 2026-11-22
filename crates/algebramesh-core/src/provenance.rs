use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::backend::{BackendInfo, CasError, CasResult};
use crate::job::{JobId, JobSpec};
use crate::state::{InvalidTransition, JobState};

/// JSON with object keys sorted and no insignificant whitespace, used as the
/// normalized form of job input.
pub fn canonical_json(value: &serde_json::Value) -> String {
    // serde_json's default map is ordered by key, so re-serializing a `Value`
    // produces sorted keys at every level.
    let normalized: serde_json::Value =
        serde_json::from_str(&value.to_string()).unwrap_or_else(|_| value.clone());
    normalized.to_string()
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateChange {
    pub state: JobState,
    pub at: DateTime<Utc>,
}

/// Everything needed to reproduce and audit one job (design §7.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobRecord {
    pub job_id: JobId,
    pub state: JobState,
    pub spec: JobSpec,
    /// `canonical_json` of the spec.
    pub input_normalized: String,
    pub input_sha256: String,
    pub backend: BackendInfo,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<DateTime<Utc>>,
    pub history: Vec<StateChange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<CasError>,
    #[serde(default)]
    pub log: Vec<String>,
    /// 1 for the first execution; a retry is a new job with `retry_of` set.
    #[serde(default = "first_attempt")]
    pub attempt: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_of: Option<JobId>,
    /// What the result claims (ADR 0018); recorded once the cluster has
    /// enabled the feature `result_guarantee` (ADR 0020).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guarantee: Option<crate::guarantee::Guarantee>,
}

fn first_attempt() -> u32 {
    1
}

impl JobRecord {
    /// A new record in state `QUEUED`, with the normalized input and its hash.
    pub fn new(job_id: JobId, spec: JobSpec, at: DateTime<Utc>) -> Self {
        let spec_json = serde_json::to_value(&spec).unwrap_or(serde_json::Value::Null);
        let input_normalized = canonical_json(&spec_json);
        Self {
            job_id,
            state: JobState::Queued,
            input_sha256: sha256_hex(input_normalized.as_bytes()),
            input_normalized,
            backend: BackendInfo {
                name: spec.backend.clone(),
                ..BackendInfo::default()
            },
            spec,
            execution_host: None,
            started_at: None,
            finished_at: None,
            history: vec![StateChange {
                state: JobState::Queued,
                at,
            }],
            result: None,
            error: None,
            log: Vec::new(),
            attempt: 1,
            agent_id: None,
            retry_of: None,
            guarantee: None,
        }
    }

    /// Moves to `to` if the state machine allows it and appends to the history.
    pub fn transition(&mut self, to: JobState, at: DateTime<Utc>) -> Result<(), InvalidTransition> {
        self.state = self.state.transition(to)?;
        self.history.push(StateChange { state: to, at });
        if to == JobState::Running {
            self.started_at = Some(at);
        }
        if to.is_terminal() {
            self.finished_at = Some(at);
        }
        Ok(())
    }

    /// The terminal state that an execution outcome leads to.
    pub fn outcome_state(outcome: &Result<CasResult, CasError>) -> JobState {
        match outcome {
            Ok(_) => JobState::Succeeded,
            Err(CasError::Timeout(_)) => JobState::TimedOut,
            Err(CasError::Cancelled) => JobState::Cancelled,
            Err(CasError::Lost(_)) => JobState::Lost,
            Err(_) => JobState::Failed,
        }
    }

    /// Records the outcome of a running (or cancelling) job and moves it to the
    /// matching terminal state. A cancellation passes through `CANCELLING`.
    pub fn complete(
        &mut self,
        outcome: Result<CasResult, CasError>,
        at: DateTime<Utc>,
    ) -> Result<(), InvalidTransition> {
        let target = Self::outcome_state(&outcome);
        if target == JobState::Cancelled && self.state == JobState::Running {
            self.transition(JobState::Cancelling, at)?;
        }
        if self.state == JobState::Cancelling
            && target != JobState::Cancelled
            && target != JobState::Lost
        {
            // The job finished before the cancellation reached it.
            self.log.push(format!(
                "finished as {target:?} while cancellation was pending"
            ));
            self.transition(JobState::Cancelled, at)?;
        } else {
            self.transition(target, at)?;
        }
        match outcome {
            Ok(result) => {
                // The build recorded at placement stays unless the backend knows better.
                let placed_build = self.backend.build.take();
                self.backend = result.backend;
                if self.backend.build.is_none() {
                    self.backend.build = placed_build;
                }
                self.execution_host = result.execution_host;
                self.log.extend(result.log);
                self.result = Some(result.value);
                self.guarantee = result.guarantee;
            }
            Err(error) => self.error = Some(error),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_json_sorts_keys() {
        let a: serde_json::Value =
            serde_json::from_str(r#"{"b": 1, "a": {"y": [1, 2], "x": null}}"#).unwrap();
        let b: serde_json::Value =
            serde_json::from_str(r#"{"a":{"x":null,"y":[1,2]},"b":1}"#).unwrap();
        assert_eq!(canonical_json(&a), r#"{"a":{"x":null,"y":[1,2]},"b":1}"#);
        assert_eq!(
            sha256_hex(canonical_json(&a).as_bytes()),
            sha256_hex(canonical_json(&b).as_bytes())
        );
    }

    fn spec() -> JobSpec {
        JobSpec {
            schema_version: 1,
            backend: "asir".into(),
            operation: "polynomial.factor".into(),
            arguments: serde_json::json!({}),
            limits: Default::default(),
            retry_policy: Default::default(),
            environment: Default::default(),
        }
    }

    #[test]
    fn complete_follows_the_state_machine() {
        let now = Utc::now();
        let mut r = JobRecord::new(JobId::new(), spec(), now);
        assert_eq!(r.state, JobState::Queued);
        assert!(
            r.complete(Err(CasError::Cas("x".into())), now).is_err(),
            "QUEUED cannot fail"
        );
        assert_eq!(r.state, JobState::Queued);
        r.transition(JobState::Assigned, now).unwrap();
        r.transition(JobState::Running, now).unwrap();
        assert!(r.started_at.is_some());
        r.complete(Err(CasError::Cancelled), now).unwrap();
        assert_eq!(r.state, JobState::Cancelled);
        let states: Vec<_> = r.history.iter().map(|c| c.state).collect();
        assert_eq!(
            states,
            [
                JobState::Queued,
                JobState::Assigned,
                JobState::Running,
                JobState::Cancelling,
                JobState::Cancelled
            ]
        );
        assert!(r.finished_at.is_some());
    }

    #[test]
    fn success_after_cancel_request_ends_cancelled_with_note() {
        let now = Utc::now();
        let mut r = JobRecord::new(JobId::new(), spec(), now);
        for s in [JobState::Assigned, JobState::Running, JobState::Cancelling] {
            r.transition(s, now).unwrap();
        }
        let ok = CasResult {
            value: serde_json::json!(1),
            backend: BackendInfo::default(),
            execution_host: None,
            log: vec![],
            guarantee: None,
        };
        r.complete(Ok(ok), now).unwrap();
        assert_eq!(r.state, JobState::Cancelled);
        assert!(r.log[0].contains("while cancellation was pending"));
    }

    #[test]
    fn sha256_known_value() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}

//! Interfaces shared by the scheduler, agents, backends and clients.
//!
//! * [`JobSpec`]: the logical job model of design §7.1.
//! * [`JobState`]: the state machine of design §7.2.
//! * [`CasBackend`]: the backend interface of design §8.
//! * [`JobRecord`]: the provenance record of design §7.3.
//! * [`run_job`]: runs one job on one backend in-process, recording every state change.

mod backend;
pub mod blob;
pub mod check;
pub mod driver;
pub mod guarantee;
mod job;
mod provenance;
mod runner;
mod state;

pub use backend::{
    BackendInfo, Capabilities, CasBackend, CasError, CasRequest, CasResult, ExecutionContext,
    LIMIT_MEMORY_MB, LIMIT_WALL_SECONDS, MAX_RESULT_BYTES, MAX_SPEC_BYTES, Signature,
    driver_sha256, features,
};
pub use job::{EnvironmentConstraints, JobId, JobSpec, ResourceLimits, RetryPolicy, SpecError};
pub use provenance::{JobRecord, StateChange, canonical_json, sha256_hex};
pub use runner::{execute, run_job};
pub use state::{InvalidTransition, JobState};

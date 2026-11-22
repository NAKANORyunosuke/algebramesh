//! Model-based tests of the job state machine (Issue #38).
//!
//! A small model of the scheduler (submissions with and without keys,
//! cancellations, assignments, start and result reports, heartbeats and
//! restarts) runs next to the real scheduler on sequences of operations that
//! proptest generates. After every operation the answers and the state of
//! every job must agree. On every sequence:
//!
//! * a job whose cancellation was requested ends `CANCELLED` or `LOST`, and
//!   is never run again (it has no retry);
//! * no job has more than `max_attempts` attempts;
//! * after the agents finish what is left, every job is in a terminal state.
//!
//! A restart (the store is copied into a new scheduler) stands for every loss
//! of the agents' work: the scheduler declares the active jobs `LOST`, as it
//! does for a silent agent, without the test depending on time.

#![allow(clippy::unwrap_used)]

use std::collections::HashMap;
use std::time::Duration;

use algebramesh_core::{CasError, JobState};
use algebramesh_rpc::pb;
use algebramesh_server::scheduler::{SchedulerError, state_name};
use algebramesh_server::{Scheduler, SchedulerOptions, Store};
use proptest::prelude::*;

const MAX_ATTEMPTS: u32 = 3;
const AGENTS: usize = 2;

#[derive(Debug, Clone, Copy)]
enum Outcome {
    Ok,
    Failed,
    Cancelled,
}

impl Outcome {
    fn json(self) -> String {
        match self {
            Outcome::Ok => serde_json::json!({"Ok": {
                "value": 1, "backend": {"name": "test", "driver_version": "0"}, "log": [],
            }}),
            Outcome::Failed => serde_json::json!({"Err": CasError::Cas("an error".into())}),
            Outcome::Cancelled => serde_json::json!({"Err": CasError::Cancelled}),
        }
        .to_string()
    }

    fn state(self) -> JobState {
        match self {
            Outcome::Ok => JobState::Succeeded,
            Outcome::Failed => JobState::Failed,
            Outcome::Cancelled => JobState::Cancelled,
        }
    }
}

/// Job references are indices into the jobs created so far, modulo their
/// number.
#[derive(Debug, Clone)]
enum Op {
    Submit {
        key: Option<u8>,
        priority: i32,
        retry: bool,
    },
    Cancel(usize),
    Poll(usize),
    Started {
        agent: usize,
        job: usize,
    },
    Finished {
        agent: usize,
        job: usize,
        outcome: Outcome,
    },
    Heartbeat {
        agent: usize,
        jobs: Vec<usize>,
    },
    Restart,
}

fn op() -> impl Strategy<Value = Op> {
    let outcome = prop_oneof![
        Just(Outcome::Ok),
        Just(Outcome::Failed),
        Just(Outcome::Cancelled)
    ];
    prop_oneof![
        4 => (proptest::option::of(0u8..3), 0i32..2, any::<bool>())
            .prop_map(|(key, priority, retry)| Op::Submit { key, priority, retry }),
        3 => any::<usize>().prop_map(Op::Cancel),
        4 => (0..AGENTS).prop_map(Op::Poll),
        3 => (0..AGENTS, any::<usize>()).prop_map(|(agent, job)| Op::Started { agent, job }),
        4 => (0..AGENTS, any::<usize>(), outcome)
            .prop_map(|(agent, job, outcome)| Op::Finished { agent, job, outcome }),
        2 => (0..AGENTS, proptest::collection::vec(any::<usize>(), 0..4))
            .prop_map(|(agent, jobs)| Op::Heartbeat { agent, jobs }),
        1 => Just(Op::Restart),
    ]
}

/// A job with a key always has the same specification, so that the same key
/// differs only in its priority.
fn retries(key: Option<u8>, retry: bool) -> bool {
    key.map_or(retry, |k| k % 2 == 0)
}

fn spec(retry: bool) -> String {
    serde_json::json!({
        "schema_version": 1, "backend": "test", "operation": "test.op", "arguments": {},
        "retry_policy": if retry { "if_deterministic" } else { "never" },
    })
    .to_string()
}

#[derive(Debug, Clone)]
struct Job {
    /// The real job's id.
    id: String,
    state: JobState,
    cancel_requested: bool,
    attempt: u32,
    agent: Option<String>,
    retried_as: Option<usize>,
    priority: i32,
    retry: bool,
}

/// The model: the jobs in the order they were created (the queue order among
/// equal priorities), the keys and the current agent ids.
struct Model {
    jobs: Vec<Job>,
    keys: HashMap<u8, (usize, i32)>,
    agents: Vec<String>,
}

const ACTIVE: [JobState; 3] = [JobState::Assigned, JobState::Running, JobState::Cancelling];

impl Model {
    fn job(&self, i: usize) -> Option<usize> {
        (!self.jobs.is_empty()).then(|| i % self.jobs.len())
    }

    /// A start or result report from `agent` for the job is accepted: the job
    /// is assigned to that agent and not finished.
    fn accepts(&self, j: usize, agent: &str) -> bool {
        let job = &self.jobs[j];
        job.agent.as_deref() == Some(agent) && !job.state.is_terminal()
    }

    /// `started` and the first half of `finished`: a job not yet reported as
    /// started passes through RUNNING, and through CANCELLING if its
    /// cancellation was requested.
    fn start(&mut self, j: usize) {
        let job = &mut self.jobs[j];
        if job.state == JobState::Assigned {
            job.state = if job.cancel_requested {
                JobState::Cancelling
            } else {
                JobState::Running
            };
        }
    }

    fn finish(&mut self, j: usize, outcome: Outcome) {
        self.start(j);
        let job = &mut self.jobs[j];
        job.state = if job.state == JobState::Cancelling {
            JobState::Cancelled
        } else {
            outcome.state()
        };
    }

    /// A restart: every active job is LOST, in queue order, and retried if
    /// its policy allows it, it has attempts left and its cancellation was not
    /// requested. Returns the indices of the retried jobs.
    fn restart(&mut self) -> Vec<usize> {
        let mut active: Vec<usize> = (0..self.jobs.len())
            .filter(|&j| ACTIVE.contains(&self.jobs[j].state))
            .collect();
        active.sort_by_key(|&j| (-self.jobs[j].priority, j));
        let mut retried = Vec::new();
        for j in active {
            let n = self.jobs.len();
            let job = &mut self.jobs[j];
            let cancelled = job.cancel_requested || job.state == JobState::Cancelling;
            job.state = JobState::Lost;
            if !cancelled && job.retry && job.attempt < MAX_ATTEMPTS {
                let next = Job {
                    id: String::new(),
                    state: JobState::Queued,
                    cancel_requested: false,
                    attempt: job.attempt + 1,
                    agent: None,
                    retried_as: None,
                    priority: job.priority,
                    retry: job.retry,
                };
                job.retried_as = Some(n);
                self.jobs.push(next);
                retried.push(j);
            }
        }
        retried
    }

    /// The job an idle agent gets: the highest priority, then the oldest.
    fn next_queued(&self) -> Option<usize> {
        (0..self.jobs.len())
            .filter(|&j| self.jobs[j].state == JobState::Queued)
            .min_by_key(|&j| (-self.jobs[j].priority, j))
    }
}

fn options() -> SchedulerOptions {
    SchedulerOptions {
        max_attempts: MAX_ATTEMPTS,
        // Long enough that no job is declared LOST by its heartbeats here.
        heartbeat_timeout: Duration::from_secs(600),
        ..SchedulerOptions::default()
    }
}

fn register(s: &Scheduler) -> String {
    s.register(
        pb::RegisterRequest {
            name: "worker".into(),
            host: "h".into(),
            agent_version: "0".into(),
            backends: vec![pb::BackendOffer {
                name: "test".into(),
                capabilities_json: r#"{"operations":["test.op"],"supported_types":[]}"#.into(),
                backend_version: String::new(),
                build: String::new(),
                slots: 1,
                ..Default::default()
            }],
            ..Default::default()
        },
        None,
    )
    .unwrap()
    .agent_id
}

struct Harness {
    real: Scheduler,
    model: Model,
    runtime: tokio::runtime::Runtime,
}

impl Harness {
    fn new() -> Self {
        let real = Scheduler::new(Store::open(None, None).unwrap(), options()).unwrap();
        let agents = (0..AGENTS).map(|_| register(&real)).collect();
        Self {
            real,
            model: Model {
                jobs: Vec::new(),
                keys: HashMap::new(),
                agents,
            },
            runtime: tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .unwrap(),
        }
    }

    fn poll(
        runtime: &tokio::runtime::Runtime,
        real: &Scheduler,
        agent: &str,
    ) -> Option<pb::Assignment> {
        runtime
            .block_on(real.poll(agent, &["test".into()], Duration::ZERO))
            .unwrap()
    }

    fn apply(&mut self, op: &Op) -> Result<(), TestCaseError> {
        let m = &mut self.model;
        match op {
            &Op::Submit {
                key,
                priority,
                retry,
            } => {
                let retry = retries(key, retry);
                let request_id = key.map(|k| format!("key-{k}")).unwrap_or_default();
                let got = self.real.submit(&spec(retry), priority, &request_id);
                match key.and_then(|k| m.keys.get(&k).copied()) {
                    Some((j, p)) if p == priority => {
                        prop_assert_eq!(got.unwrap().job_id, m.jobs[j].id.clone());
                    }
                    Some(_) => {
                        prop_assert!(matches!(got, Err(SchedulerError::Conflict(_))), "{:?}", got);
                    }
                    None => {
                        let status = got.unwrap();
                        prop_assert_eq!(&status.state, "QUEUED");
                        if let Some(k) = key {
                            m.keys.insert(k, (m.jobs.len(), priority));
                        }
                        m.jobs.push(Job {
                            id: status.job_id,
                            state: JobState::Queued,
                            cancel_requested: false,
                            attempt: 1,
                            agent: None,
                            retried_as: None,
                            priority,
                            retry,
                        });
                    }
                }
            }
            &Op::Cancel(i) => {
                let Some(j) = m.job(i) else { return Ok(()) };
                let got = self.real.cancel(&m.jobs[j].id).unwrap();
                let job = &mut m.jobs[j];
                match job.state {
                    JobState::Queued => job.state = JobState::Cancelled,
                    JobState::Running => {
                        job.cancel_requested = true;
                        job.state = JobState::Cancelling;
                    }
                    JobState::Assigned | JobState::Cancelling => job.cancel_requested = true,
                    _ => {}
                }
                prop_assert_eq!(got.state, state_name(job.state));
                prop_assert_eq!(got.cancel_requested, job.cancel_requested);
            }
            &Op::Poll(a) => {
                let agent = m.agents[a].clone();
                let got = Self::poll(&self.runtime, &self.real, &agent);
                match m.next_queued() {
                    Some(j) => {
                        let got = got.expect("a queued job was not assigned");
                        prop_assert_eq!(&got.job_id, &m.jobs[j].id);
                        prop_assert_eq!(got.attempt, m.jobs[j].attempt);
                        m.jobs[j].state = JobState::Assigned;
                        m.jobs[j].agent = Some(agent);
                    }
                    None => prop_assert!(got.is_none(), "{:?}", got),
                }
            }
            &Op::Started { agent, job } => {
                let Some(j) = m.job(job) else { return Ok(()) };
                let agent = m.agents[agent].clone();
                let ack = self
                    .real
                    .started(&pb::JobEvent {
                        agent_id: agent.clone(),
                        job_id: m.jobs[j].id.clone(),
                        attempt: m.jobs[j].attempt,
                    })
                    .unwrap();
                let accepted = m.accepts(j, &agent);
                prop_assert_eq!(ack.accepted, accepted, "{:?}", ack);
                if accepted {
                    m.start(j);
                }
            }
            &Op::Finished {
                agent,
                job,
                outcome,
            } => {
                let Some(j) = m.job(job) else { return Ok(()) };
                let agent = m.agents[agent].clone();
                let ack = self
                    .real
                    .finished(&pb::JobFinished {
                        agent_id: agent.clone(),
                        job_id: m.jobs[j].id.clone(),
                        attempt: m.jobs[j].attempt,
                        outcome_json: outcome.json(),
                    })
                    .unwrap();
                let accepted = m.accepts(j, &agent);
                prop_assert_eq!(ack.accepted, accepted, "{:?}", ack);
                if accepted {
                    m.finish(j, outcome);
                }
            }
            Op::Heartbeat { agent, jobs } => {
                let agent = m.agents[*agent].clone();
                let mut listed: Vec<usize> = Vec::new();
                for &i in jobs {
                    if let Some(j) = m.job(i)
                        && !listed.contains(&j)
                    {
                        listed.push(j);
                    }
                }
                let got = self
                    .real
                    .heartbeat(&pb::HeartbeatRequest {
                        agent_id: agent.clone(),
                        running_job_ids: listed.iter().map(|&j| m.jobs[j].id.clone()).collect(),
                    })
                    .unwrap();
                prop_assert!(got.known);
                // A listed job is to be stopped if its cancellation was
                // requested, or if it is not this agent's active job.
                let expected: Vec<String> = listed
                    .iter()
                    .filter(|&&j| !m.accepts(j, &agent) || m.jobs[j].cancel_requested)
                    .map(|&j| m.jobs[j].id.clone())
                    .collect();
                prop_assert_eq!(got.cancel_job_ids, expected);
            }
            Op::Restart => {
                let bytes = self.real.with_store(|s| s.snapshot()).unwrap();
                let mut store = Store::open(None, None).unwrap();
                store.restore(&bytes).unwrap();
                self.real = Scheduler::new(store, options()).unwrap();
                for j in m.restart() {
                    let status = self.real.status(&m.jobs[j].id).unwrap();
                    prop_assert!(!status.retried_as.is_empty(), "{:?}", status);
                    let next = m.jobs[j].retried_as.unwrap();
                    m.jobs[next].id = status.retried_as;
                }
                // The new scheduler knows no agent; they register again.
                m.agents = (0..AGENTS).map(|_| register(&self.real)).collect();
            }
        }
        self.check()
    }

    /// The real jobs agree with the model, and the invariants hold.
    fn check(&self) -> Result<(), TestCaseError> {
        let m = &self.model;
        let queued = self
            .real
            .with_store(|s| s.count_jobs(JobState::Queued))
            .unwrap();
        let expected = m
            .jobs
            .iter()
            .filter(|j| j.state == JobState::Queued)
            .count() as u64;
        prop_assert_eq!(queued, expected, "the real scheduler has other jobs");
        for job in &m.jobs {
            let status = self.real.status(&job.id).unwrap();
            prop_assert_eq!(&status.state, &state_name(job.state), "job {:?}", job);
            prop_assert_eq!(status.cancel_requested, job.cancel_requested);
            prop_assert_eq!(status.attempt, job.attempt);
            let retried_as = job
                .retried_as
                .map(|j| m.jobs[j].id.clone())
                .unwrap_or_default();
            prop_assert_eq!(&status.retried_as, &retried_as);
            // The invariants, on the real scheduler.
            prop_assert!(status.attempt >= 1 && status.attempt <= MAX_ATTEMPTS);
            if status.cancel_requested || status.state == "CANCELLING" {
                prop_assert!(status.retried_as.is_empty(), "{:?}", status);
                prop_assert!(
                    !matches!(status.state.as_str(), "SUCCEEDED" | "FAILED" | "QUEUED"),
                    "{:?}",
                    status
                );
            }
        }
        Ok(())
    }

    /// The agents' runs are lost once more, then one agent runs every queued
    /// job to its end: every job ends in a terminal state.
    fn drain(&mut self) -> Result<(), TestCaseError> {
        self.apply(&Op::Restart)?;
        for _ in 0..=self.model.jobs.len() * MAX_ATTEMPTS as usize {
            let Some(j) = self.model.next_queued() else {
                break;
            };
            self.apply(&Op::Poll(0))?;
            self.apply(&Op::Finished {
                agent: 0,
                job: j,
                outcome: Outcome::Ok,
            })?;
        }
        for job in &self.model.jobs {
            prop_assert!(job.state.is_terminal(), "{:?}", job);
        }
        Ok(())
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 300, ..ProptestConfig::default() })]

    #[test]
    fn the_scheduler_follows_the_model(ops in proptest::collection::vec(op(), 1..40)) {
        let mut h = Harness::new();
        for op in &ops {
            h.apply(op)?;
        }
        h.drain()?;
    }
}

/// The orders of a cancellation against the start, the result and the loss
/// of a run, one by one, besides the generated sequences above.
#[test]
fn every_order_of_a_cancellation_and_a_run() {
    use Op::*;
    let run = [
        Poll(0),
        Started { agent: 0, job: 0 },
        Finished {
            agent: 0,
            job: 0,
            outcome: Outcome::Ok,
        },
    ];
    let submit = Submit {
        key: None,
        priority: 0,
        retry: true,
    };
    // The cancellation at each place in the run, with and without a loss
    // after it, and with the start report lost.
    for at in 0..=run.len() {
        for lose in [false, true] {
            for skip_start in [false, true] {
                let mut ops = vec![submit.clone()];
                let mut steps: Vec<Op> = run
                    .iter()
                    .filter(|o| !(skip_start && matches!(o, Started { .. })))
                    .cloned()
                    .collect();
                let at = at.min(steps.len());
                steps.insert(at, Cancel(0));
                if lose {
                    steps.insert(at + 1, Restart);
                }
                ops.extend(steps);
                let mut h = Harness::new();
                for op in &ops {
                    h.apply(op).unwrap_or_else(|e| panic!("{ops:?}: {e}"));
                }
                h.drain().unwrap_or_else(|e| panic!("{ops:?}: {e}"));
                let first = &h.model.jobs[0];
                // A cancellation before the result keeps the job from
                // succeeding and from being run again.
                let cancelled_first = ops.iter().position(|o| matches!(o, Cancel(_)))
                    < ops.iter().position(|o| matches!(o, Finished { .. }));
                if cancelled_first {
                    assert!(
                        matches!(first.state, JobState::Cancelled | JobState::Lost),
                        "{ops:?}: {first:?}"
                    );
                    assert_eq!(h.model.jobs.len(), 1, "{ops:?}");
                }
            }
        }
    }
}

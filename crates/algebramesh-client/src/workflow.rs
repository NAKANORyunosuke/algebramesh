//! Workflows: jobs joined into a DAG on the client side, run and resumed by
//! an orchestrator that keeps no state the cluster does not (W2, ADR 0032).
//!
//! A definition names its nodes, each a job specification whose top-level
//! arguments may take the output of another node:
//!
//! ```json
//! {"workflow": "membership",
//!  "nodes": {
//!    "gb": {"spec": {"schema_version": 1, "backend": "macaulay2",
//!                    "operation": "ideal.groebner_basis", "arguments": {...}}},
//!    "member": {"spec": {"schema_version": 1, "backend": "oscar",
//!                        "operation": "ideal.membership",
//!                        "arguments": {"generators": {"$output": {"node": "gb", "pointer": "/basis"}},
//!                                      ...}},
//!               "depends_on": {"membership.in": [{"node": "gb", "claim": "gb.contained"}]}}}}
//! ```
//!
//! - The job of a node is submitted with the request id
//!   `wf/<workflow>/<node>/<attempt>`, so that running the workflow again
//!   finds the jobs it submitted before instead of creating new ones.
//! - `{"$output": {"node", "pointer"}}` becomes, once the producer has
//!   SUCCEEDED, a reference the scheduler checks against the producer's
//!   record: `{"$ref": {"job_id", "pointer", "sha256"}}` for a value of at
//!   most [`Options::inline_max_bytes`], a blob marker with `from` otherwise.
//! - Resuming goes through the nodes in topological order and asks the
//!   cluster about each (`LookupRequest`): a job that SUCCEEDED is done, one
//!   that runs is waited for, a LOST job retried as another is followed, and
//!   one that failed stops the workflow unless the caller asks to submit its
//!   next attempt ([`Options::retry_failed`]).
//! - A cancellation is kept in a state file beside the definition, under a
//!   lock, so that no orchestrator submits a node after it.
//! - The report gives, for each node, its job, the SHA-256 of its result and
//!   the effective level of each claim with the claims of other nodes it
//!   rests on (`depends_on`, written `job:<job_id>#<claim>`).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::Duration;

use algebramesh_core::blob::{self, BlobRef, OutputRef, RecordRef};
use algebramesh_core::guarantee::{self, EvidenceLevel, Guarantee, Summary};
use algebramesh_core::{JobRecord, JobSpec};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{Client, ClientError, JobStatus, ListJobsRequest, is_terminal, status_json};

/// The key of a reference to another node's output in a definition.
pub const OUTPUT_KEY: &str = "$output";
/// The longest request id the scheduler takes.
pub const MAX_REQUEST_ID_BYTES: usize = 128;
/// The highest attempt of a node; its request id must fit in
/// [`MAX_REQUEST_ID_BYTES`] with it.
pub const MAX_ATTEMPT: u32 = 9999;
/// The prefix of the request ids of workflows.
pub const REQUEST_PREFIX: &str = "wf/";

#[derive(Debug, thiserror::Error)]
pub enum WorkflowError {
    #[error("invalid workflow definition: {0}")]
    Definition(String),
    #[error(transparent)]
    Client(#[from] ClientError),
    /// The cluster holds a job of this node made from another definition
    /// (`ALREADY_EXISTS`): the node was changed after it was submitted.
    #[error("node {node} was changed after its job was submitted: {message}")]
    Changed { node: String, message: String },
    /// A producer's result has no value where a reference points.
    #[error("node {node}: {message}")]
    Output { node: String, message: String },
    #[error("workflow state file {path}: {message}")]
    State { path: PathBuf, message: String },
    #[error("another orchestrator runs this workflow; it holds {0}")]
    Busy(PathBuf),
    #[error(
        "the cluster has not enabled the feature {0}, which workflows with edges need; \
         upgrade every scheduler"
    )]
    Unsupported(&'static str),
}

/// A workflow as written: its name and its nodes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    pub workflow: String,
    pub nodes: BTreeMap<String, NodeDefinition>,
}

/// A node: a job specification whose top-level arguments may be
/// `{"$output": {"node", "pointer"}}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeDefinition {
    pub spec: Value,
    #[serde(default)]
    pub priority: i32,
    /// For a claim of this node's result, the claims of its producers'
    /// results it rests on.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub depends_on: BTreeMap<String, Vec<ClaimOf>>,
}

/// A claim of another node's result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimOf {
    pub node: String,
    pub claim: String,
}

/// `{"$output": {"node", "pointer"}}`; the pointer is `""` (the whole
/// result) when left out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Output {
    pub node: String,
    #[serde(default)]
    pub pointer: String,
}

/// A definition that was checked: names, references, no cycle, request ids
/// of a bounded length.
#[derive(Debug, Clone)]
pub struct Workflow {
    definition: Definition,
    /// The nodes, each after the nodes it takes outputs from.
    order: Vec<String>,
    /// The arguments of each node that take another node's output.
    inputs: BTreeMap<String, Vec<(String, Output)>>,
}

fn is_name(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// `wf/<workflow>/<node>/<attempt>`.
pub fn request_id(workflow: &str, node: &str, attempt: u32) -> String {
    format!("{REQUEST_PREFIX}{workflow}/{node}/{attempt}")
}

/// Whether `value` holds the key `$output` anywhere below its top level.
fn has_nested_output(value: &Value) -> bool {
    match value {
        Value::Object(m) => m
            .iter()
            .any(|(k, v)| k == OUTPUT_KEY || has_nested_output(v)),
        Value::Array(a) => a.iter().any(has_nested_output),
        _ => false,
    }
}

impl Workflow {
    /// Reads and checks a definition.
    pub fn from_json(text: &str) -> Result<Self, WorkflowError> {
        let definition: Definition =
            serde_json::from_str(text).map_err(|e| WorkflowError::Definition(e.to_string()))?;
        Self::new(definition)
    }

    /// Checks a definition: the names of the workflow and of its nodes are
    /// letters, digits, `.`, `_` and `-`; every request id fits in
    /// [`MAX_REQUEST_ID_BYTES`]; each specification is valid; outputs are
    /// taken only at top-level arguments, from other nodes, at top-level
    /// pointers; the claims a node rests on are its producers'; there is no
    /// cycle.
    pub fn new(definition: Definition) -> Result<Self, WorkflowError> {
        let bad = |m: String| Err(WorkflowError::Definition(m));
        if !is_name(&definition.workflow) {
            return bad(format!(
                "the workflow name {:?} is not made of letters, digits, '.', '_' and '-'",
                definition.workflow
            ));
        }
        if definition.nodes.is_empty() {
            return bad("the workflow has no nodes".into());
        }
        let mut inputs = BTreeMap::new();
        for (name, node) in &definition.nodes {
            if !is_name(name) {
                return bad(format!(
                    "the node name {name:?} is not made of letters, digits, '.', '_' and '-'"
                ));
            }
            let longest = request_id(&definition.workflow, name, MAX_ATTEMPT);
            if longest.len() > MAX_REQUEST_ID_BYTES {
                return bad(format!(
                    "node {name}: the request id {longest} would be longer than \
                     {MAX_REQUEST_ID_BYTES} bytes; shorten the workflow or node name"
                ));
            }
            let spec: JobSpec = serde_json::from_value(node.spec.clone())
                .map_err(|e| WorkflowError::Definition(format!("node {name}: {e}")))?;
            spec.validate()
                .map_err(|e| WorkflowError::Definition(format!("node {name}: {e}")))?;
            let mut taken = Vec::new();
            if let Value::Object(arguments) = &spec.arguments {
                for (key, value) in arguments {
                    let output = match value {
                        Value::Object(m) => m.get(OUTPUT_KEY).map(|o| (m, o)),
                        _ => None,
                    };
                    let Some((m, output)) = output else {
                        if has_nested_output(value) {
                            return bad(format!(
                                "node {name}: argument {key} holds {OUTPUT_KEY} below its top \
                                 level; an output can only be a whole top-level argument"
                            ));
                        }
                        continue;
                    };
                    let output: Output = match (m.len(), serde_json::from_value(output.clone())) {
                        (1, Ok(o)) => o,
                        _ => {
                            return bad(format!(
                                "node {name}: argument {key} is not {{\"{OUTPUT_KEY}\": \
                                 {{\"node\", \"pointer\"}}}}"
                            ));
                        }
                    };
                    if output.node == *name || !definition.nodes.contains_key(&output.node) {
                        return bad(format!(
                            "node {name}: argument {key} takes the output of {:?}, which is \
                             not another node of the workflow",
                            output.node
                        ));
                    }
                    blob::pointer_key(&output.pointer)
                        .map_err(|e| WorkflowError::Definition(format!("node {name}: {e}")))?;
                    taken.push((key.clone(), output));
                }
            } else if has_nested_output(&spec.arguments) {
                return bad(format!(
                    "node {name}: the arguments are not an object, so they can take no output"
                ));
            }
            for (claim, sources) in &node.depends_on {
                for source in sources {
                    if !taken.iter().any(|(_, o)| o.node == source.node) {
                        return bad(format!(
                            "node {name}: claim {claim} rests on {}#{}, but {} is not a node \
                             whose output it takes",
                            source.node, source.claim, source.node
                        ));
                    }
                }
            }
            inputs.insert(name.clone(), taken);
        }
        let order = topological(&definition, &inputs)?;
        Ok(Self {
            definition,
            order,
            inputs,
        })
    }

    pub fn name(&self) -> &str {
        &self.definition.workflow
    }

    pub fn definition(&self) -> &Definition {
        &self.definition
    }

    /// The nodes, each after the nodes whose outputs it takes.
    pub fn order(&self) -> &[String] {
        &self.order
    }

    /// The nodes whose outputs `node` takes.
    pub fn producers(&self, node: &str) -> BTreeSet<&str> {
        self.inputs
            .get(node)
            .map(|i| i.iter().map(|(_, o)| o.node.as_str()).collect())
            .unwrap_or_default()
    }

    /// The request id of an attempt of `node`.
    pub fn request_id(&self, node: &str, attempt: u32) -> String {
        request_id(self.name(), node, attempt)
    }

    /// The prefix of the request ids of every job of the workflow.
    pub fn prefix(&self) -> String {
        format!("{REQUEST_PREFIX}{}/", self.name())
    }
}

/// The nodes in an order where each comes after its producers, by name
/// among those ready together; an error on a cycle.
fn topological(
    definition: &Definition,
    inputs: &BTreeMap<String, Vec<(String, Output)>>,
) -> Result<Vec<String>, WorkflowError> {
    let mut waiting: BTreeMap<&str, BTreeSet<&str>> = inputs
        .iter()
        .map(|(n, i)| (n.as_str(), i.iter().map(|(_, o)| o.node.as_str()).collect()))
        .collect();
    let mut order = Vec::with_capacity(definition.nodes.len());
    while !waiting.is_empty() {
        let ready: Vec<&str> = waiting
            .iter()
            .filter(|(_, p)| p.is_empty())
            .map(|(n, _)| *n)
            .collect();
        if ready.is_empty() {
            let cycle: Vec<&str> = waiting.keys().copied().collect();
            return Err(WorkflowError::Definition(format!(
                "the nodes {} take each other's outputs in a cycle",
                cycle.join(", ")
            )));
        }
        for n in ready {
            waiting.remove(n);
            for producers in waiting.values_mut() {
                producers.remove(n);
            }
            order.push(n.to_owned());
        }
    }
    Ok(order)
}

/// How a workflow runs.
#[derive(Debug, Clone)]
pub struct Options {
    /// The state file: the cancellation of the workflow. Its lock is
    /// `<state>.lock`; an orchestrator that runs holds `<state>.run`.
    pub state: PathBuf,
    /// A value taken from a producer's result goes by reference
    /// `{"$ref": ...}` up to this size, in a blob above it; the arguments of
    /// a node that stay inline are moved to blobs, largest first, above it.
    pub inline_max_bytes: u64,
    /// Submits the next attempt of a node whose job failed, timed out, was
    /// cancelled or lost without a retry (once per run), instead of stopping.
    pub retry_failed: bool,
    /// The longest pause between two looks at the jobs that run.
    pub poll: Duration,
}

impl Options {
    pub fn new(state: PathBuf) -> Self {
        Self {
            state,
            inline_max_bytes: blob::INLINE_MAX_BYTES as u64,
            retry_failed: false,
            poll: Duration::from_secs(2),
        }
    }
}

/// The state file of the workflow defined in `definition`:
/// `<definition>.state.json`.
pub fn default_state_path(definition: &Path) -> PathBuf {
    let mut path = definition.as_os_str().to_owned();
    path.push(".state.json");
    PathBuf::from(path)
}

/// What the state file keeps.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub workflow: String,
    /// No node is submitted after this is set.
    #[serde(default)]
    pub cancelled: bool,
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut p = path.as_os_str().to_owned();
    p.push(suffix);
    PathBuf::from(p)
}

fn state_error(path: &Path, e: impl std::fmt::Display) -> WorkflowError {
    WorkflowError::State {
        path: path.to_owned(),
        message: e.to_string(),
    }
}

/// The state of the workflow `workflow` in `path`; a new one if there is no
/// file.
pub fn read_state(path: &Path, workflow: &str) -> Result<State, WorkflowError> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let state: State = serde_json::from_str(&text).map_err(|e| state_error(path, e))?;
            if state.workflow != workflow {
                return Err(state_error(
                    path,
                    format!(
                        "it is the state of workflow {:?}, not {workflow:?}",
                        state.workflow
                    ),
                ));
            }
            Ok(state)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(State {
            workflow: workflow.to_owned(),
            cancelled: false,
        }),
        Err(e) => Err(state_error(path, e)),
    }
}

/// Writes `state` to `path` whole: to a file beside it, which replaces it.
fn write_state(path: &Path, state: &State) -> Result<(), WorkflowError> {
    let part = sibling(path, ".part");
    let text = serde_json::to_string_pretty(state).map_err(|e| state_error(path, e))?;
    std::fs::write(&part, text + "\n").map_err(|e| state_error(&part, e))?;
    std::fs::rename(&part, path).map_err(|e| state_error(path, e))
}

/// The lock of the state file, held while it is changed and while a node is
/// submitted, so that a cancellation comes wholly before or after a
/// submission. The operating system lets it go if the process ends.
struct StateLock(#[allow(dead_code)] File);

impl StateLock {
    async fn take(state: &Path) -> Result<Self, WorkflowError> {
        let path = sibling(state, ".lock");
        let file = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|e| state_error(&path, e))?;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Self(file)),
                Err(std::fs::TryLockError::WouldBlock) => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Err(std::fs::TryLockError::Error(e)) => return Err(state_error(&path, e)),
            }
        }
    }
}

/// The lock an orchestrator that runs the workflow holds, so that two do not
/// run it at once.
struct RunLock(#[allow(dead_code)] File);

impl RunLock {
    fn take(state: &Path) -> Result<Self, WorkflowError> {
        let path = sibling(state, ".run");
        let file = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|e| state_error(&path, e))?;
        match file.try_lock() {
            Ok(()) => Ok(Self(file)),
            Err(std::fs::TryLockError::WouldBlock) => Err(WorkflowError::Busy(path)),
            Err(std::fs::TryLockError::Error(e)) => Err(state_error(&path, e)),
        }
    }
}

/// How a workflow stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Progress {
    /// Every node SUCCEEDED.
    Succeeded,
    /// Some nodes have not finished (a report of `status`).
    Running,
    /// A node failed and was not tried again; the nodes after it wait.
    Stopped,
    /// The workflow was cancelled; no node is submitted any more.
    Cancelled,
}

/// What a run, a resumption, a cancellation or a look at a workflow found.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub workflow: String,
    pub progress: Progress,
    pub nodes: BTreeMap<String, NodeReport>,
}

/// A node of a [`Report`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeReport {
    /// The state of its job, or `NOT_SUBMITTED`.
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// The job that answers the request (the retry of a LOST job, if any).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<Value>,
    /// The SHA-256 of the canonical JSON of its result, as a blob id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_sha256: Option<String>,
    /// The summary of its claims, by their effective levels across the
    /// edges of the workflow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<Summary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub claims: Vec<ClaimReport>,
}

/// A claim of a node's result with its effective level across the edges of
/// the workflow: the weakest of its own level and those of the claims it
/// depends on, in its own result and, as `job:<job_id>#<claim>`, in its
/// producers' results.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimReport {
    pub id: String,
    pub level: EvidenceLevel,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    pub effective_level: EvidenceLevel,
}

/// Where a node stands in this orchestrator.
#[derive(Debug, Clone, Default)]
enum Phase {
    /// Not looked up yet.
    #[default]
    Unknown,
    /// No job yet; the producers have not all succeeded.
    NotSubmitted,
    /// Its job runs.
    Waiting,
    /// Its job SUCCEEDED; the record keeps its markers.
    Done(Box<JobRecord>),
    /// Its job ended otherwise and is not tried again.
    Stopped,
}

#[derive(Debug, Clone, Default)]
struct NodeState {
    phase: Phase,
    attempt: Option<u32>,
    status: Option<JobStatus>,
    /// The definition was compared with the job found in the cluster.
    checked: bool,
    /// A next attempt was submitted in this run.
    retried: bool,
}

/// What a look at a job asks for next.
enum Next {
    Settled,
    Submit(u32),
}

/// Errors that pass: no scheduler serves now, or an answer was lost. Every
/// call of the orchestrator can be sent again: submissions carry their
/// request ids.
fn transient(e: &WorkflowError) -> bool {
    matches!(
        e,
        WorkflowError::Client(ClientError::Rpc {
            code: tonic::Code::Unavailable
                | tonic::Code::DeadlineExceeded
                | tonic::Code::Aborted
                | tonic::Code::Cancelled,
            ..
        })
    )
}

struct Driver<'a> {
    client: &'a Client,
    workflow: &'a Workflow,
    options: &'a Options,
    nodes: BTreeMap<String, NodeState>,
    /// Whether the cluster takes references (checked once, when needed).
    references: Option<bool>,
    blob_arguments: Option<bool>,
    cancelled: bool,
}

impl<'a> Driver<'a> {
    fn new(client: &'a Client, workflow: &'a Workflow, options: &'a Options) -> Self {
        Self {
            client,
            workflow,
            options,
            nodes: workflow
                .order()
                .iter()
                .map(|n| (n.clone(), NodeState::default()))
                .collect(),
            references: None,
            blob_arguments: None,
            cancelled: false,
        }
    }

    fn state(&self, node: &str) -> &NodeState {
        &self.nodes[node]
    }

    fn state_mut(&mut self, node: &str) -> &mut NodeState {
        self.nodes.entry(node.to_owned()).or_default()
    }

    fn done(&self, node: &str) -> Option<&JobRecord> {
        match &self.state(node).phase {
            Phase::Done(record) => Some(record),
            _ => None,
        }
    }

    fn ready(&self, node: &str) -> bool {
        self.workflow
            .producers(node)
            .iter()
            .all(|p| self.done(p).is_some())
    }

    /// Runs the workflow until every node succeeded, one stopped, or it was
    /// cancelled.
    async fn drive(&mut self) -> Result<Progress, WorkflowError> {
        let mut delay = Duration::from_millis(100);
        loop {
            if read_state(&self.options.state, self.workflow.name())?.cancelled {
                self.cancelled = true;
            }
            if self.cancelled {
                return Ok(Progress::Cancelled);
            }
            for node in self.workflow.order().to_vec() {
                if matches!(self.state(&node).phase, Phase::Done(_) | Phase::Stopped)
                    || !self.ready(&node)
                {
                    continue;
                }
                match self.advance(&node, true).await {
                    Ok(()) => {}
                    Err(e) if transient(&e) => break,
                    Err(e) => return Err(e),
                }
                if self.cancelled {
                    return Ok(Progress::Cancelled);
                }
            }
            if self
                .nodes
                .values()
                .all(|n| matches!(n.phase, Phase::Done(_)))
            {
                return Ok(Progress::Succeeded);
            }
            if self
                .nodes
                .values()
                .any(|n| matches!(n.phase, Phase::Stopped))
            {
                return Ok(Progress::Stopped);
            }
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(self.options.poll);
        }
    }

    /// Looks at every node once, submitting nothing.
    async fn look(&mut self) -> Result<Progress, WorkflowError> {
        let cancelled = read_state(&self.options.state, self.workflow.name())?.cancelled;
        for node in self.workflow.order().to_vec() {
            self.advance(&node, false).await?;
        }
        Ok(if cancelled {
            Progress::Cancelled
        } else if self
            .nodes
            .values()
            .all(|n| matches!(n.phase, Phase::Done(_)))
        {
            Progress::Succeeded
        } else if self
            .nodes
            .values()
            .any(|n| matches!(n.phase, Phase::Stopped))
        {
            Progress::Stopped
        } else {
            Progress::Running
        })
    }

    /// Takes one step for `node`: finds its job, or submits it when `submit`
    /// and its producers have succeeded, or looks at it again.
    async fn advance(&mut self, node: &str, submit: bool) -> Result<(), WorkflowError> {
        let state = self.state(node).clone();
        let status = match (&state.phase, &state.status) {
            (Phase::Waiting, Some(status)) => self.client.status(&status.job_id).await?,
            _ => {
                let Some(attempt) = self.latest_attempt(node).await? else {
                    self.state_mut(node).phase = Phase::NotSubmitted;
                    if submit && self.ready(node) {
                        return self.submit(node, 1).await;
                    }
                    return Ok(());
                };
                let request = self.workflow.request_id(node, attempt);
                let Some(status) = self.client.lookup_request(&request).await? else {
                    // Listed but not found: the listing came from another
                    // scheduler's committed state; looked up again later.
                    return Ok(());
                };
                self.state_mut(node).attempt = Some(attempt);
                // A job found in the cluster must answer this definition:
                // the same submission returns it, a changed one is refused.
                if submit && !state.checked && self.ready(node) {
                    let Some(sent) = self.send(node, attempt).await? else {
                        return Ok(());
                    };
                    self.state_mut(node).checked = true;
                    sent
                } else {
                    status
                }
            }
        };
        let mut status = status;
        let attempt = self.state(node).attempt.unwrap_or(1);
        // A LOST job that was retried under its policy answers through the
        // retry (the request id names the first one).
        while status.state == "LOST" && !status.retried_as.is_empty() {
            status = self.client.status(&status.retried_as).await?;
        }
        match self.settle(node, attempt, status, submit).await? {
            Next::Settled => Ok(()),
            Next::Submit(next) => self.submit(node, next).await,
        }
    }

    /// Records what `status` says of `node`.
    async fn settle(
        &mut self,
        node: &str,
        attempt: u32,
        status: JobStatus,
        submit: bool,
    ) -> Result<Next, WorkflowError> {
        let state = status.state.clone();
        if state == "SUCCEEDED" {
            let (_, record) = self.client.result_refs(&status.job_id).await?;
            let record = record.ok_or_else(|| {
                ClientError::Data(format!("job {} SUCCEEDED without a record", status.job_id))
            })?;
            let s = self.state_mut(node);
            s.phase = Phase::Done(Box::new(record));
            s.status = Some(status);
            return Ok(Next::Settled);
        }
        let retry = submit && self.options.retry_failed && !self.state(node).retried;
        let s = self.state_mut(node);
        s.status = Some(status);
        if !is_terminal(&state) {
            s.phase = Phase::Waiting;
            return Ok(Next::Settled);
        }
        if retry {
            s.retried = true;
            return Ok(Next::Submit(attempt + 1));
        }
        s.phase = Phase::Stopped;
        Ok(Next::Settled)
    }

    /// The highest attempt of `node` the cluster has a job for.
    async fn latest_attempt(&self, node: &str) -> Result<Option<u32>, WorkflowError> {
        let prefix = format!("{}{node}/", self.workflow.prefix());
        let mut request = ListJobsRequest {
            request_id_prefix: prefix.clone(),
            page_size: 1000,
            ..Default::default()
        };
        let mut latest = None;
        loop {
            let page = self.client.list_jobs(&request).await?;
            for job in &page.jobs {
                if let Some(Ok(attempt)) =
                    job.request_id.strip_prefix(&prefix).map(str::parse::<u32>)
                {
                    latest = latest.max(Some(attempt));
                }
            }
            if page.next_page_token.is_empty() {
                return Ok(latest);
            }
            request.page_token = page.next_page_token;
        }
    }

    /// Submits attempt `attempt` of `node`, unless the workflow was cancelled.
    async fn submit(&mut self, node: &str, attempt: u32) -> Result<(), WorkflowError> {
        if attempt > MAX_ATTEMPT {
            return Err(WorkflowError::Output {
                node: node.to_owned(),
                message: format!("no attempt after {MAX_ATTEMPT}"),
            });
        }
        let Some(status) = self.send(node, attempt).await? else {
            return Ok(());
        };
        let s = self.state_mut(node);
        s.attempt = Some(attempt);
        s.checked = true;
        match self.settle(node, attempt, status, true).await? {
            Next::Settled => Ok(()),
            // The job of a new attempt ended at once: no second retry.
            Next::Submit(_) => {
                self.state_mut(node).phase = Phase::Stopped;
                Ok(())
            }
        }
    }

    /// Sends the submission of attempt `attempt` of `node`, under the lock of
    /// the state file and unless the workflow was cancelled (`None` then).
    async fn send(&mut self, node: &str, attempt: u32) -> Result<Option<JobStatus>, WorkflowError> {
        let spec_json = self.prepare(node).await?;
        let _lock = StateLock::take(&self.options.state).await?;
        if read_state(&self.options.state, self.workflow.name())?.cancelled {
            self.cancelled = true;
            return Ok(None);
        }
        let priority = self.workflow.definition.nodes[node].priority;
        let request = self.workflow.request_id(node, attempt);
        match self
            .client
            .submit_json_as_is(&spec_json, priority, &request)
            .await
        {
            Ok(status) => Ok(Some(status)),
            Err(ClientError::Rpc {
                code: tonic::Code::AlreadyExists,
                message,
            }) => Err(WorkflowError::Changed {
                node: node.to_owned(),
                message,
            }),
            Err(e) => Err(e.into()),
        }
    }

    /// Whether the cluster has enabled `feature`; asked once.
    async fn has_feature(&mut self, feature: &'static str) -> Result<bool, WorkflowError> {
        let cached = if feature == algebramesh_core::features::WORKFLOW_REFS {
            &mut self.references
        } else {
            &mut self.blob_arguments
        };
        if let Some(known) = *cached {
            return Ok(known);
        }
        let enabled = self.client.backends().await?.server_features;
        let has = enabled.iter().any(|f| f == feature);
        let cached = if feature == algebramesh_core::features::WORKFLOW_REFS {
            &mut self.references
        } else {
            &mut self.blob_arguments
        };
        *cached = Some(has);
        Ok(has)
    }

    /// The specification of `node` as it is submitted: each output it takes
    /// replaced by a reference to its producer's record, and the other
    /// arguments moved to blobs, largest first, while it is larger than
    /// [`Options::inline_max_bytes`].
    async fn prepare(&mut self, node: &str) -> Result<String, WorkflowError> {
        let mut spec = self.workflow.definition.nodes[node].spec.clone();
        let inputs = self.workflow.inputs[node].clone();
        if !inputs.is_empty() {
            for feature in [
                algebramesh_core::features::WORKFLOW_REFS,
                algebramesh_core::features::BLOB_ARGUMENTS,
            ] {
                if !self.has_feature(feature).await? {
                    return Err(WorkflowError::Unsupported(feature));
                }
            }
        }
        let inline_max = self.options.inline_max_bytes;
        for (key, output) in &inputs {
            let (job_id, value) = {
                let record = self.done(&output.node).ok_or_else(|| {
                    ClientError::Data(format!("node {} has not succeeded", output.node))
                })?;
                let value = record
                    .result
                    .as_ref()
                    .map(|r| blob::pointed(r, &output.pointer))
                    .transpose()
                    .map_err(|e| WorkflowError::Definition(e.to_string()))?
                    .flatten()
                    .cloned();
                (record.job_id.to_string(), value)
            };
            let Some(value) = value else {
                return Err(WorkflowError::Output {
                    node: node.to_owned(),
                    message: format!(
                        "argument {key}: the result of node {} (job {job_id}) has no top-level \
                         value at {:?}; a result kept whole in one blob has none",
                        output.node, output.pointer
                    ),
                });
            };
            let origin = OutputRef {
                job_id: job_id.clone(),
                pointer: output.pointer.clone(),
            };
            let reference = match BlobRef::from_marker(&value) {
                Ok(Some(b)) => blob::marker_from(&b, &origin),
                _ => {
                    let r = blob::canonical_ref(&value);
                    if r.size > inline_max {
                        let stored = self.client.put_value(&value).await?;
                        blob::marker_from(&stored, &origin)
                    } else {
                        RecordRef {
                            job_id,
                            pointer: output.pointer.clone(),
                            sha256: r.id,
                        }
                        .marker()
                    }
                }
            };
            spec["arguments"][key] = reference;
        }
        let spec_json =
            serde_json::to_string(&spec).map_err(|e| ClientError::Data(e.to_string()))?;
        if spec_json.len() as u64 <= inline_max {
            return Ok(spec_json);
        }
        let plain: Map<String, Value> = spec["arguments"]
            .as_object()
            .map(|m| {
                m.iter()
                    .filter(|(k, _)| !inputs.iter().any(|(i, _)| i == *k))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect()
            })
            .unwrap_or_default();
        let keys =
            blob::plan_argument_offload(&Value::Object(plain), spec_json.len() as u64, inline_max)
                .map_err(|e| ClientError::Data(e.to_string()))?;
        if keys.is_empty()
            || !self
                .has_feature(algebramesh_core::features::BLOB_ARGUMENTS)
                .await?
        {
            return Ok(spec_json);
        }
        for key in keys {
            let stored = self.client.put_value(&spec["arguments"][&key]).await?;
            spec["arguments"][&key] = stored.marker();
        }
        serde_json::to_string(&spec).map_err(|e| ClientError::Data(e.to_string()).into())
    }

    /// The report of every node.
    async fn report(&self, progress: Progress) -> Result<Report, WorkflowError> {
        // The effective level of each claim of each node that succeeded, in
        // topological order: a producer's claims are known before its
        // consumers'.
        let mut levels: HashMap<(String, String), EvidenceLevel> = HashMap::new();
        let mut by_job: HashMap<String, String> = HashMap::new();
        let mut nodes = BTreeMap::new();
        for node in self.workflow.order() {
            let state = self.state(node);
            let mut report = NodeReport {
                state: state
                    .status
                    .as_ref()
                    .map_or_else(|| "NOT_SUBMITTED".to_owned(), |s| s.state.clone()),
                attempt: state.attempt,
                request_id: state.attempt.map(|a| self.workflow.request_id(node, a)),
                job_id: state.status.as_ref().map(|s| s.job_id.clone()),
                error: state
                    .status
                    .as_ref()
                    .and_then(|s| status_json(s).get("error").cloned())
                    .filter(|e| !e.is_null()),
                result_sha256: None,
                summary: None,
                claims: Vec::new(),
            };
            if let Phase::Done(record) = &state.phase {
                by_job.insert(record.job_id.to_string(), node.clone());
                if let Some(result) = &record.result {
                    report.result_sha256 =
                        Some(self.client.value_sha256(result).await?.to_string());
                }
                if let Some(g) = &record.guarantee {
                    let extended = self.across_edges(node, g);
                    let other = |job: &str, claim: &str| {
                        by_job
                            .get(job)
                            .and_then(|n| levels.get(&(n.clone(), claim.to_owned())))
                            .copied()
                            .unwrap_or(EvidenceLevel::Asserted)
                    };
                    let claims: Vec<ClaimReport> = extended
                        .claims
                        .iter()
                        .map(|c| ClaimReport {
                            id: c.id.clone(),
                            level: c.level,
                            depends_on: c.depends_on.clone(),
                            effective_level: extended.effective_level_with(&c.id, &other),
                        })
                        .collect();
                    report.summary = Some(extended.summary_with(&other));
                    for c in &claims {
                        levels.insert((node.clone(), c.id.clone()), c.effective_level);
                    }
                    report.claims = claims;
                }
            }
            nodes.insert(node.clone(), report);
        }
        Ok(Report {
            workflow: self.workflow.name().to_owned(),
            progress,
            nodes,
        })
    }

    /// The guarantee of `node` with the claims of its producers it rests on
    /// added to `depends_on`, as `job:<job_id>#<claim>`.
    fn across_edges(&self, node: &str, g: &Guarantee) -> Guarantee {
        let mut g = g.clone();
        let wanted = &self.workflow.definition.nodes[node].depends_on;
        for claim in &mut g.claims {
            for source in wanted.get(&claim.id).into_iter().flatten() {
                let job = self
                    .done(&source.node)
                    .map_or_else(String::new, |r| r.job_id.to_string());
                let dependency = guarantee::job_claim(&job, &source.claim);
                if !claim.depends_on.contains(&dependency) {
                    claim.depends_on.push(dependency);
                }
            }
        }
        g
    }
}

/// Runs `workflow`, or resumes it where an earlier run stopped: the jobs the
/// cluster has for it are found by their request ids, and only the nodes
/// without one are submitted. Returns once every node succeeded, a node
/// stopped, or the workflow was cancelled; refuses to run while another
/// orchestrator runs it with the same state file.
pub async fn run(
    client: &Client,
    workflow: &Workflow,
    options: &Options,
) -> Result<Report, WorkflowError> {
    let _run = RunLock::take(&options.state)?;
    let mut driver = Driver::new(client, workflow, options);
    let progress = driver.drive().await?;
    driver.report(progress).await
}

/// How `workflow` stands in the cluster, without submitting anything.
pub async fn status(
    client: &Client,
    workflow: &Workflow,
    options: &Options,
) -> Result<Report, WorkflowError> {
    let mut driver = Driver::new(client, workflow, options);
    let progress = driver.look().await?;
    driver.report(progress).await
}

/// Cancels `workflow`: records it in the state file, so that no orchestrator
/// submits another node, and cancels its jobs that have not finished.
pub async fn cancel(
    client: &Client,
    workflow: &Workflow,
    options: &Options,
) -> Result<Report, WorkflowError> {
    {
        let _lock = StateLock::take(&options.state).await?;
        let mut state = read_state(&options.state, workflow.name())?;
        state.cancelled = true;
        write_state(&options.state, &state)?;
    }
    // Every submission came before the cancellation was written, under the
    // same lock: the listing sees them all.
    let mut request = ListJobsRequest {
        request_id_prefix: workflow.prefix(),
        page_size: 1000,
        ..Default::default()
    };
    loop {
        let page = client.list_jobs(&request).await?;
        for job in &page.jobs {
            if !is_terminal(&job.state) && !job.cancel_requested {
                client.cancel(&job.job_id).await?;
            }
        }
        if page.next_page_token.is_empty() {
            break;
        }
        request.page_token = page.next_page_token;
    }
    status(client, workflow, options).await
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use serde_json::json;

    fn spec(arguments: Value) -> Value {
        json!({"schema_version": 1, "backend": "test", "operation": "test.op", "arguments": arguments})
    }

    fn definition(nodes: Value) -> Result<Workflow, WorkflowError> {
        Workflow::from_json(&json!({"workflow": "w", "nodes": nodes}).to_string())
    }

    #[test]
    fn nodes_come_after_the_nodes_they_take_outputs_from() {
        let w = definition(json!({
            "c": {"spec": spec(json!({"x": {"$output": {"node": "b", "pointer": "/v"}},
                                      "y": {"$output": {"node": "a"}}}))},
            "b": {"spec": spec(json!({"x": {"$output": {"node": "a", "pointer": ""}}}))},
            "a": {"spec": spec(json!({"n": 1}))},
            "d": {"spec": spec(json!({}))},
        }))
        .unwrap();
        assert_eq!(w.order(), ["a", "d", "b", "c"]);
        assert_eq!(w.producers("c"), BTreeSet::from(["a", "b"]));
        assert_eq!(w.request_id("c", 2), "wf/w/c/2");
        assert_eq!(w.prefix(), "wf/w/");
    }

    #[test]
    fn bad_definitions_are_refused_when_read() {
        let cases = [
            json!({"a": {"spec": spec(json!({"x": {"$output": {"node": "b"}}}))},
                   "b": {"spec": spec(json!({"x": {"$output": {"node": "a"}}}))}}),
            json!({"a": {"spec": spec(json!({"x": {"$output": {"node": "a"}}}))}}),
            json!({"a": {"spec": spec(json!({"x": {"$output": {"node": "z"}}}))}}),
            json!({"a": {"spec": spec(json!({}))},
                   "b": {"spec": spec(json!({"x": {"$output": {"node": "a", "pointer": "/p/q"}}}))}}),
            json!({"a": {"spec": spec(json!({}))},
                   "b": {"spec": spec(json!({"x": [{"$output": {"node": "a"}}]}))}}),
            json!({"a": {"spec": spec(json!({}))},
                   "b": {"spec": spec(json!({"x": {"$output": {"node": "a"}, "y": 1}}))}}),
            json!({"a": {"spec": spec(json!({}))},
                   "b": {"spec": spec(json!({})),
                         "depends_on": {"c": [{"node": "a", "claim": "gb.contained"}]}}}),
            json!({"a b": {"spec": spec(json!({}))}}),
            json!({"a": {"spec": {"schema_version": 2, "backend": "t", "operation": "o", "arguments": {}}}}),
            json!({}),
        ];
        for nodes in cases {
            assert!(
                matches!(definition(nodes.clone()), Err(WorkflowError::Definition(_))),
                "{nodes}"
            );
        }
    }

    #[test]
    fn request_ids_longer_than_the_scheduler_takes_are_refused() {
        let long = "n".repeat(MAX_REQUEST_ID_BYTES);
        let e = definition(json!({long: {"spec": spec(json!({}))}})).unwrap_err();
        assert!(e.to_string().contains("128"), "{e}");
        // The longest that fits, with room for the last attempt.
        let room = MAX_REQUEST_ID_BYTES - "wf/w//".len() - MAX_ATTEMPT.to_string().len();
        let fits = "n".repeat(room);
        assert!(definition(json!({fits.clone(): {"spec": spec(json!({}))}})).is_ok());
        let over = "n".repeat(room + 1);
        assert!(definition(json!({over: {"spec": spec(json!({}))}})).is_err());
    }

    #[test]
    fn the_state_keeps_a_cancellation_of_its_own_workflow() {
        let dir = std::env::temp_dir().join(format!("am-wf-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("w.json.state.json");
        assert!(!read_state(&path, "w").unwrap().cancelled);
        write_state(
            &path,
            &State {
                workflow: "w".into(),
                cancelled: true,
            },
        )
        .unwrap();
        assert!(read_state(&path, "w").unwrap().cancelled);
        assert!(read_state(&path, "other").is_err());
        assert_eq!(
            default_state_path(Path::new("a/w.json")),
            PathBuf::from("a/w.json.state.json")
        );
        // One orchestrator at a time.
        let first = RunLock::take(&path).unwrap();
        assert!(matches!(RunLock::take(&path), Err(WorkflowError::Busy(_))));
        drop(first);
        drop(RunLock::take(&path).unwrap());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

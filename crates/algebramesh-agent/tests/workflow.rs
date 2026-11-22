//! Workflows on the client side (W2, ADR 0032; Issues #104, #106): a DAG of
//! four nodes over two backends, run whole and run with its orchestrator
//! stopped halfway and started again; run across a change of leader;
//! cancelled; resumed after its definition changed, and with another
//! threshold for values sent inline; and the effective levels of claims that
//! rest on the claims of another node.
//!
//! The numbers are those of the roadmap's acceptance conditions of W2.

#![allow(clippy::unwrap_used)]

mod common;

use algebramesh_client::workflow::{self, Options, Progress, Report, Workflow, WorkflowError};
use algebramesh_core::guarantee::{EvidenceLevel, job_claim};
use common::*;
use serde_json::{Value, json};

fn info(name: &str) -> BackendInfo {
    BackendInfo {
        name: name.into(),
        backend_version: Some("1".into()),
        driver_version: "test".into(),
        build: None,
        ..BackendInfo::default()
    }
}

fn answer(name: &str, value: Value) -> Result<CasResult, CasError> {
    Ok(CasResult {
        value,
        backend: info(name),
        execution_host: None,
        log: vec![],
        guarantee: None,
    })
}

async fn pause(arguments: &Value) {
    let ms = arguments["ms"].as_u64().unwrap_or(0);
    tokio::time::sleep(Duration::from_millis(ms)).await;
}

/// `alpha.make {n, kib}` gives `{rows, n}` with rows of `kib` KiB (moved to
/// a blob by the agent when large); `alpha.add {x, y, pad, k}` gives their
/// sum; `alpha.generators` gives the ideal (x, y) of QQ[x, y].
struct Alpha;

#[async_trait::async_trait]
impl CasBackend for Alpha {
    fn name(&self) -> &'static str {
        "alpha"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            operations: vec![
                "alpha.make".into(),
                "alpha.add".into(),
                "alpha.generators".into(),
            ],
            ..Capabilities::default()
        }
    }

    async fn execute(&self, job: CasRequest, _: ExecutionContext) -> Result<CasResult, CasError> {
        let a = &job.arguments;
        pause(a).await;
        let value = match job.operation.as_str() {
            "alpha.make" => {
                let n = a["n"].as_u64().unwrap();
                let kib = a["kib"].as_u64().unwrap() as usize;
                json!({"rows": format!("{n}").repeat(kib << 10), "n": n})
            }
            "alpha.add" => json!({
                "sum": a["x"].as_u64().unwrap() + a["y"].as_u64().unwrap(),
                "pad": a["pad"].as_str().unwrap().len(),
                "k": a["k"],
            }),
            "alpha.generators" => json!({"ideal": {
                "type": "ideal", "ring": ring(), "generators": [x(), y()],
            }}),
            other => return Err(CasError::Internal(format!("no {other}"))),
        };
        answer("alpha", value)
    }
}

/// `beta.combine {rows, n}` gives `{len, n + 1, pad}`; `beta.wrap {whole}`
/// gives it back; `ideal.membership` of x in (x, y) gives the coefficients
/// (1, 0), which the agent's checker finds correct.
struct Beta;

#[async_trait::async_trait]
impl CasBackend for Beta {
    fn name(&self) -> &'static str {
        "beta"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            operations: vec![
                "beta.combine".into(),
                "beta.wrap".into(),
                "ideal.membership".into(),
            ],
            ..Capabilities::default()
        }
    }

    async fn execute(&self, job: CasRequest, _: ExecutionContext) -> Result<CasResult, CasError> {
        let a = &job.arguments;
        pause(a).await;
        let value = match job.operation.as_str() {
            "beta.combine" => json!({
                "len": a["rows"].as_str().unwrap().len(),
                "n": a["n"].as_u64().unwrap() + 1,
                "pad": "p".repeat(100 << 10),
            }),
            "beta.wrap" => json!({"whole": a["whole"]}),
            "ideal.membership" => {
                assert_eq!(a["ideal"]["generators"], json!([x(), y()]));
                json!({
                    "type": "ideal_membership", "member": true,
                    "certificate": {"coefficients": [one(), zero()]},
                })
            }
            other => return Err(CasError::Internal(format!("no {other}"))),
        };
        answer("beta", value)
    }
}

fn ring() -> Value {
    json!({"domain": "QQ", "variables": ["x", "y"], "order": "grevlex"})
}

fn poly(terms: &[(&str, [u32; 2])]) -> Value {
    let terms: Vec<Value> = terms
        .iter()
        .map(|(c, e)| json!({"coefficient": c, "exponents": e}))
        .collect();
    json!({"type": "polynomial", "terms": terms})
}

fn x() -> Value {
    poly(&[("1", [1, 0])])
}

fn y() -> Value {
    poly(&[("1", [0, 1])])
}

fn one() -> Value {
    poly(&[("1", [0, 0])])
}

fn zero() -> Value {
    poly(&[])
}

/// A job that the scheduler submits again when it is LOST, as after a change
/// of leader while it runs; the orchestrator follows the retry.
fn spec(backend: &str, operation: &str, arguments: Value) -> Value {
    json!({"schema_version": 1, "backend": backend, "operation": operation,
           "arguments": arguments, "retry_policy": "if_deterministic"})
}

fn output(node: &str, pointer: &str) -> Value {
    json!({"$output": {"node": node, "pointer": pointer}})
}

/// Four nodes over two backends: a large value (a blob) from a to b, small
/// ones (references) from a and b to c, a value of 100 KiB kept inline in
/// b's record to c, and c's whole result to d.
fn dag(name: &str, ms: u64, k: u64) -> Workflow {
    Workflow::from_json(
        &json!({"workflow": name, "nodes": {
            "a": {"spec": spec("alpha", "alpha.make", json!({"n": 3, "kib": 300, "ms": ms}))},
            "b": {"spec": spec("beta", "beta.combine",
                               json!({"rows": output("a", "/rows"), "n": output("a", "/n"),
                                      "ms": ms}))},
            "c": {"spec": spec("alpha", "alpha.add",
                               json!({"x": output("b", "/n"), "y": output("a", "/n"),
                                      "pad": output("b", "/pad"), "k": k, "ms": ms}))},
            "d": {"spec": spec("beta", "beta.wrap", json!({"whole": output("c", "")}))},
        }})
        .to_string(),
    )
    .unwrap()
}

fn options(cluster: &Cluster, name: &str) -> Options {
    let mut options = Options::new(cluster.dir.join(format!("{name}.state.json")));
    options.poll = Duration::from_millis(200);
    options
}

async fn started(n: usize) -> (Cluster, Vec<Option<Scheduler>>, Client) {
    let (cluster, listeners) = Cluster::new(n);
    std::fs::create_dir_all(&cluster.dir).unwrap();
    let mut schedulers = Vec::new();
    for (i, l) in listeners.into_iter().enumerate() {
        schedulers.push(Some(cluster.start(i, l).await));
    }
    features_agreed(&schedulers).await;
    serving(&all(&schedulers)).await;
    let client = cluster.client().await;
    (cluster, schedulers, client)
}

async fn agents(cluster: &Cluster) -> (CancellationToken, CancellationToken) {
    (
        cluster.agent_with(Arc::new(Alpha)).await,
        cluster.agent_with(Arc::new(Beta)).await,
    )
}

/// The jobs whose request ids start with `prefix`.
async fn jobs(client: &Client, prefix: &str) -> Vec<algebramesh_client::JobSummary> {
    let request = algebramesh_client::ListJobsRequest {
        request_id_prefix: prefix.into(),
        page_size: 1000,
        ..Default::default()
    };
    loop {
        match client.list_jobs(&request).await {
            Ok(page) => return page.jobs,
            Err(e) => {
                tracing::debug!("list again: {e}");
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }
}

/// Waits until the cluster has at least `n` jobs under `prefix`.
async fn until_jobs(client: &Client, prefix: &str, n: usize) {
    let started = std::time::Instant::now();
    while jobs(client, prefix).await.len() < n {
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "fewer than {n} jobs under {prefix}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn succeeded(report: &Report) {
    assert_eq!(report.progress, Progress::Succeeded, "{report:#?}");
    assert!(report.nodes.values().all(|n| n.state == "SUCCEEDED"));
}

fn final_sha256(report: &Report) -> String {
    report.nodes["d"].result_sha256.clone().unwrap()
}

/// 1 and 4: an orchestrator stopped halfway is started again; the cluster
/// holds one job per node, and the last result is that of a run that was not
/// stopped. Resuming with another inline threshold finds the same jobs; a
/// node changed after its job was submitted is refused.
#[tokio::test(flavor = "multi_thread")]
async fn a_workflow_stopped_halfway_resumes_without_doing_anything_twice() {
    let (cluster, _schedulers, client) = started(1).await;
    let _agents = agents(&cluster).await;

    let whole = dag("whole", 0, 1);
    let report = workflow::run(&client, &whole, &options(&cluster, "whole"))
        .await
        .unwrap();
    succeeded(&report);
    assert_eq!(
        report.nodes["d"].request_id.as_deref(),
        Some("wf/whole/d/1")
    );
    assert_eq!(jobs(&client, "wf/whole/").await.len(), 4);
    // The value d got is c's result.
    let (_, d) = client
        .result(report.nodes["d"].job_id.as_ref().unwrap())
        .await
        .unwrap();
    assert_eq!(
        d.unwrap().result.unwrap(),
        json!({"whole": {"sum": 7, "pad": 100 << 10, "k": 1}})
    );

    let halted = dag("halted", 300, 1);
    let opts = options(&cluster, "halted");
    let first = {
        let (client, halted, opts) = (client.clone(), halted.clone(), opts.clone());
        tokio::spawn(async move { workflow::run(&client, &halted, &opts).await })
    };
    until_jobs(&client, "wf/halted/", 2).await;
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    let report = workflow::run(&client, &halted, &opts).await.unwrap();
    succeeded(&report);
    assert_eq!(jobs(&client, "wf/halted/").await.len(), 4);
    assert_eq!(
        final_sha256(&report),
        final_sha256(&dag_report(&client, &cluster, "whole").await)
    );

    // 4: another inline threshold sends c's value of 100 KiB as a blob
    // instead of a reference: the same request, the same jobs.
    let mut small = opts.clone();
    small.inline_max_bytes = 64 << 10;
    let again = workflow::run(&client, &halted, &small).await.unwrap();
    succeeded(&again);
    assert_eq!(again.nodes, report.nodes);
    assert_eq!(jobs(&client, "wf/halted/").await.len(), 4);
    // A changed node is refused, and nothing is submitted.
    let changed = dag("halted", 300, 2);
    let e = workflow::run(&client, &changed, &opts).await.unwrap_err();
    assert!(
        matches!(&e, WorkflowError::Changed { node, .. } if node == "c"),
        "{e}"
    );
    assert_eq!(jobs(&client, "wf/halted/").await.len(), 4);
}

/// The report of the workflow `name` (a `dag`) as the cluster has it.
async fn dag_report(client: &Client, cluster: &Cluster, name: &str) -> Report {
    workflow::status(client, &dag(name, 0, 1), &options(cluster, name))
        .await
        .unwrap()
}

/// 2: a change of leader while a workflow runs does not stop it.
#[tokio::test(flavor = "multi_thread")]
async fn a_workflow_runs_across_a_change_of_leader() {
    let (cluster, schedulers, client) = started(3).await;
    let _agents = agents(&cluster).await;
    let flow = dag("failover", 300, 1);
    let opts = options(&cluster, "failover");
    let run = {
        let (client, flow, opts) = (client.clone(), flow.clone(), opts.clone());
        tokio::spawn(async move { workflow::run(&client, &flow, &opts).await })
    };
    until_jobs(&client, "wf/failover/", 2).await;
    let leader = serving(&all(&schedulers)).await;
    lead_or_follow(&schedulers, leader, false).await;
    let report = tokio::time::timeout(Duration::from_secs(120), run)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    succeeded(&report);
    // The request ids name the first job of each node; a retry has none.
    assert_eq!(jobs(&client, "wf/failover/").await.len(), 4);
}

/// 3: once a workflow is cancelled, no node is submitted, by the
/// orchestrator that ran it or by one started again.
#[tokio::test(flavor = "multi_thread")]
async fn a_cancelled_workflow_submits_nothing_more() {
    let (cluster, _schedulers, client) = started(1).await;
    let _agents = agents(&cluster).await;
    let flow = dag("cancelled", 1500, 1);
    let opts = options(&cluster, "cancelled");
    let run = {
        let (client, flow, opts) = (client.clone(), flow.clone(), opts.clone());
        tokio::spawn(async move { workflow::run(&client, &flow, &opts).await })
    };
    until_jobs(&client, "wf/cancelled/", 1).await;
    let report = workflow::cancel(&client, &flow, &opts).await.unwrap();
    assert_eq!(report.progress, Progress::Cancelled);
    let report = run.await.unwrap().unwrap();
    assert_eq!(report.progress, Progress::Cancelled);
    let submitted = jobs(&client, "wf/cancelled/").await;
    assert_eq!(submitted.len(), 1, "{submitted:?}");
    // The job that ran was cancelled.
    let a = client.status(&submitted[0].job_id).await.unwrap();
    assert!(
        matches!(a.state.as_str(), "CANCELLED" | "CANCELLING") || a.cancel_requested,
        "{a:?}"
    );
    // Started again, the orchestrator submits nothing.
    let report = workflow::run(&client, &flow, &opts).await.unwrap();
    assert_eq!(report.progress, Progress::Cancelled);
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(jobs(&client, "wf/cancelled/").await.len(), 1);
    assert_eq!(report.nodes["d"].state, "NOT_SUBMITTED");
}

fn membership(name: &str, rests_on_the_generators: bool) -> Workflow {
    let mut member = json!({"spec": spec("beta", "ideal.membership", json!({
        "ideal": output("gens", "/ideal"),
        "polynomial": x(),
        "certificate": "representation",
    }))});
    if rests_on_the_generators {
        member["depends_on"] = json!({"membership.in": [{"node": "gens", "claim": "value"}]});
    }
    Workflow::from_json(
        &json!({"workflow": name, "nodes": {
            "gens": {"spec": spec("alpha", "alpha.generators", json!({}))},
            "member": member,
        }})
        .to_string(),
    )
    .unwrap()
}

/// 6: a claim checked in its own result but resting on a claim of a
/// producer that is asserted is reported as asserted, with the producer's
/// claim in its `depends_on`.
#[tokio::test(flavor = "multi_thread")]
async fn a_claim_is_no_stronger_than_the_claims_of_other_nodes_it_rests_on() {
    let (cluster, _schedulers, client) = started(1).await;
    let _agents = agents(&cluster).await;

    let alone = workflow::run(
        &client,
        &membership("alone", false),
        &options(&cluster, "alone"),
    )
    .await
    .unwrap();
    succeeded(&alone);
    let claims = &alone.nodes["member"].claims;
    assert_eq!(claims.len(), 1, "{claims:?}");
    assert_eq!(claims[0].id, "membership.in");
    assert_eq!(claims[0].effective_level, EvidenceLevel::Checked);

    let flow = membership("resting", true);
    let report = workflow::run(&client, &flow, &options(&cluster, "resting"))
        .await
        .unwrap();
    succeeded(&report);
    let gens = &report.nodes["gens"];
    assert_eq!(gens.claims[0].id, "value");
    assert_eq!(gens.claims[0].effective_level, EvidenceLevel::Asserted);
    let member = &report.nodes["member"];
    let claim = &member.claims[0];
    assert_eq!(claim.level, EvidenceLevel::Checked);
    assert_eq!(
        claim.depends_on,
        vec![job_claim(gens.job_id.as_ref().unwrap(), "value")]
    );
    assert_eq!(claim.effective_level, EvidenceLevel::Asserted);
    assert_eq!(
        member.summary,
        Some(algebramesh_core::guarantee::Summary::Unverified)
    );
}

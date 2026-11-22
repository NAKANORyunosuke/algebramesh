//! References to values of committed jobs' results in a job's arguments
//! (W2, ADR 0032): a marker with its origin `from`, and `{"$ref": ...}`. The
//! scheduler checks both against the producer's record before it creates the
//! job (Issue #105); the agent reads a `$ref` from the producer's record with
//! `AgentGateway.GetRecord` (Issue #107). Without the feature
//! `workflow_refs`, the scheduler refuses both and does not answer
//! `GetRecord`.

#![allow(clippy::unwrap_used)]

mod common;

use algebramesh_core::blob::{self, OutputRef, RecordRef};
use algebramesh_rpc::pb::agent_gateway_client::AgentGatewayClient;
use algebramesh_rpc::{TokenInterceptor, client_tls, pb};
use common::*;
use serde_json::json;
use tonic::Code;
use tonic::service::interceptor::InterceptedService;
use tonic::transport::{Channel, Endpoint};

type Gateway = AgentGatewayClient<InterceptedService<Channel, TokenInterceptor>>;

/// An agent gateway client of scheduler `i`, as an agent calls it.
async fn gateway(cluster: &Cluster, i: usize) -> Gateway {
    let endpoint = Endpoint::from_shared(url(cluster.addrs[i]))
        .unwrap()
        .tls_config(client_tls(cluster.pki.ca_cert_pem.as_bytes(), "localhost"))
        .unwrap();
    let channel = algebramesh_rpc::connect(&endpoint, Duration::from_secs(5))
        .await
        .unwrap();
    AgentGatewayClient::with_interceptor(channel, TokenInterceptor::new(AGENT_TOKEN).unwrap())
}

fn echo(arguments: serde_json::Value) -> String {
    json!({
        "schema_version": 1, "backend": "test", "operation": "test.sleep",
        "arguments": arguments,
    })
    .to_string()
}

/// Submits `spec` and waits for its record.
async fn run(client: &Client, spec: &str) -> algebramesh_core::JobRecord {
    let status = client.submit_json(spec, 0).await.unwrap();
    client
        .wait(&status.job_id, Some(Duration::from_secs(30)), false)
        .await
        .unwrap()
}

async fn one_scheduler(features: Option<Vec<String>>) -> (Cluster, Scheduler) {
    let (mut cluster, mut listeners) = Cluster::new(1);
    if let Some(features) = features {
        cluster.features.insert(0, features);
    }
    let scheduler = cluster.start(0, listeners.remove(0)).await;
    serving(&[Some(&scheduler)]).await;
    let wanted = cluster
        .features
        .get(&0)
        .cloned()
        .unwrap_or_else(algebramesh_server::cluster::supported_features);
    eventually("the scheduler has enabled its features", || {
        let enabled = scheduler.node.status().enabled_features;
        wanted.iter().all(|f| enabled.contains(f))
    })
    .await;
    (cluster, scheduler)
}

fn code(e: ClientError) -> Code {
    match e {
        ClientError::Rpc { code, .. } => code,
        e => panic!("{e}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn references_are_checked_against_their_producers() {
    let (cluster, _scheduler) = one_scheduler(None).await;
    let client = cluster.client().await;
    let _agent = cluster.agent().await;
    let value = json!([1, 2]);
    let producer = run(&client, &echo(json!({"a": value, "b": "x"}))).await;
    assert_eq!(producer.state, JobState::Succeeded);
    let job_id = producer.job_id.to_string();

    // A small value by reference, a large one as a blob that says where it
    // came from: the backend gets the values themselves.
    let id = blob::canonical_ref(&value).id;
    let reference = |pointer: &str, sha256: &blob::BlobId| {
        RecordRef {
            job_id: job_id.clone(),
            pointer: pointer.into(),
            sha256: sha256.clone(),
        }
        .marker()
    };
    let stored = client.put_value(&value).await.unwrap();
    let origin = OutputRef {
        job_id: job_id.clone(),
        pointer: "/a".into(),
    };
    let whole = json!({"a": value, "b": "x"});
    let consumer = run(
        &client,
        &echo(json!({
            "v": reference("/a", &id),
            "w": blob::marker_from(&stored, &origin),
            "whole": reference("", &blob::canonical_ref(&whole).id),
        })),
    )
    .await;
    assert_eq!(consumer.state, JobState::Succeeded, "{:?}", consumer.error);
    assert_eq!(
        consumer.result.unwrap(),
        json!({"v": value, "w": value, "whole": whole})
    );

    // References that disagree with the producer's result create no job.
    let other = blob::canonical_ref(&json!([9])).id;
    let wrong_value = client.put_value(&json!([9])).await.unwrap();
    let queued = client
        .submit_json(
            &json!({"schema_version": 1, "backend": "none", "operation": "x.y", "arguments": {}})
                .to_string(),
            0,
        )
        .await
        .unwrap();
    let cancelled = client.cancel(&queued.job_id).await.unwrap();
    assert_eq!(cancelled.state, "CANCELLED");
    let unknown = RecordRef {
        job_id: uuid::Uuid::new_v4().to_string(),
        pointer: "/a".into(),
        sha256: id.clone(),
    };
    let not_succeeded = RecordRef {
        job_id: queued.job_id.clone(),
        pointer: String::new(),
        sha256: id.clone(),
    };
    let jobs = || async {
        client
            .list_jobs(&Default::default())
            .await
            .unwrap()
            .jobs
            .len()
    };
    let before = jobs().await;
    for bad in [
        reference("/a", &other),
        reference("/b", &id),
        reference("/missing", &id),
        blob::marker_from(&wrong_value, &origin),
        unknown.marker(),
        not_succeeded.marker(),
    ] {
        let e = client
            .submit_json(&echo(json!({"v": bad})), 0)
            .await
            .unwrap_err();
        assert_eq!(code(e), Code::FailedPrecondition, "{bad}");
    }
    // A pointer below the top level is no reference at all.
    let deep = json!({"$ref": {"job_id": job_id, "pointer": "/a/0", "sha256": id}});
    let e = client
        .submit_json(&echo(json!({"v": deep})), 0)
        .await
        .unwrap_err();
    assert_eq!(code(e), Code::InvalidArgument);
    assert_eq!(before, jobs().await, "a refused reference created a job");

    // An agent reads committed records, with their markers as they are.
    let mut agent = gateway(&cluster, 0).await;
    let got = agent
        .get_record(pb::RecordRequest {
            job_id: job_id.clone(),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(got.status.unwrap().state, "SUCCEEDED");
    let record: algebramesh_core::JobRecord = serde_json::from_str(&got.record_json).unwrap();
    assert_eq!(record.result.unwrap(), whole);
    let e = agent
        .get_record(pb::RecordRequest {
            job_id: uuid::Uuid::new_v4().to_string(),
        })
        .await
        .unwrap_err();
    assert_eq!(e.code(), Code::NotFound);
}

/// A scheduler without the feature (an older build) refuses references and
/// does not answer `GetRecord`; a plain job runs as before.
#[tokio::test(flavor = "multi_thread")]
async fn without_the_feature_no_reference_is_taken() {
    let features = algebramesh_server::cluster::supported_features()
        .into_iter()
        .filter(|f| f != algebramesh_core::features::WORKFLOW_REFS)
        .collect();
    let (cluster, _scheduler) = one_scheduler(Some(features)).await;
    let client = cluster.client().await;
    let _agent = cluster.agent().await;
    let producer = run(&client, &echo(json!({"a": 1}))).await;
    assert_eq!(producer.state, JobState::Succeeded);
    let reference = RecordRef {
        job_id: producer.job_id.to_string(),
        pointer: "/a".into(),
        sha256: blob::canonical_ref(&json!(1)).id,
    };
    let e = client
        .submit_json(&echo(json!({"v": reference.marker()})), 0)
        .await
        .unwrap_err();
    assert_eq!(code(e), Code::FailedPrecondition);
    let stored = client.put_value(&json!(1)).await.unwrap();
    let from = blob::marker_from(&stored, &reference.output());
    let e = client
        .submit_json(&echo(json!({"v": from})), 0)
        .await
        .unwrap_err();
    assert_eq!(code(e), Code::FailedPrecondition);
    let e = gateway(&cluster, 0)
        .await
        .get_record(pb::RecordRequest {
            job_id: producer.job_id.to_string(),
        })
        .await
        .unwrap_err();
    assert_eq!(e.code(), Code::FailedPrecondition);
}

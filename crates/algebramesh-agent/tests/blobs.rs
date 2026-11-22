//! The blob store of the schedulers (ADR 0021, Issue #71): a blob is held by
//! a majority before `PutBlob` answers and is read back after a change of
//! leader; a result refers to it by a marker, which an old client gets
//! expanded; a damaged body is fetched again; a missing one is reported as
//! such; unreferenced bodies are collected. An agent moves a large result to
//! blobs, and a client reads it back whole (Issue #72). A client token
//! uploads at most its allowance; a cluster restored from a backup taken as
//! `docs/install.md` says keeps its jobs and blobs (Issue #96).

#![allow(clippy::unwrap_used)]

mod common;

use algebramesh_core::blob::{BlobId, BlobRef};
use algebramesh_rpc::pb::scheduler_client::SchedulerClient;
use algebramesh_rpc::{MAX_MESSAGE_BYTES, TokenInterceptor, client_tls, pb};
use common::*;
use tonic::Code;
use tonic::service::interceptor::InterceptedService;
use tonic::transport::{Channel, Endpoint};

type Raw = SchedulerClient<InterceptedService<Channel, TokenInterceptor>>;

/// A client of scheduler `i` alone, without the failover of `Client`.
async fn raw(cluster: &Cluster, i: usize) -> Raw {
    let endpoint = Endpoint::from_shared(url(cluster.addrs[i]))
        .unwrap()
        .tls_config(client_tls(cluster.pki.ca_cert_pem.as_bytes(), "localhost"))
        .unwrap();
    let channel = algebramesh_rpc::connect(&endpoint, Duration::from_secs(5))
        .await
        .unwrap();
    SchedulerClient::with_interceptor(channel, TokenInterceptor::new(CLIENT_TOKEN).unwrap())
        .max_decoding_message_size(MAX_MESSAGE_BYTES)
        .max_encoding_message_size(MAX_MESSAGE_BYTES)
}

/// A JSON array of about `bytes` bytes, and its canonical JSON: the blob an
/// agent stores for it.
fn big_value(tag: &str, bytes: usize) -> (serde_json::Value, Vec<u8>) {
    let rows: Vec<String> = (0..bytes / 64)
        .map(|i| format!("{tag}-{i:08}-{}", "x".repeat(40)))
        .collect();
    let value = serde_json::json!(rows);
    let body = algebramesh_core::canonical_json(&value).into_bytes();
    (value, body)
}

/// Runs a job of [`LargeBackend`] whose result has `rows` of about `kib` KiB
/// made from `tag`, and returns it with the blob it refers to.
async fn run_large(cluster: &Cluster, jobs: &Client, kib: u64, tag: &str) -> (String, BlobRef) {
    let spec = serde_json::json!({
        "schema_version": 1, "backend": "test", "operation": "test.sleep",
        "arguments": {"kib": kib, "tag": tag},
    })
    .to_string();
    let job = jobs.submit_json(&spec, 0).await.unwrap().job_id;
    let record = jobs
        .wait_refs(&job, Some(Duration::from_secs(120 + kib / 512)), false)
        .await
        .unwrap();
    assert_eq!(record.state, JobState::Succeeded, "{:?}", record.error);
    let result = record.result.unwrap();
    let r = algebramesh_core::blob::BlobRef::from_marker(&result["rows"])
        .unwrap()
        .unwrap_or_else(|| panic!("rows are not in a blob: {result}"));
    let _ = cluster;
    (job, r)
}

async fn put(client: &mut Raw, body: &[u8]) -> Result<BlobRef, tonic::Status> {
    let id = BlobId::of(body);
    let pieces: Vec<pb::BlobUpload> = body
        .chunks(1 << 20)
        .enumerate()
        .map(|(i, data)| pb::BlobUpload {
            id: if i == 0 {
                id.to_string()
            } else {
                String::new()
            },
            size: if i == 0 { body.len() as u64 } else { 0 },
            data: data.to_vec(),
        })
        .collect();
    let got = client
        .put_blob(tokio_stream::iter(pieces))
        .await?
        .into_inner();
    assert_eq!(got.id, id.to_string());
    assert_eq!(got.size, body.len() as u64);
    Ok(BlobRef {
        id,
        size: body.len() as u64,
    })
}

/// Puts `body` on the leader, trying again while there is none.
async fn put_on_leader(cluster: &Cluster, leader: usize, body: &[u8]) -> BlobRef {
    let mut client = raw(cluster, leader).await;
    for _ in 0..50 {
        match put(&mut client, body).await {
            Ok(r) => return r,
            Err(e) if e.code() == Code::Unavailable => {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            Err(e) => panic!("{e}"),
        }
    }
    panic!("the leader did not take the blob");
}

async fn get(client: &mut Raw, id: &BlobId, offset: u64) -> Result<Vec<u8>, tonic::Status> {
    let mut stream = client
        .get_blob(pb::BlobRequest {
            id: id.to_string(),
            offset,
        })
        .await?
        .into_inner();
    let mut out = Vec::new();
    while let Some(chunk) = stream.message().await? {
        out.extend(chunk.data);
    }
    Ok(out)
}

/// Runs a job whose result is `arguments` (the echo backend returns them).
async fn run_echo(client: &Client, arguments: serde_json::Value) -> String {
    let spec = serde_json::json!({
        "schema_version": 1, "backend": "test", "operation": "test.sleep",
        "arguments": arguments,
    })
    .to_string();
    let key = uuid::Uuid::new_v4().to_string();
    let mut tries = 0;
    let id = loop {
        match client.submit_json_with_request_id(&spec, 0, &key).await {
            Ok(status) => break status.job_id,
            Err(e) if tries < 20 => {
                tries += 1;
                tracing::debug!("submit again: {e}");
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            Err(e) => panic!("{e}"),
        }
    };
    let _ = client.wait(&id, Some(Duration::from_secs(60)), false).await;
    id
}

/// The record of `job_id` as stored, with its markers.
async fn stored_record(client: &mut Raw, job_id: &str) -> String {
    client
        .result(pb::JobRef {
            job_id: job_id.into(),
            accept_blob_refs: true,
        })
        .await
        .unwrap()
        .into_inner()
        .record_json
}

async fn started(n: usize) -> (Cluster, Vec<Option<Scheduler>>, usize) {
    let (cluster, listeners) = Cluster::new(n);
    let mut schedulers = Vec::new();
    for (i, l) in listeners.into_iter().enumerate() {
        schedulers.push(Some(cluster.start(i, l).await));
    }
    features_agreed(&schedulers).await;
    let leader = serving(&all(&schedulers)).await;
    (cluster, schedulers, leader)
}

fn node(schedulers: &[Option<Scheduler>], i: usize) -> &Arc<Node> {
    &schedulers[i].as_ref().unwrap().node
}

fn holders(schedulers: &[Option<Scheduler>], id: &BlobId) -> usize {
    schedulers
        .iter()
        .flatten()
        .filter(|s| s.node.blob_disk().has(id))
        .count()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_blob_is_held_by_a_majority_and_read_after_a_change_of_leader() {
    let (cluster, mut schedulers, leader) = started(3).await;
    let follower = (leader + 1) % 3;
    let (value, body) = big_value("a", 6 << 20);

    // A follower takes no blob.
    let refused = put(&mut raw(&cluster, follower).await, &body)
        .await
        .unwrap_err();
    assert_eq!(refused.code(), Code::Unavailable, "{refused}");

    let r = put_on_leader(&cluster, leader, &body).await;
    assert!(
        holders(&schedulers, &r.id) >= 2,
        "PutBlob answered before a majority held the blob"
    );
    eventually("every scheduler holds the blob", || {
        holders(&schedulers, &r.id) == 3
    })
    .await;
    let mut client = raw(&cluster, leader).await;
    let stat = client
        .stat_blob(pb::BlobRequest {
            id: r.id.to_string(),
            offset: 0,
        })
        .await
        .unwrap()
        .into_inner();
    assert!(
        stat.present && stat.durable && stat.size == r.size,
        "{stat:?}"
    );

    // A result whose value is that blob refers to it by a marker; the agent
    // finds it stored already.
    let jobs = cluster.client().await;
    let _agent = cluster.agent_with(Arc::new(LargeBackend)).await;
    let (job, in_result) = run_large(&cluster, &jobs, 6 << 10, "a").await;
    assert_eq!(in_result, r);
    let stored = stored_record(&mut client, &job).await;
    assert!(stored.contains("\"$blob\""), "{stored}");
    // What Raft carries stays small, whatever the size of the blob.
    assert!(stored.len() < 64 * 1024, "{} bytes", stored.len());
    // A client that does not resolve markers gets the value.
    let (_, record) = jobs.result(&job).await.unwrap();
    let record = record.unwrap();
    assert_eq!(record.state, JobState::Succeeded, "{:?}", record.error);
    let result = record.result.unwrap();
    assert_eq!(result["rows"], value);
    assert_eq!(result["n"], 1);

    // The leader stops; the new one serves the blob and the result.
    schedulers[leader].take().unwrap().stop().await;
    let next = serving(&all(&schedulers)).await;
    let mut client = raw(&cluster, next).await;
    assert_eq!(get(&mut client, &r.id, 0).await.unwrap(), body);
    let half = body.len() as u64 / 2;
    assert_eq!(
        get(&mut client, &r.id, half).await.unwrap(),
        body[half as usize..]
    );
    let (_, record) = jobs.result(&job).await.unwrap();
    assert_eq!(record.unwrap().result.unwrap()["rows"], value);
    // A follower that holds the body serves it from its disk.
    let other = (0..3).find(|i| *i != leader && *i != next).unwrap();
    let mut on_follower = raw(&cluster, other).await;
    assert_eq!(get(&mut on_follower, &r.id, 0).await.unwrap(), body);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_damaged_or_missing_body_is_fetched_again_or_reported() {
    let (cluster, schedulers, leader) = started(3).await;
    let follower = (leader + 1) % 3;
    let (_, body) = big_value("b", 1 << 20);
    let r = put_on_leader(&cluster, leader, &body).await;
    eventually("every scheduler holds the blob", || {
        holders(&schedulers, &r.id) == 3
    })
    .await;
    let mut client = raw(&cluster, leader).await;

    // A damaged body on the leader goes to quarantine and is fetched again.
    let disk = node(&schedulers, leader).blob_disk();
    std::fs::write(disk.path(&r.id), b"damaged").unwrap();
    assert_eq!(get(&mut client, &r.id, 0).await.unwrap(), body);
    assert_eq!(disk.quarantined(), 1);
    assert!(disk.verify(&r.id).unwrap());

    // A follower without the body says so rather than that it is unknown.
    let follower_disk = node(&schedulers, follower).blob_disk();
    std::fs::remove_file(follower_disk.path(&r.id)).unwrap();
    let mut on_follower = raw(&cluster, follower).await;
    let e = get(&mut on_follower, &r.id, 0).await.unwrap_err();
    assert_eq!(e.code(), Code::Unavailable, "{e}");

    // An id nobody knows.
    let unknown = BlobId::of(b"never stored");
    let e = get(&mut client, &unknown, 0).await.unwrap_err();
    assert_eq!(e.code(), Code::NotFound, "{e}");
    // A record cannot refer to a blob nobody holds.
    let missing = BlobRef {
        id: unknown,
        size: 12,
    };
    let e = node(&schedulers, leader)
        .ensure_blobs(&[missing])
        .await
        .unwrap_err();
    assert_eq!(e.code(), Code::FailedPrecondition, "{e}");

    // A blob a job refers to, lost from every scheduler: DATA_LOSS, not
    // another value.
    let jobs = cluster.client().await;
    let _agent = cluster.agent_with(Arc::new(LargeBackend)).await;
    let (job, in_result) = run_large(&cluster, &jobs, 1 << 10, "b").await;
    assert_eq!(in_result, r);
    // The follower fetches the body it lost; it is removed from all once no
    // transfer is left that would bring it back.
    eventually("every scheduler holds the blob, none receiving", || {
        holders(&schedulers, &r.id) == 3
            && schedulers
                .iter()
                .flatten()
                .all(|s| s.node.blob_disk().parts() == 0)
    })
    .await;
    for s in schedulers.iter().flatten() {
        let _ = std::fs::remove_file(s.node.blob_disk().path(&r.id));
    }
    let e = get(&mut client, &r.id, 0).await.unwrap_err();
    assert_eq!(e.code(), Code::DataLoss, "{e}");
    let e = jobs.result(&job).await.unwrap_err();
    assert!(
        e.to_string().contains("DataLoss") || e.to_string().contains("no scheduler holds"),
        "{e}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn unreferenced_blobs_are_collected() {
    let (cluster, schedulers, leader) = started(3).await;
    let (_, kept) = big_value("kept", 300 << 10);
    let (_, dropped) = big_value("dropped", 300 << 10);
    let (_, recent) = big_value("recent", 300 << 10);
    let kept = put_on_leader(&cluster, leader, &kept).await;
    let dropped = put_on_leader(&cluster, leader, &dropped).await;
    let recent = put_on_leader(&cluster, leader, &recent).await;
    for r in [&kept, &dropped, &recent] {
        eventually("every scheduler holds the blob", || {
            holders(&schedulers, &r.id) == 3
        })
        .await;
    }
    let jobs = cluster.client().await;
    let _agent = cluster.agent_with(Arc::new(LargeBackend)).await;
    let (_, in_result) = run_large(&cluster, &jobs, 300, "kept").await;
    assert_eq!(in_result, kept);

    // Two of them untouched for longer than the grace.
    let long_ago = std::time::SystemTime::now() - Duration::from_secs(48 * 3600);
    for s in schedulers.iter().flatten() {
        for r in [&kept, &dropped] {
            let file = std::fs::File::options()
                .write(true)
                .open(s.node.blob_disk().path(&r.id))
                .unwrap();
            file.set_modified(long_ago).unwrap();
        }
    }
    let mut removed = 0;
    for s in schedulers.iter().flatten() {
        removed += s.node.collect_blobs().await.unwrap();
    }
    assert_eq!(removed, 3);
    assert_eq!(holders(&schedulers, &dropped.id), 0);
    assert_eq!(holders(&schedulers, &kept.id), 3);
    assert_eq!(holders(&schedulers, &recent.id), 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn without_the_feature_no_blob_is_taken_or_recorded() {
    let (mut cluster, listeners) = Cluster::new(3);
    // One scheduler of an older build.
    cluster.features.insert(
        2,
        algebramesh_server::cluster::supported_features()
            .into_iter()
            .filter(|f| f != algebramesh_core::features::BLOB_REFS)
            .collect(),
    );
    let mut schedulers = Vec::new();
    for (i, l) in listeners.into_iter().enumerate() {
        schedulers.push(Some(cluster.start(i, l).await));
    }
    let leader = serving(&all(&schedulers)).await;
    let mut client = raw(&cluster, leader).await;
    let (_, body) = big_value("c", 1 << 20);
    let e = put(&mut client, &body).await.unwrap_err();
    assert_eq!(e.code(), Code::FailedPrecondition, "{e}");

    // A result with a marker is not recorded as one.
    let jobs = cluster.client().await;
    let _agent = cluster.agent().await;
    let r = BlobRef {
        id: BlobId::of(&body),
        size: body.len() as u64,
    };
    let job = run_echo(&jobs, r.marker()).await;
    let (_, record) = jobs.result(&job).await.unwrap();
    let record = record.unwrap();
    assert_eq!(record.state, JobState::Failed);
    assert!(
        matches!(&record.error, Some(CasError::UnsupportedResult(m)) if m.contains("blob")),
        "{:?}",
        record.error
    );
}

/// Returns `{"rows": [...], "n": 1}` with rows of about `arguments.kib` KiB,
/// built from `arguments.tag`.
struct LargeBackend;

#[async_trait::async_trait]
impl CasBackend for LargeBackend {
    fn name(&self) -> &'static str {
        "test"
    }

    fn capabilities(&self) -> Capabilities {
        EchoBackend.capabilities()
    }

    async fn execute(&self, job: CasRequest, ctx: ExecutionContext) -> Result<CasResult, CasError> {
        let kib = job.arguments["kib"].as_u64().unwrap_or(1) as usize;
        let tag = job.arguments["tag"].as_str().unwrap_or("t").to_owned();
        let (rows, _) = big_value(&tag, kib << 10);
        let mut result = EchoBackend.execute(job, ctx).await?;
        result.value = serde_json::json!({"rows": rows, "n": 1});
        Ok(result)
    }
}

/// A result of `mib` MiB goes through the agent, the blob store and the
/// client, in a cluster of `n` schedulers.
async fn a_large_result_goes_through(n: usize, mib: u64) {
    let (cluster, schedulers, leader) = started(n).await;
    let jobs = cluster.client().await;
    let _agent = cluster.agent_with(Arc::new(LargeBackend)).await;
    let spec = serde_json::json!({
        "schema_version": 1, "backend": "test", "operation": "test.sleep",
        "arguments": {"kib": mib << 10, "tag": "large"},
    })
    .to_string();
    let job = jobs.submit_json(&spec, 0).await.unwrap().job_id;
    let budget = Duration::from_secs(120 + mib * 2);
    let record = jobs.wait(&job, Some(budget), false).await.unwrap();
    assert_eq!(record.state, JobState::Succeeded, "{:?}", record.error);
    let (expected, body) = big_value("large", (mib as usize) << 20);
    let value = record.result.unwrap();
    assert_eq!(value["rows"], expected);
    assert_eq!(value["n"], 1);

    // What Raft carries is the record with a marker.
    let mut client = raw(&cluster, leader).await;
    let stored = stored_record(&mut client, &job).await;
    assert!(stored.len() < 64 * 1024, "{} bytes", stored.len());
    let refs: algebramesh_core::JobRecord = serde_json::from_str(&stored).unwrap();
    let markers = algebramesh_core::blob::markers(refs.result.as_ref().unwrap()).unwrap();
    assert_eq!(markers.len(), 1, "{stored}");
    let (_, r) = &markers[0];
    assert!(holders(&schedulers, &r.id) * 2 > n);
    // The blob is the canonical JSON of the value under `rows`.
    assert_eq!(r.id, BlobId::of(&body));
    drop(body);

    // `download_blob` writes it to a file, checked.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rows.json");
    jobs.download_blob(r, &path).await.unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().len(), r.size);
    // A client of the release before the blob store gets the value expanded.
    let old = client
        .result(pb::JobRef {
            job_id: job.clone(),
            accept_blob_refs: false,
        })
        .await;
    match old {
        Ok(answer) => {
            let record: algebramesh_core::JobRecord =
                serde_json::from_str(&answer.into_inner().record_json).unwrap();
            assert_eq!(record.result.unwrap()["rows"], expected);
        }
        // Beyond 96 MiB it is told to read the blob.
        Err(e) => {
            assert!(mib >= 96, "{e}");
            assert_eq!(e.code(), Code::FailedPrecondition, "{e}");
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_moves_a_large_result_to_blobs() {
    a_large_result_goes_through(3, 24).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn one_scheduler_takes_a_large_result() {
    a_large_result_goes_through(1, 12).await;
}

/// The acceptance case of #36: 200 MiB. Slow in a debug build; run with
/// `cargo test --release -p algebramesh-agent --test blobs -- --ignored`.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn a_result_of_200_mib_goes_through() {
    a_large_result_goes_through(3, 200).await;
}

/// Issue #73: a follower stopped while a blob was pushed gets it after it
/// starts again, from the repair, without any call.
#[tokio::test(flavor = "multi_thread")]
async fn a_follower_that_missed_a_push_gets_the_blob_after_a_restart() {
    let (cluster, mut schedulers, leader) = started(3).await;
    let follower = (leader + 1) % 3;
    schedulers[follower].take().unwrap().stop().await;
    let jobs = cluster.client().await;
    let _agent = cluster.agent_with(Arc::new(LargeBackend)).await;
    let (_, r) = run_large(&cluster, &jobs, 300, "missed").await;
    assert_eq!(holders(&schedulers, &r.id), 2);

    schedulers[follower] = Some(cluster.restart(follower).await);
    eventually("the restarted follower holds the blob", || {
        node(&schedulers, follower).blob_disk().has(&r.id)
    })
    .await;
    eventually("the leader sees no blob under-replicated", || {
        node(&schedulers, follower).status().blobs_missing == 0
    })
    .await;
}

/// Issue #73: a member is not removed while a blob would then be held by
/// fewer than a majority of the voters that remain.
#[tokio::test(flavor = "multi_thread")]
async fn a_removal_that_leaves_a_blob_short_is_refused() {
    let (cluster, mut schedulers, leader) = started(3).await;
    let holder = (leader + 1) % 3;
    let absent = (leader + 2) % 3;
    schedulers[absent].take().unwrap().stop().await;
    let jobs = cluster.client().await;
    let _agent = cluster.agent_with(Arc::new(LargeBackend)).await;
    let (_, r) = run_large(&cluster, &jobs, 300, "short").await;
    assert!(node(&schedulers, holder).blob_disk().has(&r.id));

    // Without `holder`, the voters left are the leader and `absent`, which
    // lacks the blob.
    let e = node(&schedulers, leader)
        .remove_member(holder as u64 + 1)
        .await
        .unwrap_err();
    assert_eq!(e.code(), Code::FailedPrecondition, "{e}");
    assert!(e.message().contains("blobs"), "{e}");

    schedulers[absent] = Some(cluster.restart(absent).await);
    eventually("the returning member holds the blob", || {
        node(&schedulers, absent).blob_disk().has(&r.id)
    })
    .await;
    let members = node(&schedulers, leader)
        .remove_member(holder as u64 + 1)
        .await
        .unwrap();
    assert!(!members.members.iter().any(|m| m.id == holder as u64 + 1));
}

/// Issue #73 (Q24): a member added after the log was compacted stays a
/// learner while it lacks blobs records refer to; once it holds them, the
/// same call makes it a voter.
#[tokio::test(flavor = "multi_thread")]
async fn a_new_member_is_a_learner_until_it_holds_the_blobs() {
    let (cluster, mut listeners) = Cluster::with_initial(4, 3);
    let joining = listeners.pop().unwrap();
    let mut schedulers = Vec::new();
    for (i, l) in listeners.into_iter().enumerate() {
        schedulers.push(Some(cluster.start(i, l).await));
    }
    features_agreed(&schedulers).await;
    let leader = serving(&all(&schedulers)).await;
    let jobs = cluster.client().await;
    let _agent = cluster.agent_with(Arc::new(LargeBackend)).await;
    let (_, r) = run_large(&cluster, &jobs, 300, "joining").await;
    let leading = node(&schedulers, leader).clone();
    leading.compact_log().await.unwrap();

    let fourth = cluster.start(3, joining).await;
    let first = leading.add_member(4, cluster.member(3)).await.unwrap();
    let added = first.members.iter().find(|m| m.id == 4).unwrap();
    assert!(!added.voter, "{first:?}");
    eventually("the new member holds the blob", || {
        fourth.node.blob_disk().has(&r.id)
    })
    .await;
    let second = leading.add_member(4, cluster.member(3)).await.unwrap();
    assert!(
        second.members.iter().any(|m| m.id == 4 && m.voter),
        "{second:?}"
    );
    fourth.stop().await;
}

// ------------------------------------------------------------ faults (#74)

use algebramesh_server::fault;

/// The transfers a scheduler has begun to receive and not finished.
fn received_parts(cluster: &Cluster, i: usize) -> Vec<PathBuf> {
    std::fs::read_dir(cluster.data_dir(i).join("blobs").join("incoming"))
        .map(|d| d.flatten().map(|e| e.path()).collect())
        .unwrap_or_default()
}

/// Issue #74: a follower that crashes after a blob was received and synced,
/// before it was put in place, keeps no partial file and fetches the blob
/// again after it starts.
#[tokio::test(flavor = "multi_thread")]
async fn a_crash_before_a_blob_is_put_in_place_leaves_nothing_half_done() {
    let (cluster, mut schedulers, leader) = started(3).await;
    let victim = (leader + 1) % 3;
    let dir = cluster.data_dir(victim);
    // Every attempt crashes, the fetches of the repair included, until the
    // follower is stopped.
    for _ in 0..1000 {
        fault::arm(&dir, fault::BLOB_RENAME);
    }
    let jobs = cluster.client().await;
    let _agent = cluster.agent_with(Arc::new(LargeBackend)).await;
    let (_, r) = run_large(&cluster, &jobs, 300, "rename").await;
    eventually("the crash point is reached", || {
        fault::hits(&dir).contains(&fault::BLOB_RENAME)
    })
    .await;
    assert!(!node(&schedulers, victim).blob_disk().has(&r.id));
    schedulers[victim].take().unwrap().stop().await;
    fault::disarm(&dir);
    let left = received_parts(&cluster, victim);
    assert!(!left.is_empty(), "the crash left the received file");

    schedulers[victim] = Some(cluster.restart(victim).await);
    for part in &left {
        assert!(!part.exists(), "{} is left", part.display());
    }
    eventually("the restarted follower holds the blob", || {
        node(&schedulers, victim).blob_disk().has(&r.id)
    })
    .await;
    assert!(node(&schedulers, victim).blob_disk().verify(&r.id).unwrap());
}

/// Issue #74: the leader crashes while it sends a blob to the others (after
/// a push, or once a majority holds it, before `PutBlob` answers). The job
/// is either `LOST` (the next leader cannot know its outcome) or
/// `SUCCEEDED` with its blob on a majority; never `SUCCEEDED` with a blob
/// that cannot be read. The cluster then takes large results again.
async fn a_leader_crash_while_a_blob_is_replicated(point: &'static str) {
    let (cluster, mut schedulers, leader) = started(3).await;
    let dir = cluster.data_dir(leader);
    fault::arm(&dir, point);
    let jobs = cluster.client().await;
    let _agent = cluster.agent_with(Arc::new(LargeBackend)).await;
    let spec = serde_json::json!({
        "schema_version": 1, "backend": "test", "operation": "test.sleep",
        "arguments": {"kib": 2048, "tag": point},
    })
    .to_string();
    let job = jobs
        .submit_json_with_request_id(&spec, 0, point)
        .await
        .unwrap()
        .job_id;
    eventually("the crash point is reached", || {
        fault::hits(&dir).contains(&point)
    })
    .await;
    schedulers[leader].take().unwrap().stop().await;
    fault::disarm(&dir);
    serving(&all(&schedulers)).await;

    let record = jobs
        .wait_refs(&job, Some(Duration::from_secs(240)), false)
        .await
        .unwrap();
    match record.state {
        JobState::Lost => {}
        JobState::Succeeded => {
            let r = BlobRef::from_marker(&record.result.unwrap()["rows"])
                .unwrap()
                .unwrap();
            assert!(holders(&schedulers, &r.id) >= 1, "{point}");
            let (expected, _) = big_value(point, 2048 << 10);
            let whole = jobs.wait(&job, None, false).await.unwrap();
            assert_eq!(whole.result.unwrap()["rows"], expected);
        }
        other => panic!("{point}: {other:?} {:?}", record.error),
    }
    let (_, r) = run_large(&cluster, &jobs, 2048, "after").await;
    assert_eq!(holders(&schedulers, &r.id), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_leader_crash_after_a_push() {
    a_leader_crash_while_a_blob_is_replicated(fault::BLOB_PUSHED_ONE).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_leader_crash_before_put_blob_answers() {
    a_leader_crash_while_a_blob_is_replicated(fault::BLOB_DURABLE).await;
}

/// Issue #74: a blob only the leader holds may not be referred to; the
/// leader sends it to the others, and the same check passes once a majority
/// holds it.
#[tokio::test(flavor = "multi_thread")]
async fn a_blob_held_by_a_minority_waits_for_its_copies() {
    let (cluster, mut schedulers, leader) = started(3).await;
    let holder = (leader + 1) % 3;
    let absent = (leader + 2) % 3;
    schedulers[absent].take().unwrap().stop().await;
    let (_, body) = big_value("minority", 1 << 20);
    let r = put_on_leader(&cluster, leader, &body).await;
    std::fs::remove_file(node(&schedulers, holder).blob_disk().path(&r.id)).unwrap();

    let leading = node(&schedulers, leader).clone();
    let e = leading
        .ensure_blobs(std::slice::from_ref(&r))
        .await
        .unwrap_err();
    assert_eq!(e.code(), Code::Unavailable, "{e}");
    eventually("the other voter holds the blob again", || {
        node(&schedulers, holder).blob_disk().has(&r.id)
    })
    .await;
    leading.ensure_blobs(&[r]).await.unwrap();
}

/// Issue #74: a client whose connection is cut while it reads a blob takes
/// the transfer up where it stopped and checks the whole.
#[tokio::test(flavor = "multi_thread")]
async fn a_blob_read_through_cuts_arrives_whole() {
    let (cluster, schedulers, leader) = started(3).await;
    let (_, body) = big_value("cuts", 6 << 20);
    let r = put_on_leader(&cluster, leader, &body).await;
    eventually("every scheduler holds the blob", || {
        holders(&schedulers, &r.id) == 3
    })
    .await;
    // Every scheduler holds it, so the one behind the proxy answers.
    let proxy = Proxy::start(cluster.addrs[0]).await;
    // Two pieces of 1 MiB pass before each cut.
    proxy
        .cut_answers_after
        .store(5 << 19, std::sync::atomic::Ordering::SeqCst);
    proxy
        .cuts_left
        .store(2, std::sync::atomic::Ordering::SeqCst);
    let through = Client::connect(ClientConfig {
        url: url(proxy.addr),
        ca_pem: cluster.pki.ca_cert_pem.clone().into_bytes(),
        domain: None,
        token: CLIENT_TOKEN.into(),
    })
    .await
    .unwrap();
    let read = through.blob(&r).await.unwrap();
    assert!(proxy.cuts.load(std::sync::atomic::Ordering::SeqCst) >= 1);
    assert_eq!(read, body);
}

// ------------------------------------------------- a full blob disk (#74)

/// A directory on the small file system of `run-disk-full.sh`.
fn small_dir(name: &str) -> PathBuf {
    let root = std::env::var("ALGEBRAMESH_TEST_SMALL_DIR")
        .expect("ALGEBRAMESH_TEST_SMALL_DIR names a small file system");
    let dir = PathBuf::from(root).join(format!("{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Issue #74: a scheduler whose volume cannot take a blob (it would leave
/// less than `blob_min_free_bytes` free) refuses it. As a follower, it stays
/// a member without the blob while the other two hold it; as the leader, the
/// job ends with `RESOURCE_LIMIT`, and the cluster goes on.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs ALGEBRAMESH_TEST_SMALL_DIR; run by tests/integration/run-disk-full.sh"]
async fn disk_full_for_blobs() {
    let (mut cluster, listeners) = Cluster::new(3);
    cluster.data_dirs.insert(2, small_dir("blobs"));
    let mut schedulers = Vec::new();
    for (i, l) in listeners.into_iter().enumerate() {
        schedulers.push(Some(cluster.start(i, l).await));
    }
    features_agreed(&schedulers).await;
    lead_or_follow(&schedulers, 2, false).await;
    let jobs = cluster.client().await;
    let _agent = cluster.agent_with(Arc::new(LargeBackend)).await;
    let (_, r) = run_large(&cluster, &jobs, 300, "full-follower").await;
    assert!(!node(&schedulers, 2).blob_disk().has(&r.id));
    assert_eq!(holders(&schedulers, &r.id), 2);
    eventually("the full follower counts the blob it lacks", || {
        node(&schedulers, 2).status().blobs_missing == 1
    })
    .await;

    lead_or_follow(&schedulers, 2, true).await;
    let spec = serde_json::json!({
        "schema_version": 1, "backend": "test", "operation": "test.sleep",
        "arguments": {"kib": 300, "tag": "full-leader"},
    })
    .to_string();
    let job = jobs.submit_json(&spec, 0).await.unwrap().job_id;
    let record = jobs
        .wait_refs(&job, Some(Duration::from_secs(240)), false)
        .await
        .unwrap();
    assert_eq!(record.state, JobState::Failed);
    assert!(
        matches!(&record.error, Some(CasError::ResourceLimit(m)) if m.contains("free")),
        "{:?}",
        record.error
    );
    // Small results still go through.
    run_job(&jobs).await;
}

// ------------------------------------------------- arguments in blobs (#95)

/// Returns `{"sha256": ..., "len": n}` for the array `arguments.rows`: the
/// id of the blob its canonical JSON would be, and its length; counts its
/// calls.
struct ArgumentBackend {
    calls: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait::async_trait]
impl CasBackend for ArgumentBackend {
    fn name(&self) -> &'static str {
        "test"
    }

    fn capabilities(&self) -> Capabilities {
        EchoBackend.capabilities()
    }

    async fn execute(&self, job: CasRequest, ctx: ExecutionContext) -> Result<CasResult, CasError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let rows = job.arguments["rows"].clone();
        let mut result = EchoBackend.execute(job, ctx).await?;
        result.value = serde_json::json!({
            "sha256": algebramesh_core::blob::canonical_ref(&rows).id.to_string(),
            "len": rows.as_array().map_or(0, Vec::len),
        });
        Ok(result)
    }
}

fn argument_spec(rows: serde_json::Value) -> String {
    serde_json::json!({
        "schema_version": 1, "backend": "test", "operation": "test.sleep",
        "arguments": {"rows": rows, "n": 1},
    })
    .to_string()
}

/// Issue #95: a client sends large arguments as blobs; the job refers to
/// them by markers, the agent reads them back before the backend runs, the
/// records stay small, and an old client gets the arguments whole.
#[tokio::test(flavor = "multi_thread")]
async fn large_arguments_go_through_blobs() {
    let (cluster, schedulers, leader) = started(3).await;
    // The test relies on one leader: a new leader would declare the assigned
    // job lost. On a loaded host (CI) the followers may start an election
    // while the 12 MiB argument goes out, so they hold their elections; the
    // watch names an election that happens all the same (Issue #41).
    for (i, s) in schedulers.iter().enumerate() {
        if i != leader {
            s.as_ref()
                .unwrap()
                .node
                .hold_elections(election_timeout() * 400);
        }
    }
    let _watch = ElectionWatch::new(&all(&schedulers));
    let jobs = cluster.client().await;
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let _agent = cluster
        .agent_with(Arc::new(ArgumentBackend {
            calls: calls.clone(),
        }))
        .await;
    let (rows, body) = big_value("argument", 12 << 20);
    let spec = argument_spec(rows.clone());
    assert!(spec.len() > algebramesh_core::MAX_SPEC_BYTES);
    let job = jobs.submit_json(&spec, 0).await.unwrap().job_id;
    let record = jobs
        .wait_refs(&job, Some(Duration::from_secs(180)), false)
        .await
        .unwrap();
    assert_eq!(record.state, JobState::Succeeded, "{:?}", record.error);
    let r = BlobRef::from_marker(&record.spec.arguments["rows"])
        .unwrap()
        .unwrap();
    assert_eq!(r.id, BlobId::of(&body));
    let value = record.result.unwrap();
    assert_eq!(value["sha256"], serde_json::json!(r.id.to_string()));
    assert_eq!(
        value["len"],
        serde_json::json!(rows.as_array().unwrap().len())
    );
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(holders(&schedulers, &r.id) >= 2);

    // What Raft carries is the record with a marker.
    let mut client = raw(&cluster, leader).await;
    let stored = stored_record(&mut client, &job).await;
    assert!(stored.len() < 1 << 20, "{} bytes", stored.len());
    // A client that does not resolve markers gets the arguments whole.
    let old = client
        .result(pb::JobRef {
            job_id: job.clone(),
            accept_blob_refs: false,
        })
        .await
        .unwrap()
        .into_inner()
        .record_json;
    let old: algebramesh_core::JobRecord = serde_json::from_str(&old).unwrap();
    assert_eq!(old.spec.arguments["rows"], rows);

    // The same arguments again: nothing is sent twice, and the blob is the
    // same.
    let again = jobs.submit_json(&spec, 0).await.unwrap().job_id;
    let record = jobs
        .wait_refs(&again, Some(Duration::from_secs(180)), false)
        .await
        .unwrap();
    assert_eq!(record.spec.arguments["rows"], r.marker());
}

/// Issue #95: a job whose argument blob is lost from every scheduler fails
/// without calling the backend, and its error names the blob.
#[tokio::test(flavor = "multi_thread")]
async fn a_lost_argument_fails_the_job_without_the_backend() {
    let (cluster, schedulers, leader) = started(3).await;
    let jobs = cluster.client().await;
    let (rows, body) = big_value("lost", 64 << 10);
    let r = put_on_leader(&cluster, leader, &body).await;
    assert_eq!(r.id, BlobId::of(&body));
    eventually("every scheduler holds the blob", || {
        holders(&schedulers, &r.id) == 3
    })
    .await;
    // Queued while no agent runs.
    let job = jobs
        .submit_json(&argument_spec(r.marker()), 0)
        .await
        .unwrap()
        .job_id;
    for s in schedulers.iter().flatten() {
        let _ = std::fs::remove_file(s.node.blob_disk().path(&r.id));
    }
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let _agent = cluster
        .agent_with(Arc::new(ArgumentBackend {
            calls: calls.clone(),
        }))
        .await;
    let record = jobs
        .wait_refs(&job, Some(Duration::from_secs(120)), false)
        .await
        .unwrap();
    assert_eq!(record.state, JobState::Failed);
    assert!(
        matches!(&record.error, Some(CasError::Internal(m)) if m.contains(r.id.as_str())),
        "{:?}",
        record.error
    );
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    let _ = rows;
}

/// Issue #95: a submission that refers to a blob no scheduler holds, or one
/// a majority does not hold yet, creates no job; once a majority holds it,
/// the same submission does.
#[tokio::test(flavor = "multi_thread")]
async fn a_submission_waits_for_its_blobs_on_a_majority() {
    let (cluster, mut schedulers, leader) = started(3).await;
    let mut client = raw(&cluster, leader).await;
    let submit = |client: &mut Raw, spec: String, key: &str| {
        let mut client = client.clone();
        let key = key.to_owned();
        async move {
            client
                .submit(pb::SubmitRequest {
                    spec_json: spec,
                    priority: 0,
                    request_id: key,
                })
                .await
        }
    };
    let unknown = BlobRef {
        id: BlobId::of(b"never stored"),
        size: 12,
    };
    let e = submit(&mut client, argument_spec(unknown.marker()), "unknown")
        .await
        .unwrap_err();
    assert_eq!(e.code(), Code::FailedPrecondition, "{e}");

    let holder = (leader + 1) % 3;
    let absent = (leader + 2) % 3;
    schedulers[absent].take().unwrap().stop().await;
    let (_, body) = big_value("minority", 64 << 10);
    let r = put_on_leader(&cluster, leader, &body).await;
    std::fs::remove_file(node(&schedulers, holder).blob_disk().path(&r.id)).unwrap();
    let spec = argument_spec(r.marker());
    let e = submit(&mut client, spec.clone(), "minority")
        .await
        .unwrap_err();
    assert_eq!(e.code(), Code::Unavailable, "{e}");
    let looked_up = client
        .lookup_request(pb::RequestRef {
            request_id: "minority".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(looked_up.code(), Code::NotFound, "{looked_up}");
    eventually("the other voter holds the blob again", || {
        node(&schedulers, holder).blob_disk().has(&r.id)
    })
    .await;
    submit(&mut client, spec, "minority").await.unwrap();
}

// ------------------------------------------------ quotas and backups (#96)

/// Issue #96: once a client token has uploaded its hourly allowance, its
/// next upload is refused before anything is stored; another token has an
/// allowance of its own; the scheduler counts what it holds and what it
/// refused.
#[tokio::test(flavor = "multi_thread")]
async fn uploads_beyond_the_allowance_of_a_token_are_refused() {
    let (mut cluster, listeners) = Cluster::new(3);
    cluster.upload_allowance = 3 << 20;
    let mut schedulers = Vec::new();
    for (i, l) in listeners.into_iter().enumerate() {
        schedulers.push(Some(cluster.start(i, l).await));
    }
    features_agreed(&schedulers).await;
    let leader = serving(&all(&schedulers)).await;
    let first = cluster.client().await;
    let other = cluster.client_with_token(OTHER_CLIENT_TOKEN).await;

    let a = vec![b'a'; 2 << 20];
    let r = first.put_bytes(a.clone()).await.unwrap();
    // Held already: nothing is sent, and nothing is taken.
    assert_eq!(first.put_bytes(a).await.unwrap(), r);
    let b = vec![b'b'; 2 << 20];
    let e = first.put_bytes(b.clone()).await.unwrap_err();
    assert!(
        matches!(&e, ClientError::Rpc { code: tonic::Code::ResourceExhausted, message }
            if message.contains("blob_upload_bytes_per_token_per_hour")),
        "{e}"
    );
    assert_eq!(holders(&schedulers, &BlobId::of(&b)), 0);
    // More than the whole allowance at once.
    let e = other.put_bytes(vec![b'c'; 4 << 20]).await.unwrap_err();
    assert!(
        matches!(
            &e,
            ClientError::Rpc {
                code: tonic::Code::ResourceExhausted,
                ..
            }
        ),
        "{e}"
    );
    other.put_bytes(b.clone()).await.unwrap();

    let status = node(&schedulers, leader).status();
    assert_eq!(status.blob_uploads_refused, 2);
    assert_eq!(status.blobs_stored, 2);
    assert_eq!(status.blob_bytes_stored, 4 << 20);
}

fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// Issue #96: the backup of `docs/install.md` §4.1. One follower is stopped
/// and its data directory copied without `blobs/`; then the bodies of a
/// running scheduler are copied. A cluster whose every member starts from
/// that copy has the jobs, reads their blobs, misses none, and runs new jobs.
#[tokio::test(flavor = "multi_thread")]
async fn a_cluster_restored_from_a_backup_keeps_its_jobs_and_blobs() {
    let (mut cluster, mut schedulers, leader) = started(3).await;
    let jobs = cluster.client().await;
    let agent = cluster.agent_with(Arc::new(LargeBackend)).await;
    let (job, r) = run_large(&cluster, &jobs, 1024, "kept").await;
    let value = jobs.blob(&r).await.unwrap();

    // The backup.
    let backup = cluster.dir.join("backup");
    let follower = (leader + 1) % 3;
    schedulers[follower].take().unwrap().stop().await;
    let data = cluster.data_dir(follower);
    // read_dir has no fixed order: on Linux a file may come before any
    // directory whose copy would have created the backup directory.
    std::fs::create_dir_all(&backup).unwrap();
    for entry in std::fs::read_dir(&data).unwrap() {
        let entry = entry.unwrap();
        if entry.file_name() == "blobs" {
            continue;
        }
        let target = backup.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
    copy_dir(
        &cluster.data_dir(leader).join("blobs").join("sha256"),
        &backup.join("blobs").join("sha256"),
    );

    // Everything is lost; every member starts from the backup.
    agent.cancel();
    for s in schedulers.iter_mut() {
        if let Some(s) = s.take() {
            s.stop().await;
        }
    }
    // Each member is restored into an empty directory, as on a new machine.
    // The old ones are left: on Windows a stopped scheduler's Raft database
    // may stay open until the runtime ends (os error 32 on removing it), as
    // for the directories of the Raft tests.
    for i in 0..3 {
        let data = cluster.dir.join(format!("restored{}", i + 1));
        copy_dir(&backup, &data);
        cluster.data_dirs.insert(i, data);
    }
    for (i, s) in schedulers.iter_mut().enumerate() {
        *s = Some(cluster.restart(i).await);
    }
    serving(&all(&schedulers)).await;

    let jobs = cluster.client().await;
    let record = jobs
        .wait_refs(&job, Some(Duration::from_secs(60)), false)
        .await
        .unwrap();
    assert_eq!(record.state, JobState::Succeeded);
    assert_eq!(jobs.blob(&r).await.unwrap(), value);
    eventually("every scheduler holds every blob", || {
        schedulers
            .iter()
            .flatten()
            .all(|s| s.node.blob_disk().has(&r.id) && s.node.status().blobs_missing == 0)
    })
    .await;
    let _agent = cluster.agent_with(Arc::new(LargeBackend)).await;
    run_large(&cluster, &jobs, 512, "after").await;
}

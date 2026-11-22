//! A Raft cluster of three schedulers in one process (ADR 0013): elections,
//! failover of clients and agents, a stopped scheduler catching up, a leader
//! without a majority refusing calls, and members added and removed while the
//! cluster runs.

#![allow(clippy::unwrap_used)]

mod common;

use algebramesh_client::ListJobsRequest;
use common::*;

#[tokio::test(flavor = "multi_thread")]
async fn a_new_leader_takes_over_and_a_stopped_scheduler_catches_up() {
    let (cluster, listeners) = Cluster::new(3);
    let mut schedulers = Vec::new();
    for (i, l) in listeners.into_iter().enumerate() {
        schedulers.push(Some(cluster.start(i, l).await));
    }
    let leader = serving(&schedulers.iter().map(Option::as_ref).collect::<Vec<_>>()).await;
    let watch = ElectionWatch::new(&all(&schedulers));
    let first_epoch = schedulers[leader].as_ref().unwrap().node.status().epoch;

    let client = cluster.client().await;
    let _agent = cluster.agent().await;
    let first = run_job(&client).await;
    // A submission with an idempotency key, whose answer the client will act as
    // if it had lost.
    let keyed = client
        .submit_json_with_request_id(&spec(), 0, "keyed-submission")
        .await
        .unwrap()
        .job_id;
    let seq = schedulers[leader].as_ref().unwrap().node.status().last_seq;
    for s in schedulers.iter().flatten() {
        eventually("every scheduler has applied the changes", || {
            s.node.status().last_seq >= seq
        })
        .await;
    }

    // The leader stops; the other two elect a new one in a later term.
    drop(watch);
    schedulers[leader].take().unwrap().stop().await;
    let next = serving(&schedulers.iter().map(Option::as_ref).collect::<Vec<_>>()).await;
    assert_ne!(next, leader);
    let status = schedulers[next].as_ref().unwrap().node.status();
    assert!(status.epoch > first_epoch, "{status:?}");

    // The client finds the first job there, and the agent runs new jobs.
    assert_eq!(client.status(&first).await.unwrap().state, "SUCCEEDED");
    // Sending the keyed submission again to the new leader returns the same job.
    let again = client
        .submit_json_with_request_id(&spec(), 0, "keyed-submission")
        .await
        .unwrap()
        .job_id;
    assert_eq!(again, keyed);
    let second = run_job(&client).await;

    // The stopped scheduler returns as a follower and catches up.
    let back = cluster.restart(leader).await;
    let seq = schedulers[next].as_ref().unwrap().node.status().last_seq;
    eventually("the returning scheduler has caught up", || {
        back.node.status().last_seq >= seq
    })
    .await;
    let status = back.node.status();
    assert_eq!(status.role, "follower", "{status:?}");
    assert!(!status.serving);
    assert_eq!(client.status(&second).await.unwrap().state, "SUCCEEDED");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_leader_without_a_majority_refuses_calls() {
    let (cluster, listeners) = Cluster::new(3);
    let mut schedulers = Vec::new();
    for (i, l) in listeners.into_iter().enumerate() {
        schedulers.push(Some(cluster.start(i, l).await));
    }
    let leader = serving(&schedulers.iter().map(Option::as_ref).collect::<Vec<_>>()).await;
    let watch = ElectionWatch::new(&all(&schedulers));
    let client = cluster.client().await;
    let _agent = cluster.agent().await;
    run_job(&client).await;

    // Both followers stop: the leader is alone and cannot commit anything.
    drop(watch);
    let followers: Vec<usize> = (0..3).filter(|i| *i != leader).collect();
    for i in &followers {
        schedulers[*i].take().unwrap().stop().await;
    }
    let alone = schedulers[leader].as_ref().unwrap();
    eventually("the lone leader stops serving", || {
        !alone.node.status().serving
    })
    .await;
    let refused = client.submit_json(&spec(), 0).await.unwrap_err();
    assert!(refused.to_string().contains("Unavailable"), "{refused}");

    // One follower returns: there is a majority again and jobs run.
    let back = cluster.restart(followers[0]).await;
    let both = [schedulers[leader].as_ref(), Some(&back)];
    serving(&both).await;
    run_job(&client).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_member_is_added_and_the_leader_removed_while_jobs_run() {
    let (cluster, mut listeners) = Cluster::with_initial(4, 3);
    let joining = listeners.pop().unwrap();
    let mut schedulers = Vec::new();
    for (i, l) in listeners.into_iter().enumerate() {
        schedulers.push(Some(cluster.start(i, l).await));
    }
    let leader = serving(&schedulers.iter().map(Option::as_ref).collect::<Vec<_>>()).await;
    let watch = ElectionWatch::new(&all(&schedulers));
    let client = cluster.client().await;
    let _agent = cluster.agent().await;
    let first = run_job(&client).await;

    // A fourth scheduler starts with `join` and waits; the leader adds it.
    let fourth = cluster.start(3, joining).await;
    assert!(!fourth.node.status().serving);
    let members = schedulers[leader]
        .as_ref()
        .unwrap()
        .node
        .add_member(4, cluster.member(3))
        .await
        .unwrap();
    let voters: Vec<u64> = members
        .members
        .iter()
        .filter(|m| m.voter)
        .map(|m| m.id)
        .collect();
    assert_eq!(voters, [1, 2, 3, 4], "{members:?}");
    let seq = schedulers[leader].as_ref().unwrap().node.status().last_seq;
    eventually("the new member has caught up", || {
        fourth.node.status().last_seq >= seq
    })
    .await;
    // A voter once it has applied its membership, which follows the changes.
    eventually("the new member is a voter", || {
        fourth
            .node
            .members()
            .members
            .iter()
            .any(|m| m.id == 4 && m.voter)
            && fourth.node.status().role != "learner"
    })
    .await;
    schedulers.push(Some(fourth));

    // The leader removes itself; the other three, the new one among them, go on.
    drop(watch);
    let removed = leader as u64 + 1;
    schedulers[leader]
        .as_ref()
        .unwrap()
        .node
        .remove_member(removed)
        .await
        .unwrap();
    let old = schedulers[leader].take().unwrap();
    // Until it stops serving, a call the old leader takes may be answered with
    // ABORTED (it stopped leading before the change was committed).
    eventually("the removed leader stops serving", || {
        !old.node.status().serving
    })
    .await;
    let next = serving(&schedulers.iter().map(Option::as_ref).collect::<Vec<_>>()).await;
    let members = schedulers[next].as_ref().unwrap().node.members();
    let ids: Vec<u64> = members.members.iter().map(|m| m.id).collect();
    assert!(!ids.contains(&removed), "{members:?}");
    assert_eq!(ids.len(), 3, "{members:?}");
    assert!(members.members.iter().all(|m| m.voter));
    assert_eq!(client.status(&first).await.unwrap().state, "SUCCEEDED");
    run_job(&client).await;
    assert!(!old.node.status().serving);
    old.stop().await;
}

/// Issue #5: a call is acknowledged only when the entries that carry its own
/// changes are committed, not when the state machine reaches the same sequence
/// number with another leader's changes.
#[tokio::test(flavor = "multi_thread")]
async fn a_change_another_leader_overtook_is_not_acknowledged() {
    let (cluster, listeners) = Cluster::new(3);
    let mut schedulers = Vec::new();
    for (i, l) in listeners.into_iter().enumerate() {
        schedulers.push(Some(cluster.start(i, l).await));
    }
    let leader = serving(&all(&schedulers)).await;
    let watch = ElectionWatch::new(&all(&schedulers));
    let client = cluster.client().await;
    client.submit_json(&spec(), 0).await.unwrap();

    // The leader writes a keyed submission to its working store, but its
    // proposer holds the changes back.
    let old = schedulers[leader].as_ref().unwrap().node.clone();
    old.hold_proposals(true);
    let only_leader = Client::connect(ClientConfig {
        url: url(cluster.addrs[leader]),
        ca_pem: cluster.pki.ca_cert_pem.clone().into_bytes(),
        domain: None,
        token: CLIENT_TOKEN.into(),
    })
    .await
    .unwrap();
    let held = held_call(&old, || {
        let c = only_leader.clone();
        async move { c.submit_json_with_request_id(&spec(), 0, "held").await }
    })
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!held.is_finished());

    // Another scheduler leads in a later term and commits other changes under
    // the same sequence numbers; the old leader applies them as a follower.
    // The old leader starts no election meanwhile; an election the other
    // node loses (its votes refused within the leader's lease) is tried again.
    drop(watch);
    old.hold_elections(election_timeout() * 60);
    let other = (leader + 1) % 3;
    let mut moved = false;
    for _ in 0..20 {
        let _ = schedulers[other].as_ref().unwrap().node.promote().await;
        let until = std::time::Instant::now() + election_timeout() * 5;
        while std::time::Instant::now() < until && !moved {
            moved = !old.status().serving
                && schedulers.iter().enumerate().any(|(i, s)| {
                    i != leader && s.as_ref().is_some_and(|s| s.node.status().serving)
                });
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        if moved {
            break;
        }
    }
    assert!(moved, "no other scheduler was elected");
    let next = serving(&all(&schedulers)).await;
    assert_ne!(next, leader);
    for _ in 0..3 {
        client.submit_json(&spec(), 0).await.unwrap();
    }
    let seq = schedulers[next].as_ref().unwrap().node.status().last_seq;
    eventually(
        "the old leader has applied the new leader's changes",
        || old.status().last_seq >= seq,
    )
    .await;

    // The held submission was not acknowledged: its outcome is unknown, and the
    // client does not send it again by itself.
    let answer = held.await.unwrap();
    let refused = answer.unwrap_err();
    assert!(refused.to_string().contains("Aborted"), "{refused}");
    old.hold_proposals(false);

    // Sent again with the same key, it creates one job, which exists.
    let job = client
        .submit_json_with_request_id(&spec(), 0, "held")
        .await
        .unwrap()
        .job_id;
    assert_eq!(client.status(&job).await.unwrap().state, "QUEUED");
    let again = client
        .submit_json_with_request_id(&spec(), 0, "held")
        .await
        .unwrap()
        .job_id;
    assert_eq!(again, job);

    // The cluster goes on: the old leader follows and catches up.
    let seq = schedulers[next].as_ref().unwrap().node.status().last_seq;
    eventually("the old leader has caught up", || {
        old.status().last_seq >= seq
    })
    .await;
    assert!(!old.status().serving);
}

/// Issue #13: `Status` and `Result` answer from the committed state. A change
/// the leader made to its working copy but did not commit is not shown, and a
/// change of leader that drops it does not undo anything a client has seen.
#[tokio::test(flavor = "multi_thread")]
async fn reads_do_not_show_changes_that_are_not_committed() {
    let (cluster, listeners) = Cluster::new(3);
    let mut schedulers = Vec::new();
    for (i, l) in listeners.into_iter().enumerate() {
        schedulers.push(Some(cluster.start(i, l).await));
    }
    let leader = serving(&all(&schedulers)).await;
    let watch = ElectionWatch::new(&all(&schedulers));
    let client = cluster.client().await;
    // A queued job, committed (there is no worker).
    let job = client.submit_json(&spec(), 0).await.unwrap().job_id;
    assert_eq!(client.status(&job).await.unwrap().state, "QUEUED");

    // The leader cancels it in its working copy, but its proposer holds the
    // change back, so the cancellation waits for a commit that does not come.
    let old = schedulers[leader].as_ref().unwrap().node.clone();
    old.hold_proposals(true);
    let only_leader = Client::connect(ClientConfig {
        url: url(cluster.addrs[leader]),
        ca_pem: cluster.pki.ca_cert_pem.clone().into_bytes(),
        domain: None,
        token: CLIENT_TOKEN.into(),
    })
    .await
    .unwrap();
    let cancelling = held_call(&old, || {
        let (c, job) = (only_leader.clone(), job.clone());
        async move { c.cancel(&job).await }
    })
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!cancelling.is_finished());

    // The same leader answers Status and Result from the committed state.
    assert_eq!(only_leader.status(&job).await.unwrap().state, "QUEUED");
    let result = only_leader.result(&job).await.unwrap();
    assert_eq!(result.0.state, "QUEUED");
    assert!(result.1.is_none());

    // The leader stops without proposing the change; the next leader never
    // had it, and the job is still queued there, as the client saw.
    drop(watch);
    schedulers[leader].take().unwrap().stop().await;
    assert!(cancelling.await.unwrap().is_err());
    serving(&all(&schedulers)).await;
    assert_eq!(client.status(&job).await.unwrap().state, "QUEUED");
}

/// Issue #63: `LookupRequest` and `ListJobs` read the committed state. They
/// make no job, do not show a submission the leader took but did not commit,
/// and answer the same after a change of leader.
#[tokio::test(flavor = "multi_thread")]
async fn lookups_and_listings_read_the_committed_state() {
    let (cluster, listeners) = Cluster::new(3);
    let mut schedulers = Vec::new();
    for (i, l) in listeners.into_iter().enumerate() {
        schedulers.push(Some(cluster.start(i, l).await));
    }
    let leader = serving(&all(&schedulers)).await;
    let watch = ElectionWatch::new(&all(&schedulers));
    let client = cluster.client().await;
    let mut ids = Vec::new();
    for key in ["wf/a/2", "wf/a/1", "other"] {
        let job = client.submit_json_with_request_id(&spec(), 0, key).await;
        ids.push(job.unwrap().job_id);
    }
    ids.push(client.submit_json(&spec(), 0).await.unwrap().job_id);

    let everything = ListJobsRequest::default();
    let by_prefix = ListJobsRequest {
        request_id_prefix: "wf/a/".into(),
        ..Default::default()
    };
    let listing = |c: Client, r: ListJobsRequest| async move {
        let page = c.list_jobs(&r).await.unwrap();
        assert!(page.next_page_token.is_empty());
        let jobs = page.jobs.into_iter();
        jobs.map(|j| (j.job_id, j.request_id)).collect::<Vec<_>>()
    };
    let jobs = listing(client.clone(), everything.clone()).await;
    let listed: Vec<String> = jobs.iter().map(|j| j.0.clone()).collect();
    assert_eq!(listed, ids, "in the order of submission");
    let prefixed = listing(client.clone(), by_prefix.clone()).await;
    let expected = [(&ids[1], "wf/a/1"), (&ids[0], "wf/a/2")];
    let expected: Vec<_> = expected.map(|(j, r)| (j.clone(), r.to_owned())).into();
    assert_eq!(prefixed, expected, "in the order of the request ids");
    // Page by page, one job a page, the same jobs.
    let paged = |c: Client, mut r: ListJobsRequest| async move {
        r.page_size = 1;
        let mut all = Vec::new();
        loop {
            let page = c.list_jobs(&r).await.unwrap();
            assert!(page.jobs.len() <= 1);
            all.extend(page.jobs.into_iter().map(|j| (j.job_id, j.request_id)));
            if page.next_page_token.is_empty() {
                return all;
            }
            r.page_token = page.next_page_token;
        }
    };
    assert_eq!(paged(client.clone(), everything.clone()).await, jobs);
    assert_eq!(paged(client.clone(), by_prefix.clone()).await, prefixed);
    let first_page = ListJobsRequest {
        page_size: 1,
        ..Default::default()
    };
    let token = client.list_jobs(&first_page).await.unwrap().next_page_token;
    let found = client.lookup_request("wf/a/1").await.unwrap().unwrap();
    assert_eq!(
        (found.job_id.as_str(), found.state.as_str()),
        (ids[1].as_str(), "QUEUED")
    );
    assert!(client.lookup_request("wf/a/3").await.unwrap().is_none());
    // Neither call made a job.
    assert_eq!(listing(client.clone(), everything.clone()).await, jobs);

    // The leader takes a submission but holds its change back.
    let old = schedulers[leader].as_ref().unwrap().node.clone();
    old.hold_proposals(true);
    let only_leader = Client::connect(ClientConfig {
        url: url(cluster.addrs[leader]),
        ca_pem: cluster.pki.ca_cert_pem.clone().into_bytes(),
        domain: None,
        token: CLIENT_TOKEN.into(),
    })
    .await
    .unwrap();
    let held = held_call(&old, || {
        let c = only_leader.clone();
        async move { c.submit_json_with_request_id(&spec(), 0, "wf/a/3").await }
    })
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!held.is_finished());
    // The same leader does not show it.
    assert!(
        only_leader
            .lookup_request("wf/a/3")
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        listing(only_leader.clone(), by_prefix.clone()).await,
        prefixed
    );
    assert_eq!(listing(only_leader.clone(), everything.clone()).await, jobs);

    // The next leader gives the same answers.
    drop(watch);
    schedulers[leader].take().unwrap().stop().await;
    assert!(held.await.unwrap().is_err());
    serving(&all(&schedulers)).await;
    assert_eq!(listing(client.clone(), everything).await, jobs);
    assert_eq!(listing(client.clone(), by_prefix).await, prefixed);
    // A token of the old leader goes on with the new one.
    let rest = ListJobsRequest {
        page_token: token,
        ..Default::default()
    };
    let rest = client.list_jobs(&rest).await.unwrap().jobs.into_iter();
    let rest: Vec<_> = rest.map(|j| (j.job_id, j.request_id)).collect();
    assert_eq!(rest, jobs[1..]);
    let again = client.lookup_request("wf/a/1").await.unwrap().unwrap();
    assert_eq!(again, found);
    assert!(client.lookup_request("wf/a/3").await.unwrap().is_none());
}

/// Issue #18: a call that writes nothing (a submission sent again with a known
/// key) does not wait for other calls' changes to be committed, but a
/// submission whose own job is not committed yet is not reported as done.
#[tokio::test(flavor = "multi_thread")]
async fn a_known_key_is_answered_without_waiting_for_other_changes() {
    let (cluster, listeners) = Cluster::new(3);
    let mut schedulers = Vec::new();
    for (i, l) in listeners.into_iter().enumerate() {
        schedulers.push(Some(cluster.start(i, l).await));
    }
    let leader = serving(&all(&schedulers)).await;
    let _watch = ElectionWatch::new(&all(&schedulers));
    let client = cluster.client().await;
    let first = client
        .submit_json_with_request_id(&spec(), 0, "known")
        .await
        .unwrap()
        .job_id;

    // The leader takes other submissions but holds their changes back.
    let old = schedulers[leader].as_ref().unwrap().node.clone();
    old.hold_proposals(true);
    let only_leader = Client::connect(ClientConfig {
        url: url(cluster.addrs[leader]),
        ca_pem: cluster.pki.ca_cert_pem.clone().into_bytes(),
        domain: None,
        token: CLIENT_TOKEN.into(),
    })
    .await
    .unwrap();
    let submit = |key: &'static str| {
        let c = only_leader.clone();
        async move { c.submit_json_with_request_id(&spec(), 0, key).await }
    };
    let other = held_call(&old, || submit("other")).await;
    let pending = held_call(&old, || submit("pending")).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!other.is_finished() && !pending.is_finished());

    // The known key is answered at once, with its committed job.
    let started = std::time::Instant::now();
    let again = only_leader
        .submit_json_with_request_id(&spec(), 0, "known")
        .await
        .unwrap();
    assert_eq!(again.job_id, first);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    // The same key with another job is still a conflict.
    let other_spec = spec().replace("\"ms\":1", "\"ms\":2");
    let conflict = only_leader
        .submit_json_with_request_id(&other_spec, 0, "known")
        .await
        .unwrap_err();
    assert!(conflict.to_string().contains("AlreadyExists"), "{conflict}");

    // A key whose job is not committed yet waits for it, and is not answered
    // before the commit.
    // It writes nothing, so it is watched for the time it waits; a refusal
    // before it reached the scheduler (UNAVAILABLE, see `held_call`) is sent
    // again.
    let duplicate = loop {
        let duplicate = tokio::spawn(submit("pending"));
        tokio::time::sleep(Duration::from_millis(500)).await;
        if !duplicate.is_finished() {
            break duplicate;
        }
        match duplicate.await.unwrap() {
            Err(e) if e.to_string().contains("Unavailable") => {}
            other => panic!("the duplicate was answered before the commit: {other:?}"),
        }
    };
    old.hold_proposals(false);
    let original = pending.await.unwrap().unwrap().job_id;
    assert_eq!(duplicate.await.unwrap().unwrap().job_id, original);
    other.await.unwrap().unwrap();
}

/// Issue #38: submissions with the same key sent at the same time, over
/// several connections, create one job in the committed state, and every one
/// of them is answered with its id.
#[tokio::test(flavor = "multi_thread")]
async fn the_same_key_at_the_same_time_creates_one_job() {
    let (cluster, listeners) = Cluster::new(3);
    let mut schedulers = Vec::new();
    for (i, l) in listeners.into_iter().enumerate() {
        schedulers.push(Some(cluster.start(i, l).await));
    }
    serving(&all(&schedulers)).await;
    let mut clients = Vec::new();
    for _ in 0..12 {
        clients.push(cluster.client().await);
    }
    for round in 0..3 {
        let key = format!("concurrent-{round}");
        let barrier = Arc::new(tokio::sync::Barrier::new(clients.len()));
        let tasks: Vec<_> = clients
            .iter()
            .map(|c| {
                let (c, barrier, key) = (c.clone(), barrier.clone(), key.clone());
                tokio::spawn(async move {
                    barrier.wait().await;
                    // A call whose outcome is unknown (the leader changed) is
                    // sent again with the same key, as clients are told to.
                    let mut last = None;
                    for _ in 0..10 {
                        match c.submit_json_with_request_id(&spec(), 0, &key).await {
                            Ok(status) => return status,
                            Err(e)
                                if e.to_string().contains("Aborted")
                                    || e.to_string().contains("Unavailable") =>
                            {
                                last = Some(e);
                                tokio::time::sleep(Duration::from_millis(200)).await;
                            }
                            Err(e) => panic!("{e}"),
                        }
                    }
                    panic!("no answer: {last:?}")
                })
            })
            .collect();
        let mut ids = Vec::new();
        for t in tasks {
            ids.push(t.await.unwrap().job_id);
        }
        assert!(ids.iter().all(|id| *id == ids[0]), "{ids:?}");
        let leader = serving(&all(&schedulers)).await;
        let node = &schedulers[leader].as_ref().unwrap().node;
        let jobs = node
            .read_committed(|s| {
                s.jobs_in_states(&[JobState::Queued])
                    .unwrap()
                    .into_iter()
                    .map(|r| r.record.job_id.to_string())
                    .collect::<Vec<_>>()
            })
            .await
            .unwrap();
        assert_eq!(jobs.len(), round + 1, "{jobs:?}");
        assert!(jobs.contains(&ids[0]));
    }
}

/// Issue #21: a follower that cannot write artifacts goes on applying the log;
/// its artifacts are written once the directory can be written again.
#[tokio::test(flavor = "multi_thread")]
async fn a_follower_that_cannot_write_artifacts_keeps_following() {
    let (cluster, listeners) = Cluster::new(3);
    let mut schedulers = Vec::new();
    for (i, l) in listeners.into_iter().enumerate() {
        schedulers.push(Some(cluster.start(i, l).await));
    }
    let leader = serving(&all(&schedulers)).await;
    let _watch = ElectionWatch::new(&all(&schedulers));
    let client = cluster.client().await;
    let _agent = cluster.agent().await;
    let follower = (leader + 1) % 3;
    let artifacts = cluster
        .dir
        .join(format!("s{}", follower + 1))
        .join("artifacts");
    std::fs::remove_dir_all(&artifacts).unwrap();
    std::fs::write(&artifacts, b"not a directory").unwrap();

    let mut jobs = Vec::new();
    for _ in 0..3 {
        jobs.push(run_job(&client).await);
    }
    let seq = schedulers[leader].as_ref().unwrap().node.status().last_seq;
    let f = schedulers[follower].as_ref().unwrap();
    eventually("the follower has applied every change", || {
        f.node.status().last_seq >= seq
    })
    .await;
    assert_eq!(f.node.status().role, "follower");

    // The directory can be written again: a later change writes the missing
    // artifacts, which match the jobs.
    std::fs::remove_file(&artifacts).unwrap();
    std::fs::create_dir_all(&artifacts).unwrap();
    tokio::time::sleep(Duration::from_secs(6)).await;
    jobs.push(run_job(&client).await);
    for job in &jobs {
        let path = artifacts.join(job).join("record.json");
        eventually("the follower has written the artifact", || path.exists()).await;
        let record: algebramesh_core::JobRecord =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(record.state, JobState::Succeeded);
    }
}

/// Adds a fourth scheduler after the log is dropped up to a snapshot, so that
/// it can catch up only from the snapshot, sent in chunks (Issue #20). The
/// metadata database holds `jobs` jobs whose arguments carry `bytes` bytes.
async fn a_new_member_catches_up_from_a_chunked_snapshot(jobs: usize, bytes: usize, large: bool) {
    let (mut cluster, mut listeners) = Cluster::with_initial(4, 3);
    // The arguments stay in the records, so that the snapshot is large.
    cluster.without_blob_arguments();
    if large {
        // Writing and sending large entries takes longer than the short
        // default; the chunks are of the default size.
        cluster.election_timeout = election_timeout().max(Duration::from_secs(3));
        cluster.snapshot_chunk_bytes = algebramesh_server::SNAPSHOT_CHUNK_BYTES;
    }
    let joining = listeners.pop().unwrap();
    let mut schedulers = Vec::new();
    for (i, l) in listeners.into_iter().enumerate() {
        schedulers.push(Some(cluster.start(i, l).await));
    }
    let leader = serving(&all(&schedulers)).await;
    let _watch = ElectionWatch::new(&all(&schedulers));
    let client = cluster.client().await;
    let blob = "x".repeat(bytes);
    for k in 0..jobs {
        let spec = serde_json::json!({
            "schema_version": 1, "backend": "test", "operation": "test.sleep",
            "arguments": {"ms": 1, "blob": blob},
        })
        .to_string();
        // A change of leader answers ABORTED; the same key converges.
        let key = format!("large-{k}");
        let mut tries = 0;
        while let Err(e) = client.submit_json_with_request_id(&spec, 0, &key).await {
            tries += 1;
            assert!(tries < 10, "{e}");
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
    let node = schedulers[leader].as_ref().unwrap().node.clone();
    node.compact_log().await.unwrap();
    let database = cluster
        .dir
        .join(format!("s{}", leader + 1))
        .join("metadata.sqlite3");
    println!(
        "metadata database: {} bytes",
        std::fs::metadata(&database).map_or(0, |m| m.len())
    );

    let fourth = cluster.start(3, joining).await;
    node.add_member(4, cluster.member(3)).await.unwrap();
    let seq = node.status().last_seq;
    let started = std::time::Instant::now();
    while fourth.node.status().last_seq < seq {
        assert!(
            started.elapsed() < Duration::from_secs(120),
            "the new member did not catch up: {:?}",
            fourth.node.status()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    println!("caught up in {:?} after it was added", started.elapsed());
    // A voter once it has applied its membership, which follows the changes.
    eventually("the new member is a voter", || {
        fourth
            .node
            .members()
            .members
            .iter()
            .any(|m| m.id == 4 && m.voter)
            && fourth.node.status().role != "learner"
    })
    .await;
    // No partial transfer is left behind.
    let inbox = cluster.dir.join("s4").join("snapshots");
    let left = std::fs::read_dir(&inbox).map_or(0, |d| d.count());
    assert_eq!(left, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_snapshot_goes_in_chunks() {
    // About 1.5 MB of jobs: more than 20 chunks of 64 KiB.
    a_new_member_catches_up_from_a_chunked_snapshot(5, 300 * 1024, false).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "builds a metadata database of more than 128 MiB; run by hand"]
async fn a_snapshot_larger_than_a_message_goes_in_chunks() {
    // 80 jobs of 1 MiB: each is kept in the jobs table and in the history of
    // changes, so the database and its snapshot exceed 128 MiB, the limit of
    // one client message.
    a_new_member_catches_up_from_a_chunked_snapshot(80, 1024 * 1024, true).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "writes more than 64 MiB of log entries; run by hand"]
async fn a_lagging_follower_catches_up_through_split_appends() {
    // A follower stops while 80 jobs of 1 MiB are committed; when it returns,
    // the leader has more than 64 MiB of entries for it, which go in several
    // AppendEntries requests (Issue #20).
    let (mut cluster, listeners) = Cluster::new(3);
    cluster.election_timeout = election_timeout().max(Duration::from_secs(3));
    // The arguments stay in the log entries.
    cluster.without_blob_arguments();
    let mut schedulers = Vec::new();
    for (i, l) in listeners.into_iter().enumerate() {
        schedulers.push(Some(cluster.start(i, l).await));
    }
    let leader = serving(&all(&schedulers)).await;
    let _watch = ElectionWatch::new(&all(&schedulers));
    let client = cluster.client().await;
    let follower = (leader + 1) % 3;
    schedulers[follower].take().unwrap().stop().await;
    let blob = "x".repeat(1024 * 1024);
    for k in 0..80 {
        let spec = serde_json::json!({
            "schema_version": 1, "backend": "test", "operation": "test.sleep",
            "arguments": {"ms": 1, "blob": blob},
        })
        .to_string();
        let key = format!("lag-{k}");
        let mut tries = 0;
        while let Err(e) = client.submit_json_with_request_id(&spec, 0, &key).await {
            tries += 1;
            assert!(tries < 10, "{e}");
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
    let seq = schedulers[leader].as_ref().unwrap().node.status().last_seq;
    let back = cluster.restart(follower).await;
    let started = std::time::Instant::now();
    while back.node.status().last_seq < seq {
        assert!(
            started.elapsed() < Duration::from_secs(120),
            "the follower did not catch up: {:?}",
            back.node.status()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    println!("caught up in {:?}", started.elapsed());
    assert_eq!(back.node.status().role, "follower");
}

/// The features a scheduler can use, as the tests give them (ADR 0020).
fn all_features() -> Vec<String> {
    algebramesh_server::cluster::supported_features()
}

/// Issue #43 (ADR 0020): features are enabled only once every member can use
/// them; a member that cannot holds them back and cannot join later. Once
/// enabled, they stay so when a member stops.
#[tokio::test(flavor = "multi_thread")]
async fn features_are_enabled_once_every_member_can_use_them() {
    let (mut cluster, mut listeners) = Cluster::with_initial(4, 3);
    // The third scheduler stands for one older than the agreement.
    cluster.features.insert(2, Vec::new());
    let fourth = listeners.pop().unwrap();
    let mut schedulers = Vec::new();
    for (i, l) in listeners.into_iter().enumerate() {
        schedulers.push(Some(cluster.start(i, l).await));
    }
    serving(&all(&schedulers)).await;
    let client = cluster.client().await;
    client.submit_json(&spec(), 0).await.unwrap();
    // Several rounds of the leader's check go by: nothing is enabled.
    tokio::time::sleep(Duration::from_secs(6)).await;
    for s in schedulers.iter().flatten() {
        let status = s.node.status();
        assert!(status.enabled_features.is_empty(), "{status:?}");
    }
    assert!(
        schedulers[2]
            .as_ref()
            .unwrap()
            .node
            .status()
            .features
            .is_empty()
    );
    assert!(client.backends().await.unwrap().server_features.is_empty());

    // The third scheduler starts again with every feature: they are enabled
    // on every member, and clients and agents are told.
    schedulers[2].take().unwrap().stop().await;
    cluster.features.remove(&2);
    schedulers[2] = Some(cluster.restart(2).await);
    eventually("every member has enabled the features", || {
        schedulers
            .iter()
            .flatten()
            .all(|s| s.node.status().enabled_features == all_features())
    })
    .await;
    let mut listed = Vec::new();
    for _ in 0..50 {
        listed = client.backends().await.unwrap().server_features;
        if listed == all_features() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(listed, all_features());

    // A scheduler that cannot use them is not added.
    let leader = serving(&all(&schedulers)).await;
    cluster.features.insert(3, Vec::new());
    let joining = cluster.start(3, fourth).await;
    let refused = schedulers[leader]
        .as_ref()
        .unwrap()
        .node
        .add_member(4, cluster.member(3))
        .await
        .unwrap_err();
    assert_eq!(
        refused.code(),
        tonic::Code::FailedPrecondition,
        "{refused:?}"
    );
    assert!(
        refused.message().contains("cannot use the features"),
        "{refused:?}"
    );
    joining.stop().await;
    let ids: Vec<u64> = schedulers[leader]
        .as_ref()
        .unwrap()
        .node
        .members()
        .members
        .iter()
        .map(|m| m.id)
        .collect();
    assert_eq!(ids, [1, 2, 3]);

    // A member stops: the features stay enabled.
    let follower = (leader + 1) % 3;
    schedulers[follower].take().unwrap().stop().await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    for s in schedulers.iter().flatten() {
        assert_eq!(s.node.status().enabled_features, all_features());
    }
}

/// Issue #85 (ADR 0020): a scheduler that cannot use a feature is not added
/// while the leader is still enabling it. The leader's wait for the commit
/// runs out and frees the lock that orders the agreement and `add_member`,
/// but the change stays on its way to the log and is committed later.
#[tokio::test(flavor = "multi_thread")]
async fn a_member_is_not_added_while_a_feature_is_being_enabled() {
    let (mut cluster, mut listeners) = Cluster::with_initial(4, 3);
    // The third scheduler stands for one older than the agreement, and so
    // does the fourth, which asks to join.
    cluster.features.insert(2, Vec::new());
    cluster.features.insert(3, Vec::new());
    let fourth = listeners.pop().unwrap();
    let mut schedulers = Vec::new();
    for (i, l) in listeners.into_iter().enumerate() {
        schedulers.push(Some(cluster.start(i, l).await));
    }
    serving(&all(&schedulers)).await;

    // The third scheduler stops; the leader of the other two holds its
    // proposals back, and the third starts again with every feature.
    schedulers[2].take().unwrap().stop().await;
    let leader = serving(&all(&schedulers)).await;
    let node = schedulers[leader].as_ref().unwrap().node.clone();
    node.hold_proposals(true);
    cluster.features.remove(&2);
    schedulers[2] = Some(cluster.restart(2).await);

    // The leader agrees on the features and writes them to its working
    // store; their entry is not committed.
    eventually("the leader is enabling the features", || {
        node.scheduler()
            .is_ok_and(|s| s.with_store(|st| st.enabled_features().unwrap()) == all_features())
    })
    .await;
    assert!(node.enabled_features().is_empty());

    // The fourth scheduler asks to join while the agreement waits for the
    // commit; `add_member` goes on once that wait has run out and freed the
    // lock, with the entry still held back.
    let joining = cluster.start(3, fourth).await;
    let refused = node.add_member(4, cluster.member(3)).await.unwrap_err();
    assert_eq!(
        refused.code(),
        tonic::Code::FailedPrecondition,
        "{refused:?}"
    );
    assert!(
        refused.message().contains("cannot use the features"),
        "{refused:?}"
    );
    assert!(node.enabled_features().is_empty());

    // The held entry is committed: the features are enabled, and the fourth
    // scheduler is not a member.
    node.hold_proposals(false);
    eventually("every member has enabled the features", || {
        schedulers
            .iter()
            .flatten()
            .all(|s| s.node.status().enabled_features == all_features())
    })
    .await;
    let ids: Vec<u64> = node.members().members.iter().map(|m| m.id).collect();
    assert_eq!(ids, [1, 2, 3]);
    joining.stop().await;
}

/// The backend part of a job's record on every scheduler, and whether the
/// records are the same everywhere.
fn backends_of(schedulers: &[Option<Scheduler>], job: &str) -> (Vec<serde_json::Value>, bool) {
    let records: Vec<String> = schedulers
        .iter()
        .flatten()
        .map(|s| {
            s.node
                .with_applied_state(|st| {
                    let row = st.get_job(job).unwrap().unwrap();
                    serde_json::to_string(&row.record).unwrap()
                })
                .unwrap()
        })
        .collect();
    let same = records.windows(2).all(|w| w[0] == w[1]);
    let backends = records
        .iter()
        .map(|r| serde_json::from_str::<serde_json::Value>(r).unwrap()["backend"].clone())
        .collect();
    (backends, same)
}

/// Whether the record of `job` holds a guarantee, on every scheduler.
fn guarantees_of(schedulers: &[Option<Scheduler>], job: &str) -> Vec<bool> {
    schedulers
        .iter()
        .flatten()
        .map(|s| {
            s.node
                .with_applied_state(|st| {
                    st.get_job(job).unwrap().unwrap().record.guarantee.is_some()
                })
                .unwrap()
        })
        .collect()
}

/// Waits until every scheduler has applied the leader's last change.
async fn applied_everywhere(schedulers: &[Option<Scheduler>]) {
    let leader = serving(&all(schedulers)).await;
    let seq = schedulers[leader].as_ref().unwrap().node.status().last_seq;
    for s in schedulers.iter().flatten() {
        eventually("every scheduler has applied the changes", || {
            s.node.status().last_seq >= seq
        })
        .await;
    }
}

/// Issue #44 (ADR 0020): a result records the SHA-256 of the backend's driver
/// files only once every scheduler can keep it; before that the records hold
/// no such field, and they are the same on every scheduler.
#[tokio::test(flavor = "multi_thread")]
async fn results_record_the_driver_digest_once_every_scheduler_can() {
    let (mut cluster, listeners) = Cluster::new(3);
    cluster.features.insert(2, Vec::new());
    let mut schedulers = Vec::new();
    for (i, l) in listeners.into_iter().enumerate() {
        schedulers.push(Some(cluster.start(i, l).await));
    }
    serving(&all(&schedulers)).await;
    let driver = cluster.dir.join("driver.m2");
    std::fs::write(&driver, b"-- a driver\n").unwrap();
    let expected = algebramesh_core::driver_sha256(std::slice::from_ref(&driver))
        .unwrap()
        .unwrap();
    let client = cluster.client().await;
    let _agent = cluster
        .agent_with(Arc::new(DriverBackend {
            driver: driver.clone(),
        }))
        .await;

    // With an older scheduler in the cluster: no digest and no guarantee
    // (Issue #47), the same records.
    let before = run_job(&client).await;
    applied_everywhere(&schedulers).await;
    let (backends, same) = backends_of(&schedulers, &before);
    assert!(same, "{backends:?}");
    assert!(
        backends.iter().all(|b| b.get("driver_sha256").is_none()),
        "{backends:?}"
    );
    assert_eq!(guarantees_of(&schedulers, &before), [false; 3]);

    // Every scheduler can keep it: the agent hears of the agreement in its
    // heartbeats, and the next result records the digest everywhere.
    schedulers[2].take().unwrap().stop().await;
    cluster.features.remove(&2);
    schedulers[2] = Some(cluster.restart(2).await);
    eventually("every member has enabled the features", || {
        schedulers
            .iter()
            .flatten()
            .all(|s| s.node.status().enabled_features == all_features())
    })
    .await;
    let mut recorded = None;
    for _ in 0..20 {
        let job = run_job(&client).await;
        applied_everywhere(&schedulers).await;
        let (backends, same) = backends_of(&schedulers, &job);
        assert!(same, "{backends:?}");
        if let Some(d) = backends[0].get("driver_sha256") {
            recorded = Some(d.as_str().unwrap().to_owned());
            assert_eq!(guarantees_of(&schedulers, &job), [true; 3]);
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert_eq!(recorded.as_deref(), Some(expected.as_str()));
}

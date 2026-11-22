//! Faults under a Raft cluster of three schedulers (Issue #37): a crash at each
//! step of a snapshot, a log purge and an install; a full disk under a follower,
//! under the leader and under a snapshot transfer; a transfer whose connections
//! are cut, slowed down and paused; and a leader whose scheduler cannot start.

#![allow(clippy::unwrap_used)]

mod common;

use common::*;

// --------------------------------------------------------------- faults (#37)

use algebramesh_server::fault;

/// Issue #37: a scheduler that crashes at `point` (its Raft stops there, the
/// files stay as they are) starts again over its files and agrees with the
/// others: the same committed jobs, log position and membership, and no file
/// of the interrupted step left behind.
async fn a_crash_at(point: &'static str) {
    let (cluster, listeners) = Cluster::new(3);
    let mut schedulers = Vec::new();
    for (i, l) in listeners.into_iter().enumerate() {
        schedulers.push(Some(cluster.start(i, l).await));
    }
    let leader = serving(&all(&schedulers)).await;
    let client = cluster.client().await;
    // Several chunks of snapshot (64 KiB each).
    submit_padded(&client, "before", 5, 100 * 1024).await;
    let victim = (leader + 1) % 3;
    let dir = cluster.data_dir(victim);
    match point {
        fault::SNAPSHOT_COPIED | fault::LOG_PURGED => {
            // The follower snapshots its own state machine and purges its log.
            agrees_with_leader(&schedulers, &schedulers[victim].as_ref().unwrap().node).await;
            fault::arm(&dir, point);
            let node = schedulers[victim].as_ref().unwrap().node.clone();
            let _ = node.compact_log().await;
        }
        _ => {
            // The follower is stopped while the other two purge their logs
            // past its position: whichever leads sends it a snapshot when it
            // returns.
            schedulers[victim].take().unwrap().stop().await;
            submit_padded(&client, "while-away", 3, 100 * 1024).await;
            for s in schedulers.iter().flatten() {
                s.node.compact_log().await.unwrap();
            }
            fault::arm(&dir, point);
            schedulers[victim] = Some(cluster.restart(victim).await);
        }
    }
    eventually("the crash point is reached", || {
        fault::hits(&dir).contains(&point)
    })
    .await;
    let error = schedulers[victim].take().unwrap().stopped_by_itself().await;
    assert!(error.contains("raft stopped"), "{point}: {error}");
    fault::disarm(&dir);

    // Started again over the same files: nothing of the interrupted step is
    // left, and it catches up and agrees with the leader.
    let back = cluster.restart(victim).await;
    assert_eq!(stale_files(&dir), Vec::<String>::new(), "{point}");
    submit_padded(&client, "after", 2, 1024).await;
    agrees_with_leader(&schedulers, &back.node).await;
    schedulers[victim] = Some(back);
    let ids: Vec<u64> = schedulers[victim]
        .as_ref()
        .unwrap()
        .node
        .members()
        .members
        .iter()
        .map(|m| m.id)
        .collect();
    assert_eq!(ids, [1, 2, 3], "{point}");
    // The cluster still runs jobs.
    let _agent = cluster.agent().await;
    run_job(&client).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_crash_while_copying_a_snapshot() {
    a_crash_at(fault::SNAPSHOT_COPIED).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_crash_after_purging_the_log() {
    a_crash_at(fault::LOG_PURGED).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_crash_after_staging_a_received_snapshot() {
    a_crash_at(fault::RESTORE_STAGED).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_crash_after_closing_the_old_database() {
    a_crash_at(fault::RESTORE_CLOSED).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_crash_after_renaming_the_new_database() {
    a_crash_at(fault::RESTORE_RENAMED).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_crash_before_recording_the_installed_position() {
    a_crash_at(fault::INSTALL_RESTORED).await;
}

// ----------------------------------------------------- a full disk (#37)

/// A small file system for the disk tests (for example a tmpfs of 64 MiB), from
/// `ALGEBRAMESH_TEST_SMALL_DIR`; `tests/integration/run-disk-full.sh` runs them.
fn small_dir(name: &str) -> PathBuf {
    let root = std::env::var("ALGEBRAMESH_TEST_SMALL_DIR")
        .expect("ALGEBRAMESH_TEST_SMALL_DIR names a small file system");
    let dir = PathBuf::from(root).join(format!("{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Fills the file system of `dir` with a file, then frees `leave` bytes of it.
/// Returns the file, to remove when the space is to come back.
fn fill(dir: &std::path::Path, leave: u64) -> PathBuf {
    use std::io::Write;
    let path = dir
        .parent()
        .unwrap()
        .join(format!("filler-{}", uuid::Uuid::new_v4()));
    let mut file = std::fs::File::create(&path).unwrap();
    let chunk = vec![0u8; 1 << 20];
    while file.write(&chunk).is_ok_and(|n| n > 0) {}
    // Byte by byte for what is left in the last block.
    while file.write(&[0]).is_ok_and(|n| n == 1) {}
    let len = file.metadata().unwrap().len();
    file.set_len(len.saturating_sub(leave)).unwrap();
    file.sync_all().unwrap();
    path
}

/// A cluster of three whose third scheduler keeps its data on the small file
/// system.
async fn cluster_with_a_small_disk(name: &str) -> (Cluster, Vec<Option<Scheduler>>, PathBuf) {
    let (mut cluster, listeners) = Cluster::new(3);
    let small = small_dir(name);
    cluster.data_dirs.insert(2, small.clone());
    let mut schedulers = Vec::new();
    for (i, l) in listeners.into_iter().enumerate() {
        schedulers.push(Some(cluster.start(i, l).await));
    }
    serving(&all(&schedulers)).await;
    (cluster, schedulers, small)
}

/// Submits until `victim` stops by itself; the submissions go on succeeding
/// (sent again while a new leader is elected).
async fn submit_until_stopped(client: &Client, victim: &Scheduler, prefix: &str) {
    for k in 0..400 {
        if victim.served.is_finished() {
            return;
        }
        submit_padded(client, &format!("{prefix}-{k}"), 1, 64 * 1024).await;
    }
    panic!("the scheduler on the full disk did not stop");
}

/// Issue #37: a follower whose disk is full stops with an explicit error
/// while the others go on; once there is space again it starts and catches up.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs ALGEBRAMESH_TEST_SMALL_DIR; run by tests/integration/run-disk-full.sh"]
async fn disk_full_on_a_follower() {
    let (cluster, mut schedulers, small) = cluster_with_a_small_disk("follower").await;
    lead_or_follow(&schedulers, 2, false).await;
    let client = cluster.client().await;
    submit_padded(&client, "before", 3, 1024).await;
    let filler = fill(&small, 0);
    let victim = schedulers[2].take().unwrap();
    submit_until_stopped(&client, &victim, "full").await;
    let error = victim.stopped_by_itself().await;
    assert!(
        error.contains("raft stopped") && error.contains("full"),
        "{error}"
    );
    // The others still commit.
    submit_padded(&client, "without", 3, 1024).await;
    std::fs::remove_file(filler).unwrap();
    let back = cluster.restart(2).await;
    agrees_with_leader(&schedulers, &back.node).await;
}

/// Issue #37: a leader whose disk is full stops with an explicit error; the
/// others elect a leader and go on; it comes back as a follower.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs ALGEBRAMESH_TEST_SMALL_DIR; run by tests/integration/run-disk-full.sh"]
async fn disk_full_on_the_leader() {
    let (cluster, mut schedulers, small) = cluster_with_a_small_disk("leader").await;
    lead_or_follow(&schedulers, 2, true).await;
    let client = cluster.client().await;
    submit_padded(&client, "before", 3, 1024).await;
    let filler = fill(&small, 0);
    let victim = schedulers[2].take().unwrap();
    submit_until_stopped(&client, &victim, "full").await;
    let error = victim.stopped_by_itself().await;
    assert!(
        error.contains("raft stopped") && error.contains("full"),
        "{error}"
    );
    let next = serving(&all(&schedulers)).await;
    assert_ne!(next, 2);
    submit_padded(&client, "after", 3, 1024).await;
    std::fs::remove_file(filler).unwrap();
    let back = cluster.restart(2).await;
    agrees_with_leader(&schedulers, &back.node).await;
}

/// Issue #37: a snapshot that does not fit on the receiver's disk is refused
/// chunk by chunk and sent again; once there is space it goes through, without
/// a restart.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs ALGEBRAMESH_TEST_SMALL_DIR; run by tests/integration/run-disk-full.sh"]
async fn disk_full_while_receiving_a_snapshot() {
    let (cluster, mut schedulers, small) = cluster_with_a_small_disk("snapshot").await;
    lead_or_follow(&schedulers, 2, false).await;
    let leader = serving(&all(&schedulers)).await;
    let client = cluster.client().await;
    submit_padded(&client, "before", 2, 1024).await;
    schedulers[2].take().unwrap().stop().await;
    // About 4 MiB of snapshot, of which 1 MiB fits.
    submit_padded(&client, "away", 40, 100 * 1024).await;
    // Both others purge their logs, so whichever leads sends a snapshot.
    for s in schedulers.iter().flatten() {
        s.node.compact_log().await.unwrap();
    }
    let filler = fill(&small, 1 << 20);
    let back = cluster.restart(2).await;
    // Transfers fail on the full disk; the scheduler keeps running.
    tokio::time::sleep(election_timeout() * 20).await;
    assert!(!back.served.is_finished());
    let seq = schedulers[leader].as_ref().unwrap().node.status().last_seq;
    assert!(back.node.status().last_seq < seq);
    std::fs::remove_file(filler).unwrap();
    agrees_with_leader(&schedulers, &back.node).await;
    let inbox = cluster.data_dir(2).join("snapshots");
    let left = std::fs::read_dir(&inbox).map_or(0, |d| d.count());
    assert_eq!(left, 0, "{}", inbox.display());
}

// -------------------------------------------- the network in a transfer (#37)

use std::sync::atomic::Ordering;

/// Issue #37: a snapshot whose connections are cut in the middle, slowed down
/// and paused still arrives whole, in a later transfer; the receiver agrees
/// with the leader and keeps no partial file.
#[tokio::test(flavor = "multi_thread")]
async fn a_snapshot_arrives_through_cuts_delays_and_a_pause() {
    let (mut cluster, listeners) = Cluster::new(3);
    let proxy = Proxy::start(cluster.addrs[2]).await;
    cluster.reached_at.insert(2, proxy.addr);
    let mut schedulers = Vec::new();
    for (i, l) in listeners.into_iter().enumerate() {
        schedulers.push(Some(cluster.start(i, l).await));
    }
    lead_or_follow(&schedulers, 2, false).await;
    serving(&all(&schedulers)).await;
    // Before the cuts are set: the connections that ask for the features
    // would count among the three.
    features_agreed(&schedulers).await;
    let client = cluster.client().await;
    submit_padded(&client, "before", 2, 1024).await;
    schedulers[2].take().unwrap().stop().await;
    // About 2 MiB of snapshot: some 30 chunks of 64 KiB.
    submit_padded(&client, "away", 20, 100 * 1024).await;
    // Both others purge their logs, so whichever leads sends a snapshot.
    for s in schedulers.iter().flatten() {
        s.node.compact_log().await.unwrap();
    }
    // The next three connections to the follower are cut after 300 KiB, and
    // every 16 KiB is delayed.
    proxy.cut_after.store(300 * 1024, Ordering::SeqCst);
    proxy.cuts_left.store(3, Ordering::SeqCst);
    proxy.delay_ms.store(2, Ordering::SeqCst);
    let back = cluster.restart(2).await;
    eventually("three connections are cut", || {
        proxy.cuts.load(Ordering::SeqCst) >= 3
    })
    .await;
    // A pause of a few heartbeats in the middle of the next transfer.
    proxy.paused.send_replace(true);
    tokio::time::sleep(election_timeout() * 3).await;
    proxy.paused.send_replace(false);
    agrees_with_leader(&schedulers, &back.node).await;
    let inbox = cluster.data_dir(2).join("snapshots");
    let left = std::fs::read_dir(&inbox).map_or(0, |d| d.count());
    assert_eq!(left, 0, "{}", inbox.display());
}

/// Issue #37: a node elected leader whose scheduler cannot start (here its
/// working store cannot be created) lets another node lead, instead of
/// holding the term without serving, and goes on as a follower.
#[tokio::test(flavor = "multi_thread")]
async fn a_leader_that_cannot_start_lets_another_lead() {
    let (cluster, listeners) = Cluster::new(3);
    let mut schedulers = Vec::new();
    for (i, l) in listeners.into_iter().enumerate() {
        schedulers.push(Some(cluster.start(i, l).await));
    }
    lead_or_follow(&schedulers, 2, false).await;
    let client = cluster.client().await;
    submit_padded(&client, "before", 2, 1024).await;
    // The working stores of the next terms cannot be created on node 3.
    let dir = cluster.data_dir(2);
    let term = schedulers[2].as_ref().unwrap().node.status().epoch;
    for t in term + 1..term + 20 {
        std::fs::create_dir_all(dir.join(format!("working-{t}.sqlite3"))).unwrap();
    }
    // Node 3 is elected: the leader hands over and the third node holds its
    // elections.
    let node = schedulers[2].as_ref().unwrap().node.clone();
    let leader = serving(&all(&schedulers)).await;
    schedulers[3 - leader - 2]
        .as_ref()
        .unwrap()
        .node
        .hold_elections(election_timeout() * 20);
    schedulers[leader].as_ref().unwrap().node.hand_over();
    eventually("node 3 is elected but does not serve", || {
        let status = node.status();
        status.role == "leader" && !status.serving
    })
    .await;
    // Another node serves, and jobs are accepted.
    let leader = serving(&all(&schedulers)).await;
    assert_ne!(leader, 2);
    submit_padded(&client, "while", 2, 1024).await;
    for t in term + 1..term + 20 {
        let _ = std::fs::remove_dir(dir.join(format!("working-{t}.sqlite3")));
    }
    // Node 3 follows the new leader and agrees with it.
    submit_padded(&client, "after", 2, 1024).await;
    agrees_with_leader(&schedulers, &node).await;
}

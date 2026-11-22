//! The Raft state machine: the scheduler's metadata store. Committed batches of
//! [`Change`](crate::store::Change)s are applied in log order; the last applied
//! log position and the membership are written to the store's `meta` table in the
//! same transaction. A snapshot is a copy of the whole database.

// `StorageError` is fixed by the openraft traits these functions serve.
#![allow(clippy::result_large_err)]

use std::sync::{Arc, Mutex};

use openraft::storage::{RaftStateMachine, Snapshot, SnapshotMeta};
use openraft::{
    AnyError, Entry, EntryPayload, LogId, OptionalSend, RaftSnapshotBuilder, StorageError,
    StorageIOError, StoredMembership,
};
use tokio::sync::watch;

use super::{BatchReply, ChangeBatch, Member, TypeConfig};
use crate::store::Store;

type NodeId = u64;

const APPLIED: &str = "raft_applied";
const MEMBERSHIP: &str = "raft_membership";

fn read_err<E: std::error::Error + 'static>(e: E) -> StorageError<NodeId> {
    StorageIOError::read_state_machine(AnyError::new(&e)).into()
}

fn write_err<E: std::error::Error + 'static>(e: E) -> StorageError<NodeId> {
    StorageIOError::write_state_machine(AnyError::new(&e)).into()
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Runs `f`, turning a panic into a storage error (Issue #37). openraft notices
/// a state machine whose task panicked only when it next sends it a command;
/// meanwhile the node would neither apply nor stop. As an error, Raft stops at
/// once, and the scheduler ends with it.
fn guarded<T>(
    what: &str,
    f: impl FnOnce() -> Result<T, StorageError<NodeId>>,
) -> Result<T, StorageError<NodeId>> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(result) => result,
        Err(panic) => {
            let message = panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                .unwrap_or_default();
            Err(StorageIOError::write_state_machine(AnyError::error(format!(
                "panic while {what}: {message}"
            )))
            .into())
        }
    }
}

/// Why the changes of `batch`, in an entry of `term`, are not applied after the
/// change `last`; `None` if they are.
///
/// * A batch written to the working store of another term: a proposer that
///   outlived its term (the node led again later) must not apply changes made
///   on a copy of an older state.
/// * Changes that do not follow `last` one by one: applying them would break the
///   store, and with it the state machine on every node.
fn refusal(batch: &ChangeBatch, term: u64, last: i64) -> Option<&'static str> {
    if batch.term.is_some_and(|t| t != term) {
        return Some("the batch was written in another term");
    }
    let mut expected = last;
    for (seq, _) in &batch.changes {
        if *seq != expected + 1 {
            return Some("the changes do not follow the last applied change");
        }
        expected = *seq;
    }
    None
}

/// A snapshot kept to send to followers that are behind.
#[derive(Clone)]
struct StoredSnapshot {
    meta: SnapshotMeta<NodeId, Member>,
    data: Vec<u8>,
}

#[derive(Clone)]
pub struct StateMachine {
    store: Arc<Mutex<Store>>,
    /// Sequence number of the last change applied to the store.
    applied_seq: Arc<watch::Sender<i64>>,
    /// The last log position applied to the store, once committed there.
    applied_log: Arc<watch::Sender<Option<LogId<NodeId>>>>,
    current: Arc<Mutex<Option<StoredSnapshot>>>,
}

type Applied = (Option<LogId<NodeId>>, StoredMembership<NodeId, Member>);

impl StateMachine {
    pub fn new(store: Arc<Mutex<Store>>) -> Result<Self, crate::store::StoreError> {
        let (seq, applied) = {
            let s = lock(&store);
            let applied = s
                .meta(APPLIED)?
                .map(|v| serde_json::from_str(&v))
                .transpose()?;
            (s.last_seq()?, applied)
        };
        Ok(Self {
            store,
            applied_seq: Arc::new(watch::channel(seq).0),
            applied_log: Arc::new(watch::channel(applied).0),
            current: Arc::new(Mutex::new(None)),
        })
    }

    /// Follows the last log position applied to the store.
    pub fn applied_log(&self) -> watch::Receiver<Option<LogId<NodeId>>> {
        self.applied_log.subscribe()
    }

    /// Follows the sequence number of the last applied change.
    pub fn applied_seq(&self) -> watch::Receiver<i64> {
        self.applied_seq.subscribe()
    }

    fn read_applied(store: &Store) -> Result<Applied, StorageError<NodeId>> {
        let applied = store
            .meta(APPLIED)
            .map_err(read_err)?
            .map(|v| serde_json::from_str(&v).map_err(read_err))
            .transpose()?;
        let membership = store
            .meta(MEMBERSHIP)
            .map_err(read_err)?
            .map(|v| serde_json::from_str(&v).map_err(read_err))
            .transpose()?
            .unwrap_or_default();
        Ok((applied, membership))
    }

    fn build(&self) -> Result<Snapshot<TypeConfig>, StorageError<NodeId>> {
        let (applied, membership, data) = {
            let store = lock(&self.store);
            let (applied, membership) = Self::read_applied(&store)?;
            (applied, membership, store.snapshot().map_err(read_err)?)
        };
        let snapshot_id = format!(
            "{}-{}",
            applied.map_or(0, |l| l.index),
            uuid::Uuid::new_v4()
        );
        let meta = SnapshotMeta {
            last_log_id: applied,
            last_membership: membership,
            snapshot_id,
        };
        *lock(&self.current) = Some(StoredSnapshot {
            meta: meta.clone(),
            data: data.clone(),
        });
        Ok(Snapshot {
            meta,
            snapshot: Box::new(data),
        })
    }
}

impl RaftSnapshotBuilder<TypeConfig> for StateMachine {
    async fn build_snapshot(&mut self) -> Result<Snapshot<TypeConfig>, StorageError<NodeId>> {
        guarded("building a snapshot", || self.build())
    }
}

impl RaftStateMachine<TypeConfig> for StateMachine {
    type SnapshotBuilder = Self;

    async fn applied_state(&mut self) -> Result<Applied, StorageError<NodeId>> {
        Self::read_applied(&lock(&self.store))
    }

    async fn apply<I>(&mut self, entries: I) -> Result<Vec<BatchReply>, StorageError<NodeId>>
    where
        I: IntoIterator<Item = Entry<TypeConfig>> + OptionalSend,
        I::IntoIter: OptionalSend,
    {
        let entries: Vec<Entry<TypeConfig>> = entries.into_iter().collect();
        guarded("applying entries", || self.apply_entries(entries))
    }

    async fn get_snapshot_builder(&mut self) -> Self::SnapshotBuilder {
        self.clone()
    }

    async fn begin_receiving_snapshot(&mut self) -> Result<Box<Vec<u8>>, StorageError<NodeId>> {
        Ok(Box::default())
    }

    async fn install_snapshot(
        &mut self,
        meta: &SnapshotMeta<NodeId, Member>,
        snapshot: Box<Vec<u8>>,
    ) -> Result<(), StorageError<NodeId>> {
        guarded("installing a snapshot", || self.install(meta, *snapshot))
    }

    async fn get_current_snapshot(
        &mut self,
    ) -> Result<Option<Snapshot<TypeConfig>>, StorageError<NodeId>> {
        if let Some(s) = lock(&self.current).clone() {
            return Ok(Some(Snapshot {
                meta: s.meta,
                snapshot: Box::new(s.data),
            }));
        }
        // After a restart: build one from the store if anything was applied.
        if Self::read_applied(&lock(&self.store))?.0.is_none() {
            return Ok(None);
        }
        guarded("building a snapshot", || self.build()).map(Some)
    }
}

impl StateMachine {
    fn apply_entries(
        &self,
        entries: Vec<Entry<TypeConfig>>,
    ) -> Result<Vec<BatchReply>, StorageError<NodeId>> {
        let store = lock(&self.store);
        let (_, mut membership) = Self::read_applied(&store)?;
        let mut replies = Vec::new();
        for entry in entries {
            let mut applied = true;
            let changes = match entry.payload {
                EntryPayload::Blank => Vec::new(),
                EntryPayload::Normal(batch) => {
                    let last = store.last_seq().map_err(read_err)?;
                    match refusal(&batch, entry.log_id.leader_id.term, last) {
                        None => batch.changes,
                        Some(reason) => {
                            // Decided from the log and the state alone, so every
                            // node skips the same entries.
                            tracing::warn!(log_id = %entry.log_id, reason, "entry applied as no change");
                            applied = false;
                            Vec::new()
                        }
                    }
                }
                EntryPayload::Membership(m) => {
                    membership = StoredMembership::new(Some(entry.log_id), m);
                    Vec::new()
                }
            };
            let meta = [
                (
                    APPLIED,
                    serde_json::to_string(&entry.log_id).map_err(write_err)?,
                ),
                (
                    MEMBERSHIP,
                    serde_json::to_string(&membership).map_err(write_err)?,
                ),
            ];
            store.apply_batch(&changes, &meta).map_err(|e| {
                StorageError::from(StorageIOError::apply(entry.log_id, AnyError::new(&e)))
            })?;
            replies.push(BatchReply { applied });
            self.applied_log.send_replace(Some(entry.log_id));
        }
        self.applied_seq
            .send_replace(store.last_seq().map_err(read_err)?);
        Ok(replies)
    }

    fn install(
        &self,
        meta: &SnapshotMeta<NodeId, Member>,
        snapshot: Vec<u8>,
    ) -> Result<(), StorageError<NodeId>> {
        let seq = {
            let mut store = lock(&self.store);
            store.restore(&snapshot).map_err(write_err)?;
            if let Some(path) = store.path() {
                crate::fault::point(path, crate::fault::INSTALL_RESTORED);
            }
            store
                .apply_batch(
                    &[],
                    &[
                        (
                            APPLIED,
                            serde_json::to_string(&meta.last_log_id).map_err(write_err)?,
                        ),
                        (
                            MEMBERSHIP,
                            serde_json::to_string(&meta.last_membership).map_err(write_err)?,
                        ),
                    ],
                )
                .map_err(write_err)?;
            store.last_seq().map_err(read_err)?
        };
        *lock(&self.current) = Some(StoredSnapshot {
            meta: meta.clone(),
            data: snapshot,
        });
        self.applied_seq.send_replace(seq);
        self.applied_log.send_replace(meta.last_log_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use openraft::{CommittedLeaderId, LogId};

    use super::*;
    use crate::scheduler::{Scheduler, SchedulerOptions};

    fn entry(term: u64, index: u64, batch: ChangeBatch) -> Entry<TypeConfig> {
        Entry {
            log_id: LogId::new(CommittedLeaderId::new(term, 1), index),
            payload: EntryPayload::Normal(batch),
        }
    }

    /// The changes a scheduler writes for one submission on a copy of `state`.
    fn changes(state: &Arc<Mutex<Store>>) -> Vec<(i64, String)> {
        let mut store = Store::open(None, None).unwrap();
        store.restore(&lock(state).snapshot().unwrap()).unwrap();
        let (sink, mut rx) = tokio::sync::mpsc::unbounded_channel();
        store.set_change_sink(Some(sink));
        let scheduler = Scheduler::new(store, SchedulerOptions::default()).unwrap();
        let spec = serde_json::json!({
            "schema_version": 1, "backend": "test", "operation": "test.op", "arguments": {},
        });
        scheduler.submit(&spec.to_string(), 0, "").unwrap();
        let mut out = Vec::new();
        while let Ok(c) = rx.try_recv() {
            out.push(c);
        }
        assert!(!out.is_empty());
        out
    }

    #[tokio::test]
    async fn batches_of_another_term_or_out_of_order_are_applied_as_no_change() {
        let store = Arc::new(Mutex::new(Store::open(None, None).unwrap()));
        let mut sm = StateMachine::new(store.clone()).unwrap();
        let first = changes(&store);
        let n = first.len() as i64;

        // A batch written in term 3 is not applied in an entry of term 5.
        let stale = ChangeBatch {
            changes: first.clone(),
            term: Some(3),
        };
        let replies = sm.apply([entry(5, 1, stale)]).await.unwrap();
        assert_eq!(replies, [BatchReply { applied: false }]);
        assert_eq!(lock(&store).last_seq().unwrap(), 0);

        // The same batch in an entry of its own term is.
        let own = ChangeBatch {
            changes: first.clone(),
            term: Some(5),
        };
        let replies = sm.apply([entry(5, 2, own)]).await.unwrap();
        assert_eq!(replies, [BatchReply { applied: true }]);
        assert_eq!(lock(&store).last_seq().unwrap(), n);

        // The same sequence numbers again (another leader's changes under the
        // same numbers) do not follow the last change: no change, no error.
        let again = ChangeBatch {
            changes: first,
            term: Some(6),
        };
        let replies = sm.apply([entry(6, 3, again)]).await.unwrap();
        assert_eq!(replies, [BatchReply { applied: false }]);
        assert_eq!(lock(&store).last_seq().unwrap(), n);

        // An entry from before the field existed is applied if it follows.
        let legacy = ChangeBatch {
            changes: changes(&store),
            term: None,
        };
        let replies = sm.apply([entry(6, 4, legacy)]).await.unwrap();
        assert_eq!(replies, [BatchReply { applied: true }]);
        assert!(lock(&store).last_seq().unwrap() > n);
        // The skipped entries are still recorded as applied log positions.
        let (applied, _) = StateMachine::read_applied(&lock(&store)).unwrap();
        assert_eq!(applied.map(|l| l.index), Some(4));
    }

    #[test]
    fn entries_written_before_the_term_field_still_decode() {
        let old = r#"{"changes":[[1,"{}"]]}"#;
        let batch: ChangeBatch = serde_json::from_str(old).unwrap();
        assert_eq!(batch.term, None);
    }
}

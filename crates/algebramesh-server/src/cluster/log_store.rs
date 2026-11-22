//! The Raft log, the vote and the committed position in SQLite (`raft.sqlite3`).
//! Every write is committed with `synchronous = FULL` before Raft is told it is
//! durable.

// `StorageError` is fixed by the openraft traits these functions serve.
#![allow(clippy::result_large_err)]

use std::fmt::Debug;
use std::ops::{Bound, RangeBounds};
use std::path::Path;
use std::sync::{Arc, Mutex};

use openraft::storage::{LogFlushed, LogState, RaftLogStorage};
use openraft::{
    AnyError, Entry, LogId, OptionalSend, RaftLogReader, StorageError, StorageIOError, Vote,
};
use rusqlite::{Connection, OptionalExtension, params};

use super::TypeConfig;

type NodeId = u64;

#[derive(Clone)]
pub struct LogStore {
    conn: Arc<Mutex<Connection>>,
    path: Option<std::path::PathBuf>,
    /// The last position the state machine has applied, if known: a purge
    /// waits for it (Issue #37).
    applied: Option<tokio::sync::watch::Receiver<Option<LogId<NodeId>>>>,
}

fn read_err<E: std::error::Error + 'static>(e: E) -> StorageError<NodeId> {
    StorageIOError::read_logs(AnyError::new(&e)).into()
}

fn write_err<E: std::error::Error + 'static>(e: E) -> StorageError<NodeId> {
    StorageIOError::write_logs(AnyError::new(&e)).into()
}

impl LogStore {
    /// Opens (or creates) the log at `path`; `None` keeps it in memory, for tests.
    pub fn open(path: Option<&Path>) -> Result<Self, rusqlite::Error> {
        let conn = match path {
            Some(p) => {
                let conn = Connection::open(p)?;
                conn.pragma_update(None, "journal_mode", "WAL")?;
                conn.pragma_update(None, "synchronous", "FULL")?;
                conn
            }
            None => Connection::open_in_memory()?,
        };
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS log (idx INTEGER PRIMARY KEY, entry_json TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS kv (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            path: path.map(Path::to_path_buf),
            applied: None,
        })
    }

    /// Purges only what the state machine has applied, as `applied` reports.
    pub fn with_applied(
        mut self,
        applied: tokio::sync::watch::Receiver<Option<LogId<NodeId>>>,
    ) -> Self {
        self.applied = Some(applied);
        self
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn get<T: serde::de::DeserializeOwned>(
        &self,
        key: &str,
    ) -> Result<Option<T>, StorageError<NodeId>> {
        let value: Option<String> = self
            .lock()
            .query_row("SELECT value FROM kv WHERE key = ?1", params![key], |r| {
                r.get(0)
            })
            .optional()
            .map_err(read_err)?;
        value
            .map(|v| serde_json::from_str(&v).map_err(read_err))
            .transpose()
    }

    fn put<T: serde::Serialize>(&self, key: &str, value: &T) -> Result<(), StorageError<NodeId>> {
        let json = serde_json::to_string(value).map_err(write_err)?;
        self.lock()
            .execute(
                "INSERT OR REPLACE INTO kv (key, value) VALUES (?1, ?2)",
                params![key, json],
            )
            .map_err(write_err)?;
        Ok(())
    }
}

/// SQLite integers are signed; indexes beyond `i64::MAX` do not occur.
fn sql_index(i: u64) -> i64 {
    i64::try_from(i).unwrap_or(i64::MAX)
}

impl RaftLogReader<TypeConfig> for LogStore {
    async fn try_get_log_entries<RB: RangeBounds<u64> + Clone + Debug + OptionalSend>(
        &mut self,
        range: RB,
    ) -> Result<Vec<Entry<TypeConfig>>, StorageError<NodeId>> {
        let start = match range.start_bound() {
            Bound::Included(i) => sql_index(*i),
            Bound::Excluded(i) => sql_index(*i).saturating_add(1),
            Bound::Unbounded => 0,
        };
        let end = match range.end_bound() {
            Bound::Included(i) => sql_index(*i).saturating_add(1),
            Bound::Excluded(i) => sql_index(*i),
            Bound::Unbounded => i64::MAX,
        };
        let conn = self.lock();
        let mut stmt = conn
            .prepare("SELECT entry_json FROM log WHERE idx >= ?1 AND idx < ?2 ORDER BY idx")
            .map_err(read_err)?;
        let rows = stmt
            .query_map(params![start, end], |r| r.get::<_, String>(0))
            .map_err(read_err)?;
        rows.map(|r| {
            let json = r.map_err(read_err)?;
            serde_json::from_str(&json).map_err(read_err)
        })
        .collect()
    }
}

impl RaftLogStorage<TypeConfig> for LogStore {
    type LogReader = Self;

    async fn get_log_state(&mut self) -> Result<LogState<TypeConfig>, StorageError<NodeId>> {
        let last_purged: Option<LogId<NodeId>> = self.get("last_purged")?;
        let last: Option<String> = self
            .lock()
            .query_row(
                "SELECT entry_json FROM log ORDER BY idx DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(read_err)?;
        let last_log_id = match last {
            Some(json) => {
                let entry: Entry<TypeConfig> = serde_json::from_str(&json).map_err(read_err)?;
                Some(entry.log_id)
            }
            None => last_purged,
        };
        Ok(LogState {
            last_purged_log_id: last_purged,
            last_log_id,
        })
    }

    async fn get_log_reader(&mut self) -> Self::LogReader {
        self.clone()
    }

    async fn save_vote(&mut self, vote: &Vote<NodeId>) -> Result<(), StorageError<NodeId>> {
        self.put("vote", vote)
    }

    async fn read_vote(&mut self) -> Result<Option<Vote<NodeId>>, StorageError<NodeId>> {
        self.get("vote")
    }

    async fn save_committed(
        &mut self,
        committed: Option<LogId<NodeId>>,
    ) -> Result<(), StorageError<NodeId>> {
        self.put("committed", &committed)
    }

    async fn read_committed(&mut self) -> Result<Option<LogId<NodeId>>, StorageError<NodeId>> {
        Ok(self.get::<Option<LogId<NodeId>>>("committed")?.flatten())
    }

    async fn append<I>(
        &mut self,
        entries: I,
        callback: LogFlushed<TypeConfig>,
    ) -> Result<(), StorageError<NodeId>>
    where
        I: IntoIterator<Item = Entry<TypeConfig>> + OptionalSend,
        I::IntoIter: OptionalSend,
    {
        {
            let mut conn = self.lock();
            let tx = conn.transaction().map_err(write_err)?;
            for entry in entries {
                let json = serde_json::to_string(&entry).map_err(write_err)?;
                tx.execute(
                    "INSERT OR REPLACE INTO log (idx, entry_json) VALUES (?1, ?2)",
                    params![sql_index(entry.log_id.index), json],
                )
                .map_err(write_err)?;
            }
            tx.commit().map_err(write_err)?;
        }
        callback.log_io_completed(Ok(()));
        Ok(())
    }

    async fn truncate(&mut self, log_id: LogId<NodeId>) -> Result<(), StorageError<NodeId>> {
        self.lock()
            .execute(
                "DELETE FROM log WHERE idx >= ?1",
                params![sql_index(log_id.index)],
            )
            .map_err(write_err)?;
        Ok(())
    }

    /// Records the purged position and deletes the entries up to it in one
    /// transaction (Issue #37): a crash leaves both or neither.
    ///
    /// A follower that is sent a snapshot is asked by openraft to purge the
    /// log up to it at the same time as the state machine is asked to install
    /// it, without waiting for the install. A crash in between would leave the
    /// log purged past what the state machine holds, from which openraft
    /// cannot start. The purge therefore waits until the state machine has
    /// applied the position (Issue #37): on disk, the purged position is never
    /// past the applied one.
    async fn purge(&mut self, log_id: LogId<NodeId>) -> Result<(), StorageError<NodeId>> {
        if let Some(applied) = &self.applied {
            let mut applied = applied.clone();
            applied
                .wait_for(|a| a.as_ref().is_some_and(|a| a.index >= log_id.index))
                .await
                .map_err(|e| {
                    write_err(std::io::Error::other(format!(
                        "the state machine stopped before applying {log_id}: {e}"
                    )))
                })?;
        }
        let json = serde_json::to_string(&log_id).map_err(write_err)?;
        {
            let mut conn = self.lock();
            let tx = conn.transaction().map_err(write_err)?;
            tx.execute(
                "INSERT OR REPLACE INTO kv (key, value) VALUES ('last_purged', ?1)",
                params![json],
            )
            .map_err(write_err)?;
            tx.execute(
                "DELETE FROM log WHERE idx <= ?1",
                params![sql_index(log_id.index)],
            )
            .map_err(write_err)?;
            tx.commit().map_err(write_err)?;
        }
        if let Some(path) = &self.path {
            crate::fault::point(path, crate::fault::LOG_PURGED);
        }
        Ok(())
    }
}

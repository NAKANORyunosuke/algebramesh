//! Points at which a test stops a scheduler as a crash would (Issue #37).
//!
//! With the feature `fault-injection`, a test arms a point for the files under
//! one directory (the data directory of one scheduler). The next time the code
//! passes that point for a file there, it panics, so that nothing after the
//! point runs, as after a crash of the process; the files are left as they are
//! at that moment. The test then starts the scheduler again over the same
//! directory. Without the feature every point is empty.

use std::path::Path;

/// After `VACUUM INTO` wrote a copy of the database for a snapshot, before it
/// is read and removed.
pub const SNAPSHOT_COPIED: &str = "snapshot.copied";
/// After the new database of a restore is written and synced, before it is
/// checked.
pub const RESTORE_STAGED: &str = "restore.staged";
/// After the old database is checkpointed and closed, and its WAL removed,
/// before the new one is renamed over it.
pub const RESTORE_CLOSED: &str = "restore.closed";
/// After the rename, before the directory is synced and the database opened.
pub const RESTORE_RENAMED: &str = "restore.renamed";
/// After a snapshot replaced the state machine's database, before the log
/// position and the membership are written.
pub const INSTALL_RESTORED: &str = "install.restored";
/// After the log up to a snapshot was purged.
pub const LOG_PURGED: &str = "log.purged";
/// After a received blob is checked and synced, before it is renamed into
/// place (ADR 0021).
pub const BLOB_RENAME: &str = "blob.rename";
/// On the leader, after a blob was pushed to another member, before that is
/// recorded.
pub const BLOB_PUSHED_ONE: &str = "blob.pushed_one";
/// On the leader, once a majority holds a blob, before `PutBlob` answers.
pub const BLOB_DURABLE: &str = "blob.durable";

#[cfg(feature = "fault-injection")]
mod armed {
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    static ARMED: Mutex<Vec<(PathBuf, &'static str)>> = Mutex::new(Vec::new());
    static HIT: Mutex<Vec<(PathBuf, &'static str)>> = Mutex::new(Vec::new());

    fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
        m.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Arms `point` for the files under `dir`, once.
    pub fn arm(dir: &Path, point: &'static str) {
        lock(&ARMED).push((dir.to_path_buf(), point));
    }

    /// Disarms every point under `dir`.
    pub fn disarm(dir: &Path) {
        lock(&ARMED).retain(|(d, _)| d != dir);
    }

    /// The points hit under `dir` so far.
    pub fn hits(dir: &Path) -> Vec<&'static str> {
        lock(&HIT)
            .iter()
            .filter(|(d, _)| d == dir)
            .map(|(_, p)| *p)
            .collect()
    }

    pub(crate) fn point(path: &Path, name: &'static str) {
        let dir = {
            let mut armed = lock(&ARMED);
            let Some(i) = armed
                .iter()
                .position(|(d, p)| *p == name && path.starts_with(d))
            else {
                return;
            };
            armed.remove(i).0
        };
        lock(&HIT).push((dir, name));
        panic!("injected crash at {name} ({})", path.display());
    }
}

#[cfg(feature = "fault-injection")]
pub use armed::{arm, disarm, hits};

/// Passes the point `name` for the file `path`: a crash there if a test armed
/// it.
#[allow(unused_variables)]
pub(crate) fn point(path: &Path, name: &'static str) {
    #[cfg(feature = "fault-injection")]
    armed::point(path, name);
}

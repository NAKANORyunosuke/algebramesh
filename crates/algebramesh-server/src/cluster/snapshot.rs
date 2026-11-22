//! Snapshots received in pieces (Issue #20).
//!
//! A snapshot is the whole metadata database, which outgrows one gRPC message.
//! The leader sends it in chunks of a transfer, then the snapshot's metadata
//! with the transfer's size and SHA-256. The follower writes the chunks, in
//! order, to a file of the transfer, and installs the snapshot only when the
//! file is complete and its digest matches. A transfer that is interrupted
//! leaves a partial file, which the next transfer replaces.

use std::io::Write;
use std::path::{Path, PathBuf};

const PREFIX: &str = "snapshot-in-";

/// Where the chunks of incoming snapshots are put together.
pub struct SnapshotInbox {
    dir: PathBuf,
}

impl SnapshotInbox {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The file of a transfer. The id comes from another scheduler: only a UUID
    /// is accepted, so that it cannot name another path.
    fn file(&self, transfer_id: &str) -> Result<PathBuf, String> {
        uuid::Uuid::parse_str(transfer_id)
            .map_err(|_| format!("transfer id {transfer_id:?} is not a UUID"))?;
        Ok(self.dir.join(format!("{PREFIX}{transfer_id}.part")))
    }

    /// Appends `data` at `offset` of the transfer. Chunks must come in order:
    /// the first at offset 0, each next one where the previous ended. The first
    /// chunk of a transfer removes the files of earlier transfers.
    pub fn write(&self, transfer_id: &str, offset: u64, data: &[u8]) -> Result<(), String> {
        let file = self.file(transfer_id)?;
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        if offset == 0 {
            self.remove_transfers();
        }
        let have = std::fs::metadata(&file).map_or(0, |m| m.len());
        if have != offset {
            return Err(format!(
                "chunk at offset {offset}, but {have} bytes of transfer {transfer_id} are here"
            ));
        }
        let mut out = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&file)
            .map_err(|e| e.to_string())?;
        out.write_all(data).map_err(|e| e.to_string())
    }

    /// The database of a complete transfer, after checking its size and
    /// digest. The transfer's file is removed in every case.
    pub fn take(&self, transfer_id: &str, size: u64, sha256: &str) -> Result<Vec<u8>, String> {
        let file = self.file(transfer_id)?;
        let data = std::fs::read(&file);
        let _ = std::fs::remove_file(&file);
        let data = data.map_err(|e| format!("transfer {transfer_id}: {e}"))?;
        if data.len() as u64 != size {
            return Err(format!(
                "transfer {transfer_id} has {} bytes, not {size}",
                data.len()
            ));
        }
        let digest = algebramesh_core::sha256_hex(&data);
        if digest != sha256 {
            return Err(format!(
                "transfer {transfer_id} has SHA-256 {digest}, not {sha256}"
            ));
        }
        Ok(data)
    }

    /// Removes the files of all transfers (partial ones left by an
    /// interruption).
    pub fn remove_transfers(&self) {
        remove_transfers(&self.dir);
    }
}

fn remove_transfers(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with(PREFIX) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    /// An inbox in a temporary directory, removed when the guard is dropped,
    /// also when the test fails.
    fn inbox() -> (SnapshotInbox, PathBuf, tempfile::TempDir) {
        let temp = tempfile::Builder::new()
            .prefix("am-inbox-")
            .tempdir()
            .unwrap();
        let dir = temp.path().to_path_buf();
        (SnapshotInbox::new(dir.clone()), dir, temp)
    }

    fn id() -> String {
        uuid::Uuid::new_v4().to_string()
    }

    #[test]
    fn chunks_in_order_make_the_database() {
        let (inbox, _, _temp) = inbox();
        let data: Vec<u8> = (0..1_000_000u32).map(|i| (i % 251) as u8).collect();
        let t = id();
        for (k, piece) in data.chunks(65_536).enumerate() {
            inbox.write(&t, (k * 65_536) as u64, piece).unwrap();
        }
        let sha = algebramesh_core::sha256_hex(&data);
        assert_eq!(inbox.take(&t, data.len() as u64, &sha).unwrap(), data);
        // The file is gone.
        assert!(inbox.take(&t, data.len() as u64, &sha).is_err());
    }

    #[test]
    fn missing_reordered_or_corrupt_chunks_are_refused() {
        let (inbox, _, _temp) = inbox();
        let data = vec![7u8; 300];
        let sha = algebramesh_core::sha256_hex(&data);
        // A gap.
        let t = id();
        inbox.write(&t, 0, &data[..100]).unwrap();
        assert!(inbox.write(&t, 200, &data[200..]).is_err());
        // Cut short: the size does not match.
        assert!(inbox.take(&t, 300, &sha).unwrap_err().contains("not 300"));
        // A changed byte: the digest does not match.
        let t = id();
        let mut bad = data.clone();
        bad[150] = 8;
        inbox.write(&t, 0, &bad).unwrap();
        assert!(inbox.take(&t, 300, &sha).unwrap_err().contains("SHA-256"));
        // A transfer id that is not a UUID cannot name a path.
        assert!(inbox.write("../../etc/passwd", 0, b"x").is_err());
    }

    #[test]
    fn a_new_transfer_replaces_an_interrupted_one() {
        let (inbox, dir, _temp) = inbox();
        let old = id();
        inbox.write(&old, 0, b"partial").unwrap();
        let new = id();
        inbox.write(&new, 0, b"whole").unwrap();
        assert!(!dir.join(format!("{PREFIX}{old}.part")).exists());
        let sha = algebramesh_core::sha256_hex(b"whole");
        assert_eq!(inbox.take(&new, 5, &sha).unwrap(), b"whole");
    }

    /// Issue #37: a late first chunk of an old transfer, arriving while a
    /// newer one is under way, cannot make a wrong database: the newer
    /// transfer fails and is sent again, and only a whole transfer whose size
    /// and digest match is taken.
    #[test]
    fn a_late_chunk_of_an_old_transfer_breaks_nothing() {
        let (inbox, dir, _temp) = inbox();
        let data: Vec<u8> = (0..200u8).collect();
        let sha = algebramesh_core::sha256_hex(&data);
        let (old, new) = (id(), id());
        inbox.write(&new, 0, &data[..100]).unwrap();
        // The old transfer's first chunk, delayed on the network.
        inbox.write(&old, 0, b"stale").unwrap();
        // The newer transfer cannot go on where it was ...
        assert!(inbox.write(&new, 100, &data[100..]).is_err());
        assert!(inbox.take(&new, 200, &sha).is_err());
        // ... and the old one is not taken for the snapshot.
        assert!(inbox.take(&old, 200, &sha).is_err());
        // The leader sends it again, whole.
        let again = id();
        inbox.write(&again, 0, &data[..100]).unwrap();
        inbox.write(&again, 100, &data[100..]).unwrap();
        assert_eq!(inbox.take(&again, 200, &sha).unwrap(), data);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
    }
}

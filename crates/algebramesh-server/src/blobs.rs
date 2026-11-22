//! The blobs one scheduler holds on its disk (ADR 0021 §5).
//!
//! * `<root>/sha256/<2 hex digits>/<62 hex digits>`: the bodies.
//! * `<root>/incoming/<uuid>.part`: transfers being received, one file each.
//! * `<root>/quarantine/`: bodies whose bytes no longer match their name.
//!
//! A body is written as `record.json` is (#37): to a file of its own, hashed
//! while it is written, synced every [`SYNC_EVERY`] bytes and at the end,
//! renamed into place, and the directory synced. A body that is already there
//! is not replaced. The time a body was last used is its modification time,
//! which survives restarts; unreferenced bodies untouched for long are removed
//! (`remove_untouched`).
//!
//! Everything here blocks: callers run it in `spawn_blocking`.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use algebramesh_core::blob::{BlobId, BlobRef};
use sha2::{Digest, Sha256};

/// A body being received is synced to disk every this many bytes.
const SYNC_EVERY: u64 = 64 * 1024 * 1024;

/// Settings of the blob store of a scheduler (ADR 0021 §11).
#[derive(Debug, Clone)]
pub struct BlobOptions {
    /// Where the bodies are kept; by default `<data_dir>/blobs`, or a
    /// temporary directory for a scheduler without a data directory.
    pub dir: Option<PathBuf>,
    pub max_blob_bytes: u64,
    /// 0: no bound.
    pub max_store_bytes: u64,
    pub min_free_bytes: u64,
    /// An unreferenced body untouched for this long is removed.
    pub orphan_grace: Duration,
    /// How often the store looks for such bodies.
    pub collect_every: Duration,
    /// Bandwidth of the pushes to other schedulers; 0: not limited.
    pub replication_bytes_per_second: u64,
    /// The bytes each client token may upload in an hour (B3); 0: no bound.
    pub upload_bytes_per_token_per_hour: u64,
}

/// The default of [`BlobOptions::min_free_bytes`].
pub const DEFAULT_MIN_FREE_BYTES: u64 = 1024 * 1024 * 1024;
/// The default of [`BlobOptions::orphan_grace`].
pub const DEFAULT_ORPHAN_GRACE: Duration = Duration::from_secs(24 * 3600);

impl Default for BlobOptions {
    fn default() -> Self {
        Self {
            dir: None,
            max_blob_bytes: algebramesh_core::blob::DEFAULT_MAX_BLOB_BYTES,
            max_store_bytes: 0,
            min_free_bytes: DEFAULT_MIN_FREE_BYTES,
            orphan_grace: DEFAULT_ORPHAN_GRACE,
            collect_every: Duration::from_secs(600),
            replication_bytes_per_second: 0,
            upload_bytes_per_token_per_hour: 0,
        }
    }
}

impl BlobOptions {
    pub fn limits(&self) -> DiskLimits {
        DiskLimits {
            max_blob_bytes: self.max_blob_bytes,
            max_store_bytes: self.max_store_bytes,
            min_free_bytes: self.min_free_bytes,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DiskLimits {
    /// The largest blob accepted.
    pub max_blob_bytes: u64,
    /// The bytes of all bodies at most; 0 for no bound.
    pub max_store_bytes: u64,
    /// Transfers are refused while less than this is free on the volume.
    pub min_free_bytes: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum DiskError {
    #[error("the blob of {size} bytes exceeds the limit of {limit} bytes")]
    TooLarge { size: u64, limit: u64 },
    #[error("{0}")]
    Exhausted(String),
    #[error("the bytes received do not match {expected}: {detail}")]
    Mismatch { expected: BlobId, detail: String },
    #[error("no blob {0} on this scheduler")]
    Missing(BlobId),
    #[error("blob store I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl DiskError {
    pub fn status(&self) -> tonic::Status {
        match self {
            Self::TooLarge { .. } | Self::Exhausted(_) => {
                tonic::Status::resource_exhausted(self.to_string())
            }
            Self::Mismatch { .. } => tonic::Status::invalid_argument(self.to_string()),
            Self::Missing(_) => tonic::Status::not_found(self.to_string()),
            Self::Io(_) => tonic::Status::internal(self.to_string()),
        }
    }
}

pub struct BlobDisk {
    root: PathBuf,
    limits: DiskLimits,
    /// One lock per blob that is being finished or removed, so that a removal
    /// does not race with the body being written or touched again.
    locks: Mutex<HashMap<BlobId, Arc<Mutex<()>>>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl BlobDisk {
    /// The directories are made when the first blob arrives, so that a
    /// scheduler that never stores one leaves nothing behind.
    pub fn new(root: PathBuf, limits: DiskLimits) -> Self {
        Self {
            root,
            limits,
            locks: Mutex::new(HashMap::new()),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn limits(&self) -> &DiskLimits {
        &self.limits
    }

    pub fn path(&self, id: &BlobId) -> PathBuf {
        let hex = id.hex();
        self.root.join("sha256").join(&hex[..2]).join(&hex[2..])
    }

    fn incoming(&self) -> PathBuf {
        self.root.join("incoming")
    }

    fn quarantine_dir(&self) -> PathBuf {
        self.root.join("quarantine")
    }

    fn blob_lock(&self, id: &BlobId) -> Arc<Mutex<()>> {
        let mut locks = lock(&self.locks);
        locks.retain(|_, l| Arc::strong_count(l) > 1);
        locks.entry(id.clone()).or_default().clone()
    }

    /// The size of the body of `id`, if this scheduler holds it.
    pub fn size(&self, id: &BlobId) -> Option<u64> {
        fs::metadata(self.path(id))
            .ok()
            .filter(|m| m.is_file())
            .map(|m| m.len())
    }

    pub fn has(&self, id: &BlobId) -> bool {
        self.size(id).is_some()
    }

    /// Marks the body of `id` as used now, if it is here.
    pub fn touch(&self, id: &BlobId) {
        let _guard = self.blob_lock(id);
        let _guard = lock(&_guard);
        if let Ok(f) = File::options().write(true).open(self.path(id)) {
            let _ = f.set_modified(SystemTime::now());
        }
    }

    /// The free bytes of the volume of the store.
    pub fn free_bytes(&self) -> std::io::Result<u64> {
        let mut dir = self.root.as_path();
        // The root may not exist yet: ask about the nearest existing parent.
        while !dir.exists() {
            match dir.parent() {
                Some(p) if !p.as_os_str().is_empty() => dir = p,
                _ => break,
            }
        }
        fs4::available_space(dir)
    }

    /// The bodies here, with their sizes and the times they were last used.
    pub fn list(&self) -> Vec<(BlobRef, SystemTime)> {
        let mut found = Vec::new();
        let Ok(prefixes) = fs::read_dir(self.root.join("sha256")) else {
            return found;
        };
        for prefix in prefixes.flatten() {
            let Ok(files) = fs::read_dir(prefix.path()) else {
                continue;
            };
            let head = prefix.file_name().to_string_lossy().into_owned();
            for file in files.flatten() {
                let name = format!("sha256:{head}{}", file.file_name().to_string_lossy());
                let (Ok(id), Ok(meta)) = (BlobId::parse(&name), file.metadata()) else {
                    continue;
                };
                found.push((
                    BlobRef {
                        id,
                        size: meta.len(),
                    },
                    meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                ));
            }
        }
        found.sort();
        found
    }

    /// The bytes of all bodies.
    pub fn stored_bytes(&self) -> u64 {
        self.list().iter().map(|(r, _)| r.size).sum()
    }

    /// The files in quarantine.
    pub fn quarantined(&self) -> usize {
        fs::read_dir(self.quarantine_dir()).map_or(0, |d| d.flatten().count())
    }

    /// Starts receiving the blob `id` of `size` bytes. Refused if it is too
    /// large, the volume is nearly full, or the store is at its bound.
    pub fn begin(&self, id: &BlobId, size: u64) -> Result<Incoming, DiskError> {
        if size > self.limits.max_blob_bytes {
            return Err(DiskError::TooLarge {
                size,
                limit: self.limits.max_blob_bytes,
            });
        }
        let free = self.free_bytes()?;
        if free < self.limits.min_free_bytes.saturating_add(size) {
            return Err(DiskError::Exhausted(format!(
                "only {free} bytes are free for the blob store, which keeps {} free",
                self.limits.min_free_bytes
            )));
        }
        if self.limits.max_store_bytes > 0 {
            let stored = self.stored_bytes();
            if stored.saturating_add(size) > self.limits.max_store_bytes {
                return Err(DiskError::Exhausted(format!(
                    "the blob store holds {stored} bytes; {size} more would exceed its bound of \
                     {} bytes",
                    self.limits.max_store_bytes
                )));
            }
        }
        fs::create_dir_all(self.incoming())?;
        let part = self
            .incoming()
            .join(format!("{}.part", uuid::Uuid::new_v4()));
        let file = File::options().write(true).create_new(true).open(&part)?;
        Ok(Incoming {
            file: Some(file),
            part,
            id: id.clone(),
            size,
            written: 0,
            unsynced: 0,
            hasher: Sha256::new(),
        })
    }

    /// Puts a fully received body in place (see the module documentation).
    pub fn finish(&self, mut incoming: Incoming) -> Result<BlobRef, DiskError> {
        let mismatch = |detail: String| DiskError::Mismatch {
            expected: incoming.id.clone(),
            detail,
        };
        if incoming.written != incoming.size {
            return Err(mismatch(format!(
                "{} bytes instead of {}",
                incoming.written, incoming.size
            )));
        }
        let digest: [u8; 32] = std::mem::take(&mut incoming.hasher).finalize().into();
        let got = BlobId::from_digest(digest);
        if got != incoming.id {
            return Err(mismatch(format!("their SHA-256 is {got}")));
        }
        let file = incoming.file.take().ok_or_else(|| {
            DiskError::Io(std::io::Error::other("the transfer was already finished"))
        })?;
        file.sync_all()?;
        drop(file);
        crate::fault::point(&self.root, crate::fault::BLOB_RENAME);
        let target = self.path(&incoming.id);
        let guard = self.blob_lock(&incoming.id);
        let _guard = lock(&guard);
        if target.is_file() {
            // Already here (another transfer of the same bytes): the body in
            // place may be being read, so it is not replaced.
            let _ = fs::remove_file(&incoming.part);
        } else {
            let dir = target.parent().ok_or_else(|| {
                DiskError::Io(std::io::Error::other("a blob path has no directory"))
            })?;
            fs::create_dir_all(dir)?;
            fs::rename(&incoming.part, &target)?;
            crate::store::sync_directory(&target);
        }
        incoming.part = PathBuf::new();
        if let Ok(f) = File::options().write(true).open(&target) {
            let _ = f.set_modified(SystemTime::now());
        }
        Ok(BlobRef {
            id: incoming.id.clone(),
            size: incoming.size,
        })
    }

    /// Like [`finish`](Self::finish) for a body whose size was not known when
    /// it was begun (a fetch from another scheduler): `begin` was given a
    /// bound, and the body has the bytes written.
    pub fn finish_sized(&self, mut incoming: Incoming, written: u64) -> Result<BlobRef, DiskError> {
        if incoming.written != written {
            return Err(DiskError::Mismatch {
                expected: incoming.id.clone(),
                detail: format!("{} bytes written, {written} received", incoming.written),
            });
        }
        incoming.size = written;
        self.finish(incoming)
    }

    /// Whether the body of `id` still has the bytes its name says. A body
    /// that does not is moved to quarantine. `Missing` if there is none.
    pub fn verify(&self, id: &BlobId) -> Result<bool, DiskError> {
        let path = self.path(id);
        let mut file = match File::open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(DiskError::Missing(id.clone()));
            }
            Err(e) => return Err(e.into()),
        };
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        drop(file);
        let digest: [u8; 32] = hasher.finalize().into();
        if BlobId::from_digest(digest) == *id {
            return Ok(true);
        }
        self.quarantine(id);
        Ok(false)
    }

    /// Moves the body of `id` aside; the scheduler then fetches it again.
    pub fn quarantine(&self, id: &BlobId) {
        let guard = self.blob_lock(id);
        let _guard = lock(&guard);
        let dir = self.quarantine_dir();
        let moved = fs::create_dir_all(&dir).and_then(|()| {
            fs::rename(
                self.path(id),
                dir.join(format!("{}-{}", id.hex(), uuid::Uuid::new_v4())),
            )
        });
        match moved {
            Ok(()) => {
                tracing::error!(blob = %id, "a blob body does not match its name; moved to quarantine")
            }
            Err(e) => tracing::error!(blob = %id, "cannot move a damaged blob to quarantine: {e}"),
        }
    }

    /// The body of `id`, positioned at `offset`.
    pub fn open_at(&self, id: &BlobId, offset: u64) -> Result<File, DiskError> {
        let mut file = match File::open(self.path(id)) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(DiskError::Missing(id.clone()));
            }
            Err(e) => return Err(e.into()),
        };
        file.seek(SeekFrom::Start(offset))?;
        Ok(file)
    }

    /// Removes the body of `id` if it was not used after `before`. Returns
    /// whether it was removed.
    pub fn remove_untouched(&self, id: &BlobId, before: SystemTime) -> bool {
        let guard = self.blob_lock(id);
        let _guard = lock(&guard);
        let path = self.path(id);
        let untouched = fs::metadata(&path)
            .and_then(|m| m.modified())
            .is_ok_and(|t| t <= before);
        untouched && fs::remove_file(&path).is_ok()
    }

    /// Removes transfers that stopped long ago (their file was not written for
    /// `stale`).
    pub fn clean_parts(&self, stale: Duration) -> usize {
        let Ok(entries) = fs::read_dir(self.incoming()) else {
            return 0;
        };
        let Some(before) = SystemTime::now().checked_sub(stale) else {
            return 0;
        };
        let mut removed = 0;
        for entry in entries.flatten() {
            let old = entry
                .metadata()
                .and_then(|m| m.modified())
                .is_ok_and(|t| t <= before);
            if old && fs::remove_file(entry.path()).is_ok() {
                removed += 1;
            }
        }
        removed
    }

    /// The transfers being received.
    pub fn parts(&self) -> usize {
        fs::read_dir(self.incoming()).map_or(0, |d| d.flatten().count())
    }
}

/// A body being received; its file is removed if it is dropped unfinished.
pub struct Incoming {
    file: Option<File>,
    part: PathBuf,
    id: BlobId,
    size: u64,
    written: u64,
    unsynced: u64,
    hasher: Sha256,
}

impl Incoming {
    pub fn id(&self) -> &BlobId {
        &self.id
    }

    pub fn write(&mut self, data: &[u8]) -> Result<(), DiskError> {
        let written = self.written + data.len() as u64;
        if written > self.size {
            return Err(DiskError::Mismatch {
                expected: self.id.clone(),
                detail: format!("more than the {} bytes announced", self.size),
            });
        }
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| DiskError::Io(std::io::Error::other("the transfer is finished")))?;
        file.write_all(data)?;
        self.hasher.update(data);
        self.written = written;
        self.unsynced += data.len() as u64;
        if self.unsynced >= SYNC_EVERY {
            file.sync_data()?;
            self.unsynced = 0;
        }
        Ok(())
    }
}

impl Drop for Incoming {
    fn drop(&mut self) {
        self.file.take();
        // An injected crash (`fault`) leaves the file as a crash would; the
        // next start removes it.
        if !self.part.as_os_str().is_empty() && !std::thread::panicking() {
            let _ = fs::remove_file(&self.part);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn disk() -> (tempfile::TempDir, BlobDisk) {
        let dir = tempfile::tempdir().unwrap();
        let disk = BlobDisk::new(
            dir.path().join("blobs"),
            DiskLimits {
                max_blob_bytes: 1 << 20,
                max_store_bytes: 0,
                min_free_bytes: 0,
            },
        );
        (dir, disk)
    }

    fn store(disk: &BlobDisk, bytes: &[u8]) -> BlobRef {
        let id = BlobId::of(bytes);
        let mut incoming = disk.begin(&id, bytes.len() as u64).unwrap();
        for piece in bytes.chunks(3) {
            incoming.write(piece).unwrap();
        }
        disk.finish(incoming).unwrap()
    }

    #[test]
    fn a_body_is_stored_under_its_hash_and_read_back() {
        let (_dir, disk) = disk();
        let r = store(&disk, b"hello blobs");
        assert_eq!(r.size, 11);
        assert!(disk.has(&r.id));
        assert!(disk.verify(&r.id).unwrap());
        let mut f = disk.open_at(&r.id, 6).unwrap();
        let mut rest = String::new();
        f.read_to_string(&mut rest).unwrap();
        assert_eq!(rest, "blobs");
        assert_eq!(disk.parts(), 0);
        assert_eq!(disk.list().len(), 1);
        // The same bytes again leave one body and no part.
        store(&disk, b"hello blobs");
        assert_eq!((disk.list().len(), disk.parts()), (1, 0));
    }

    #[test]
    fn wrong_bytes_are_refused_and_leave_nothing() {
        let (_dir, disk) = disk();
        let id = BlobId::of(b"right");
        let mut incoming = disk.begin(&id, 5).unwrap();
        incoming.write(b"wrong").unwrap();
        assert!(matches!(
            disk.finish(incoming),
            Err(DiskError::Mismatch { .. })
        ));
        let mut short = disk.begin(&id, 5).unwrap();
        short.write(b"rig").unwrap();
        assert!(matches!(
            disk.finish(short),
            Err(DiskError::Mismatch { .. })
        ));
        let mut long = disk.begin(&id, 5).unwrap();
        assert!(long.write(b"right!").is_err());
        drop(long);
        assert!(!disk.has(&id));
        assert_eq!(disk.parts(), 0);
    }

    #[test]
    fn limits_are_kept() {
        let (_dir, disk) = disk();
        let id = BlobId::of(b"x");
        assert!(matches!(
            disk.begin(&id, 2 << 20),
            Err(DiskError::TooLarge { .. })
        ));
        let full = BlobDisk::new(
            disk.root().to_path_buf(),
            DiskLimits {
                max_blob_bytes: 1 << 20,
                max_store_bytes: 0,
                min_free_bytes: u64::MAX / 2,
            },
        );
        assert!(matches!(full.begin(&id, 1), Err(DiskError::Exhausted(_))));
        store(&disk, b"0123456789");
        let bounded = BlobDisk::new(
            disk.root().to_path_buf(),
            DiskLimits {
                max_blob_bytes: 1 << 20,
                max_store_bytes: 15,
                min_free_bytes: 0,
            },
        );
        assert!(matches!(
            bounded.begin(&id, 6),
            Err(DiskError::Exhausted(_))
        ));
        assert!(bounded.begin(&id, 5).is_ok());
    }

    #[test]
    fn a_damaged_body_goes_to_quarantine() {
        let (_dir, disk) = disk();
        let r = store(&disk, b"precious");
        fs::write(disk.path(&r.id), b"Precious").unwrap();
        assert!(!disk.verify(&r.id).unwrap());
        assert!(!disk.has(&r.id));
        assert_eq!(disk.quarantined(), 1);
        assert!(matches!(disk.verify(&r.id), Err(DiskError::Missing(_))));
    }

    #[test]
    fn only_untouched_bodies_are_removed() {
        let (_dir, disk) = disk();
        let r = store(&disk, b"old");
        let before = SystemTime::now() - Duration::from_secs(3600);
        assert!(!disk.remove_untouched(&r.id, before));
        let f = File::options().write(true).open(disk.path(&r.id)).unwrap();
        f.set_modified(before - Duration::from_secs(1)).unwrap();
        drop(f);
        assert!(disk.remove_untouched(&r.id, before));
        assert!(!disk.has(&r.id));
    }

    #[test]
    fn stale_parts_are_cleaned() {
        let (_dir, disk) = disk();
        let id = BlobId::of(b"abc");
        let incoming = disk.begin(&id, 3).unwrap();
        assert_eq!(disk.parts(), 1);
        drop(incoming);
        assert_eq!(disk.parts(), 0);
        // A transfer that a crash cut off leaves its file.
        let left = disk.incoming().join("left.part");
        fs::write(&left, b"ab").unwrap();
        assert_eq!(disk.clean_parts(Duration::from_secs(3600)), 0);
        let f = File::options().write(true).open(&left).unwrap();
        f.set_modified(SystemTime::now() - Duration::from_secs(7200))
            .unwrap();
        drop(f);
        assert_eq!(disk.clean_parts(Duration::from_secs(3600)), 1);
        assert_eq!(disk.parts(), 0);
    }
}

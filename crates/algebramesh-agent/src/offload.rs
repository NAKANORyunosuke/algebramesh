//! Large values of a result moved to blobs before the report (ADR 0021 §4,
//! §5): the agent writes the canonical JSON of each to a temporary file while
//! hashing it, asks the scheduler whether it holds the blob already
//! (`StatBlob`), sends it otherwise (`PutBlob`), and puts the marker in its
//! place.

use std::io::{BufWriter, Read, Write};
use std::path::PathBuf;
use std::time::Duration;

use algebramesh_core::CasError;
use algebramesh_core::blob::{
    BlobRef, CHUNK_BYTES, HashingWriter, INLINE_MAX_BYTES, Offload, plan_offload, write_canonical,
};
use algebramesh_rpc::pb;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::{Gateway, RPC_TIMEOUT, within};

/// The rate a transfer is assumed to reach at least, to give a large report
/// more time.
const ASSUMED_RATE: u64 = 8 * 1024 * 1024;

/// How much longer than the usual patience a report of `bytes` in blobs may
/// take (ADR 0021 Q25).
pub(crate) fn extra_patience(bytes: u64) -> Duration {
    Duration::from_secs(bytes / ASSUMED_RATE)
}

/// The canonical JSON of a value in a temporary file, removed when dropped.
struct Spilled {
    path: PathBuf,
    blob: BlobRef,
}

impl Drop for Spilled {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn spill(value: &Value) -> std::io::Result<Spilled> {
    let path = std::env::temp_dir().join(format!("algebramesh-blob-{}.json", uuid::Uuid::new_v4()));
    let file = std::fs::File::create(&path)?;
    let mut spilled = Spilled {
        path,
        blob: BlobRef {
            id: algebramesh_core::blob::BlobId::of(b""),
            size: 0,
        },
    };
    let mut writer = HashingWriter::new(BufWriter::with_capacity(1 << 20, file));
    write_canonical(value, &mut writer)?;
    let (buffered, blob) = writer.finish();
    buffered.into_inner().map_err(|e| e.into_error())?.flush()?;
    spilled.blob = blob;
    Ok(spilled)
}

/// Sends the file of `spilled` with `PutBlob`, unless the scheduler holds the
/// blob on a majority already.
async fn upload(gateway: &mut Gateway, spilled: &Spilled) -> Result<(), tonic::Status> {
    let blob = &spilled.blob;
    let request = pb::BlobRequest {
        id: blob.id.to_string(),
        offset: 0,
    };
    let stat = within(RPC_TIMEOUT, gateway.stat_blob(request))
        .await?
        .into_inner();
    if stat.present && stat.durable && stat.size == blob.size {
        return Ok(());
    }
    let (tx, rx) = mpsc::channel::<pb::BlobUpload>(2);
    let path = spilled.path.clone();
    let id = blob.id.to_string();
    let size = blob.size;
    let reader = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        let mut file = std::fs::File::open(path)?;
        let mut first = true;
        loop {
            let mut buf = vec![0u8; CHUNK_BYTES];
            let mut filled = 0;
            while filled < buf.len() {
                match file.read(&mut buf[filled..])? {
                    0 => break,
                    n => filled += n,
                }
            }
            buf.truncate(filled);
            if filled == 0 && !first {
                return Ok(());
            }
            let piece = pb::BlobUpload {
                id: if first { id.clone() } else { String::new() },
                size: if first { size } else { 0 },
                data: buf,
            };
            first = false;
            if tx.blocking_send(piece).is_err() || filled == 0 {
                return Ok(());
            }
        }
    });
    let limit = RPC_TIMEOUT + extra_patience(size) * 2;
    let answer = within(limit, gateway.put_blob(ReceiverStream::new(rx))).await;
    match reader.await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            return Err(tonic::Status::internal(format!(
                "cannot read the blob: {e}"
            )));
        }
        Err(e) => return Err(tonic::Status::internal(e.to_string())),
    }
    let got = answer?.into_inner();
    if got.id != blob.id.as_str() || got.size != blob.size {
        return Err(tonic::Status::internal(format!(
            "the scheduler stored {} of {} bytes for {}",
            got.id, got.size, blob.id
        )));
    }
    Ok(())
}

/// Sends `spilled`, again after failures that may pass (another leader, a
/// blob not yet on a majority), until `deadline`.
async fn upload_patiently(
    gateway: &(dyn Fn() -> Gateway + Sync),
    spilled: &Spilled,
    deadline: tokio::time::Instant,
) -> Result<(), tonic::Status> {
    let mut delay = Duration::from_millis(200);
    loop {
        match upload(&mut gateway(), spilled).await {
            Ok(()) => return Ok(()),
            Err(e)
                if matches!(
                    e.code(),
                    tonic::Code::Unavailable
                        | tonic::Code::Aborted
                        | tonic::Code::DeadlineExceeded
                        // A connection that broke in the middle, or a stream
                        // the scheduler reset as it stopped.
                        | tonic::Code::Unknown
                        | tonic::Code::Cancelled
                ) && tokio::time::Instant::now() + delay < deadline =>
            {
                tracing::warn!(blob = %spilled.blob.id, "sending a blob failed, retrying: {e}");
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(5));
            }
            Err(e) => return Err(e),
        }
    }
}

/// The bytes `value` would put in blobs, before anything is sent.
pub(crate) fn planned_bytes(value: &Value) -> u64 {
    use algebramesh_core::blob::canonical_len;
    match plan_offload(value, INLINE_MAX_BYTES as u64) {
        Offload::Nothing => 0,
        Offload::Whole => canonical_len(value),
        Offload::Keys(keys) => keys
            .iter()
            .filter_map(|k| value.get(k))
            .map(canonical_len)
            .sum(),
    }
}

/// Moves the large parts of `value` to blobs and puts their markers in their
/// place. An error the job ends with if a blob cannot be stored.
pub(crate) async fn offload(
    value: &mut Value,
    gateway: &(dyn Fn() -> Gateway + Sync),
    deadline: tokio::time::Instant,
) -> Result<(), CasError> {
    let plan = plan_offload(value, INLINE_MAX_BYTES as u64);
    let places: Vec<Option<String>> = match plan {
        Offload::Nothing => return Ok(()),
        Offload::Whole => vec![None],
        Offload::Keys(keys) => keys.into_iter().map(Some).collect(),
    };
    for place in places {
        let part = match &place {
            None => std::mem::take(value),
            Some(k) => value.get_mut(k).map(std::mem::take).unwrap_or_default(),
        };
        let spilled = tokio::task::spawn_blocking(move || spill(&part).map(|s| (s, part)))
            .await
            .map_err(|e| CasError::Internal(e.to_string()))?
            .map_err(|e| {
                CasError::ResourceLimit(format!("cannot write a large value to a file: {e}"))
            })?;
        let (spilled, part) = spilled;
        // The value is not kept while it is sent.
        drop(part);
        upload_patiently(gateway, &spilled, deadline)
            .await
            .map_err(|e| {
                CasError::ResourceLimit(format!(
                    "a value of {} bytes could not be stored with the scheduler: {e}",
                    spilled.blob.size
                ))
            })?;
        let marker = spilled.blob.marker();
        match &place {
            None => *value = marker,
            Some(k) => {
                if let Value::Object(m) = value {
                    m.insert(k.clone(), marker);
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn a_spilled_value_is_its_canonical_json() {
        let value = serde_json::json!({"b": [1, 2], "a": "x"});
        let spilled = spill(&value).unwrap();
        let bytes = std::fs::read(&spilled.path).unwrap();
        assert_eq!(bytes, algebramesh_core::canonical_json(&value).into_bytes());
        assert_eq!(spilled.blob.id, algebramesh_core::blob::BlobId::of(&bytes));
        assert_eq!(spilled.blob.size, bytes.len() as u64);
        let path = spilled.path.clone();
        drop(spilled);
        assert!(!path.exists());
    }
}

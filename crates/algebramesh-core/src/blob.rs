//! Values kept outside the Raft log and referred to by content (ADR 0021).
//!
//! A large value is stored as a blob: the UTF-8 bytes of its canonical JSON
//! ([`crate::canonical_json`]), named by their SHA-256. In the record it is
//! replaced by a marker `{"$blob": "sha256:<hex>", "size": n}`.
//!
//! Markers may stand only at three places: the result value itself, a
//! top-level value of an object result, and (from B2 on) a top-level value of
//! the arguments. At those places an object with any key that starts with `$`
//! is read as a marker and must be well formed; a producer puts such a value
//! in a blob instead of sending it inline.
//!
//! In a job's arguments (W2) a top-level value may also refer to a value of a
//! committed job's result: a marker that carries its origin in `from`, or,
//! for a small value, `{"$ref": {"job_id", "pointer", "sha256"}}`, which an
//! agent resolves from the producer's record. The scheduler checks both
//! against the producer's record before it creates the job.

use std::fmt;
use std::io::Write;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

/// Bytes of the result (or arguments) kept inline at most; larger values are
/// moved to blobs, largest top-level value first.
pub const INLINE_MAX_BYTES: usize = 256 * 1024;
/// Markers in one value at most; with more, the whole value is one blob.
pub const MAX_MARKERS: usize = 1024;
/// Size of the pieces of a blob transfer.
pub const CHUNK_BYTES: usize = 4 * 1024 * 1024;
/// The default upper bound of one blob; not above a driver's
/// `max_output_bytes`.
pub const DEFAULT_MAX_BLOB_BYTES: u64 = 256 * 1024 * 1024;
/// Upper bound of a record whose markers a scheduler expands for a client that
/// does not resolve them itself.
pub const EXPANDED_MAX_BYTES: u64 = 96 * 1024 * 1024;

/// The key of a marker that names the blob.
pub const MARKER_KEY: &str = "$blob";
const SIZE_KEY: &str = "size";
/// The origin of a value in a marker of an argument (W2): `{"job_id",
/// "pointer"}`. Readers of results skip it.
const FROM_KEY: &str = "from";
/// The key of a reference to a value of a committed job's result (W2).
pub const RECORD_REF_KEY: &str = "$ref";
const PREFIX: &str = "sha256:";

/// `sha256:<64 lowercase hex digits>`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct BlobId(String);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BlobError {
    #[error("not a blob id (sha256:<64 lowercase hex digits>): {0:?}")]
    BadId(String),
    #[error("a value at a marker position has a key starting with '$' but is not a marker: {0}")]
    BadMarker(String),
    #[error("{0} markers in one value; at most {MAX_MARKERS}")]
    TooManyMarkers(usize),
    #[error("not a reference to a value of a job's result: {0}")]
    BadReference(String),
    #[error(
        "the pointer {0:?} is neither \"\" nor \"/<key>\": a reference names a result or one of its top-level values"
    )]
    BadPointer(String),
}

impl BlobId {
    pub fn parse(s: &str) -> Result<Self, BlobError> {
        let hex = s
            .strip_prefix(PREFIX)
            .ok_or_else(|| BlobError::BadId(s.to_owned()))?;
        if hex.len() == 64 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            Ok(Self(s.to_owned()))
        } else {
            Err(BlobError::BadId(s.to_owned()))
        }
    }

    /// The id of `bytes`.
    pub fn of(bytes: &[u8]) -> Self {
        Self::from_digest(Sha256::digest(bytes).into())
    }

    pub fn from_digest(digest: [u8; 32]) -> Self {
        Self(format!("{PREFIX}{}", hex::encode(digest)))
    }

    /// The 64 hex digits.
    pub fn hex(&self) -> &str {
        &self.0[PREFIX.len()..]
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for BlobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for BlobId {
    type Error = BlobError;
    fn try_from(s: String) -> Result<Self, BlobError> {
        Self::parse(&s)
    }
}

impl From<BlobId> for String {
    fn from(id: BlobId) -> Self {
        id.0
    }
}

/// A blob and its size in bytes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlobRef {
    pub id: BlobId,
    pub size: u64,
}

impl BlobRef {
    /// `{"$blob": id, "size": size}`.
    pub fn marker(&self) -> Value {
        let mut m = Map::new();
        m.insert(MARKER_KEY.into(), Value::String(self.id.0.clone()));
        m.insert(SIZE_KEY.into(), Value::from(self.size));
        Value::Object(m)
    }

    /// The marker `value`, if it is one. `Ok(None)` for a value with no key
    /// starting with `$`; an error for any other object with such a key.
    pub fn from_marker(value: &Value) -> Result<Option<Self>, BlobError> {
        let Value::Object(m) = value else {
            return Ok(None);
        };
        if !m.keys().any(|k| k.starts_with('$')) {
            return Ok(None);
        }
        let bad = || BlobError::BadMarker(truncated(value));
        if m.keys()
            .any(|k| k != MARKER_KEY && k != SIZE_KEY && k != FROM_KEY)
        {
            return Err(bad());
        }
        let id = m
            .get(MARKER_KEY)
            .and_then(Value::as_str)
            .ok_or_else(bad)
            .and_then(|s| BlobId::parse(s).map_err(|_| bad()))?;
        let size = m.get(SIZE_KEY).and_then(Value::as_u64).ok_or_else(bad)?;
        Ok(Some(Self { id, size }))
    }
}

fn truncated(value: &Value) -> String {
    let mut s = value.to_string();
    if s.len() > 200 {
        let mut end = 200;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
        s.push('…');
    }
    s
}

/// Where a marker stands in a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkerPlace {
    /// The value itself.
    Whole,
    /// The top-level value under this key of an object.
    Key(String),
}

/// The markers of a result (or arguments) value, at the places markers may
/// stand. An error if a value there looks like a marker but is not one, or if
/// there are more than [`MAX_MARKERS`].
pub fn markers(value: &Value) -> Result<Vec<(MarkerPlace, BlobRef)>, BlobError> {
    if let Some(r) = BlobRef::from_marker(value)? {
        return Ok(vec![(MarkerPlace::Whole, r)]);
    }
    let mut found = Vec::new();
    if let Value::Object(m) = value {
        for (k, v) in m {
            if let Some(r) = BlobRef::from_marker(v)? {
                found.push((MarkerPlace::Key(k.clone()), r));
            }
        }
    }
    if found.len() > MAX_MARKERS {
        return Err(BlobError::TooManyMarkers(found.len()));
    }
    Ok(found)
}

/// The markers among the top-level values of a job's `arguments` (ADR 0021
/// §3), by key. `arguments` itself is not a place for a marker: its own keys
/// are argument names. An error if a top-level value looks like a marker but
/// is not one, or if there are more than [`MAX_MARKERS`].
///
/// A well-formed reference `{"$ref": ...}` (W2) is no blob and is skipped;
/// [`argument_refs`] reads both.
pub fn argument_markers(arguments: &Value) -> Result<Vec<(String, BlobRef)>, BlobError> {
    Ok(argument_refs(arguments)?
        .into_iter()
        .filter_map(|(k, r)| match r {
            ArgumentRef::Blob { blob, .. } => Some((k, blob)),
            ArgumentRef::Record(_) => None,
        })
        .collect())
}

/// Where a value of a job's result is (W2): the job, and the result itself
/// (`pointer` `""`) or one of its top-level values (`"/<key>"`, escaped as in
/// RFC 6901).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputRef {
    pub job_id: String,
    pub pointer: String,
}

/// The top-level key a pointer names (`"/<key>"`, with `~1` for `/` and `~0`
/// for `~`), or `None` for `""`, the whole value. Deeper pointers are refused:
/// a value is checked against its producer's record without reading blobs,
/// and a blob holds a top-level value at most.
pub fn pointer_key(pointer: &str) -> Result<Option<String>, BlobError> {
    if pointer.is_empty() {
        return Ok(None);
    }
    let bad = || BlobError::BadPointer(pointer.to_owned());
    let token = pointer.strip_prefix('/').ok_or_else(bad)?;
    if token.contains('/') {
        return Err(bad());
    }
    let mut key = String::with_capacity(token.len());
    let mut chars = token.chars();
    while let Some(c) = chars.next() {
        if c == '~' {
            match chars.next() {
                Some('0') => key.push('~'),
                Some('1') => key.push('/'),
                _ => return Err(bad()),
            }
        } else {
            key.push(c);
        }
    }
    Ok(Some(key))
}

/// The pointer of the top-level value `key` (RFC 6901).
pub fn key_pointer(key: &str) -> String {
    format!("/{}", key.replace('~', "~0").replace('/', "~1"))
}

/// A reference to a small value of a committed job's result (W2): its place
/// and the SHA-256 of its canonical JSON, as a blob id.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordRef {
    pub job_id: String,
    pub pointer: String,
    pub sha256: BlobId,
}

impl RecordRef {
    /// `{"$ref": {"job_id", "pointer", "sha256"}}`.
    pub fn marker(&self) -> Value {
        let mut m = Map::new();
        m.insert(
            RECORD_REF_KEY.into(),
            serde_json::to_value(self).unwrap_or_default(),
        );
        Value::Object(m)
    }

    pub fn output(&self) -> OutputRef {
        OutputRef {
            job_id: self.job_id.clone(),
            pointer: self.pointer.clone(),
        }
    }
}

/// A marker of a value that came from `from` (W2):
/// `{"$blob", "size", "from": {"job_id", "pointer"}}`.
pub fn marker_from(blob: &BlobRef, from: &OutputRef) -> Value {
    let mut marker = blob.marker();
    if let Value::Object(m) = &mut marker {
        m.insert(
            FROM_KEY.into(),
            serde_json::to_value(from).unwrap_or_default(),
        );
    }
    marker
}

/// A top-level value of a job's arguments that stands for another value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArgumentRef {
    /// A marker; `from` is where its value came from, if it says (W2).
    Blob {
        blob: BlobRef,
        from: Option<OutputRef>,
    },
    /// `{"$ref": ...}` (W2).
    Record(RecordRef),
}

impl ArgumentRef {
    /// The blob id the value has or would have: the SHA-256 of its canonical
    /// JSON.
    pub fn id(&self) -> &BlobId {
        match self {
            Self::Blob { blob, .. } => &blob.id,
            Self::Record(r) => &r.sha256,
        }
    }

    /// The producer's output this value comes from, if it says.
    pub fn origin(&self) -> Option<OutputRef> {
        match self {
            Self::Blob { from, .. } => from.clone(),
            Self::Record(r) => Some(r.output()),
        }
    }
}

/// The reference `value` is, if it is one: a marker, with its origin, or
/// `{"$ref": ...}`. `Ok(None)` for a value with no key starting with `$`; an
/// error for any other object with such a key, a malformed `from`, or a
/// pointer below the top level.
pub fn argument_ref(value: &Value) -> Result<Option<ArgumentRef>, BlobError> {
    let Value::Object(m) = value else {
        return Ok(None);
    };
    if let Some(inner) = m.get(RECORD_REF_KEY) {
        let bad = || BlobError::BadReference(truncated(value));
        if m.len() != 1 {
            return Err(bad());
        }
        let r: RecordRef = serde_json::from_value(inner.clone()).map_err(|_| bad())?;
        pointer_key(&r.pointer)?;
        return Ok(Some(ArgumentRef::Record(r)));
    }
    let Some(blob) = BlobRef::from_marker(value)? else {
        return Ok(None);
    };
    let from = match m.get(FROM_KEY) {
        None => None,
        Some(f) => {
            let from: OutputRef = serde_json::from_value(f.clone())
                .map_err(|_| BlobError::BadMarker(truncated(value)))?;
            pointer_key(&from.pointer)?;
            Some(from)
        }
    };
    Ok(Some(ArgumentRef::Blob { blob, from }))
}

/// The references among the top-level values of a job's `arguments`, by
/// key: markers and `{"$ref": ...}`. An error if a top-level value looks like
/// one but is not, or if there are more than [`MAX_MARKERS`].
pub fn argument_refs(arguments: &Value) -> Result<Vec<(String, ArgumentRef)>, BlobError> {
    let Value::Object(m) = arguments else {
        return Ok(Vec::new());
    };
    let mut found = Vec::new();
    for (k, v) in m {
        if let Some(r) = argument_ref(v)? {
            found.push((k.clone(), r));
        }
    }
    if found.len() > MAX_MARKERS {
        return Err(BlobError::TooManyMarkers(found.len()));
    }
    Ok(found)
}

/// The value `pointer` names in a producer's `result`: the result itself or
/// one of its top-level values, inline or as a marker. `None` if the result
/// has no such value, or if it is one blob and the pointer names a value
/// inside it (that would take reading the blob).
pub fn pointed<'a>(result: &'a Value, pointer: &str) -> Result<Option<&'a Value>, BlobError> {
    match pointer_key(pointer)? {
        None => Ok(Some(result)),
        Some(key) => Ok(match result {
            Value::Object(m) if !matches!(BlobRef::from_marker(result), Ok(Some(_))) => m.get(&key),
            _ => None,
        }),
    }
}

/// The blob id of a value as a producer's record holds it: the id of its
/// marker, or the SHA-256 of its canonical JSON when it is inline.
pub fn recorded_id(value: &Value) -> BlobId {
    match BlobRef::from_marker(value) {
        Ok(Some(r)) => r.id,
        _ => canonical_ref(value).id,
    }
}

/// The content form of `arguments` (ADR 0021 §13): each top-level value
/// replaced by the id of the blob it is or would be (the SHA-256 of its
/// canonical JSON), a marker by its own id and a reference `{"$ref": ...}`
/// by its `sha256` (W2); `size` and `from` do not count. The same arguments give the same
/// form whether their values are inline or in blobs. Arguments that are not
/// an object are replaced whole. A value that looks like a marker but is not
/// one is hashed as a value.
pub fn content_form(arguments: &Value) -> Value {
    let id_of = |v: &Value| match argument_ref(v) {
        Ok(Some(r)) => r.id().to_string(),
        _ => canonical_ref(v).id.to_string(),
    };
    match arguments {
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, v)| (k.clone(), Value::String(id_of(v))))
                .collect(),
        ),
        other => Value::String(canonical_ref(other).id.to_string()),
    }
}

/// Writes the canonical JSON of `value` (the bytes [`crate::canonical_json`]
/// gives) to `out`, without building it in memory. `serde_json::Map` keeps
/// its keys sorted unless the feature `preserve_order` is enabled, which the
/// test `canonical_json_sorts_keys` would detect.
pub fn write_canonical<W: Write>(value: &Value, out: W) -> std::io::Result<()> {
    serde_json::to_writer(out, value).map_err(std::io::Error::other)
}

/// The size of the canonical JSON of `value`.
pub fn canonical_len(value: &Value) -> u64 {
    let mut counter = Counter::default();
    // Writing to a counter never fails.
    let _ = write_canonical(value, &mut counter);
    counter.bytes
}

/// The id and size of the canonical JSON of `value`.
pub fn canonical_ref(value: &Value) -> BlobRef {
    let mut hasher = HashingWriter::new(std::io::sink());
    let _ = write_canonical(value, &mut hasher);
    hasher.finish().1
}

#[derive(Default)]
struct Counter {
    bytes: u64,
}

impl Write for Counter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.bytes += buf.len() as u64;
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Passes bytes on to `inner` and hashes them.
pub struct HashingWriter<W> {
    inner: W,
    hasher: Sha256,
    bytes: u64,
}

impl<W: Write> HashingWriter<W> {
    pub fn new(inner: W) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
            bytes: 0,
        }
    }

    /// The inner writer and the reference of the bytes written.
    pub fn finish(self) -> (W, BlobRef) {
        let id = BlobId::from_digest(self.hasher.finalize().into());
        (
            self.inner,
            BlobRef {
                id,
                size: self.bytes,
            },
        )
    }
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        self.bytes += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// What to move out of a value (ADR 0021 §4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Offload {
    /// Everything stays inline.
    Nothing,
    /// The whole value is one blob.
    Whole,
    /// These top-level values of the object become blobs.
    Keys(Vec<String>),
}

/// Decides which parts of `value` to move to blobs so that at most
/// `inline_max` bytes stay inline: the largest top-level values first; the
/// whole value if it is not an object, if that takes more than
/// [`MAX_MARKERS`] markers, or if it is still too large. A top-level value
/// with a key starting with `$` always becomes a blob, and so does a value
/// whose own top-level keys start with `$` (it could not be told from a
/// marker).
pub fn plan_offload(value: &Value, inline_max: u64) -> Offload {
    let Value::Object(m) = value else {
        return if canonical_len(value) > inline_max {
            Offload::Whole
        } else {
            Offload::Nothing
        };
    };
    if m.keys().any(|k| k.starts_with('$')) {
        return Offload::Whole;
    }
    let (chosen, inline) = choose_keys(m, canonical_len(value), inline_max);
    if chosen.is_empty() {
        Offload::Nothing
    } else if chosen.len() > MAX_MARKERS || inline > inline_max {
        Offload::Whole
    } else {
        Offload::Keys(chosen)
    }
}

/// The top-level values of a job's `arguments` to move to blobs so that the
/// specification `spec_len` bytes long keeps at most `inline_max` bytes
/// inline (ADR 0021 §4, B2), largest first, and every value that looks like
/// a marker. `arguments` cannot become one blob, as its keys are the names
/// of the arguments: an error if more than [`MAX_MARKERS`] would be needed.
/// The specification may still exceed `inline_max` (with many small
/// arguments); the scheduler's limit on its size applies to the rest.
pub fn plan_argument_offload(
    arguments: &Value,
    spec_len: u64,
    inline_max: u64,
) -> Result<Vec<String>, BlobError> {
    let Value::Object(m) = arguments else {
        return Ok(Vec::new());
    };
    let (chosen, _) = choose_keys(m, spec_len, inline_max);
    if chosen.len() > MAX_MARKERS {
        return Err(BlobError::TooManyMarkers(chosen.len()));
    }
    Ok(chosen)
}

/// The keys of `m` to replace by markers so that `total` bytes (of which `m`
/// is a part) come down to `inline_max`, and the bytes left inline: every
/// value that looks like a marker, then the largest. Sorted by key.
fn choose_keys(m: &Map<String, Value>, total: u64, inline_max: u64) -> (Vec<String>, u64) {
    let looks_like_marker =
        |v: &Value| matches!(v, Value::Object(o) if o.keys().any(|k| k.starts_with('$')));
    let mut sizes: Vec<(u64, &String)> = m.iter().map(|(k, v)| (canonical_len(v), k)).collect();
    // Largest first; ties in key order, so the plan is deterministic.
    sizes.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1)));
    let mut chosen: Vec<String> = m
        .iter()
        .filter(|(_, v)| looks_like_marker(v))
        .map(|(k, _)| k.clone())
        .collect();
    // The canonical size of the marker of a value of `size` bytes.
    let marker_len = |size: u64| 91 + size.to_string().len() as u64;
    let mut inline = total;
    for (size, key) in &sizes {
        if chosen.contains(key) {
            inline = inline
                .saturating_sub(*size)
                .saturating_add(marker_len(*size));
        }
    }
    for (size, key) in sizes {
        if inline <= inline_max {
            break;
        }
        if chosen.contains(key) {
            continue;
        }
        chosen.push(key.clone());
        inline = inline.saturating_sub(size).saturating_add(marker_len(size));
    }
    chosen.sort();
    (chosen, inline)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use serde_json::json;

    const ID: &str = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn ids_are_lowercase_sha256() {
        assert!(BlobId::parse(ID).is_ok());
        assert!(BlobId::parse(&ID.to_uppercase()).is_err());
        assert!(BlobId::parse(&ID[..ID.len() - 1]).is_err());
        assert!(BlobId::parse(&ID.replace("sha256:", "md5:")).is_err());
        assert_eq!(BlobId::of(b"").hex(), crate::sha256_hex(b""));
    }

    #[test]
    fn markers_are_read_only_where_they_may_stand() {
        let marker = json!({"$blob": ID, "size": 3});
        let r = BlobRef::from_marker(&marker).unwrap().unwrap();
        assert_eq!(r.size, 3);
        assert_eq!(r.marker(), marker);
        assert_eq!(
            markers(&marker).unwrap(),
            vec![(MarkerPlace::Whole, r.clone())]
        );
        let v = json!({"a": marker, "b": {"c": {"$blob": "deep, not read"}}});
        assert_eq!(
            markers(&v).unwrap(),
            vec![(MarkerPlace::Key("a".into()), r)]
        );
        assert!(markers(&json!([1, 2])).unwrap().is_empty());
        // `from` is reserved and skipped.
        assert!(
            BlobRef::from_marker(&json!({"$blob": ID, "size": 1, "from": {"job_id": "j"}}))
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn values_that_only_look_like_markers_are_refused() {
        for bad in [
            json!({"$blob": ID}),
            json!({"$blob": ID, "size": -1}),
            json!({"$blob": "sha256:xyz", "size": 1}),
            json!({"$blob": ID, "size": 1, "other": 2}),
            json!({"$other": 1}),
        ] {
            assert!(markers(&bad).is_err(), "{bad}");
            assert!(markers(&json!({"k": bad})).is_err());
        }
    }

    #[test]
    fn canonical_writing_matches_canonical_json() {
        let v = json!({"b": [1, 2.5, "x\u{e9}\n"], "a": {"z": null, "y": true}, "c": -3});
        let mut out = Vec::new();
        write_canonical(&v, &mut out).unwrap();
        assert_eq!(
            String::from_utf8(out.clone()).unwrap(),
            crate::canonical_json(&v)
        );
        assert_eq!(canonical_len(&v), out.len() as u64);
        assert_eq!(canonical_ref(&v).id, BlobId::of(&out));
    }

    #[test]
    fn the_largest_values_move_first() {
        let v = json!({"small": "x", "big": "y".repeat(300), "mid": "z".repeat(150)});
        assert_eq!(plan_offload(&v, 1_000), Offload::Nothing);
        assert_eq!(plan_offload(&v, 300), Offload::Keys(vec!["big".into()]));
        assert_eq!(
            plan_offload(&v, 220),
            Offload::Keys(vec!["big".into(), "mid".into()])
        );
        assert_eq!(plan_offload(&v, 10), Offload::Whole);
        assert_eq!(plan_offload(&json!("y".repeat(50)), 10), Offload::Whole);
        assert_eq!(plan_offload(&json!("y"), 10), Offload::Nothing);
    }

    #[test]
    fn the_marker_length_is_exact() {
        let r = BlobRef {
            id: BlobId::parse(ID).unwrap(),
            size: 12345,
        };
        assert_eq!(canonical_len(&r.marker()), 91 + 5);
    }

    #[test]
    fn values_with_dollar_keys_always_move() {
        assert_eq!(
            plan_offload(&json!({"a": {"$x": 1}, "b": 2}), 1_000),
            Offload::Keys(vec!["a".into()])
        );
        assert_eq!(plan_offload(&json!({"$x": 1}), 1_000), Offload::Whole);
    }

    #[test]
    fn too_many_markers_make_one_blob() {
        let m: Map<String, Value> = (0..MAX_MARKERS + 1)
            .map(|i| (format!("k{i:05}"), Value::String("v".repeat(200))))
            .collect();
        assert_eq!(plan_offload(&Value::Object(m), 1_000), Offload::Whole);
    }

    #[test]
    fn argument_markers_stand_only_under_the_argument_names() {
        let r = BlobRef {
            id: BlobId::parse(ID).unwrap(),
            size: 7,
        };
        let args = json!({"a": r.marker(), "b": [r.marker()], "c": 1});
        assert_eq!(
            argument_markers(&args).unwrap(),
            vec![("a".into(), r.clone())]
        );
        // `arguments` itself is not a place for a marker.
        assert_eq!(argument_markers(&r.marker()).unwrap(), Vec::new());
        assert!(argument_markers(&json!({"a": {"$blob": "x"}})).is_err());
    }

    #[test]
    fn the_content_form_does_not_depend_on_where_a_value_is() {
        let value = json!({"rows": [1, 2, 3]});
        let r = canonical_ref(&value);
        let inline = json!({"a": value, "b": 2});
        let in_blob = json!({"a": r.marker(), "b": 2});
        assert_eq!(content_form(&inline), content_form(&in_blob));
        assert_eq!(content_form(&inline)["a"], json!(r.id.to_string()));
        assert_ne!(
            content_form(&inline),
            content_form(&json!({"a": value, "b": 3}))
        );
        assert_ne!(content_form(&json!([1])), content_form(&json!([2])));
    }

    #[test]
    fn references_name_top_level_values_of_a_result() {
        let value = json!([1, 2]);
        let r = canonical_ref(&value);
        let reference = RecordRef {
            job_id: "j".into(),
            pointer: "/a~1b".into(),
            sha256: r.id.clone(),
        };
        let origin = OutputRef {
            job_id: "k".into(),
            pointer: String::new(),
        };
        let args = json!({"x": reference.marker(), "y": marker_from(&r, &origin), "z": 3});
        let found = argument_refs(&args).unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].1, ArgumentRef::Record(reference.clone()));
        assert_eq!(found[1].1.origin(), Some(origin));
        // A reference is no blob.
        assert_eq!(argument_markers(&args).unwrap().len(), 1);
        // The same value inline, in a blob or by reference has one content form.
        assert_eq!(
            content_form(&args),
            content_form(&json!({"x": value, "y": value, "z": 3}))
        );
        assert_eq!(pointer_key("/a~1b").unwrap(), Some("a/b".to_owned()));
        assert_eq!(pointer_key("/~0").unwrap(), Some("~".to_owned()));
        assert_eq!(key_pointer("a/b~"), "/a~1b~0");
        for bad in ["a", "/a/b", "/~2"] {
            assert!(pointer_key(bad).is_err(), "{bad}");
        }
        for bad in [
            json!({"$ref": {"job_id": "j", "pointer": "/a/b", "sha256": r.id}}),
            json!({"$ref": {"job_id": "j", "pointer": ""}}),
            json!({"$ref": {"job_id": "j", "pointer": "", "sha256": r.id}, "size": 1}),
            json!({"$blob": r.id, "size": 1, "from": {"job_id": "j"}}),
            json!({"$blob": r.id, "size": 1, "from": {"job_id": "j", "pointer": "/a/b"}}),
        ] {
            assert!(argument_refs(&json!({"a": bad})).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_pointer_finds_the_value_a_record_holds() {
        let marker = canonical_ref(&json!("big")).marker();
        let result = json!({"a": 1, "b": marker});
        assert_eq!(pointed(&result, "").unwrap(), Some(&result));
        assert_eq!(pointed(&result, "/a").unwrap(), Some(&json!(1)));
        assert_eq!(pointed(&result, "/c").unwrap(), None);
        assert_eq!(recorded_id(&result["b"]), canonical_ref(&json!("big")).id);
        assert_eq!(recorded_id(&json!(1)), canonical_ref(&json!(1)).id);
        // A result that is one blob holds no top-level values to point at.
        assert_eq!(pointed(&marker, "/a").unwrap(), None);
        assert_eq!(pointed(&json!([1]), "/0").unwrap(), None);
    }

    #[test]
    fn large_arguments_move_largest_first() {
        let args = json!({"big": "x".repeat(5_000), "mid": "y".repeat(2_000), "small": 1});
        let len = canonical_len(&args) + 100;
        assert_eq!(
            plan_argument_offload(&args, len, 1_000_000).unwrap(),
            Vec::<String>::new()
        );
        assert_eq!(
            plan_argument_offload(&args, len, 3_000).unwrap(),
            vec!["big".to_owned()]
        );
        assert_eq!(
            plan_argument_offload(&args, len, 500).unwrap(),
            vec!["big".to_owned(), "mid".to_owned()]
        );
        // A value that looks like a marker always moves.
        let odd = json!({"a": {"$x": 1}, "b": 2});
        assert_eq!(
            plan_argument_offload(&odd, 30, 1_000).unwrap(),
            vec!["a".to_owned()]
        );
        let many: Map<String, Value> = (0..MAX_MARKERS + 1)
            .map(|i| (format!("k{i:05}"), Value::String("v".repeat(200))))
            .collect();
        let many = Value::Object(many);
        assert!(plan_argument_offload(&many, canonical_len(&many), 100).is_err());
    }
}

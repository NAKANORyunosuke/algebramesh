//! Measures how decoding a message read in 64 KiB pieces scales with its size
//! (Issue #8). Not run by default:
//!
//! ```text
//! cargo test --release -p algebramesh-openxm-protocol --test decode_scaling -- --ignored --nocapture
//! ```

#![allow(clippy::unwrap_used)]

use std::time::{Duration, Instant};

use algebramesh_openxm_protocol::{
    Cmo, DecodeLimits, MessageReader, OxMessage, WireOrder, decode_message, encode_message,
};

const PIECE: usize = 64 * 1024;

/// A message of about `bytes` bytes: a list of data of at most 1 MiB each, or a
/// list of 32-bit integers (8 bytes each on the wire).
fn message(bytes: usize, list: bool) -> Vec<u8> {
    let cmo = if list {
        Cmo::List((0..bytes / 8).map(|i| Cmo::Int32(i as i32)).collect())
    } else {
        let block = bytes.min(1 << 20);
        Cmo::List(
            (0..bytes / block)
                .map(|_| Cmo::Datum(vec![0x5a; block]))
                .collect(),
        )
    };
    encode_message(&OxMessage::data(1, cmo), WireOrder::Big).unwrap()
}

/// Feeds `stream` in pieces with the reader; returns the time and the largest
/// buffer.
fn with_reader(stream: &[u8]) -> (Duration, usize) {
    let mut reader = MessageReader::new(WireOrder::Big, DecodeLimits::default());
    let mut buffer = Vec::new();
    let mut peak = 0;
    let started = Instant::now();
    for piece in stream.chunks(PIECE) {
        buffer.extend_from_slice(piece);
        peak = peak.max(buffer.capacity());
        if let Some((_, used)) = reader.read(&buffer).unwrap() {
            buffer.drain(..used);
        }
    }
    assert!(buffer.is_empty());
    (started.elapsed(), peak)
}

/// The earlier way: decode the whole buffer after every piece.
fn reparsing(stream: &[u8]) -> Duration {
    let mut buffer = Vec::new();
    let started = Instant::now();
    for piece in stream.chunks(PIECE) {
        buffer.extend_from_slice(piece);
        if let Some((_, used)) =
            decode_message(&buffer, WireOrder::Big, DecodeLimits::default()).unwrap()
        {
            buffer.drain(..used);
        }
    }
    assert!(buffer.is_empty());
    started.elapsed()
}

#[test]
#[ignore = "a measurement, run by hand in release mode"]
fn decoding_time_grows_with_the_message_size() {
    println!("| message | size | reader | reader per MiB | peak buffer | decoding every prefix |");
    println!("|---|---|---|---|---|---|");
    for list in [false, true] {
        for size in [1 << 10, 1 << 20, 16 << 20, 128 << 20] {
            let stream = message(size, list);
            let (time, peak) = with_reader(&stream);
            // The earlier way is quadratic; measured up to 16 MiB.
            let old = if size <= 16 << 20 {
                format!("{:.3} s", reparsing(&stream).as_secs_f64())
            } else {
                "-".into()
            };
            let mib = stream.len() as f64 / f64::from(1 << 20);
            println!(
                "| {} | {} | {:.3} s | {:.4} s | {:.1} MiB | {old} |",
                if list { "integers" } else { "data" },
                stream.len(),
                time.as_secs_f64(),
                time.as_secs_f64() / mib,
                peak as f64 / f64::from(1 << 20),
            );
        }
    }
}

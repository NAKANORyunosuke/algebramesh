//! Property tests: arbitrary bytes never panic the decoder, and encoded values round-trip.

#![allow(clippy::unwrap_used)]

use algebramesh_openxm_protocol::{
    Cmo, DecodeLimits, MessageReader, OxMessage, WireOrder, decode_cmo, decode_message, encode_cmo,
    encode_message,
};
use num_bigint::BigInt;
use proptest::prelude::*;

fn small_limits() -> DecodeLimits {
    DecodeLimits {
        max_message_bytes: 1 << 16,
        max_depth: 32,
        max_list_len: 1 << 10,
        max_string_bytes: 1 << 12,
        max_zz_words: 64,
    }
}

fn arb_cmo() -> impl Strategy<Value = Cmo> {
    let leaf = prop_oneof![
        Just(Cmo::Null),
        Just(Cmo::Zero),
        any::<i32>().prop_map(Cmo::Int32),
        ".{0,16}".prop_map(Cmo::String),
        proptest::collection::vec(any::<u8>(), 0..16).prop_map(Cmo::Datum),
        any::<i128>().prop_map(|v| Cmo::Zz(BigInt::from(v))),
        (
            any::<i64>(),
            any::<i64>().prop_filter("non-zero", |d| *d != 0)
        )
            .prop_map(|(n, d)| {
                Cmo::Qq {
                    num: BigInt::from(n),
                    den: BigInt::from(d),
                }
            }),
    ];
    leaf.prop_recursive(4, 64, 8, |inner| {
        prop_oneof![
            proptest::collection::vec(inner.clone(), 0..8).prop_map(Cmo::List),
            inner.clone().prop_map(|c| Cmo::Error2(Box::new(c))),
            inner.prop_map(|c| Cmo::Mathcap(Box::new(c))),
        ]
    })
}

proptest! {
    #[test]
    fn decoder_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
        for order in [WireOrder::Big, WireOrder::Little] {
            let _ = decode_message(&bytes, order, small_limits());
            let _ = decode_cmo(&bytes, order, small_limits());
            let mut reader = MessageReader::new(order, small_limits());
            for end in 0..=bytes.len() {
                if reader.read(&bytes[..end]).is_err() {
                    break;
                }
            }
        }
    }

    /// The reader, fed in arbitrary pieces, returns the messages that were
    /// encoded, as decoding each whole message does.
    #[test]
    fn the_reader_agrees_with_the_decoder(
        cmos in proptest::collection::vec(arb_cmo(), 1..4),
        pieces in proptest::collection::vec(1usize..64, 1..32),
        little in any::<bool>(),
    ) {
        let order = if little { WireOrder::Little } else { WireOrder::Big };
        let messages: Vec<OxMessage> = cmos
            .into_iter()
            .enumerate()
            .map(|(i, c)| OxMessage::data(i as u32, c))
            .collect();
        let mut stream = Vec::new();
        for m in &messages {
            stream.extend(encode_message(m, order).unwrap());
        }
        let mut reader = MessageReader::new(order, small_limits());
        let mut buffer = Vec::new();
        let mut got = Vec::new();
        let mut at = 0;
        let mut sizes = pieces.iter().cycle();
        while at < stream.len() {
            let end = (at + sizes.next().unwrap()).min(stream.len());
            buffer.extend_from_slice(&stream[at..end]);
            at = end;
            while let Some((m, used)) = reader.read(&buffer).unwrap() {
                buffer.drain(..used);
                got.push(m);
            }
        }
        prop_assert!(buffer.is_empty());
        prop_assert_eq!(got, messages);
    }

    #[test]
    fn encoded_values_roundtrip(cmo in arb_cmo()) {
        for order in [WireOrder::Big, WireOrder::Little] {
            let mut out = Vec::new();
            encode_cmo(&cmo, order, &mut out).unwrap();
            let (back, used) = decode_cmo(&out, order, small_limits()).unwrap();
            prop_assert_eq!(&back, &cmo);
            prop_assert_eq!(used, out.len());
        }
    }

    #[test]
    fn truncation_is_incomplete_not_error(cmo in arb_cmo(), cut_ratio in 0.0f64..1.0) {
        let mut out = Vec::new();
        encode_cmo(&cmo, WireOrder::Big, &mut out).unwrap();
        let cut = ((out.len() as f64) * cut_ratio) as usize;
        if cut < out.len() {
            prop_assert_eq!(
                decode_cmo(&out[..cut], WireOrder::Big, small_limits()),
                Err(algebramesh_openxm_protocol::DecodeError::Incomplete)
            );
        }
    }
}

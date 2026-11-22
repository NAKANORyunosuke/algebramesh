//! OpenXM protocol layer: OX message framing, byte-order negotiation and the CMO codec.
//!
//! This crate performs no I/O. Decoding works on byte slices and reports
//! [`DecodeError::Incomplete`]-style "need more data" as `Ok(None)` from
//! [`decode_message`], so any transport (Tokio, blocking sockets, files) can drive it.
//! A transport that reads a message in pieces uses [`MessageReader`], which looks
//! at each byte a bounded number of times instead of decoding every prefix.
//!
//! The wire format follows OpenXM-RFC 100 and the reference implementation in
//! `OpenXM_contrib2/asir2018/io/cio.c`; see `docs/adr/0001-openxm-wire-format.md`.

pub mod byte_order;
pub mod cmo;
pub mod codec;
pub mod constants;
pub mod message;
pub mod scan;

pub use byte_order::{ByteOrderByte, WireOrder, negotiate};
pub use cmo::Cmo;
pub use codec::{DecodeError, DecodeLimits, EncodeError, decode_cmo, encode_cmo};
pub use constants::{SmCommand, cmo_tag, ox_tag};
pub use message::{OxBody, OxMessage, decode_message, encode_message};
pub use scan::MessageReader;

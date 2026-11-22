use std::io;
use std::net::SocketAddr;
use std::time::Duration;

use algebramesh_openxm_protocol::byte_order::ByteOrderError;
use algebramesh_openxm_protocol::{Cmo, DecodeError, EncodeError};
use thiserror::Error;

use crate::channel::Channel;

#[derive(Debug, Error)]
pub enum OxError {
    #[error("I/O error on the {channel} channel: {source}")]
    Io {
        channel: Channel,
        #[source]
        source: io::Error,
    },
    #[error("the {channel} channel was closed by the peer")]
    Closed { channel: Channel },
    #[error("byte-order negotiation failed on the {channel} channel: {source}")]
    ByteOrder {
        channel: Channel,
        #[source]
        source: ByteOrderError,
    },
    #[error("malformed data on the {channel} channel: {source}")]
    Decode {
        channel: Channel,
        #[source]
        source: DecodeError,
    },
    #[error("cannot encode request: {0}")]
    Encode(#[from] EncodeError),
    #[error("unexpected message on the {channel} channel: {detail}")]
    UnexpectedMessage { channel: Channel, detail: String },
    #[error("the server returned an error object: {}", server_error_message(.0))]
    Server(Cmo),
    #[error("timed out after {after:?} while {during}")]
    Timeout {
        after: Duration,
        during: &'static str,
    },
    #[error("the session is unusable: {0}")]
    Poisoned(String),
    #[error(
        "control connection from {control} and data connection from {data} have different hosts"
    )]
    PeerMismatch {
        control: SocketAddr,
        data: SocketAddr,
    },
    #[error("malformed mathcap: {0}")]
    Mathcap(String),
    #[error("cannot bind listener: {0}")]
    Bind(#[source] io::Error),
    #[error("cannot start the OpenXM launcher: {0}")]
    Spawn(#[source] io::Error),
}

impl OxError {
    /// Whether the stack of the remote server may be inconsistent after this error.
    /// Only a well-formed error object returned by the server leaves it consistent.
    pub fn poisons_session(&self) -> bool {
        !matches!(self, Self::Server(_))
    }
}

/// Extracts a readable message from a `CMO_ERROR2` object.
///
/// Asir sends `CMO_ERROR2(CMO_LIST[serial, message, ...])`; the first string found is
/// used, and the debug form of the object otherwise.
pub fn server_error_message(cmo: &Cmo) -> String {
    fn first_string(cmo: &Cmo) -> Option<&str> {
        match cmo {
            Cmo::String(s) => Some(s),
            Cmo::List(items) => items.iter().find_map(first_string),
            Cmo::Error2(inner) => first_string(inner),
            _ => None,
        }
    }
    first_string(cmo)
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{cmo:?}"))
}

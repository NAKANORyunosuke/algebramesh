use std::fmt;
use std::io;

use algebramesh_openxm_protocol::{
    ByteOrderByte, DecodeLimits, MessageReader, OxMessage, WireOrder, encode_message, negotiate,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::error::OxError;

/// The two OpenXM connections of a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    /// Carries control commands (`SM_control_*`, `SM_shutdown`) to the launcher.
    Control,
    /// Carries data and Stack Machine commands to the server.
    Data,
}

impl fmt::Display for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Control => "control",
            Self::Data => "data",
        })
    }
}

const READ_CHUNK: usize = 64 * 1024;

pub(crate) struct OxChannel {
    kind: Channel,
    stream: TcpStream,
    order: WireOrder,
    /// Bytes received and not yet decoded.
    buffer: Vec<u8>,
    /// Where the message at the start of `buffer` stands (Issue #8).
    reader: MessageReader,
}

impl OxChannel {
    /// Performs byte-order negotiation in the client role: the server writes its
    /// byte first, then the client answers with network byte order.
    pub(crate) async fn handshake(
        kind: Channel,
        mut stream: TcpStream,
        limits: DecodeLimits,
    ) -> Result<Self, OxError> {
        stream.set_nodelay(true).map_err(|e| io_error(kind, e))?;
        let mut byte = [0u8; 1];
        stream
            .read_exact(&mut byte)
            .await
            .map_err(|e| io_error(kind, e))?;
        let order =
            negotiate(ByteOrderByte::Network, byte[0]).map_err(|source| OxError::ByteOrder {
                channel: kind,
                source,
            })?;
        stream
            .write_all(&[ByteOrderByte::Network.to_byte()])
            .await
            .map_err(|e| io_error(kind, e))?;
        stream.flush().await.map_err(|e| io_error(kind, e))?;
        Ok(Self {
            kind,
            stream,
            order,
            buffer: Vec::new(),
            reader: MessageReader::new(order, limits),
        })
    }

    /// Wraps a stream whose byte order is already negotiated.
    #[cfg(any(test, feature = "testing"))]
    pub(crate) fn negotiated(
        kind: Channel,
        stream: TcpStream,
        order: WireOrder,
        limits: DecodeLimits,
    ) -> Self {
        Self {
            kind,
            stream,
            order,
            buffer: Vec::new(),
            reader: MessageReader::new(order, limits),
        }
    }

    pub(crate) async fn send(&mut self, message: &OxMessage) -> Result<(), OxError> {
        let bytes = encode_message(message, self.order)?;
        self.stream
            .write_all(&bytes)
            .await
            .map_err(|e| io_error(self.kind, e))?;
        self.stream
            .flush()
            .await
            .map_err(|e| io_error(self.kind, e))
    }

    /// Receives the next message. The reader keeps its place in a message between
    /// reads, so a message of `n` bytes costs time proportional to `n` however
    /// it is split (Issue #8).
    ///
    /// Cancel-safe: a receive that is dropped while waiting keeps every byte
    /// read so far, and the next one goes on from there.
    pub(crate) async fn recv(&mut self) -> Result<OxMessage, OxError> {
        loop {
            let decoded = self
                .reader
                .read(&self.buffer)
                .map_err(|source| OxError::Decode {
                    channel: self.kind,
                    source,
                })?;
            if let Some((message, used)) = decoded {
                self.buffer.drain(..used);
                return Ok(message);
            }
            self.buffer.reserve(READ_CHUNK);
            // `read_buf` appends only the bytes it read.
            let n = self
                .stream
                .read_buf(&mut self.buffer)
                .await
                .map_err(|e| io_error(self.kind, e))?;
            if n == 0 {
                return Err(OxError::Closed { channel: self.kind });
            }
        }
    }

    pub(crate) fn wire_order(&self) -> WireOrder {
        self.order
    }
}

fn io_error(channel: Channel, source: io::Error) -> OxError {
    if source.kind() == io::ErrorKind::UnexpectedEof {
        OxError::Closed { channel }
    } else {
        OxError::Io { channel, source }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::time::Duration;

    use algebramesh_openxm_protocol::Cmo;
    use tokio::net::TcpListener;

    use super::*;

    async fn pair() -> (OxChannel, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (ours, theirs) = tokio::join!(TcpStream::connect(addr), listener.accept());
        let channel = OxChannel::negotiated(
            Channel::Data,
            ours.unwrap(),
            WireOrder::Big,
            DecodeLimits::default(),
        );
        (channel, theirs.unwrap().0)
    }

    #[tokio::test]
    async fn a_dropped_receive_loses_no_bytes() {
        let (mut channel, mut peer) = pair().await;
        let message = OxMessage::data(1, Cmo::string("x".repeat(100_000)));
        let bytes = encode_message(&message, WireOrder::Big).unwrap();
        let (first, rest) = bytes.split_at(50_000);
        peer.write_all(first).await.unwrap();
        // The receive waits for the rest and is dropped, as by a timeout.
        let waited = tokio::time::timeout(Duration::from_millis(200), channel.recv()).await;
        assert!(waited.is_err());
        peer.write_all(rest).await.unwrap();
        assert_eq!(channel.recv().await.unwrap(), message);
    }

    #[tokio::test]
    async fn large_and_concatenated_messages_arrive_whole() {
        let (mut channel, mut peer) = pair().await;
        let messages = [
            OxMessage::data(1, Cmo::List((0..200_000).map(Cmo::Int32).collect())),
            OxMessage::data(2, Cmo::Datum(vec![7; 3 * READ_CHUNK + 5])),
            OxMessage::data(3, Cmo::Null),
        ];
        let mut stream = Vec::new();
        for m in &messages {
            stream.extend(encode_message(m, WireOrder::Big).unwrap());
        }
        let writer = tokio::spawn(async move {
            // Uneven pieces.
            for piece in stream.chunks(10_007) {
                peer.write_all(piece).await.unwrap();
            }
            peer
        });
        for m in &messages {
            assert_eq!(&channel.recv().await.unwrap(), m);
        }
        drop(writer.await.unwrap());
        // The peer is gone: an end of input, or on Windows sometimes a reset.
        let after = channel.recv().await;
        assert!(
            matches!(after, Err(OxError::Closed { .. } | OxError::Io { .. })),
            "{after:?}"
        );
    }
}

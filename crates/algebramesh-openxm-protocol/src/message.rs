//! OX message framing: `message tag`, `serial number`, `body`.
//!
//! The serial number identifies an OX message on one channel only. It is not a job
//! identifier and must not be used to match results to jobs (design §5.1, §5.4).

use crate::byte_order::WireOrder;
use crate::cmo::Cmo;
use crate::codec::{DecodeError, DecodeLimits, EncodeError, Reader, encode_cmo, put_u32};
use crate::constants::{SmCommand, ox_tag};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OxBody {
    /// `OX_COMMAND` with the raw Stack Machine code; see [`SmCommand::from_code`].
    Command(u32),
    Data(Cmo),
    SyncBall,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OxMessage {
    pub serial: u32,
    pub body: OxBody,
}

impl OxMessage {
    pub fn command(serial: u32, command: SmCommand) -> Self {
        Self {
            serial,
            body: OxBody::Command(command.code()),
        }
    }

    pub fn data(serial: u32, cmo: Cmo) -> Self {
        Self {
            serial,
            body: OxBody::Data(cmo),
        }
    }

    pub fn tag(&self) -> u32 {
        match self.body {
            OxBody::Command(_) => ox_tag::COMMAND,
            OxBody::Data(_) => ox_tag::DATA,
            OxBody::SyncBall => ox_tag::SYNC_BALL,
        }
    }
}

pub fn encode_message(message: &OxMessage, order: WireOrder) -> Result<Vec<u8>, EncodeError> {
    let mut out = Vec::new();
    put_u32(&mut out, order, message.tag());
    put_u32(&mut out, order, message.serial);
    match &message.body {
        OxBody::Command(code) => put_u32(&mut out, order, *code),
        OxBody::Data(cmo) => encode_cmo(cmo, order, &mut out)?,
        OxBody::SyncBall => {}
    }
    Ok(out)
}

/// Decodes one OX message from the start of `buf`.
///
/// Returns `Ok(None)` if `buf` holds only a prefix of a message, and
/// `Ok(Some((message, consumed)))` otherwise. A prefix longer than
/// [`DecodeLimits::max_message_bytes`] is an error.
pub fn decode_message(
    buf: &[u8],
    order: WireOrder,
    limits: DecodeLimits,
) -> Result<Option<(OxMessage, usize)>, DecodeError> {
    let window = &buf[..buf.len().min(limits.max_message_bytes)];
    let mut reader = Reader::new(window, order, limits);
    match read_message(&mut reader) {
        Ok(message) => Ok(Some((message, reader.position()))),
        Err(DecodeError::Incomplete) if buf.len() >= limits.max_message_bytes => {
            Err(DecodeError::LimitExceeded {
                what: "OX message",
                size: buf.len(),
                limit: limits.max_message_bytes,
            })
        }
        Err(DecodeError::Incomplete) => Ok(None),
        Err(e) => Err(e),
    }
}

fn read_message(reader: &mut Reader<'_>) -> Result<OxMessage, DecodeError> {
    let tag = reader.u32()?;
    let serial = reader.u32()?;
    let body = match tag {
        ox_tag::COMMAND => OxBody::Command(reader.u32()?),
        ox_tag::DATA => OxBody::Data(reader.cmo(0)?),
        ox_tag::SYNC_BALL => OxBody::SyncBall,
        other => return Err(DecodeError::UnsupportedOxTag { tag: other }),
    };
    Ok(OxMessage { serial, body })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn golden_command_message() {
        let msg = OxMessage::command(7, SmCommand::PopCmo);
        assert_eq!(
            hex(&encode_message(&msg, WireOrder::Big).unwrap()),
            "000002010000000700000106"
        );
        assert_eq!(
            hex(&encode_message(&msg, WireOrder::Little).unwrap()),
            "010200000700000006010000"
        );
    }

    #[test]
    fn golden_data_message() {
        let msg = OxMessage::data(1, Cmo::Int32(5));
        assert_eq!(
            hex(&encode_message(&msg, WireOrder::Big).unwrap()),
            "00000202000000010000000200000005"
        );
    }

    #[test]
    fn decode_handles_partial_and_concatenated_messages() {
        let a = encode_message(&OxMessage::data(1, Cmo::string("abc")), WireOrder::Big).unwrap();
        let b = encode_message(&OxMessage::command(2, SmCommand::Mathcap), WireOrder::Big).unwrap();
        let mut stream = a.clone();
        stream.extend_from_slice(&b);

        for cut in 0..a.len() {
            assert_eq!(
                decode_message(&stream[..cut], WireOrder::Big, DecodeLimits::default()),
                Ok(None)
            );
        }
        let (first, used) = decode_message(&stream, WireOrder::Big, DecodeLimits::default())
            .unwrap()
            .unwrap();
        assert_eq!(first, OxMessage::data(1, Cmo::string("abc")));
        let (second, used2) =
            decode_message(&stream[used..], WireOrder::Big, DecodeLimits::default())
                .unwrap()
                .unwrap();
        assert_eq!(second, OxMessage::command(2, SmCommand::Mathcap));
        assert_eq!(used + used2, stream.len());
    }

    #[test]
    fn oversized_prefix_is_an_error() {
        let limits = DecodeLimits {
            max_message_bytes: 16,
            ..DecodeLimits::default()
        };
        let msg = encode_message(
            &OxMessage::data(1, Cmo::string("this string is longer than sixteen bytes")),
            WireOrder::Big,
        )
        .unwrap();
        assert!(matches!(
            decode_message(&msg, WireOrder::Big, limits),
            Err(DecodeError::LimitExceeded { .. })
        ));
    }

    #[test]
    fn unknown_ox_tag_is_rejected() {
        let bytes = [0, 0, 0, 9, 0, 0, 0, 1];
        assert_eq!(
            decode_message(&bytes, WireOrder::Big, DecodeLimits::default()),
            Err(DecodeError::UnsupportedOxTag { tag: 9 })
        );
    }
}

//! Byte-order negotiation (OpenXM-RFC 100, "byte order").
//!
//! Right after a channel is connected, each side sends one byte naming the byte
//! order it prefers. If both bytes are equal that order is used; otherwise the
//! channel uses network byte order. The reference server (`ox_asir`, `ox_launch`)
//! writes its byte first and then reads the client's byte.

use thiserror::Error;

/// The single byte exchanged during negotiation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ByteOrderByte {
    Network = 0x00,
    LittleEndian = 0x01,
    BigEndian = 0xff,
}

impl ByteOrderByte {
    pub const fn to_byte(self) -> u8 {
        self as u8
    }

    pub fn from_byte(byte: u8) -> Result<Self, ByteOrderError> {
        match byte {
            0x00 => Ok(Self::Network),
            0x01 => Ok(Self::LittleEndian),
            0xff => Ok(Self::BigEndian),
            other => Err(ByteOrderError::InvalidByte(other)),
        }
    }

    const fn wire(self) -> WireOrder {
        match self {
            Self::Network | Self::BigEndian => WireOrder::Big,
            Self::LittleEndian => WireOrder::Little,
        }
    }
}

/// The byte order actually used for 32-bit words on a negotiated channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WireOrder {
    Big,
    Little,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ByteOrderError {
    /// `ox_launch` writes `2` on the data channel when it fails to spawn the
    /// server process, so this byte is reported separately.
    #[error("the launcher reported that the OpenXM server could not be spawned (byte 0x02)")]
    ServerSpawnFailed,
    #[error("invalid byte-order byte 0x{0:02x}")]
    InvalidByte(u8),
}

/// Determines the wire order from the two exchanged bytes.
pub fn negotiate(local: ByteOrderByte, peer: u8) -> Result<WireOrder, ByteOrderError> {
    if peer == 0x02 {
        return Err(ByteOrderError::ServerSpawnFailed);
    }
    let peer = ByteOrderByte::from_byte(peer)?;
    if local == peer {
        Ok(local.wire())
    } else {
        Ok(WireOrder::Big)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requesting_network_order_always_yields_big_endian() {
        for peer in [0x00, 0x01, 0xff] {
            assert_eq!(negotiate(ByteOrderByte::Network, peer), Ok(WireOrder::Big));
        }
    }

    #[test]
    fn little_endian_only_when_both_sides_agree() {
        assert_eq!(
            negotiate(ByteOrderByte::LittleEndian, 0x01),
            Ok(WireOrder::Little)
        );
        assert_eq!(
            negotiate(ByteOrderByte::LittleEndian, 0xff),
            Ok(WireOrder::Big)
        );
        assert_eq!(
            negotiate(ByteOrderByte::BigEndian, 0xff),
            Ok(WireOrder::Big)
        );
    }

    #[test]
    fn rejects_invalid_bytes() {
        assert_eq!(
            negotiate(ByteOrderByte::Network, 0x02),
            Err(ByteOrderError::ServerSpawnFailed)
        );
        assert_eq!(
            negotiate(ByteOrderByte::Network, 0x7f),
            Err(ByteOrderError::InvalidByte(0x7f))
        );
    }
}

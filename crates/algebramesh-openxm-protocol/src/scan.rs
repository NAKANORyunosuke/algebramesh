//! Finding the end of an OX message in a growing buffer (Issue #8).
//!
//! [`decode_message`](crate::decode_message) decodes from the start of a buffer
//! and reports a prefix as incomplete. A transport that calls it after every read
//! decodes the start of a large message again and again: the work grows with the
//! square of the message size. [`MessageReader`] instead walks the structure of
//! the message once, keeping its place between calls, and decodes the message
//! only when all of it has arrived:
//!
//! * Strings, data and integers are skipped by their length, without looking at
//!   their bytes, once the bytes are there.
//! * Lists and polynomials are followed with an explicit stack, one element at a
//!   time; an element is never looked at twice.
//! * The limits of [`DecodeLimits`] apply as soon as a length is read: a message
//!   that claims to be larger than `max_message_bytes` is refused before its
//!   bytes arrive.
//!
//! Errors that need the decoded values (invalid UTF-8, a zero denominator, a bad
//! variable index) are reported by the decoding, once the message is complete.

use crate::byte_order::WireOrder;
use crate::codec::{DecodeError, DecodeLimits};
use crate::constants::{cmo_tag, ox_tag};
use crate::message::{OxMessage, decode_message};

/// Objects still to be read at one nesting level.
#[derive(Debug, Clone, Copy)]
struct Frame {
    remaining: usize,
    /// Each object is preceded by a 32-bit exponent (univariate polynomials).
    exponents: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Header,
    Body,
}

/// Decodes OX messages from a buffer that grows at its end, looking at each byte
/// of a message a bounded number of times however the bytes arrive.
#[derive(Debug, Clone)]
pub struct MessageReader {
    order: WireOrder,
    limits: DecodeLimits,
    /// End of the part of the current message already walked.
    pos: usize,
    stage: Stage,
    stack: Vec<Frame>,
    /// Objects walked since the reader was created (for tests of the cost).
    visited: u64,
}

impl MessageReader {
    pub fn new(order: WireOrder, limits: DecodeLimits) -> Self {
        Self {
            order,
            limits,
            pos: 0,
            stage: Stage::Header,
            stack: Vec::new(),
            visited: 0,
        }
    }

    /// Decodes the message at the start of `buf`, which holds the bytes given to
    /// the previous calls and possibly more.
    ///
    /// Returns `Ok(None)` while the message is incomplete, and
    /// `Ok(Some((message, consumed)))` once it is complete; the caller then
    /// removes the `consumed` bytes, and the next call starts on the next
    /// message.
    pub fn read(&mut self, buf: &[u8]) -> Result<Option<(OxMessage, usize)>, DecodeError> {
        let result = self.walk(buf);
        let end = match result {
            Ok(Some(end)) => end,
            Ok(None) => return Ok(None),
            Err(e) => {
                self.reset();
                return Err(e);
            }
        };
        self.reset();
        match decode_message(&buf[..end], self.order, self.limits)? {
            Some((message, used)) if used == end => Ok(Some((message, used))),
            // The walk and the decoder disagree: never trust either.
            _ => Err(DecodeError::Invalid {
                what: "OX message",
                detail: format!("the message structure ends at byte {end}, but decoding does not"),
            }),
        }
    }

    /// Objects walked so far.
    pub fn visited(&self) -> u64 {
        self.visited
    }

    fn reset(&mut self) {
        self.pos = 0;
        self.stage = Stage::Header;
        self.stack.clear();
    }

    /// Bytes `pos..pos + n` of `buf`, if they are there and within the message
    /// limit.
    fn need(&self, buf: &[u8], from: usize, n: usize) -> Result<Option<usize>, DecodeError> {
        let end = from.checked_add(n).ok_or(DecodeError::LimitExceeded {
            what: "OX message",
            size: usize::MAX,
            limit: self.limits.max_message_bytes,
        })?;
        if end > self.limits.max_message_bytes {
            return Err(DecodeError::LimitExceeded {
                what: "OX message",
                size: end,
                limit: self.limits.max_message_bytes,
            });
        }
        Ok((end <= buf.len()).then_some(end))
    }

    fn word(&self, buf: &[u8], at: usize) -> u32 {
        let bytes = [buf[at], buf[at + 1], buf[at + 2], buf[at + 3]];
        match self.order {
            WireOrder::Big => u32::from_be_bytes(bytes),
            WireOrder::Little => u32::from_le_bytes(bytes),
        }
    }

    /// A non-negative length at `at`, at most `limit`.
    fn length(
        &self,
        buf: &[u8],
        at: usize,
        what: &'static str,
        limit: usize,
    ) -> Result<usize, DecodeError> {
        let value = self.word(buf, at) as i32;
        if value < 0 {
            return Err(DecodeError::NegativeLength {
                what,
                value,
                offset: at,
            });
        }
        let size = value as usize;
        if size > limit {
            return Err(DecodeError::LimitExceeded { what, size, limit });
        }
        Ok(size)
    }

    /// The end of a `CMO_ZZ` body (count word and magnitude) at `at`, if it is
    /// all there.
    fn zz_end(&self, buf: &[u8], at: usize) -> Result<Option<usize>, DecodeError> {
        let Some(after_count) = self.need(buf, at, 4)? else {
            return Ok(None);
        };
        let words = (self.word(buf, at) as i32).unsigned_abs() as usize;
        if words > self.limits.max_zz_words {
            return Err(DecodeError::LimitExceeded {
                what: "CMO_ZZ word count",
                size: words,
                limit: self.limits.max_zz_words,
            });
        }
        self.need(buf, after_count, words * 4)
    }

    /// Walks on from where the previous call stopped. Returns the end of the
    /// message once it is complete.
    fn walk(&mut self, buf: &[u8]) -> Result<Option<usize>, DecodeError> {
        if self.stage == Stage::Header {
            let Some(end) = self.need(buf, 0, 8)? else {
                return Ok(None);
            };
            match self.word(buf, 0) {
                ox_tag::COMMAND => {
                    return self.need(buf, end, 4);
                }
                ox_tag::SYNC_BALL => return Ok(Some(end)),
                ox_tag::DATA => {
                    self.pos = end;
                    self.stage = Stage::Body;
                    self.stack.push(Frame {
                        remaining: 1,
                        exponents: false,
                    });
                }
                other => return Err(DecodeError::UnsupportedOxTag { tag: other }),
            }
        }
        // Each step reads one object header (with the exponent before it, if any)
        // and commits it only when it is complete, so that a step that needs more
        // bytes is retried from the same place.
        while let Some(&top) = self.stack.last() {
            if top.remaining == 0 {
                self.stack.pop();
                continue;
            }
            let depth = self.stack.len() - 1;
            if depth > self.limits.max_depth {
                return Err(DecodeError::TooDeep {
                    limit: self.limits.max_depth,
                });
            }
            let start = if top.exponents {
                match self.need(buf, self.pos, 4)? {
                    Some(after) => {
                        // The exponent is a non-negative 32-bit length.
                        self.length(buf, self.pos, "exponent", u32::MAX as usize)?;
                        after
                    }
                    None => return Ok(None),
                }
            } else {
                self.pos
            };
            let Some(after_tag) = self.need(buf, start, 4)? else {
                return Ok(None);
            };
            let tag = self.word(buf, start);
            // (end of this object's own bytes, children to read after it)
            let (end, children) = match tag {
                cmo_tag::NULL | cmo_tag::ZERO => (Some(after_tag), None),
                cmo_tag::INT32 => (self.need(buf, after_tag, 4)?, None),
                cmo_tag::DATUM | cmo_tag::STRING => {
                    let Some(after_len) = self.need(buf, after_tag, 4)? else {
                        return Ok(None);
                    };
                    let what = if tag == cmo_tag::STRING {
                        "CMO_STRING"
                    } else {
                        "CMO_DATUM"
                    };
                    let len = self.length(buf, after_tag, what, self.limits.max_string_bytes)?;
                    (self.need(buf, after_len, len)?, None)
                }
                cmo_tag::ZZ => (self.zz_end(buf, after_tag)?, None),
                cmo_tag::QQ => match self.zz_end(buf, after_tag)? {
                    Some(after_num) => (self.zz_end(buf, after_num)?, None),
                    None => (None, None),
                },
                cmo_tag::MATHCAP | cmo_tag::ERROR2 | cmo_tag::INDETERMINATE => (
                    Some(after_tag),
                    Some(Frame {
                        remaining: 1,
                        exponents: false,
                    }),
                ),
                cmo_tag::RECURSIVE_POLYNOMIAL => (
                    Some(after_tag),
                    // The variable list, then the body.
                    Some(Frame {
                        remaining: 2,
                        exponents: false,
                    }),
                ),
                cmo_tag::LIST => match self.need(buf, after_tag, 4)? {
                    Some(after_len) => {
                        let len =
                            self.length(buf, after_tag, "CMO_LIST", self.limits.max_list_len)?;
                        (
                            Some(after_len),
                            Some(Frame {
                                remaining: len,
                                exponents: false,
                            }),
                        )
                    }
                    None => (None, None),
                },
                cmo_tag::UNIVARIATE_POLYNOMIAL => match self.need(buf, after_tag, 8)? {
                    Some(after_var) => {
                        let len = self.length(
                            buf,
                            after_tag,
                            "CMO_UNIVARIATE_POLYNOMIAL",
                            self.limits.max_list_len,
                        )?;
                        self.length(buf, after_tag + 4, "variable index", u32::MAX as usize)?;
                        (
                            Some(after_var),
                            Some(Frame {
                                remaining: len,
                                exponents: true,
                            }),
                        )
                    }
                    None => (None, None),
                },
                other => {
                    return Err(DecodeError::UnsupportedCmo {
                        tag: other,
                        offset: start,
                    });
                }
            };
            let Some(end) = end else {
                return Ok(None);
            };
            // Commit the object.
            self.visited += 1;
            self.pos = end;
            if let Some(top) = self.stack.last_mut() {
                top.remaining -= 1;
            }
            if let Some(frame) = children {
                self.stack.push(frame);
            }
        }
        Ok(Some(self.pos))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmo::Cmo;
    use crate::constants::SmCommand;
    use crate::message::encode_message;

    fn limits() -> DecodeLimits {
        DecodeLimits::default()
    }

    /// Feeds `stream` to a reader `chunk` bytes at a time, as a socket would, and
    /// returns the messages.
    fn feed(
        stream: &[u8],
        chunk: usize,
        limits: DecodeLimits,
    ) -> Result<Vec<OxMessage>, DecodeError> {
        let mut reader = MessageReader::new(WireOrder::Big, limits);
        let mut buffer = Vec::new();
        let mut out = Vec::new();
        for piece in stream.chunks(chunk.max(1)) {
            buffer.extend_from_slice(piece);
            while let Some((message, used)) = reader.read(&buffer)? {
                buffer.drain(..used);
                out.push(message);
            }
        }
        assert!(buffer.is_empty(), "{} bytes left", buffer.len());
        Ok(out)
    }

    fn polynomial() -> Cmo {
        // x^2 - y as a recursive polynomial in (x, y).
        Cmo::RecursivePolynomial {
            variables: vec![Cmo::string("x"), Cmo::string("y")],
            body: Box::new(Cmo::UnivariatePolynomial {
                variable: 0,
                terms: vec![
                    (2, Cmo::Int32(1)),
                    (
                        0,
                        Cmo::UnivariatePolynomial {
                            variable: 1,
                            terms: vec![(1, Cmo::Int32(-1))],
                        },
                    ),
                ],
            }),
        }
    }

    #[test]
    fn every_split_of_concatenated_messages_decodes_the_same() {
        let messages = vec![
            OxMessage::data(
                1,
                Cmo::List(vec![Cmo::string("abc"), Cmo::Zz(12345.into()), Cmo::Null]),
            ),
            OxMessage::command(2, SmCommand::PopCmo),
            OxMessage::data(3, polynomial()),
            OxMessage::data(
                4,
                Cmo::Qq {
                    num: (-7).into(),
                    den: 3.into(),
                },
            ),
            OxMessage::data(
                5,
                Cmo::Error2(Box::new(Cmo::List(vec![
                    Cmo::Int32(1),
                    Cmo::Datum(vec![1, 2, 3]),
                ]))),
            ),
            OxMessage::data(6, Cmo::List(vec![])),
        ];
        let mut stream = Vec::new();
        for m in &messages {
            stream.extend(encode_message(m, WireOrder::Big).unwrap());
        }
        for chunk in 1..=stream.len() {
            assert_eq!(
                feed(&stream, chunk, limits()).unwrap(),
                messages,
                "chunk {chunk}"
            );
        }
    }

    #[test]
    fn each_object_is_walked_once_however_the_bytes_arrive() {
        // A list of 10 000 integers, one byte at a time.
        let n = 10_000;
        let message = OxMessage::data(1, Cmo::List((0..n).map(Cmo::Int32).collect()));
        let stream = encode_message(&message, WireOrder::Big).unwrap();
        let mut reader = MessageReader::new(WireOrder::Big, limits());
        let mut buffer = Vec::new();
        let mut decoded = None;
        for &byte in &stream {
            buffer.push(byte);
            if let Some((m, _)) = reader.read(&buffer).unwrap() {
                decoded = Some(m);
            }
        }
        assert_eq!(decoded, Some(message));
        // The list and its elements, each once.
        assert_eq!(reader.visited(), n as u64 + 1);
    }

    #[test]
    fn malformed_messages_are_errors() {
        let bad_tag = [0, 0, 2, 2, 0, 0, 0, 1, 0x7f, 0, 0, 0];
        assert!(matches!(
            feed(&bad_tag, 3, limits()),
            Err(DecodeError::UnsupportedCmo { .. })
        ));
        let bad_ox = [0, 0, 9, 9, 0, 0, 0, 1];
        assert!(matches!(
            feed(&bad_ox, 1, limits()),
            Err(DecodeError::UnsupportedOxTag { .. })
        ));
        // A string of length -1.
        let negative = [0, 0, 2, 2, 0, 0, 0, 1, 0, 0, 0, 4, 0xff, 0xff, 0xff, 0xff];
        assert!(matches!(
            feed(&negative, 5, limits()),
            Err(DecodeError::NegativeLength { .. })
        ));
        // Invalid UTF-8 is found when the complete message is decoded.
        let invalid =
            encode_message(&OxMessage::data(1, Cmo::Datum(vec![0xff])), WireOrder::Big).unwrap();
        let mut invalid = invalid;
        invalid[11] = 4; // CMO_DATUM -> CMO_STRING
        assert!(matches!(
            feed(&invalid, 2, limits()),
            Err(DecodeError::InvalidUtf8 { .. })
        ));
    }

    #[test]
    fn limits_apply_before_the_bytes_arrive() {
        let small = DecodeLimits {
            max_message_bytes: 1024,
            max_depth: 4,
            ..DecodeLimits::default()
        };
        // A string that claims 2000 bytes: refused from its header alone.
        let mut header =
            encode_message(&OxMessage::data(1, Cmo::string("")), WireOrder::Big).unwrap();
        header[12..16].copy_from_slice(&2000u32.to_be_bytes());
        let mut reader = MessageReader::new(WireOrder::Big, small);
        assert!(matches!(
            reader.read(&header),
            Err(DecodeError::LimitExceeded {
                what: "OX message",
                ..
            })
        ));
        // Nesting deeper than the limit.
        let mut deep = Cmo::Null;
        for _ in 0..6 {
            deep = Cmo::List(vec![deep]);
        }
        let stream = encode_message(&OxMessage::data(1, deep), WireOrder::Big).unwrap();
        assert!(matches!(
            feed(&stream, 7, small),
            Err(DecodeError::TooDeep { .. })
        ));
        // A list that claims more elements than allowed.
        let list = encode_message(
            &OxMessage::data(1, Cmo::List(vec![Cmo::Null; 3])),
            WireOrder::Big,
        )
        .unwrap();
        let tight = DecodeLimits {
            max_list_len: 2,
            ..DecodeLimits::default()
        };
        assert!(matches!(
            feed(&list, 4, tight),
            Err(DecodeError::LimitExceeded { .. })
        ));
    }
}

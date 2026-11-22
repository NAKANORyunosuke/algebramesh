//! CMO encoder and bounded decoder.
//!
//! Integers on the wire are 32-bit words in the negotiated [`WireOrder`]. Strings and
//! byte arrays are a 32-bit length followed by raw bytes. `CMO_ZZ` is a signed word
//! count (its sign is the sign of the integer) followed by the magnitude as 32-bit
//! words, least significant word first.

use num_bigint::{BigInt, BigUint, Sign};
use num_traits::Zero;
use thiserror::Error;

use crate::byte_order::WireOrder;
use crate::cmo::Cmo;
use crate::constants::cmo_tag;

/// Upper bounds applied while decoding untrusted input (design §5.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeLimits {
    /// Maximum size of one OX message, header included.
    pub max_message_bytes: usize,
    /// Maximum nesting depth of CMO objects.
    pub max_depth: usize,
    /// Maximum number of elements in one list or univariate polynomial.
    pub max_list_len: usize,
    /// Maximum length of one string or datum.
    pub max_string_bytes: usize,
    /// Maximum number of 32-bit words in one `CMO_ZZ`.
    pub max_zz_words: usize,
}

impl Default for DecodeLimits {
    fn default() -> Self {
        Self {
            max_message_bytes: 256 * 1024 * 1024,
            max_depth: 512,
            max_list_len: 1 << 24,
            max_string_bytes: 64 * 1024 * 1024,
            max_zz_words: 1 << 22,
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DecodeError {
    /// More bytes are needed. [`crate::decode_message`] turns this into `Ok(None)`.
    #[error("incomplete input")]
    Incomplete,
    #[error("unsupported CMO tag {tag} (0x{tag:08x}) at byte offset {offset}")]
    UnsupportedCmo { tag: u32, offset: usize },
    #[error("unsupported OX message tag {tag} (0x{tag:08x})")]
    UnsupportedOxTag { tag: u32 },
    #[error("negative length {value} for {what} at byte offset {offset}")]
    NegativeLength {
        what: &'static str,
        value: i32,
        offset: usize,
    },
    #[error("{what} of size {size} exceeds the limit {limit}")]
    LimitExceeded {
        what: &'static str,
        size: usize,
        limit: usize,
    },
    #[error("CMO nesting deeper than {limit}")]
    TooDeep { limit: usize },
    #[error("string at byte offset {offset} is not valid UTF-8")]
    InvalidUtf8 { offset: usize },
    #[error("invalid {what}: {detail}")]
    Invalid { what: &'static str, detail: String },
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum EncodeError {
    #[error("{what} of size {size} does not fit in a 32-bit length")]
    TooLong { what: &'static str, size: usize },
    #[error("invalid {what}: {detail}")]
    Invalid { what: &'static str, detail: String },
}

// ---------------------------------------------------------------------------
// Encoding
// ---------------------------------------------------------------------------

pub(crate) fn put_u32(out: &mut Vec<u8>, order: WireOrder, value: u32) {
    match order {
        WireOrder::Big => out.extend_from_slice(&value.to_be_bytes()),
        WireOrder::Little => out.extend_from_slice(&value.to_le_bytes()),
    }
}

fn put_len(
    out: &mut Vec<u8>,
    order: WireOrder,
    what: &'static str,
    len: usize,
) -> Result<(), EncodeError> {
    let len = i32::try_from(len).map_err(|_| EncodeError::TooLong { what, size: len })?;
    put_u32(out, order, len as u32);
    Ok(())
}

fn put_zz_body(out: &mut Vec<u8>, order: WireOrder, value: &BigInt) -> Result<(), EncodeError> {
    let words = value.magnitude().to_u32_digits();
    let count = i32::try_from(words.len()).map_err(|_| EncodeError::TooLong {
        what: "CMO_ZZ",
        size: words.len(),
    })?;
    let signed = if value.sign() == Sign::Minus {
        -count
    } else {
        count
    };
    put_u32(out, order, signed as u32);
    for word in words {
        put_u32(out, order, word);
    }
    Ok(())
}

/// Appends the encoding of `cmo` (tag included) to `out`.
pub fn encode_cmo(cmo: &Cmo, order: WireOrder, out: &mut Vec<u8>) -> Result<(), EncodeError> {
    put_u32(out, order, cmo.tag());
    match cmo {
        Cmo::Null | Cmo::Zero => {}
        Cmo::Int32(v) => put_u32(out, order, *v as u32),
        Cmo::Datum(bytes) => {
            put_len(out, order, "CMO_DATUM", bytes.len())?;
            out.extend_from_slice(bytes);
        }
        Cmo::String(s) => {
            put_len(out, order, "CMO_STRING", s.len())?;
            out.extend_from_slice(s.as_bytes());
        }
        Cmo::Mathcap(inner) | Cmo::Error2(inner) | Cmo::Indeterminate(inner) => {
            encode_cmo(inner, order, out)?;
        }
        Cmo::List(items) => {
            put_len(out, order, "CMO_LIST", items.len())?;
            for item in items {
                encode_cmo(item, order, out)?;
            }
        }
        Cmo::Zz(value) => put_zz_body(out, order, value)?,
        Cmo::Qq { num, den } => {
            if den.is_zero() {
                return Err(EncodeError::Invalid {
                    what: "CMO_QQ",
                    detail: "zero denominator".into(),
                });
            }
            put_zz_body(out, order, num)?;
            put_zz_body(out, order, den)?;
        }
        Cmo::RecursivePolynomial { variables, body } => {
            encode_cmo(&Cmo::List(variables.clone()), order, out)?;
            encode_cmo(body, order, out)?;
        }
        Cmo::UnivariatePolynomial { variable, terms } => {
            put_len(out, order, "CMO_UNIVARIATE_POLYNOMIAL", terms.len())?;
            let variable = i32::try_from(*variable).map_err(|_| EncodeError::Invalid {
                what: "CMO_UNIVARIATE_POLYNOMIAL",
                detail: format!("variable index {variable} out of range"),
            })?;
            put_u32(out, order, variable as u32);
            for (exponent, coefficient) in terms {
                let exponent = i32::try_from(*exponent).map_err(|_| EncodeError::Invalid {
                    what: "CMO_UNIVARIATE_POLYNOMIAL",
                    detail: format!("exponent {exponent} out of range"),
                })?;
                put_u32(out, order, exponent as u32);
                encode_cmo(coefficient, order, out)?;
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Decoding
// ---------------------------------------------------------------------------

pub(crate) struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
    order: WireOrder,
    limits: DecodeLimits,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(buf: &'a [u8], order: WireOrder, limits: DecodeLimits) -> Self {
        Self {
            buf,
            pos: 0,
            order,
            limits,
        }
    }

    pub(crate) fn position(&self) -> usize {
        self.pos
    }

    fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        if self.remaining() < n {
            return Err(DecodeError::Incomplete);
        }
        let bytes = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(bytes)
    }

    pub(crate) fn u32(&mut self) -> Result<u32, DecodeError> {
        let bytes: [u8; 4] = self
            .take(4)?
            .try_into()
            .map_err(|_| DecodeError::Incomplete)?;
        Ok(match self.order {
            WireOrder::Big => u32::from_be_bytes(bytes),
            WireOrder::Little => u32::from_le_bytes(bytes),
        })
    }

    fn i32(&mut self) -> Result<i32, DecodeError> {
        Ok(self.u32()? as i32)
    }

    fn length(&mut self, what: &'static str, limit: usize) -> Result<usize, DecodeError> {
        let offset = self.pos;
        let value = self.i32()?;
        if value < 0 {
            return Err(DecodeError::NegativeLength {
                what,
                value,
                offset,
            });
        }
        let size = value as usize;
        if size > limit {
            return Err(DecodeError::LimitExceeded { what, size, limit });
        }
        Ok(size)
    }

    fn zz_body(&mut self) -> Result<BigInt, DecodeError> {
        let count = self.i32()?;
        let sign = match count {
            0 => return Ok(BigInt::zero()),
            c if c < 0 => Sign::Minus,
            _ => Sign::Plus,
        };
        let words = count.unsigned_abs() as usize;
        if words > self.limits.max_zz_words {
            return Err(DecodeError::LimitExceeded {
                what: "CMO_ZZ word count",
                size: words,
                limit: self.limits.max_zz_words,
            });
        }
        // Check availability before allocating so that a forged count cannot force
        // a large allocation.
        if self.remaining() < words.saturating_mul(4) {
            return Err(DecodeError::Incomplete);
        }
        let mut digits = Vec::with_capacity(words);
        for _ in 0..words {
            digits.push(self.u32()?);
        }
        Ok(BigInt::from_biguint(sign, BigUint::new(digits)))
    }

    fn bounded_capacity(&self, len: usize) -> usize {
        // Every element occupies at least four bytes on the wire.
        len.min(self.remaining() / 4)
    }

    pub(crate) fn cmo(&mut self, depth: usize) -> Result<Cmo, DecodeError> {
        if depth > self.limits.max_depth {
            return Err(DecodeError::TooDeep {
                limit: self.limits.max_depth,
            });
        }
        let offset = self.pos;
        let tag = self.u32()?;
        let cmo = match tag {
            cmo_tag::NULL => Cmo::Null,
            cmo_tag::ZERO => Cmo::Zero,
            cmo_tag::INT32 => Cmo::Int32(self.i32()?),
            cmo_tag::DATUM => {
                let len = self.length("CMO_DATUM", self.limits.max_string_bytes)?;
                Cmo::Datum(self.take(len)?.to_vec())
            }
            cmo_tag::STRING => {
                let len = self.length("CMO_STRING", self.limits.max_string_bytes)?;
                let start = self.pos;
                let bytes = self.take(len)?;
                let s = std::str::from_utf8(bytes)
                    .map_err(|_| DecodeError::InvalidUtf8 { offset: start })?;
                Cmo::String(s.to_owned())
            }
            cmo_tag::MATHCAP => Cmo::Mathcap(Box::new(self.cmo(depth + 1)?)),
            cmo_tag::ERROR2 => Cmo::Error2(Box::new(self.cmo(depth + 1)?)),
            cmo_tag::INDETERMINATE => Cmo::Indeterminate(Box::new(self.cmo(depth + 1)?)),
            cmo_tag::LIST => {
                let len = self.length("CMO_LIST", self.limits.max_list_len)?;
                let mut items = Vec::with_capacity(self.bounded_capacity(len));
                for _ in 0..len {
                    items.push(self.cmo(depth + 1)?);
                }
                Cmo::List(items)
            }
            cmo_tag::ZZ => Cmo::Zz(self.zz_body()?),
            cmo_tag::QQ => {
                let num = self.zz_body()?;
                let den = self.zz_body()?;
                if den.is_zero() {
                    return Err(DecodeError::Invalid {
                        what: "CMO_QQ",
                        detail: format!("zero denominator at byte offset {offset}"),
                    });
                }
                Cmo::Qq { num, den }
            }
            cmo_tag::RECURSIVE_POLYNOMIAL => {
                let variables = match self.cmo(depth + 1)? {
                    Cmo::List(items) => items,
                    other => {
                        return Err(DecodeError::Invalid {
                            what: "CMO_RECURSIVE_POLYNOMIAL",
                            detail: format!(
                                "variable list has CMO tag {} instead of CMO_LIST",
                                other.tag()
                            ),
                        });
                    }
                };
                let body = self.cmo(depth + 1)?;
                check_variable_indices(&body, variables.len())?;
                Cmo::RecursivePolynomial {
                    variables,
                    body: Box::new(body),
                }
            }
            cmo_tag::UNIVARIATE_POLYNOMIAL => {
                let len = self.length("CMO_UNIVARIATE_POLYNOMIAL", self.limits.max_list_len)?;
                let variable = self.length("variable index", u32::MAX as usize)? as u32;
                let mut terms = Vec::with_capacity(self.bounded_capacity(len));
                for _ in 0..len {
                    let exponent = self.length("exponent", u32::MAX as usize)? as u32;
                    terms.push((exponent, self.cmo(depth + 1)?));
                }
                Cmo::UnivariatePolynomial { variable, terms }
            }
            other => return Err(DecodeError::UnsupportedCmo { tag: other, offset }),
        };
        Ok(cmo)
    }
}

/// Every univariate node directly inside a recursive polynomial must refer to an
/// existing variable. Nested recursive polynomials carry their own variable lists
/// and were checked when they were decoded.
fn check_variable_indices(body: &Cmo, nvars: usize) -> Result<(), DecodeError> {
    let mut stack = vec![body];
    while let Some(node) = stack.pop() {
        if let Cmo::UnivariatePolynomial { variable, terms } = node {
            if *variable as usize >= nvars {
                return Err(DecodeError::Invalid {
                    what: "CMO_RECURSIVE_POLYNOMIAL",
                    detail: format!("variable index {variable} but only {nvars} variables"),
                });
            }
            stack.extend(terms.iter().map(|(_, c)| c));
        }
    }
    Ok(())
}

/// Decodes one CMO from the start of `buf`, returning it with the number of bytes used.
///
/// Returns [`DecodeError::Incomplete`] when `buf` ends in the middle of the object.
pub fn decode_cmo(
    buf: &[u8],
    order: WireOrder,
    limits: DecodeLimits,
) -> Result<(Cmo, usize), DecodeError> {
    let mut reader = Reader::new(buf, order, limits);
    let cmo = reader.cmo(0)?;
    Ok((cmo, reader.position()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc(cmo: &Cmo) -> Vec<u8> {
        let mut out = Vec::new();
        encode_cmo(cmo, WireOrder::Big, &mut out).unwrap();
        out
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn roundtrip(cmo: Cmo) {
        for order in [WireOrder::Big, WireOrder::Little] {
            let mut out = Vec::new();
            encode_cmo(&cmo, order, &mut out).unwrap();
            let (back, used) = decode_cmo(&out, order, DecodeLimits::default()).unwrap();
            assert_eq!(back, cmo);
            assert_eq!(used, out.len());
        }
    }

    // Golden encodings in network byte order, derived from OpenXM-RFC 100 and
    // write_cmo() in asir2018/io/cio.c.

    #[test]
    fn golden_null_and_int32() {
        assert_eq!(hex(&enc(&Cmo::Null)), "00000001");
        assert_eq!(hex(&enc(&Cmo::Int32(1234))), "00000002000004d2");
        assert_eq!(hex(&enc(&Cmo::Int32(-1))), "00000002ffffffff");
    }

    #[test]
    fn golden_string() {
        assert_eq!(
            hex(&enc(&Cmo::string("Hello"))),
            "000000040000000548656c6c6f"
        );
        assert_eq!(hex(&enc(&Cmo::string(""))), "0000000400000000");
    }

    #[test]
    fn golden_list() {
        let list = Cmo::List(vec![Cmo::Int32(1), Cmo::Null]);
        assert_eq!(
            hex(&enc(&list)),
            "00000011000000020000000200000001\
             00000001"
        );
    }

    #[test]
    fn golden_zz() {
        assert_eq!(hex(&enc(&Cmo::Zz(BigInt::from(0)))), "0000001400000000");
        assert_eq!(
            hex(&enc(&Cmo::Zz(BigInt::from(-1)))),
            "00000014ffffffff00000001"
        );
        // 2^32 + 1: two words, least significant word first.
        assert_eq!(
            hex(&enc(&Cmo::Zz(BigInt::from(4_294_967_297_u64)))),
            "000000140000000200000001\
             00000001"
        );
        assert_eq!(
            hex(&enc(&Cmo::Zz(BigInt::from(-4_294_967_296_i64)))),
            "00000014fffffffe0000000000000001"
        );
    }

    #[test]
    fn golden_qq() {
        let half = Cmo::Qq {
            num: BigInt::from(1),
            den: BigInt::from(2),
        };
        assert_eq!(
            hex(&enc(&half)),
            "00000015000000010000000100000001\
             00000002"
        );
    }

    #[test]
    fn golden_recursive_polynomial_x_minus_y() {
        // x - y with variables [x, y]: univariate in x with terms
        // (1, 1) and (0, univariate in y with term (1, -1)).
        let poly = Cmo::RecursivePolynomial {
            variables: vec![
                Cmo::Indeterminate(Box::new(Cmo::string("x"))),
                Cmo::Indeterminate(Box::new(Cmo::string("y"))),
            ],
            body: Box::new(Cmo::UnivariatePolynomial {
                variable: 0,
                terms: vec![
                    (1, Cmo::Zz(BigInt::from(1))),
                    (
                        0,
                        Cmo::UnivariatePolynomial {
                            variable: 1,
                            terms: vec![(1, Cmo::Zz(BigInt::from(-1)))],
                        },
                    ),
                ],
            }),
        };
        let expected = concat!(
            "0000001b",         // CMO_RECURSIVE_POLYNOMIAL
            "0000001100000002", // CMO_LIST, 2
            "0000003c",
            "00000004",
            "00000001",
            "78", // CMO_INDETERMINATE "x"
            "0000003c",
            "00000004",
            "00000001",
            "79", // CMO_INDETERMINATE "y"
            "00000021",
            "00000002",
            "00000000", // univariate, 2 terms, var 0
            "00000001",
            "00000014",
            "00000001",
            "00000001", // x^1 * 1
            "00000000", // x^0 * ...
            "00000021",
            "00000001",
            "00000001", // univariate, 1 term, var 1
            "00000001",
            "00000014",
            "ffffffff",
            "00000001", // y^1 * -1
        );
        assert_eq!(hex(&enc(&poly)), expected);
        roundtrip(poly);
    }

    #[test]
    fn roundtrips() {
        roundtrip(Cmo::Null);
        roundtrip(Cmo::Zero);
        roundtrip(Cmo::Int32(i32::MIN));
        roundtrip(Cmo::Datum(vec![0, 1, 2, 255]));
        roundtrip(Cmo::string("日本語"));
        roundtrip(Cmo::Zz(
            BigInt::parse_bytes(b"-123456789012345678901234567890", 10).unwrap(),
        ));
        roundtrip(Cmo::Error2(Box::new(Cmo::List(vec![
            Cmo::Int32(3),
            Cmo::string("error"),
        ]))));
        roundtrip(Cmo::Mathcap(Box::new(Cmo::List(vec![]))));
    }

    #[test]
    fn incomplete_input_is_reported() {
        let bytes = enc(&Cmo::string("Hello"));
        for cut in 0..bytes.len() {
            assert_eq!(
                decode_cmo(&bytes[..cut], WireOrder::Big, DecodeLimits::default()),
                Err(DecodeError::Incomplete),
                "cut at {cut}"
            );
        }
    }

    #[test]
    fn unsupported_tag_is_not_skipped() {
        let mut bytes = Vec::new();
        put_u32(&mut bytes, WireOrder::Big, cmo_tag::IEEE_DOUBLE_FLOAT);
        bytes.extend_from_slice(&[0; 8]);
        assert_eq!(
            decode_cmo(&bytes, WireOrder::Big, DecodeLimits::default()),
            Err(DecodeError::UnsupportedCmo {
                tag: cmo_tag::IEEE_DOUBLE_FLOAT,
                offset: 0
            })
        );
    }

    #[test]
    fn limits_are_enforced() {
        let limits = DecodeLimits {
            max_depth: 3,
            max_list_len: 2,
            max_string_bytes: 4,
            max_zz_words: 1,
            ..DecodeLimits::default()
        };
        let deep = Cmo::List(vec![Cmo::List(vec![Cmo::List(vec![Cmo::List(vec![
            Cmo::List(vec![]),
        ])])])]);
        assert_eq!(
            decode_cmo(&enc(&deep), WireOrder::Big, limits),
            Err(DecodeError::TooDeep { limit: 3 })
        );
        assert!(matches!(
            decode_cmo(&enc(&Cmo::List(vec![Cmo::Null; 3])), WireOrder::Big, limits),
            Err(DecodeError::LimitExceeded { .. })
        ));
        assert!(matches!(
            decode_cmo(&enc(&Cmo::string("Hello")), WireOrder::Big, limits),
            Err(DecodeError::LimitExceeded { .. })
        ));
        assert!(matches!(
            decode_cmo(
                &enc(&Cmo::Zz(BigInt::from(1_u64 << 40))),
                WireOrder::Big,
                limits
            ),
            Err(DecodeError::LimitExceeded { .. })
        ));
    }

    #[test]
    fn forged_lengths_do_not_allocate() {
        // A list that claims 2^24 elements but carries none must stop at Incomplete.
        let mut bytes = Vec::new();
        put_u32(&mut bytes, WireOrder::Big, cmo_tag::LIST);
        put_u32(&mut bytes, WireOrder::Big, 1 << 24);
        assert_eq!(
            decode_cmo(&bytes, WireOrder::Big, DecodeLimits::default()),
            Err(DecodeError::Incomplete)
        );
        let mut bytes = Vec::new();
        put_u32(&mut bytes, WireOrder::Big, cmo_tag::LIST);
        put_u32(&mut bytes, WireOrder::Big, (-5_i32) as u32);
        assert!(matches!(
            decode_cmo(&bytes, WireOrder::Big, DecodeLimits::default()),
            Err(DecodeError::NegativeLength { .. })
        ));
    }

    #[test]
    fn bad_variable_index_is_rejected() {
        let poly = Cmo::RecursivePolynomial {
            variables: vec![Cmo::Indeterminate(Box::new(Cmo::string("x")))],
            body: Box::new(Cmo::UnivariatePolynomial {
                variable: 1,
                terms: vec![(1, Cmo::Zz(BigInt::from(1)))],
            }),
        };
        assert!(matches!(
            decode_cmo(&enc(&poly), WireOrder::Big, DecodeLimits::default()),
            Err(DecodeError::Invalid { .. })
        ));
    }

    #[test]
    fn zero_denominator_is_rejected() {
        let mut out = Vec::new();
        assert!(
            encode_cmo(
                &Cmo::Qq {
                    num: BigInt::from(1),
                    den: BigInt::from(0)
                },
                WireOrder::Big,
                &mut out
            )
            .is_err()
        );
    }
}

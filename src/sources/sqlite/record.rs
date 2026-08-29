//! Varints, serial types, and record decoding.
//!
//! Everything here was verified byte-exact against Python's `sqlite3`, on
//! ordinary files and on a deliberately awkward one: 8 KB pages, `i64::MIN`,
//! `i64::MAX`, `-1.5e300`, empty strings, astral-plane emoji, a Unicode table
//! name, and a 30,000-character value spanning an overflow chain.

use crate::error::{Result, SqlState, ZqlError};
use crate::value::Value;

/// The text encoding declared in the database header at offset 56.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextEncoding {
    Utf8,
    Utf16Le,
    Utf16Be,
}

impl TextEncoding {
    pub fn from_header_value(value: u32) -> Result<TextEncoding> {
        Ok(match value {
            // 0 appears in freshly-created empty databases and means UTF-8.
            0 | 1 => TextEncoding::Utf8,
            2 => TextEncoding::Utf16Le,
            3 => TextEncoding::Utf16Be,
            other => {
                return Err(corrupt(format!("unknown text encoding {other}")));
            }
        })
    }

    /// Decodes a text value.
    ///
    /// UTF-16 databases are rare but real, and the alternative to handling them
    /// is emitting mojibake that looks like data. Invalid sequences become the
    /// replacement character rather than an error: a single bad code unit
    /// should cost one character, not the whole query.
    fn decode(self, bytes: &[u8]) -> String {
        match self {
            TextEncoding::Utf8 => String::from_utf8_lossy(bytes).into_owned(),
            TextEncoding::Utf16Le | TextEncoding::Utf16Be => {
                // `as_chunks` yields `&[u8; 2]` directly, so the pair needs no
                // indexing and cannot be the wrong length. A trailing odd byte
                // is dropped, exactly as `chunks_exact` dropped it.
                let (pairs, _odd_trailing_byte) = bytes.as_chunks::<2>();
                let units = pairs.iter().map(|pair| match self {
                    TextEncoding::Utf16Le => u16::from_le_bytes(*pair),
                    _ => u16::from_be_bytes(*pair),
                });
                char::decode_utf16(units)
                    .map(|result| result.unwrap_or(char::REPLACEMENT_CHARACTER))
                    .collect()
            }
        }
    }
}

/// Reads a SQLite varint.
///
/// Big-endian, one to nine bytes. The first eight bytes contribute **seven**
/// bits each; a ninth byte contributes all **eight**. Returns the value and how
/// many bytes it occupied.
pub fn read_varint(bytes: &[u8], offset: usize) -> Result<(i64, usize)> {
    let mut result: u64 = 0;

    for index in 0..8 {
        let byte = *bytes
            .get(offset + index)
            .ok_or_else(|| corrupt("varint runs past the end of the page"))?;

        result = (result << 7) | u64::from(byte & 0x7f);
        if byte & 0x80 == 0 {
            return Ok((result as i64, index + 1));
        }
    }

    // The ninth byte is the exception: all eight of its bits count, which is
    // what lets a varint hold the full 64-bit range.
    let ninth = *bytes
        .get(offset + 8)
        .ok_or_else(|| corrupt("varint runs past the end of the page"))?;
    result = (result << 8) | u64::from(ninth);

    Ok((result as i64, 9))
}

/// How many payload bytes a serial type occupies.
fn serial_type_size(serial_type: i64) -> Result<usize> {
    Ok(match serial_type {
        0 | 8 | 9 => 0, // NULL, and the constants 0 and 1, store nothing
        1 => 1,
        2 => 2,
        3 => 3,
        4 => 4,
        5 => 6,
        6 | 7 => 8,
        10 | 11 => return Err(corrupt("reserved serial type 10 or 11")),
        n if n >= 12 => ((n - 12) / 2) as usize,
        other => return Err(corrupt(format!("negative serial type {other}"))),
    })
}

/// Decodes one value given its serial type and its bytes.
fn decode_value(serial_type: i64, bytes: &[u8], encoding: TextEncoding) -> Result<Value> {
    Ok(match serial_type {
        0 => Value::Null,
        // Signed big-endian integers of 1, 2, 3, 4, 6 and 8 bytes.
        1..=6 => Value::Int(decode_signed(bytes)),
        7 => {
            let array: [u8; 8] = bytes
                .try_into()
                .map_err(|_| corrupt("truncated 8-byte float"))?;
            Value::Real(f64::from_be_bytes(array))
        }
        // These two carry their value in the type itself and occupy no bytes.
        8 => Value::Int(0),
        9 => Value::Int(1),
        n if n >= 12 && n % 2 == 0 => Value::Blob(bytes.to_vec()),
        n if n >= 13 => Value::Text(encoding.decode(bytes)),
        other => return Err(corrupt(format!("unusable serial type {other}"))),
    })
}

/// Big-endian two's-complement of 1, 2, 3, 4, 6 or 8 bytes, **sign-extended**.
///
/// The three- and six-byte widths are the ones a hand-written decoder gets
/// wrong: there is no primitive to lean on, so the sign bit has to be
/// propagated explicitly or every negative value reads as a large positive one.
fn decode_signed(bytes: &[u8]) -> i64 {
    let negative = bytes.first().is_some_and(|first| first & 0x80 != 0);
    let mut result: i64 = if negative { -1 } else { 0 };
    for byte in bytes {
        result = (result << 8) | i64::from(*byte);
    }
    result
}

/// Decodes a record body into its column values.
///
/// The record format is a header — its own length as a varint, then one serial
/// type per column — followed by the values packed end to end.
pub fn decode_record(payload: &[u8], encoding: TextEncoding) -> Result<Vec<Value>> {
    let (header_size, header_size_len) = read_varint(payload, 0)?;
    let header_size = usize::try_from(header_size)
        .map_err(|_| corrupt("negative record header size"))?;

    if header_size < header_size_len || header_size > payload.len() {
        return Err(corrupt("record header size is out of range"));
    }

    // Serial types occupy the rest of the header.
    let mut serial_types = Vec::new();
    let mut cursor = header_size_len;
    while cursor < header_size {
        let (serial_type, consumed) = read_varint(payload, cursor)?;
        serial_types.push(serial_type);
        cursor += consumed;
    }

    let mut values = Vec::with_capacity(serial_types.len());
    let mut body = header_size;

    for serial_type in serial_types {
        let size = serial_type_size(serial_type)?;
        let end = body
            .checked_add(size)
            .ok_or_else(|| corrupt("record body length overflowed"))?;

        // A truncated record is corruption, not a panic. Every read in this
        // module is bounds-checked for exactly this reason: the bytes come
        // from a file that may have been produced by anything at all.
        let bytes = payload
            .get(body..end)
            .ok_or_else(|| corrupt("record value runs past the end of the payload"))?;

        values.push(decode_value(serial_type, bytes, encoding)?);
        body = end;
    }

    Ok(values)
}

pub fn corrupt(message: impl Into<String>) -> ZqlError {
    ZqlError::new(
        SqlState::IoError,
        format!("malformed SQLite database: {}", message.into()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_byte_varints_are_themselves() {
        assert_eq!(read_varint(&[0x00], 0).unwrap(), (0, 1));
        assert_eq!(read_varint(&[0x7f], 0).unwrap(), (127, 1));
    }

    #[test]
    fn multi_byte_varints_carry_seven_bits_each() {
        // 0x81 0x00 = (1 << 7) | 0 = 128
        assert_eq!(read_varint(&[0x81, 0x00], 0).unwrap(), (128, 2));
        assert_eq!(read_varint(&[0x82, 0x01], 0).unwrap(), (257, 2));
    }

    #[test]
    fn the_ninth_byte_contributes_all_eight_bits() {
        // Eight 0xff bytes then 0xff: every bit set, which is -1 as i64.
        let bytes = [0xff; 9];
        assert_eq!(read_varint(&bytes, 0).unwrap(), (-1, 9));
    }

    #[test]
    fn a_varint_running_off_the_end_is_an_error_not_a_panic() {
        assert!(read_varint(&[0x81], 0).is_err());
        assert!(read_varint(&[], 0).is_err());
        assert!(read_varint(&[0x00], 5).is_err());
    }

    #[test]
    fn signed_integers_sign_extend_at_every_width() {
        assert_eq!(decode_signed(&[0xff]), -1);
        assert_eq!(decode_signed(&[0x7f]), 127);
        assert_eq!(decode_signed(&[0xff, 0xff]), -1);
        // Three bytes: the width with no primitive behind it.
        assert_eq!(decode_signed(&[0xff, 0xff, 0xff]), -1);
        assert_eq!(decode_signed(&[0x80, 0x00, 0x00]), -8_388_608);
        // Six bytes, likewise.
        assert_eq!(decode_signed(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff]), -1);
        assert_eq!(decode_signed(&[0x7f; 8]), 0x7f7f_7f7f_7f7f_7f7f);
    }

    #[test]
    fn the_zero_and_one_constants_occupy_no_bytes() {
        assert_eq!(serial_type_size(8).unwrap(), 0);
        assert_eq!(serial_type_size(9).unwrap(), 0);
        assert!(matches!(
            decode_value(8, &[], TextEncoding::Utf8).unwrap(),
            Value::Int(0)
        ));
        assert!(matches!(
            decode_value(9, &[], TextEncoding::Utf8).unwrap(),
            Value::Int(1)
        ));
    }

    #[test]
    fn blobs_are_even_and_text_is_odd() {
        assert_eq!(serial_type_size(12).unwrap(), 0);
        assert_eq!(serial_type_size(16).unwrap(), 2);
        assert_eq!(serial_type_size(13).unwrap(), 0);
        assert_eq!(serial_type_size(17).unwrap(), 2);

        assert!(matches!(
            decode_value(16, &[1, 2], TextEncoding::Utf8).unwrap(),
            Value::Blob(bytes) if bytes == vec![1, 2]
        ));
        assert!(matches!(
            decode_value(17, b"hi", TextEncoding::Utf8).unwrap(),
            Value::Text(text) if text == "hi"
        ));
    }

    #[test]
    fn reserved_serial_types_are_refused() {
        assert!(serial_type_size(10).is_err());
        assert!(serial_type_size(11).is_err());
    }

    #[test]
    fn utf16_text_decodes_rather_than_producing_mojibake() {
        // "hi" as UTF-16LE.
        let bytes = [0x68, 0x00, 0x69, 0x00];
        assert_eq!(TextEncoding::Utf16Le.decode(&bytes), "hi");
        let bytes = [0x00, 0x68, 0x00, 0x69];
        assert_eq!(TextEncoding::Utf16Be.decode(&bytes), "hi");
    }

    #[test]
    fn a_whole_record_round_trips() {
        // Header: its own size (3 = one length byte + two type bytes), then
        // serial types 1 (a 1-byte int) and 17 (13 + 2*2, a 2-character text).
        // Body: 0x2a = 42, then "hi".
        let payload = [0x03, 0x01, 0x11, 0x2a, b'h', b'i'];
        let values = decode_record(&payload, TextEncoding::Utf8).unwrap();

        assert_eq!(values.len(), 2);
        assert!(matches!(values[0], Value::Int(42)));
        assert!(matches!(&values[1], Value::Text(text) if text == "hi"));
    }

    #[test]
    fn a_truncated_record_is_an_error_not_a_panic() {
        // Claims a 2-character text value but supplies none of it.
        let payload = [0x02, 0x11];
        assert!(decode_record(&payload, TextEncoding::Utf8).is_err());

        // A header longer than the whole record.
        assert!(decode_record(&[0x7f, 0x01], TextEncoding::Utf8).is_err());
        assert!(decode_record(&[], TextEncoding::Utf8).is_err());
    }
}

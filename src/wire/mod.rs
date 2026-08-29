//! PostgreSQL v3 message framing.
//!
//! This module knows nothing about SQL. It moves bytes and typed messages,
//! which keeps the protocol independently testable against a recorded trace.
//!
//! **The framing rule, stated once because it is encoded once:** every message
//! except the startup packet is a single tag byte, then an `Int32` length, then
//! the body. *The length includes its own four bytes and excludes the tag.*
//! Getting that off by one is the classic first bug in a hand-written
//! implementation, so no caller ever computes it — [`Message::write_to`] does.

pub mod frontend;
pub mod oid;

use std::io::{self, Read, Write};

/// Refuse to allocate for an absurd length field.
///
/// A client — or something pretending to be one — can claim a body of
/// `i32::MAX` bytes. Trusting that is a two-gigabyte allocation on a four-byte
/// input. Real queries are kilobytes; 64 MiB is generous by three orders of
/// magnitude.
pub const MAX_MESSAGE_LEN: i32 = 64 * 1024 * 1024;

/// A backend message under construction.
///
/// Bodies are built with the typed writers below and framed on the way out, so
/// the length arithmetic exists in exactly one place in the program.
pub struct Message {
    tag: u8,
    body: Vec<u8>,
}

impl Message {
    pub fn new(tag: u8) -> Self {
        Message {
            tag,
            body: Vec::new(),
        }
    }

    pub fn byte(&mut self, value: u8) -> &mut Self {
        self.body.push(value);
        self
    }

    pub fn i16(&mut self, value: i16) -> &mut Self {
        self.body.extend_from_slice(&value.to_be_bytes());
        self
    }

    pub fn i32(&mut self, value: i32) -> &mut Self {
        self.body.extend_from_slice(&value.to_be_bytes());
        self
    }

    pub fn bytes(&mut self, value: &[u8]) -> &mut Self {
        self.body.extend_from_slice(value);
        self
    }

    /// Appends a NUL-terminated string.
    ///
    /// Interior NUL bytes are dropped rather than rejected. Postgres text
    /// cannot contain them, but a SQLite blob column read as text might, and a
    /// stray NUL would silently truncate the field for the client — a data bug
    /// that looks like a rendering bug.
    pub fn cstr(&mut self, value: &str) -> &mut Self {
        if value.as_bytes().contains(&0) {
            self.body
                .extend(value.bytes().filter(|byte| *byte != 0));
        } else {
            self.body.extend_from_slice(value.as_bytes());
        }
        self.body.push(0);
        self
    }

    /// Frames and writes the message: tag, then length-including-itself, then
    /// body.
    pub fn write_to(&self, out: &mut impl Write) -> io::Result<()> {
        let len = i32::try_from(self.body.len() + 4).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "backend message exceeds 2 GiB")
        })?;
        out.write_all(&[self.tag])?;
        out.write_all(&len.to_be_bytes())?;
        out.write_all(&self.body)
    }
}

/// Reads exactly one byte, mapping a clean EOF to `None`.
pub fn read_u8(input: &mut impl Read) -> io::Result<Option<u8>> {
    let mut buf = [0u8; 1];
    match input.read_exact(&mut buf) {
        Ok(()) => Ok(Some(buf[0])),
        Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => Ok(None),
        Err(err) => Err(err),
    }
}

/// Reads a big-endian `Int32`.
pub fn read_i32(input: &mut impl Read) -> io::Result<i32> {
    let mut buf = [0u8; 4];
    input.read_exact(&mut buf)?;
    Ok(i32::from_be_bytes(buf))
}

/// Reads a message body of `len` bytes, where `len` is the wire length field
/// (so the four length bytes have already been consumed).
pub fn read_body(input: &mut impl Read, len: i32) -> io::Result<Vec<u8>> {
    if !(4..=MAX_MESSAGE_LEN).contains(&len) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("implausible message length {len}"),
        ));
    }
    let mut body = vec![0u8; (len - 4) as usize];
    input.read_exact(&mut body)?;
    Ok(body)
}

/// Splits a NUL-terminated string off the front of a body slice, returning it
/// and the remainder.
///
/// An unterminated string is an error rather than a best-effort read: the
/// remaining fields would be misaligned anyway.
pub fn take_cstr(body: &[u8]) -> io::Result<(String, &[u8])> {
    let end = body
        .iter()
        .position(|byte| *byte == 0)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "unterminated string"))?;
    let text = String::from_utf8_lossy(&body[..end]).into_owned();
    Ok((text, &body[end + 1..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn length_counts_itself_but_not_the_tag() {
        let mut message = Message::new(b'X');
        message.bytes(b"abc");
        let mut out = Vec::new();
        message.write_to(&mut out).unwrap();
        // tag + 4 length bytes + 3 body bytes
        assert_eq!(out, vec![b'X', 0, 0, 0, 7, b'a', b'b', b'c']);
    }

    #[test]
    fn an_empty_body_still_carries_length_four() {
        let mut out = Vec::new();
        Message::new(b'Z').write_to(&mut out).unwrap();
        assert_eq!(out, vec![b'Z', 0, 0, 0, 4]);
    }

    #[test]
    fn interior_nuls_are_dropped_not_truncating() {
        let mut message = Message::new(b'E');
        message.cstr("a\0b");
        let mut out = Vec::new();
        message.write_to(&mut out).unwrap();
        assert_eq!(&out[5..], b"ab\0");
    }

    #[test]
    fn absurd_lengths_are_refused_before_allocating() {
        let mut empty: &[u8] = &[];
        assert!(read_body(&mut empty, i32::MAX).is_err());
        assert!(read_body(&mut empty, 3).is_err());
    }

    #[test]
    fn cstr_round_trips_through_take_cstr() {
        let (text, rest) = take_cstr(b"user\0zql\0").unwrap();
        assert_eq!(text, "user");
        assert_eq!(rest, b"zql\0");
        assert!(take_cstr(b"no terminator").is_err());
    }
}

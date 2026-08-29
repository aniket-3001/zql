//! The startup exchange — everything before the first query.
//!
//! This is the part of the protocol that punishes guessing, and it is built
//! first for that reason.
//!
//! The first packet a client sends has **no tag byte**: just an `Int32` length
//! and an `Int32` code. And a client may send several of these before the real
//! `StartupMessage`, so this is a loop, not a sequence.
//!
//! | Code | Meaning | Correct reply |
//! |---|---|---|
//! | `80877103` | SSLRequest | a **single bare byte `N`** — not a tagged message |
//! | `80877104` | GSSENCRequest | the same single `N` |
//! | `80877102` | CancelRequest | set the flag for that PID, reply with nothing |
//! | `196608` | StartupMessage v3.0 | read the parameter pairs and proceed |
//!
//! **The SSLRequest reply is one raw byte.** Answer it with a framed message,
//! or not at all, and there is no error anywhere — the connection simply hangs.
//! Confirmed experimentally: real `psql` leads with an SSLRequest every time,
//! and `sslmode=require` produces "server does not support SSL", which is proof
//! the byte was read.

use std::collections::HashMap;
use std::io::{self, Read, Write};

use crate::wire::{read_body, read_i32, take_cstr};

const SSL_REQUEST: i32 = 80_877_103;
const GSSENC_REQUEST: i32 = 80_877_104;
const CANCEL_REQUEST: i32 = 80_877_102;
const PROTOCOL_V3: i32 = 196_608; // 3 << 16

/// A startup packet's length field is bounded in the protocol at 10000 bytes.
const MAX_STARTUP_LEN: i32 = 10_000;

/// How a connection turned out to be intended.
#[derive(Debug)]
pub enum Startup {
    /// An ordinary session, carrying the client's startup parameters.
    Connect(Parameters),
    /// A second connection opened purely to cancel a query on the first.
    Cancel { pid: i32, secret: i32 },
}

/// The key/value pairs from a `StartupMessage`.
#[derive(Debug, Default)]
pub struct Parameters(HashMap<String, String>);

impl Parameters {
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(String::as_str)
    }

    /// The user the client connected as. zql does not authenticate, but it
    /// echoes the name back in `session_authorization` and the log.
    pub fn user(&self) -> &str {
        self.get("user").unwrap_or("unknown")
    }

    pub fn database(&self) -> &str {
        self.get("database").unwrap_or_else(|| self.user())
    }

    pub fn application_name(&self) -> &str {
        self.get("application_name").unwrap_or("")
    }
}

/// Runs the startup exchange to the point where the connection's purpose is
/// known.
///
/// Loops because SSL and GSSAPI negotiation can each precede the real startup
/// packet.
pub fn negotiate<S: Read + Write>(stream: &mut S) -> io::Result<Startup> {
    loop {
        let len = read_i32(stream)?;
        if !(8..=MAX_STARTUP_LEN).contains(&len) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("implausible startup packet length {len}"),
            ));
        }
        let code = read_i32(stream)?;

        match code {
            SSL_REQUEST | GSSENC_REQUEST => {
                // A single, unframed byte. 'N' means "not supported, continue
                // in the clear". Getting this wrong looks like a hang, not an
                // error, which is why it is the first thing zql was built to do.
                stream.write_all(b"N")?;
                stream.flush()?;
            }

            CANCEL_REQUEST => {
                // Body is exactly the PID and the secret. The protocol
                // specifies no reply at all, and the client does not wait for
                // one — it has already gone back to waiting on its first
                // connection.
                if len != 16 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "malformed CancelRequest",
                    ));
                }
                let pid = read_i32(stream)?;
                let secret = read_i32(stream)?;
                return Ok(Startup::Cancel { pid, secret });
            }

            PROTOCOL_V3 => {
                // `len` counts itself and the version word, both already read.
                let body = read_startup_body(stream, len)?;
                return Ok(Startup::Connect(parse_parameters(&body)?));
            }

            other => {
                let (major, minor) = (other >> 16, other & 0xffff);
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unsupported protocol version {major}.{minor}"),
                ));
            }
        }
    }
}

/// Reads the remaining `len - 8` bytes of a startup packet.
fn read_startup_body(stream: &mut impl Read, len: i32) -> io::Result<Vec<u8>> {
    // `read_body` expects a wire length that counts its own four bytes; the
    // version word has already been consumed, so hand it `len - 4`.
    read_body(stream, len - 4)
}

/// Parses the NUL-separated key/value run that ends with an empty key.
fn parse_parameters(body: &[u8]) -> io::Result<Parameters> {
    let mut parameters = HashMap::new();
    let mut rest = body;

    loop {
        let (key, remainder) = take_cstr(rest)?;
        if key.is_empty() {
            break;
        }
        let (value, remainder) = take_cstr(remainder)?;
        parameters.insert(key, value);
        rest = remainder;

        if rest.is_empty() {
            // A terminator-less parameter list. Tolerated: every field that
            // arrived is intact, and refusing the connection over a missing
            // trailing zero would be pedantry a client cannot act on.
            break;
        }
    }

    Ok(Parameters(parameters))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// A two-way stream over fixed input, collecting whatever is written.
    struct Pipe {
        input: Cursor<Vec<u8>>,
        output: Vec<u8>,
    }

    impl Pipe {
        fn new(input: Vec<u8>) -> Self {
            Pipe {
                input: Cursor::new(input),
                output: Vec::new(),
            }
        }
    }

    impl Read for Pipe {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.input.read(buf)
        }
    }

    impl Write for Pipe {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.output.write(buf)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn packet(code: i32, body: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&((body.len() + 8) as i32).to_be_bytes());
        bytes.extend_from_slice(&code.to_be_bytes());
        bytes.extend_from_slice(body);
        bytes
    }

    fn startup_body() -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(b"user\0aniket\0database\0zql\0\0");
        body
    }

    #[test]
    fn ssl_request_is_answered_with_one_bare_byte_then_startup_continues() {
        let mut input = packet(SSL_REQUEST, &[]);
        input.extend(packet(PROTOCOL_V3, &startup_body()));
        let mut pipe = Pipe::new(input);

        let startup = negotiate(&mut pipe).unwrap();

        // Exactly one byte, and it is not a framed message.
        assert_eq!(pipe.output, b"N");
        match startup {
            Startup::Connect(parameters) => {
                assert_eq!(parameters.user(), "aniket");
                assert_eq!(parameters.database(), "zql");
            }
            other => panic!("expected a connection, got {other:?}"),
        }
    }

    #[test]
    fn gssenc_request_gets_the_same_treatment() {
        let mut input = packet(GSSENC_REQUEST, &[]);
        input.extend(packet(PROTOCOL_V3, &startup_body()));
        let mut pipe = Pipe::new(input);
        negotiate(&mut pipe).unwrap();
        assert_eq!(pipe.output, b"N");
    }

    #[test]
    fn several_negotiation_packets_may_precede_the_real_one() {
        let mut input = packet(SSL_REQUEST, &[]);
        input.extend(packet(GSSENC_REQUEST, &[]));
        input.extend(packet(PROTOCOL_V3, &startup_body()));
        let mut pipe = Pipe::new(input);
        assert!(matches!(negotiate(&mut pipe).unwrap(), Startup::Connect(_)));
        assert_eq!(pipe.output, b"NN");
    }

    #[test]
    fn cancel_request_carries_the_pid_and_secret_and_gets_no_reply() {
        let mut body = Vec::new();
        body.extend_from_slice(&4004i32.to_be_bytes());
        body.extend_from_slice(&1_302_818_513i32.to_be_bytes());
        let mut pipe = Pipe::new(packet(CANCEL_REQUEST, &body));

        match negotiate(&mut pipe).unwrap() {
            Startup::Cancel { pid, secret } => {
                assert_eq!(pid, 4004);
                assert_eq!(secret, 1_302_818_513);
            }
            other => panic!("expected a cancel, got {other:?}"),
        }
        assert!(pipe.output.is_empty(), "the protocol specifies no reply");
    }

    #[test]
    fn database_defaults_to_the_user_name_as_libpq_does() {
        let mut pipe = Pipe::new(packet(PROTOCOL_V3, b"user\0aniket\0\0"));
        match negotiate(&mut pipe).unwrap() {
            Startup::Connect(parameters) => assert_eq!(parameters.database(), "aniket"),
            other => panic!("expected a connection, got {other:?}"),
        }
    }

    #[test]
    fn an_old_protocol_version_is_refused_by_name() {
        let mut pipe = Pipe::new(packet(2 << 16, b""));
        let err = negotiate(&mut pipe).unwrap_err();
        assert!(err.to_string().contains("2.0"));
    }

    #[test]
    fn an_absurd_length_is_refused_before_allocating() {
        let mut bytes = i32::MAX.to_be_bytes().to_vec();
        bytes.extend_from_slice(&PROTOCOL_V3.to_be_bytes());
        assert!(negotiate(&mut Pipe::new(bytes)).is_err());
    }
}

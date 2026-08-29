//! Messages zql receives from the client, after the startup exchange.
//!
//! Only the simple query protocol is implemented. `Parse`/`Bind`/`Describe`/
//! `Execute`/`Sync` — the extended protocol that GUI clients use — is refused
//! by name rather than ignored, so a client that tries it gets a real
//! `0A000` error instead of a hang. That refusal path was verified: node-postgres
//! parsed it cleanly as `severity: 'ERROR', code: '0A000'`.

use std::io::{self, Read};

use crate::wire::{read_body, read_i32, read_u8, take_cstr};

/// A message from the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frontend {
    /// `'Q'` — a simple query.
    Query(String),
    /// `'X'` — the client is leaving.
    Terminate,
    /// `'S'` — the end of an extended-protocol batch. Answered with readiness
    /// and nothing else.
    Sync,
    /// A message zql understands the framing of but not the meaning.
    /// Carried rather than dropped so the session can name it in the error.
    Unsupported { tag: u8 },
}

/// The human name for a tag, used in the `0A000` message.
fn tag_name(tag: u8) -> &'static str {
    match tag {
        b'P' => "the extended query protocol (Parse)",
        b'B' => "the extended query protocol (Bind)",
        b'D' => "the extended query protocol (Describe)",
        b'E' => "the extended query protocol (Execute)",
        b'C' => "the extended query protocol (Close)",
        b'H' => "the extended query protocol (Flush)",
        b'F' => "the fastpath function-call protocol",
        b'd' | b'c' | b'f' => "COPY",
        _ => "this message type",
    }
}

impl Frontend {
    /// Whether this message is part of an extended-protocol batch.
    ///
    /// Decides whether the rest of the batch has to be discarded after a
    /// refusal: a lone unrecognised message has no batch behind it, and
    /// waiting for a `Sync` that is never coming would hang the session.
    pub fn is_extended_protocol(&self) -> bool {
        matches!(
            self,
            Frontend::Unsupported {
                tag: b'P' | b'B' | b'D' | b'E' | b'H' | b'C' | b'F'
            }
        )
    }

    /// A description suitable for an error message.
    pub fn describe(&self) -> String {
        match self {
            Frontend::Query(_) => "a simple query".to_string(),
            Frontend::Terminate => "Terminate".to_string(),
            Frontend::Sync => "Sync".to_string(),
            Frontend::Unsupported { tag } => tag_name(*tag).to_string(),
        }
    }
}

/// Reads one message.
///
/// `Ok(None)` means the client closed the connection cleanly — which is a
/// normal way for a session to end, not an error. `psql` usually sends
/// `Terminate` first, but a killed client simply vanishes and both have to be
/// handled the same way.
pub fn read(input: &mut impl Read) -> io::Result<Option<Frontend>> {
    let Some(tag) = read_u8(input)? else {
        return Ok(None);
    };
    let len = read_i32(input)?;
    let body = read_body(input, len)?;

    match tag {
        b'Q' => {
            let (sql, _) = take_cstr(&body)?;
            Ok(Some(Frontend::Query(sql)))
        }
        b'X' => Ok(Some(Frontend::Terminate)),
        b'S' => Ok(Some(Frontend::Sync)),
        other => Ok(Some(Frontend::Unsupported { tag: other })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query_message(sql: &str) -> Vec<u8> {
        let mut bytes = vec![b'Q'];
        let len = (sql.len() + 5) as i32;
        bytes.extend_from_slice(&len.to_be_bytes());
        bytes.extend_from_slice(sql.as_bytes());
        bytes.push(0);
        bytes
    }

    #[test]
    fn reads_a_simple_query() {
        let bytes = query_message("SELECT 1");
        let message = read(&mut bytes.as_slice()).unwrap().unwrap();
        assert_eq!(message, Frontend::Query("SELECT 1".to_string()));
    }

    #[test]
    fn a_closed_connection_is_not_an_error() {
        let empty: &[u8] = &[];
        assert_eq!(read(&mut { empty }).unwrap(), None);
    }

    #[test]
    fn terminate_is_recognised() {
        let bytes = vec![b'X', 0, 0, 0, 4];
        assert_eq!(
            read(&mut bytes.as_slice()).unwrap().unwrap(),
            Frontend::Terminate
        );
    }

    #[test]
    fn extended_protocol_messages_are_named_in_the_refusal() {
        let bytes = vec![b'P', 0, 0, 0, 5, 0];
        let message = read(&mut bytes.as_slice()).unwrap().unwrap();
        assert_eq!(message, Frontend::Unsupported { tag: b'P' });
        assert!(message.describe().contains("Parse"));
    }

    #[test]
    fn a_truncated_message_errors_rather_than_hanging() {
        let bytes = vec![b'Q', 0, 0, 0, 20, b'a'];
        assert!(read(&mut bytes.as_slice()).is_err());
    }
}

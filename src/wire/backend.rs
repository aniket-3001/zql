//! Messages zql sends to the client.
//!
//! Every one is built through a constructor here rather than assembled at the
//! call site, so a tag byte or a field order appears exactly once in the
//! program.

use crate::error::ZqlError;
use crate::plan::schema::Schema;
use crate::value::{Row, Value};
use crate::wire::{oid, Message};

/// `AuthenticationOk` — `'R'` with an `Int32` 0.
///
/// zql accepts any user with no password. That is Postgres `trust` auth, it
/// involves no cryptography, and the README says so plainly: the server is
/// read-only, binds loopback by default, and holds nothing to protect.
pub fn authentication_ok() -> Message {
    let mut message = Message::new(b'R');
    message.i32(0);
    message
}

/// `ParameterStatus` — `'S'`, a key and a value.
pub fn parameter_status(name: &str, value: &str) -> Message {
    let mut message = Message::new(b'S');
    message.cstr(name).cstr(value);
    message
}

/// The parameter set clients expect at startup.
///
/// These are not decoration. libpq caches several of them and `psql` will
/// misrender results without `client_encoding` and `DateStyle`; omitting
/// `standard_conforming_strings` changes how a client escapes literals it sends
/// back. The list was settled by connecting real clients, not by reading.
pub fn startup_parameters(server_version: &str) -> Vec<Message> {
    [
        ("server_version", server_version),
        ("server_encoding", "UTF8"),
        ("client_encoding", "UTF8"),
        ("application_name", "zql"),
        ("is_superuser", "off"),
        ("session_authorization", "zql"),
        ("DateStyle", "ISO, MDY"),
        ("IntervalStyle", "postgres"),
        // No time-zone database in the standard library, so everything zql
        // reports is UTC and it says so here too.
        ("TimeZone", "UTC"),
        ("integer_datetimes", "on"),
        ("standard_conforming_strings", "on"),
    ]
    .iter()
    .map(|(name, value)| parameter_status(name, value))
    .collect()
}

/// `BackendKeyData` — `'K'`, the PID and secret a client must quote back in a
/// `CancelRequest`.
pub fn backend_key_data(pid: i32, secret: i32) -> Message {
    let mut message = Message::new(b'K');
    message.i32(pid).i32(secret);
    message
}

/// Transaction status reported by `ReadyForQuery`.
#[derive(Debug, Clone, Copy)]
pub enum TransactionStatus {
    /// Idle, outside a transaction block. zql is always this: it is read-only
    /// and has no transactions to be inside.
    Idle,
}

/// `ReadyForQuery` — `'Z'`. The client will not send another query until it
/// sees one of these, so every path out of the query loop owes the client one.
pub fn ready_for_query(status: TransactionStatus) -> Message {
    let mut message = Message::new(b'Z');
    match status {
        TransactionStatus::Idle => message.byte(b'I'),
    };
    message
}

/// `RowDescription` — `'T'`. Must precede the first `DataRow`.
pub fn row_description(schema: &Schema) -> Message {
    let mut message = Message::new(b'T');
    message.i16(schema.len().min(i16::MAX as usize) as i16);
    for column in &schema.columns {
        let oid = oid::oid_for(column.ty);
        message
            .cstr(&column.name)
            .i32(0) // table OID: zql has no catalogue tables to point at
            .i16(0) // column attribute number, likewise
            .i32(oid)
            .i16(oid::type_size(column.ty))
            .i32(-1) // type modifier: none
            .i16(0); // format code: 0 = text, for every column
    }
    message
}

/// `DataRow` — `'D'`. A NULL is a length of -1 with no bytes, which is what
/// distinguishes it from the empty string.
pub fn data_row(row: &Row) -> Message {
    let mut message = Message::new(b'D');
    message.i16(row.len().min(i16::MAX as usize) as i16);
    for value in &row.0 {
        match oid::render(value) {
            None => {
                message.i32(-1);
            }
            Some(bytes) => {
                message.i32(bytes.len() as i32).bytes(&bytes);
            }
        }
    }
    message
}

/// `CommandComplete` — `'C'`. `psql` reads the row count out of this string,
/// so `SELECT 3` is not a label, it is data.
pub fn command_complete(tag: &str) -> Message {
    let mut message = Message::new(b'C');
    message.cstr(tag);
    message
}

/// `EmptyQueryResponse` — `'I'`, the correct reply to a query that is only
/// whitespace or a comment. `psql` sends one every time a user types a bare
/// semicolon.
pub fn empty_query_response() -> Message {
    Message::new(b'I')
}

/// `ErrorResponse` — `'E'`.
///
/// The body is a field *map*, not a string: a key byte, a NUL-terminated
/// value, repeated, then a zero byte to close. Clients that receive a
/// malformed one report nothing useful, which makes this the hardest message
/// to debug and the one most worth getting right first.
pub fn error_response(error: &ZqlError) -> Message {
    let mut message = Message::new(b'E');
    message
        .byte(b'S')
        .cstr("ERROR")
        // 'V' is the non-localized severity. Clients prefer it when present.
        .byte(b'V')
        .cstr("ERROR")
        .byte(b'C')
        .cstr(error.state.code())
        .byte(b'M')
        .cstr(&error.message);

    if let Some(detail) = &error.detail {
        message.byte(b'D').cstr(detail);
    }
    if let Some(hint) = &error.hint {
        message.byte(b'H').cstr(hint);
    }
    if let Some(position) = error.position {
        // `psql` turns this into the `LINE 1: ... ^` caret block itself.
        message.byte(b'P').cstr(&position.to_string());
    }

    message.byte(0); // terminator
    message
}

/// A `CommandComplete` tag for a `SELECT` that returned `rows` rows.
pub fn select_tag(rows: u64) -> String {
    format!("SELECT {rows}")
}

/// Convenience for the common "one text column" result, used by `SHOW`.
pub fn single_text_row(text: impl Into<String>) -> Row {
    Row::new(vec![Value::Text(text.into())])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::SqlState;
    use crate::plan::schema::Column;
    use crate::value::Type;

    fn encode(message: &Message) -> Vec<u8> {
        let mut out = Vec::new();
        message.write_to(&mut out).unwrap();
        out
    }

    #[test]
    fn ready_for_query_is_five_bytes() {
        assert_eq!(
            encode(&ready_for_query(TransactionStatus::Idle)),
            vec![b'Z', 0, 0, 0, 5, b'I']
        );
    }

    #[test]
    fn a_null_field_is_negative_one_not_empty() {
        let bytes = encode(&data_row(&Row::new(vec![
            Value::Null,
            Value::Text(String::new()),
        ])));
        // tag(1) len(4) count(2), then the two fields
        assert_eq!(&bytes[7..11], &(-1i32).to_be_bytes());
        assert_eq!(&bytes[11..15], &0i32.to_be_bytes());
        assert_eq!(bytes.len(), 15);
    }

    #[test]
    fn error_response_is_a_terminated_field_map() {
        let error = ZqlError::new(SqlState::SyntaxError, "boom").at(7);
        let bytes = encode(&error_response(&error));
        assert_eq!(bytes[0], b'E');
        assert_eq!(*bytes.last().unwrap(), 0, "field map must be terminated");
        let body = String::from_utf8_lossy(&bytes[5..]);
        assert!(body.contains("42601"));
        assert!(body.contains("boom"));
        assert!(body.contains('7'));
    }

    #[test]
    fn row_description_advertises_text_format_for_every_column() {
        let schema = Schema::new(vec![
            Column::new("name", Type::Text),
            Column::new("size", Type::Int),
        ]);
        let bytes = encode(&row_description(&schema));
        assert_eq!(bytes[0], b'T');
        assert_eq!(i16::from_be_bytes([bytes[5], bytes[6]]), 2);
        // Each column ends with a two-byte format code of 0 (text).
        assert_eq!(*bytes.last().unwrap(), 0);
    }

    #[test]
    fn the_startup_parameter_set_is_the_verified_eleven() {
        assert_eq!(startup_parameters("16.2").len(), 11);
    }
}

//! Type OIDs, and the rendering of a [`Value`] into wire text.
//!
//! These two things live in the same file on purpose: the OID zql advertises in
//! `RowDescription` and the bytes it then writes in `DataRow` have to agree, and
//! keeping them apart is how they drift.
//!
//! **Text format only.** The v3 protocol allows every value to be sent as text,
//! and the binary format is optional. That was confirmed working against both
//! `psql` and node-postgres, and it removes an entire encoding layer.

use crate::datetime;
use crate::value::{Type, Value};

pub const BOOL: i32 = 16;
pub const BYTEA: i32 = 17;
pub const INT8: i32 = 20;
pub const INT4: i32 = 23;
pub const TEXT: i32 = 25;
pub const FLOAT8: i32 = 701;
pub const TIMESTAMP: i32 = 1114;

/// The OID advertised for a column of this type.
pub fn oid_for(ty: Type) -> i32 {
    match ty {
        Type::Bool => BOOL,
        Type::Int => INT8,
        Type::Real => FLOAT8,
        Type::Text => TEXT,
        Type::Blob => BYTEA,
        Type::Timestamp => TIMESTAMP,
        // A column that only ever held NULLs has no type of its own. Postgres
        // resolves an unknown literal to text, and so does zql.
        Type::Unknown => TEXT,
    }
}

/// The width the client should expect: -1 for variable-length types.
pub fn type_size(ty: Type) -> i16 {
    match ty {
        Type::Bool => 1,
        Type::Int | Type::Real | Type::Timestamp => 8,
        Type::Text | Type::Blob | Type::Unknown => -1,
    }
}

/// Renders a value for a `DataRow` field.
///
/// `None` is SQL `NULL`, which the protocol encodes as a length of -1 and no
/// bytes — distinct from a zero-length string, which is a length of 0.
pub fn render(value: &Value) -> Option<Vec<u8>> {
    match value {
        Value::Null => None,
        // Postgres renders booleans as `t` and `f`, not `true`/`false` or 1/0.
        Value::Bool(b) => Some(if *b { b"t".to_vec() } else { b"f".to_vec() }),
        Value::Int(i) => Some(i.to_string().into_bytes()),
        Value::Real(r) => Some(render_real(*r).into_bytes()),
        Value::Text(s) => Some(s.clone().into_bytes()),
        // Postgres `bytea` text output, with `standard_conforming_strings` on.
        Value::Blob(bytes) => Some(render_blob(bytes).into_bytes()),
        Value::Timestamp(seconds) => Some(datetime::format_timestamp(*seconds).into_bytes()),
    }
}

/// Renders an `f64` the way Postgres does.
///
/// Two differences from Rust's `Display`, both of which show up on real data:
///
/// 1. Postgres spells the specials in words — `NaN`, `Infinity` — where Rust
///    writes `NaN` and `inf`.
/// 2. **Rust never uses exponent notation.** `-1.5e300` formats as a minus sign
///    and three hundred digits, which is not wrong so much as unusable: one
///    such value stretches a `psql` column across three screens. Postgres
///    switches to exponent form for very large and very small magnitudes, and
///    writes the sign of the exponent, which Rust's `{:e}` omits.
fn render_real(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_string();
    }
    if value.is_infinite() {
        return if value.is_sign_negative() {
            "-Infinity".to_string()
        } else {
            "Infinity".to_string()
        };
    }

    let magnitude = value.abs();
    let needs_exponent = magnitude != 0.0 && !(1e-4..1e15).contains(&magnitude);
    if !needs_exponent {
        return value.to_string();
    }

    // Rust writes `1.5e300` and `1.5e-300`; Postgres writes `1.5e+300`.
    let formatted = format!("{value:e}");
    match formatted.split_once('e') {
        Some((mantissa, exponent)) if !exponent.starts_with('-') => {
            format!("{mantissa}e+{exponent}")
        }
        _ => formatted,
    }
}

fn render_blob(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(2 + bytes.len() * 2);
    out.push_str("\\x");
    for byte in bytes {
        // Two lowercase hex digits per byte, which is what libpq emits.
        out.push(char::from_digit((byte >> 4) as u32, 16).unwrap_or('0'));
        out.push(char::from_digit((byte & 0x0f) as u32, 16).unwrap_or('0'));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_is_absent_not_empty() {
        assert!(render(&Value::Null).is_none());
        assert_eq!(render(&Value::Text(String::new())), Some(Vec::new()));
    }

    #[test]
    fn booleans_use_postgres_spelling() {
        assert_eq!(render(&Value::Bool(true)).unwrap(), b"t");
        assert_eq!(render(&Value::Bool(false)).unwrap(), b"f");
    }

    #[test]
    fn float_specials_match_postgres() {
        assert_eq!(render_real(f64::NAN), "NaN");
        assert_eq!(render_real(f64::INFINITY), "Infinity");
        assert_eq!(render_real(f64::NEG_INFINITY), "-Infinity");
        assert_eq!(render_real(1.5), "1.5");
    }

    #[test]
    fn extreme_magnitudes_use_exponent_notation_with_a_signed_exponent() {
        // Without this, -1.5e300 renders as three hundred digits.
        assert_eq!(render_real(-1.5e300), "-1.5e+300");
        assert_eq!(render_real(1e-300), "1e-300");
        assert_eq!(render_real(0.0), "0");
        // Ordinary values stay in plain notation, as Postgres writes them.
        assert_eq!(render_real(375.0), "375");
        assert_eq!(render_real(0.001), "0.001");
        assert_eq!(render_real(std::f64::consts::PI), "3.141592653589793");
    }

    #[test]
    fn blobs_use_the_hex_format() {
        assert_eq!(render_blob(&[0x00, 0xde, 0xad, 0xff]), "\\x00deadff");
        assert_eq!(render_blob(&[]), "\\x");
    }

    #[test]
    fn timestamps_render_as_utc_text() {
        assert_eq!(
            render(&Value::Timestamp(0)).unwrap(),
            b"1970-01-01 00:00:00"
        );
    }
}

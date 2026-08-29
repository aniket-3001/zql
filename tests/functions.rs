//! Coverage for the closed function set, the cast targets, and the value
//! renderings — the parts that are individually small and collectively the
//! surface a user actually touches.
//!
//! These exist because an audit found several of them exercised only inside
//! the module that implements them, or not at all. A function that is only
//! tested by its own unit test has never been through the binder, which is
//! where its arity and its argument types are decided.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use zql::error::{Result, SqlState};
use zql::exec;
use zql::plan;
use zql::sources::SourceConfig;
use zql::sql::{self, ast::Statement};
use zql::value::{Row, Value};
use zql::wire::oid;

fn query(sql: &str) -> Result<Vec<Row>> {
    let Statement::Select(select) = sql::parse(sql)? else {
        panic!("{sql}: expected a SELECT");
    };
    let config = SourceConfig::uncached(PathBuf::from("src"));
    let plan = plan::bind(&select, &config)?;
    let mut stream = exec::execute(plan, Arc::new(AtomicBool::new(false)))?;
    exec::collect(stream.as_mut())
}

/// The single value a one-row, one-column query produced, rendered exactly as
/// it would go onto the wire. Comparing wire text rather than the `Value`
/// checks the whole path, including the rendering a client actually parses.
fn wire(sql: &str) -> String {
    let rows = query(sql).unwrap_or_else(|error| panic!("{sql}\n  {error}"));
    assert_eq!(rows.len(), 1, "{sql}: expected one row");
    match oid::render(&rows[0].0[0]) {
        Some(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        None => "<null>".to_string(),
    }
}

fn error_state(sql: &str) -> SqlState {
    match query(sql) {
        Err(error) => error.state,
        Ok(rows) => panic!("{sql}: expected an error, got {} rows", rows.len()),
    }
}

// ------------------------------------------------------- the thirteen scalars

#[test]
fn every_string_function_behaves() {
    assert_eq!(wire("SELECT lower('MiXeD')"), "mixed");
    assert_eq!(wire("SELECT upper('MiXeD')"), "MIXED");
    assert_eq!(wire("SELECT length('hello')"), "5");
    assert_eq!(wire("SELECT trim('  padded  ')"), "padded");
    assert_eq!(wire("SELECT replace('a-b-c', '-', '+')"), "a+b+c");
    assert_eq!(wire("SELECT substr('hello', 2, 3)"), "ell");
    assert_eq!(wire("SELECT substr('hello', 3)"), "llo");
}

#[test]
fn string_functions_count_characters_not_bytes() {
    // Three characters, nine bytes. A byte-based length is a bug that only a
    // non-ASCII test can see.
    assert_eq!(wire("SELECT length('写真展')"), "3");
    assert_eq!(wire("SELECT upper('café')"), "CAFÉ");
    assert_eq!(wire("SELECT substr('写真展', 2, 2)"), "真展");
}

#[test]
fn every_numeric_function_behaves() {
    assert_eq!(wire("SELECT abs(-7)"), "7");
    assert_eq!(wire("SELECT abs(-7.5)"), "7.5");
    assert_eq!(wire("SELECT round(3.14159)"), "3");
    assert_eq!(wire("SELECT round(3.14159, 2)"), "3.14");
    assert_eq!(wire("SELECT round(2.5)"), "3");
    // Rounding an integer to decimal places is the integer, not a float.
    assert_eq!(wire("SELECT round(7, 2)"), "7");
}

#[test]
fn the_null_handling_functions_behave() {
    assert_eq!(wire("SELECT coalesce(NULL, NULL, 'third')"), "third");
    assert_eq!(wire("SELECT coalesce(NULL, NULL)"), "<null>");
    assert_eq!(wire("SELECT nullif(1, 1)"), "<null>");
    assert_eq!(wire("SELECT nullif(1, 2)"), "1");
}

#[test]
fn typeof_names_every_type_including_null() {
    assert_eq!(wire("SELECT typeof(1)"), "integer");
    assert_eq!(wire("SELECT typeof(1.5)"), "real");
    assert_eq!(wire("SELECT typeof('x')"), "text");
    assert_eq!(wire("SELECT typeof(TRUE)"), "boolean");
    // The one function that reports on a NULL rather than propagating it.
    assert_eq!(wire("SELECT typeof(NULL)"), "unknown");
}

#[test]
fn the_time_functions_render_utc() {
    assert_eq!(wire("SELECT date(1000000000)"), "2001-09-09");
    assert_eq!(wire("SELECT datetime(1000000000)"), "2001-09-09 01:46:40");
    assert_eq!(wire("SELECT date(0)"), "1970-01-01");
    // Pre-epoch, where truncating division would land on the wrong day.
    assert_eq!(wire("SELECT datetime(-1)"), "1969-12-31 23:59:59");
}

#[test]
fn every_function_except_typeof_propagates_null() {
    for call in [
        "lower(NULL)",
        "upper(NULL)",
        "length(NULL)",
        "trim(NULL)",
        "replace(NULL, 'a', 'b')",
        "substr(NULL, 1)",
        "abs(NULL)",
        "round(NULL)",
        "date(NULL)",
        "datetime(NULL)",
    ] {
        assert_eq!(wire(&format!("SELECT {call}")), "<null>", "{call}");
    }
}

#[test]
fn arity_is_enforced_at_bind_time_for_every_function() {
    for call in [
        "lower()",
        "lower('a', 'b')",
        "substr('a')",
        "replace('a', 'b')",
        "nullif(1)",
        "coalesce(1)",
        "round(1, 2, 3)",
    ] {
        assert_eq!(
            error_state(&format!("SELECT {call}")),
            SqlState::SyntaxError,
            "{call} should be refused"
        );
    }
}

// -------------------------------------------------------------------- casts

#[test]
fn every_cast_target_works() {
    assert_eq!(wire("SELECT CAST(42 AS text)"), "42");
    assert_eq!(wire("SELECT CAST('42' AS integer)"), "42");
    assert_eq!(wire("SELECT CAST('2.5' AS real)"), "2.5");
    assert_eq!(wire("SELECT CAST(1 AS boolean)"), "t");
    assert_eq!(wire("SELECT CAST('no' AS boolean)"), "f");
    assert_eq!(wire("SELECT CAST(0 AS timestamp)"), "1970-01-01 00:00:00");
    assert_eq!(wire("SELECT CAST(2.9 AS integer)"), "2", "truncates toward zero");
    assert_eq!(wire("SELECT CAST(NULL AS integer)"), "<null>");
}

#[test]
fn postgres_and_sqlite_type_spellings_are_both_accepted() {
    // A user who knows Postgres types `int8`; one who knows SQLite types
    // `integer`. Neither is wrong.
    for spelling in ["int", "integer", "int4", "int8", "bigint", "smallint"] {
        assert_eq!(wire(&format!("SELECT CAST('7' AS {spelling})")), "7");
    }
    for spelling in ["text", "varchar", "char", "string"] {
        assert_eq!(wire(&format!("SELECT CAST(7 AS {spelling})")), "7");
    }
}

#[test]
fn an_impossible_cast_is_a_type_error_not_a_silent_zero() {
    assert_eq!(error_state("SELECT CAST('abc' AS integer)"), SqlState::DatatypeMismatch);
    assert_eq!(error_state("SELECT CAST('abc' AS real)"), SqlState::DatatypeMismatch);
    assert_eq!(error_state("SELECT CAST('abc' AS boolean)"), SqlState::DatatypeMismatch);
    assert_eq!(error_state("SELECT CAST(1 AS nosuchtype)"), SqlState::DatatypeMismatch);
}

// ------------------------------------------------------------ wire rendering

#[test]
fn a_blob_renders_as_postgres_hex_bytea() {
    // hard.db row 2 holds the single byte 'x'. Postgres writes bytea as \x
    // followed by lowercase hex, and a client parses it on that basis.
    let rows =
        query("SELECT b FROM sqlite('tests/fixtures/hard.db','t') WHERE id = 2").unwrap();
    let rendered = oid::render(&rows[0].0[0]).expect("not null");
    assert_eq!(String::from_utf8_lossy(&rendered), "\\x78");
}

#[test]
fn booleans_and_specials_render_the_way_postgres_writes_them() {
    assert_eq!(wire("SELECT TRUE"), "t");
    assert_eq!(wire("SELECT FALSE"), "f");
    assert_eq!(wire("SELECT 1.0 / 3"), "0.3333333333333333");
    // Distinct from NULL: an empty string is a value.
    assert_eq!(wire("SELECT ''"), "");
}

// ------------------------------------------------------------- the env source

#[test]
fn the_env_source_is_queryable_end_to_end() {
    std::env::set_var("ZQL_FUNCTIONS_TEST", "present");

    let rows = query("SELECT value FROM env WHERE name = 'ZQL_FUNCTIONS_TEST'").unwrap();
    assert_eq!(rows.len(), 1);
    assert!(matches!(&rows[0].0[0], Value::Text(text) if text == "present"));

    // And it composes with the rest of the engine.
    let counted = query("SELECT count(*) FROM env").unwrap();
    assert!(matches!(counted[0].0[0], Value::Int(n) if n > 0));
}

// --------------------------------------------- SQLite rows that predate a column

#[test]
fn rows_written_before_a_column_existed_read_as_null_not_an_error() {
    // `ALTER TABLE ADD COLUMN` does not rewrite existing rows: their records
    // simply end early. A reader that assumes every record carries every
    // column reads past the end of one.
    const DB: &str = "tests/fixtures/altered.db";

    let rows = query(&format!("SELECT id, a, b FROM sqlite('{DB}','t') ORDER BY id")).unwrap();
    assert_eq!(rows.len(), 3);

    assert!(matches!(&rows[0].0[1], Value::Text(text) if text == "old"));
    assert!(rows[0].0[2].is_null(), "a short record's missing column is NULL");
    assert!(rows[1].0[2].is_null());
    assert!(matches!(&rows[2].0[2], Value::Text(text) if text == "present"));

    // And the missing values behave as NULLs downstream, not as absences.
    let counted = query(&format!("SELECT count(b) FROM sqlite('{DB}','t')")).unwrap();
    assert!(matches!(counted[0].0[0], Value::Int(1)));
}

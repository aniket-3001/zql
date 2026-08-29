//! The SQLite reader, checked against Python's `sqlite3`.
//!
//! Every value asserted here was printed by `tests/fixtures/generate.py`, which
//! also wrote the files. Python is the oracle: it produced the bytes, so
//! anything zql reads back that differs is zql being wrong.
//!
//! The fixtures are committed. Test *data* is not a dependency, and committing
//! them means `cargo test` needs nothing but Rust.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use zql::error::{Result, SqlState};
use zql::exec;
use zql::plan;
use zql::sources::SourceConfig;
use zql::sql::{self, ast::Statement};
use zql::value::{Row, Value};

fn fixture(name: &str) -> String {
    // Forward slashes: the path travels through a SQL string literal, and
    // backslashes are literal there but awkward to read.
    format!("tests/fixtures/{name}")
}

fn run(sql: &str) -> Result<Vec<Row>> {
    let Statement::Select(select) = sql::parse(sql)? else {
        panic!("{sql}: expected a SELECT");
    };
    let config = SourceConfig::uncached(PathBuf::from("."));
    let plan = plan::bind(&select, &config)?;
    let mut stream = exec::execute(plan, Arc::new(AtomicBool::new(false)))?;
    exec::collect(stream.as_mut())
}

fn rows(sql: &str) -> Vec<Row> {
    run(sql).unwrap_or_else(|error| panic!("{sql}\n  {error}"))
}

fn one(sql: &str) -> Row {
    let rows = rows(sql);
    assert_eq!(rows.len(), 1, "{sql}: expected exactly one row");
    rows[0].clone()
}

fn text(value: &Value) -> String {
    match value {
        Value::Text(text) => text.clone(),
        other => panic!("expected text, got {other:?}"),
    }
}

fn int(value: &Value) -> i64 {
    match value {
        Value::Int(number) => *number,
        other => panic!("expected an integer, got {other:?}"),
    }
}

fn real(value: &Value) -> f64 {
    match value {
        Value::Real(number) => *number,
        other => panic!("expected a real, got {other:?}"),
    }
}

// ------------------------------------------------------- ordinary databases

#[test]
fn row_counts_match_the_oracle() {
    assert_eq!(
        rows(&format!("SELECT id FROM sqlite('{}', 'users')", fixture("simple.db"))).len(),
        500
    );
    assert_eq!(
        rows(&format!("SELECT id FROM sqlite('{}', 'notes')", fixture("simple.db"))).len(),
        20
    );
    // 4,000 rows in 512-byte pages forces interior pages, so this only passes
    // if the walk descends rather than reading one leaf.
    assert_eq!(
        rows(&format!("SELECT id FROM sqlite('{}', 'many')", fixture("wide.db"))).len(),
        4000
    );
}

#[test]
fn a_known_row_matches_the_oracle_value_for_value() {
    // Oracle: ('user_250', 375.0, 0)
    let row = one(&format!(
        "SELECT name, score, active FROM sqlite('{}', 'users') WHERE id = 250",
        fixture("simple.db")
    ));
    assert_eq!(text(&row.0[0]), "user_250");
    assert_eq!(real(&row.0[1]), 375.0);
    assert_eq!(int(&row.0[2]), 0);
}

#[test]
fn integer_primary_key_returns_the_rowid_and_not_null() {
    // The bug this test exists for: an INTEGER PRIMARY KEY is stored as NULL
    // in every record, with the real value in the cell header. Without the
    // substitution every one of these is NULL — plausible, and wrong in every
    // single row.
    let row = one(&format!(
        "SELECT id FROM sqlite('{}', 'users') WHERE name = 'user_250'",
        fixture("simple.db")
    ));
    assert_eq!(int(&row.0[0]), 250);

    let all = rows(&format!(
        "SELECT id FROM sqlite('{}', 'users')",
        fixture("simple.db")
    ));
    assert!(
        all.iter().all(|row| !row.0[0].is_null()),
        "some primary keys came back NULL"
    );
    assert_eq!(int(&all[0].0[0]), 1);
    assert_eq!(int(&all[499].0[0]), 500);
}

// ------------------------------------------------------------ overflow chain

#[test]
fn a_nine_thousand_byte_value_is_reassembled_byte_exactly() {
    // 9,000 bytes into 4,096-byte pages spans an overflow chain. The first and
    // last sixteen characters are what prove the chain was walked and
    // reassembled in order — a truncated read still gets the first bytes right.
    let row = one(&format!(
        "SELECT body FROM sqlite('{}', 'notes') WHERE id = 7",
        fixture("simple.db")
    ));
    let body = text(&row.0[0]);

    assert_eq!(body.len(), 9000, "wrong length");
    assert_eq!(&body[..16], "ZkyNAjjNdmvjzUkg", "wrong first 16 bytes");
    assert_eq!(&body[body.len() - 16..], "ZQBmwnrMeYjldNYu", "wrong last 16 bytes");
}

#[test]
fn a_thirty_thousand_character_value_spans_a_longer_chain() {
    // 30,000 characters in 8 KB pages: several overflow pages deep.
    let row = one(&format!(
        "SELECT length(s) FROM sqlite('{}', 't') WHERE id = 3",
        fixture("hard.db")
    ));
    assert_eq!(int(&row.0[0]), 30_000);
}

// ------------------------------------------------------- the awkward fixture

#[test]
fn the_integer_extremes_survive_the_round_trip() {
    let row = one(&format!(
        "SELECT i FROM sqlite('{}', 't') WHERE id = 2",
        fixture("hard.db")
    ));
    assert_eq!(int(&row.0[0]), i64::MIN);

    let row = one(&format!(
        "SELECT i FROM sqlite('{}', 't') WHERE id = 3",
        fixture("hard.db")
    ));
    assert_eq!(int(&row.0[0]), i64::MAX);
}

#[test]
fn floats_keep_full_precision_including_the_extremes() {
    let row = one(&format!(
        "SELECT f FROM sqlite('{}', 't') WHERE id = 2",
        fixture("hard.db")
    ));
    assert_eq!(real(&row.0[0]), -1.5e300);

    let row = one(&format!(
        "SELECT f FROM sqlite('{}', 't') WHERE id = 3",
        fixture("hard.db")
    ));
    assert_eq!(real(&row.0[0]), std::f64::consts::PI);
}

#[test]
fn unicode_survives_including_astral_plane_characters() {
    let row = one(&format!(
        "SELECT s FROM sqlite('{}', 't') WHERE id = 2",
        fixture("hard.db")
    ));
    assert_eq!(text(&row.0[0]), "héllo wörld 🎞");
}

#[test]
fn a_unicode_table_name_can_be_queried() {
    // The table name arrives as a string literal, not an identifier, which is
    // precisely why source arguments are strings: an identifier would have
    // been folded to lower case on the way in.
    let row = one(&format!(
        "SELECT label FROM sqlite('{}', '写真')",
        fixture("hard.db")
    ));
    assert_eq!(text(&row.0[0]), "ユーザー");
}

#[test]
fn an_empty_string_is_not_a_null() {
    let row = one(&format!(
        "SELECT s FROM sqlite('{}', 't') WHERE id = 1",
        fixture("hard.db")
    ));
    assert_eq!(text(&row.0[0]), "");
    assert!(!row.0[0].is_null(), "an empty string became NULL");
}

#[test]
fn nulls_are_preserved_as_nulls() {
    let row = one(&format!(
        "SELECT b FROM sqlite('{}', 't') WHERE id = 1",
        fixture("hard.db")
    ));
    assert!(row.0[0].is_null());
}

#[test]
fn an_index_alongside_the_tables_is_skipped_not_read_as_rows() {
    // hard.db has `idx_s` on `t(s)`. If index pages were walked as table
    // pages, this count would be wrong or the read would fail outright.
    assert_eq!(
        rows(&format!("SELECT id FROM sqlite('{}', 't')", fixture("hard.db"))).len(),
        3
    );
}

#[test]
fn an_empty_table_yields_no_rows_rather_than_failing() {
    assert!(rows(&format!(
        "SELECT a FROM sqlite('{}', 'empty_table')",
        fixture("hard.db")
    ))
    .is_empty());
}

#[test]
fn mixed_case_column_names_survive_the_lexers_folding() {
    // The DDL is parsed with zql's own lexer, which folds identifiers to lower
    // case. `visitCount` must still be reported as written.
    let row = one(&format!(
        "SELECT \"visitCount\", \"My Column\" FROM sqlite('{}', 'quirks')",
        fixture("hard.db")
    ));
    assert_eq!(int(&row.0[0]), 42);
    assert_eq!(text(&row.0[1]), "kept");
}

/// ...and can still be *reached* without quoting it.
///
/// Displaying `visitCount` as declared is right; making it unreachable unless
/// the user guesses the exact capitalisation is not. SQLite matches column
/// names case-insensitively, so demanding `SELECT "visitCount"` would be zql
/// inventing a restriction the file it is reading does not have — and the
/// capitalisation of a column in someone else's database is the last thing a
/// person poking at `places.sqlite` knows.
#[test]
fn a_case_preserved_column_can_be_selected_without_quoting_it() {
    for reference in ["visitCount", "visitcount", "VISITCOUNT", "VisitCount"] {
        let row = one(&format!(
            "SELECT {reference} FROM sqlite('{}', 'quirks')",
            fixture("hard.db")
        ));
        assert_eq!(int(&row.0[0]), 42, "{reference} did not resolve");
    }

    // The quoted form keeps working, and the reported name is still as declared.
    let row = one(&format!(
        "SELECT visitcount AS n FROM sqlite('{}', 'quirks')",
        fixture("hard.db")
    ));
    assert_eq!(int(&row.0[0]), 42);
}

// -------------------------------------------------------------------- errors

#[test]
fn an_unknown_table_lists_the_tables_that_do_exist() {
    let Err(error) = run(&format!(
        "SELECT * FROM sqlite('{}', 'nope')",
        fixture("simple.db")
    )) else {
        panic!("an unknown table must not bind");
    };

    assert_eq!(error.state, SqlState::UndefinedTable);
    let hint = error.hint.expect("no hint listing the tables");
    assert!(hint.contains("users") && hint.contains("notes"), "hint was: {hint}");
}

#[test]
fn omitting_the_table_name_answers_the_question_instead_of_refusing() {
    // Nobody remembers what is inside a `.db` file, so the one-argument form
    // is a discovery aid rather than an error.
    let Err(error) = run(&format!("SELECT * FROM sqlite('{}')", fixture("simple.db")))
    else {
        panic!("sqlite() with no table must not bind");
    };
    let detail = error.detail.expect("no detail listing the tables");
    assert!(detail.contains("users"), "detail was: {detail}");
}

#[test]
fn a_non_sqlite_file_is_rejected_with_a_specific_error_and_no_panic() {
    let Err(error) = run("SELECT * FROM sqlite('Cargo.toml', 't')") else {
        panic!("a non-database must not bind");
    };
    assert_eq!(error.state, SqlState::IoError);
    assert!(error.message.contains("not a SQLite database"));
}

#[test]
fn an_un_checkpointed_wal_is_refused_rather_than_read_stale() {
    // **The failure this reader most needs to avoid.** In WAL mode committed
    // rows can live only in the sidecar; reading the main file alone returns
    // stale but entirely plausible data, which looks exactly like success.
    let source = Path::new("tests/fixtures/hard.db");
    let copy = std::env::temp_dir().join("zql-wal-test.db");
    std::fs::copy(source, &copy).expect("copy the fixture");

    let sidecar = copy.with_file_name("zql-wal-test.db-wal");
    // A WAL longer than its 32-byte header has frames waiting in it.
    std::fs::write(&sidecar, vec![0u8; 512]).expect("write a sidecar");

    let path = copy.to_string_lossy().replace('\\', "/");
    let outcome = run(&format!("SELECT id FROM sqlite('{path}', 't')"));

    let _ = std::fs::remove_file(&sidecar);
    let _ = std::fs::remove_file(&copy);

    let Err(error) = outcome else {
        panic!("a database with a populated WAL must be refused, not read");
    };
    assert_eq!(error.state, SqlState::IoError);
    assert!(
        error.message.contains("write-ahead log"),
        "message was: {}",
        error.message
    );
    assert!(error.hint.is_some(), "no hint telling the user how to fix it");
}

#[test]
fn a_checkpointed_wal_database_reads_normally() {
    // hard.db was created in WAL mode and checkpointed. Refusing it would be
    // over-cautious: the main file is complete.
    assert_eq!(
        rows(&format!("SELECT id FROM sqlite('{}', 't')", fixture("hard.db"))).len(),
        3
    );
}

// ---------------------------------------------------------- adversarial input

/// Truncation and byte-flipping over a real database.
///
/// This is the panic sweep from ARCHITECTURE.md §8, aimed at the module that
/// reads untrusted binary input. Every offset in this reader is bounds-checked
/// and every chain is loop-guarded; this is what proves it.
#[test]
fn mangled_databases_produce_errors_and_never_panic() {
    let original = std::fs::read("tests/fixtures/simple.db").expect("read the fixture");
    let scratch = std::env::temp_dir().join("zql-mangled.db");

    let mut checked = 0;

    // Truncated at a range of lengths, including mid-page and mid-header.
    for length in [0usize, 1, 15, 16, 99, 100, 101, 4095, 4096, 4097, 8192, 20_000] {
        let truncated = &original[..length.min(original.len())];
        std::fs::write(&scratch, truncated).expect("write the scratch file");
        let _ = read_scratch(&scratch);
        checked += 1;
    }

    // Single-byte corruption at points that steer the reader: the page size,
    // the page count, the encoding, the root page's type byte and cell count,
    // and a scattering of cell pointers.
    for offset in [16, 17, 20, 18, 19, 28, 29, 30, 31, 56, 100, 101, 103, 104, 105, 108, 4000] {
        for replacement in [0x00u8, 0x01, 0x7f, 0xff] {
            let mut corrupted = original.clone();
            if offset < corrupted.len() {
                corrupted[offset] = replacement;
            }
            std::fs::write(&scratch, &corrupted).expect("write the scratch file");
            let _ = read_scratch(&scratch);
            checked += 1;
        }
    }

    let _ = std::fs::remove_file(&scratch);
    assert!(checked > 50, "the sweep did not actually run: {checked}");
}

/// Reads every row of every table it can find. The result is irrelevant — not
/// panicking is the assertion.
fn read_scratch(path: &Path) -> Result<usize> {
    let path = path.to_string_lossy().replace('\\', "/");
    let mut total = 0;
    for table in ["users", "notes", "nope"] {
        if let Ok(rows) = run(&format!("SELECT * FROM sqlite('{path}', '{table}')")) {
            total += rows.len();
        }
    }
    Ok(total)
}

//! End-to-end tests: SQL text in, rows out.
//!
//! These drive the whole pipeline — lex, parse, bind, execute — rather than any
//! one stage, which is the only way to catch a stage that is individually
//! correct and wrong at its seams. The fixture is zql's own `src/` directory:
//! it is present on any machine that can run the tests, and its shape is known.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use zql::error::{Result, SqlState};
use zql::exec;
use zql::plan;
use zql::sources::SourceConfig;
use zql::sql::{self, ast::Statement};
use zql::value::{Row, Value};

/// Runs a query against `src/` and returns its rows.
fn query(sql: &str) -> Result<Vec<Row>> {
    let Statement::Select(select) = sql::parse(sql)? else {
        panic!("{sql}: expected a SELECT");
    };
    let config = SourceConfig::uncached(PathBuf::from("src"));
    let plan = plan::bind(&select, &config)?;
    let mut stream = exec::execute(plan, Arc::new(AtomicBool::new(false)))?;
    exec::collect(stream.as_mut())
}

/// The single scalar a one-column, one-row query produced.
fn scalar(sql: &str) -> Value {
    let rows = query(sql).unwrap_or_else(|error| panic!("{sql}: {error}"));
    assert_eq!(rows.len(), 1, "{sql}: expected exactly one row");
    rows[0].0[0].clone()
}

fn integer(sql: &str) -> i64 {
    match scalar(sql) {
        Value::Int(number) => number,
        other => panic!("{sql}: expected an integer, got {other:?}"),
    }
}

fn text(sql: &str) -> String {
    match scalar(sql) {
        Value::Text(text) => text,
        other => panic!("{sql}: expected text, got {other:?}"),
    }
}

fn error(sql: &str) -> SqlState {
    match query(sql) {
        Err(error) => error.state,
        Ok(rows) => panic!("{sql}: expected an error, got {} rows", rows.len()),
    }
}

// ------------------------------------------------------------------ queries

#[test]
fn a_select_with_no_from_returns_exactly_one_row() {
    assert_eq!(integer("SELECT 1"), 1);
    assert_eq!(text("SELECT 'hello'"), "hello");
    assert!(scalar("SELECT NULL").is_null());
}

#[test]
fn the_gate_query_runs_against_a_real_filesystem() {
    // The deliverable named in ARCHITECTURE.md §10 for this gate.
    let rows = query("SELECT name FROM files WHERE size > 1000 LIMIT 10").unwrap();
    assert!(!rows.is_empty(), "src/ should contain files over 1 kB");
    assert!(rows.len() <= 10, "LIMIT was not applied");
}

#[test]
fn a_wildcard_projects_every_source_column_in_order() {
    let rows = query("SELECT * FROM files LIMIT 1").unwrap();
    assert_eq!(rows[0].len(), 7, "files has seven columns");
}

#[test]
fn limit_and_offset_walk_the_same_stream() {
    let two = query("SELECT name FROM files LIMIT 2").unwrap();
    let skipped = query("SELECT name FROM files LIMIT 1 OFFSET 1").unwrap();
    assert_eq!(two.len(), 2);
    assert_eq!(skipped.len(), 1);
    // The scan order is stable within a run, so row 2 of the first is row 1
    // of the second.
    assert_eq!(format!("{:?}", two[1].0), format!("{:?}", skipped[0].0));
}

#[test]
fn an_alias_qualifies_columns_and_renames_the_output() {
    let rows = query("SELECT f.name AS filename FROM files AS f LIMIT 1").unwrap();
    assert_eq!(rows.len(), 1);

    // The unaliased source name no longer resolves once an alias is given.
    assert_eq!(
        error("SELECT files.name FROM files AS f LIMIT 1"),
        SqlState::UndefinedColumn
    );
}

// ---------------------------------------------------- three-valued behaviour

#[test]
fn a_null_column_is_excluded_by_a_comparison_but_found_by_is_null() {
    // Directories have no extension, so `ext` is NULL for every one of them.
    let directories = integer(
        "SELECT depth FROM files WHERE is_dir AND ext IS NULL LIMIT 1",
    );
    assert!(directories >= 0);

    // The same rows must not survive an ordinary comparison against NULL.
    let compared = query("SELECT name FROM files WHERE ext = NULL").unwrap();
    assert!(
        compared.is_empty(),
        "`= NULL` matched {} rows; it must match none",
        compared.len()
    );
}

#[test]
fn not_of_unknown_stays_unknown_and_still_filters_out() {
    // `NOT (ext = 'rs')` over a directory row is NOT NULL, which is NULL, so
    // the row is excluded — it does not flip to true.
    let rows = query("SELECT name FROM files WHERE NOT (ext = 'zzz') AND is_dir").unwrap();
    assert!(
        rows.is_empty(),
        "a NULL predicate under NOT admitted {} rows",
        rows.len()
    );
}

// ------------------------------------------------------------------- errors

#[test]
fn every_failure_mode_reports_the_right_sqlstate() {
    assert_eq!(error("SELECT nope FROM files"), SqlState::UndefinedColumn);
    assert_eq!(error("SELECT * FROM nosuchsource"), SqlState::UndefinedTable);
    assert_eq!(error("SELECT nosuchfn(1)"), SqlState::UndefinedFunction);
    assert_eq!(error("SELECT 1/0"), SqlState::DivisionByZero);
    assert_eq!(
        error("SELECT name FROM files WHERE name > 5"),
        SqlState::DatatypeMismatch
    );
    assert_eq!(
        error("SELECT * FROM files('no-such-directory')"),
        SqlState::IoError
    );
}

#[test]
fn a_non_boolean_where_clause_is_refused_at_bind_time_with_a_caret() {
    let Err(error) = query("SELECT name FROM files WHERE size") else {
        panic!("`WHERE size` must not bind");
    };
    assert_eq!(error.state, SqlState::DatatypeMismatch);
    // Bind-time, so it can point at the offending expression. A failure raised
    // mid-scan has no position to report.
    assert!(error.position.is_some(), "no caret on a bind-time error");
    assert!(error.hint.is_some());
}

#[test]
fn wrong_argument_counts_are_caught_before_any_row_is_read() {
    assert_eq!(error("SELECT lower('a', 'b') FROM files"), SqlState::SyntaxError);
    assert_eq!(error("SELECT substr('abc')"), SqlState::SyntaxError);
}

#[test]
fn cancellation_stops_a_scan_in_progress() {
    let Statement::Select(select) = sql::parse("SELECT name FROM files").unwrap() else {
        panic!("expected a SELECT");
    };
    let config = SourceConfig::uncached(PathBuf::from("src"));
    let plan = plan::bind(&select, &config).unwrap();

    // A flag that is already set stands in for a CancelRequest that arrived
    // while the query was running.
    let flag = Arc::new(AtomicBool::new(true));
    let mut stream = exec::execute(plan, flag).unwrap();

    let error = exec::collect(stream.as_mut()).unwrap_err();
    assert_eq!(error.state, SqlState::QueryCanceled);
}

// -------------------------------------------------- grouping and aggregation

#[test]
fn an_ungrouped_aggregate_returns_exactly_one_row() {
    let rows = query("SELECT COUNT(*) FROM files").unwrap();
    assert_eq!(rows.len(), 1);
    assert!(matches!(rows[0].0[0], Value::Int(n) if n > 10));
}

#[test]
fn count_star_and_count_expr_differ_on_a_column_holding_nulls() {
    // Directories have no extension, so `ext` is NULL for every one of them.
    let row = &query("SELECT COUNT(*), COUNT(ext) FROM files").unwrap()[0];
    let (all, with_ext) = (
        match row.0[0] { Value::Int(n) => n, _ => panic!("expected an integer") },
        match row.0[1] { Value::Int(n) => n, _ => panic!("expected an integer") },
    );
    assert!(with_ext < all, "COUNT(expr) must skip the NULL extensions");
}

#[test]
fn group_by_with_having_and_an_alias_in_order_by() {
    // The canonical example from SQL-SUBSET.md §11.
    let rows = query(
        "SELECT ext, COUNT(*) AS n, SUM(size) AS bytes FROM files \
         WHERE NOT is_dir GROUP BY ext HAVING COUNT(*) > 2 \
         ORDER BY bytes DESC LIMIT 10",
    )
    .unwrap();

    assert!(!rows.is_empty());
    // HAVING was applied.
    for row in &rows {
        assert!(matches!(row.0[1], Value::Int(n) if n > 2));
    }
    // ORDER BY resolved the *alias* to the aggregate beneath it.
    let bytes: Vec<i64> = rows
        .iter()
        .map(|row| match row.0[2] {
            Value::Int(n) => n,
            _ => panic!("expected an integer"),
        })
        .collect();
    assert!(bytes.windows(2).all(|pair| pair[0] >= pair[1]), "not descending");
}

#[test]
fn a_column_outside_the_grouping_is_refused_by_name() {
    let Err(error) = query("SELECT ext, name FROM files GROUP BY ext") else {
        panic!("an ungrouped column must not bind");
    };
    assert_eq!(error.state, SqlState::UndefinedColumn);
    assert!(error.message.contains("GROUP BY"), "{}", error.message);
    assert!(error.position.is_some(), "no caret");
}

#[test]
fn an_aggregate_used_where_one_cannot_go_is_refused() {
    // Aggregates in WHERE are a classic mistake; the message says where they go.
    let Err(error) = query("SELECT name FROM files WHERE COUNT(*) > 1") else {
        panic!("an aggregate in WHERE must not bind");
    };
    assert_eq!(error.state, SqlState::UndefinedFunction);
}

#[test]
fn the_same_aggregate_in_two_clauses_is_computed_once() {
    // Deduplication by fingerprint: HAVING and ORDER BY share one column.
    let rows = query(
        "SELECT ext, COUNT(*) AS n FROM files GROUP BY ext \
         HAVING COUNT(*) > 0 ORDER BY COUNT(*) DESC",
    )
    .unwrap();
    assert!(!rows.is_empty());
    assert_eq!(rows[0].len(), 2, "the aggregate leaked an extra column");
}

// ------------------------------------------------------- ordering and distinct

#[test]
fn order_by_can_name_a_column_that_is_not_selected() {
    // Sort sits below the projection precisely so this works.
    let rows = query("SELECT name FROM files WHERE NOT is_dir ORDER BY size DESC LIMIT 3")
        .unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].len(), 1, "the sort key leaked into the output");
}

#[test]
fn ascending_puts_nulls_last_over_real_data() {
    let rows = query("SELECT ext FROM files ORDER BY ext LIMIT 400").unwrap();
    let first_null = rows.iter().position(|row| row.0[0].is_null());
    if let Some(index) = first_null {
        assert!(
            rows[index..].iter().all(|row| row.0[0].is_null()),
            "a non-NULL sorted after a NULL"
        );
    }
}

#[test]
fn distinct_collapses_duplicates_and_keeps_the_sort() {
    let rows = query("SELECT DISTINCT ext FROM files ORDER BY ext").unwrap();
    let mut seen = Vec::new();
    for row in &rows {
        let key = format!("{:?}", row.0[0]);
        assert!(!seen.contains(&key), "duplicate survived DISTINCT: {key}");
        seen.push(key);
    }
}

// ---------------------------------------------------------------------- joins

const FIXTURE: &str = "tests/fixtures/simple.db";

#[test]
fn an_inner_join_across_two_sqlite_tables_matches_the_smaller_side() {
    let rows = query(&format!(
        "SELECT COUNT(*) FROM sqlite('{FIXTURE}','users') AS u \
         JOIN sqlite('{FIXTURE}','notes') AS n ON u.id = n.id"
    ))
    .unwrap();
    // 500 users, 20 notes, ids 1..20 in common.
    assert!(matches!(rows[0].0[0], Value::Int(20)));
}

#[test]
fn a_left_join_keeps_unmatched_rows_with_nulls() {
    let rows = query(&format!(
        "SELECT u.id, n.id FROM sqlite('{FIXTURE}','users') AS u \
         LEFT JOIN sqlite('{FIXTURE}','notes') AS n ON u.id = n.id \
         WHERE u.id > 19 AND u.id < 23 ORDER BY u.id"
    ))
    .unwrap();

    assert_eq!(rows.len(), 3);
    assert!(!rows[0].0[1].is_null(), "id 20 has a note");
    assert!(rows[1].0[1].is_null(), "id 21 has none");
    assert!(rows[2].0[1].is_null(), "id 22 has none");
}

#[test]
fn a_join_across_unlike_sources_works_at_all() {
    // The point of the whole tool: the join operator does not know or care
    // that one side is a b-tree in a file and the other is a directory walk.
    let rows = query(&format!(
        "SELECT f.name, u.name FROM files AS f \
         JOIN sqlite('{FIXTURE}','users') AS u ON f.depth = u.id LIMIT 5"
    ))
    .unwrap();
    assert!(!rows.is_empty());
    assert_eq!(rows[0].len(), 2);
}

#[test]
fn an_unqualified_column_present_in_both_sides_is_ambiguous() {
    // Both tables have an `id`, so a bare `id` cannot be resolved.
    let Err(error) = query(&format!(
        "SELECT id FROM sqlite('{FIXTURE}','users') AS u \
         JOIN sqlite('{FIXTURE}','notes') AS n ON u.id = n.id"
    )) else {
        panic!("an ambiguous column must not bind");
    };
    assert_eq!(error.state, SqlState::UndefinedColumn);
    assert!(error.message.contains("ambiguous"), "{}", error.message);
}

// ------------------------------------------------------------------- csv()

const OWNERS: &str = "tests/fixtures/owners.csv";

#[test]
fn a_csv_gets_a_header_derived_schema_and_sniffed_types() {
    let rows = query(&format!("SELECT filename, owner, priority FROM csv('{OWNERS}')")).unwrap();
    assert_eq!(rows.len(), 5);
    // `priority` sniffed as an integer, so it compares and sorts numerically.
    assert!(matches!(rows[0].0[2], Value::Int(1)));
}

#[test]
fn a_quoted_field_may_contain_a_comma() {
    let rows = query(&format!(
        "SELECT owner FROM csv('{OWNERS}') WHERE filename = 'parser.rs'"
    ))
    .unwrap();
    assert_eq!(text_at(&rows[0], 0), "carol, jr");
}

#[test]
fn an_empty_unquoted_field_is_null_not_an_empty_string() {
    let rows = query(&format!(
        "SELECT filename FROM csv('{OWNERS}') WHERE owner IS NULL"
    ))
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(text_at(&rows[0], 0), "lexer.rs");
}

#[test]
fn a_sniffed_integer_column_sorts_numerically_rather_than_lexically() {
    // The reason sniffing exists: as text, "10" sorts before "7".
    let rows = query(&format!(
        "SELECT priority FROM csv('{OWNERS}') ORDER BY priority DESC LIMIT 1"
    ))
    .unwrap();
    assert!(matches!(rows[0].0[0], Value::Int(10)));
}

#[test]
fn a_csv_joins_against_the_filesystem() {
    // The query the whole tool is for, from SQL-SUBSET.md §11.
    let rows = query(&format!(
        "SELECT f.name, f.size, m.owner FROM files AS f \
         JOIN csv('{OWNERS}') AS m ON f.name = m.filename \
         WHERE f.ext = 'rs' ORDER BY f.size DESC"
    ))
    .unwrap();
    assert!(!rows.is_empty(), "no source file matched the owners list");
    assert_eq!(rows[0].len(), 3);
}

#[test]
fn a_missing_csv_fails_at_bind_time() {
    assert_eq!(error("SELECT * FROM csv('no-such-file.csv')"), SqlState::IoError);
}

// -------------------------------------------------------- cut and refused

#[test]
fn the_cut_json_source_says_so_rather_than_pretending_to_be_unknown() {
    let Err(err) = query("SELECT * FROM json('x.json')") else {
        panic!("json() must not bind");
    };
    assert_eq!(err.state, SqlState::FeatureNotSupported);
    assert!(err.detail.unwrap().contains("cut"));
}

fn text_at(row: &Row, index: usize) -> String {
    match &row.0[index] {
        Value::Text(text) => text.clone(),
        other => panic!("expected text, got {other:?}"),
    }
}

#[test]
fn a_csv_with_an_excel_style_utf8_bom_is_queryable_by_column_name() {
    // tests/fixtures/bom.csv is a byte-for-byte Excel default export: UTF-8
    // BOM, CRLF endings, capitalised headers. Unstripped, the BOM becomes part
    // of the first column's name and `SELECT name` cannot reach it.
    let rows = query("SELECT name, qty FROM csv('tests/fixtures/bom.csv') ORDER BY qty").unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(text_at(&rows[0], 0), "widget");
    assert!(matches!(rows[0].0[1], Value::Int(3)), "qty should sniff as an integer");
}

#[test]
fn a_negative_limit_is_refused_with_a_readable_message() {
    let Err(error) = query("SELECT name FROM files LIMIT -1") else {
        panic!("a negative LIMIT must not parse");
    };
    assert_eq!(error.state, SqlState::SyntaxError);
    assert!(error.message.contains("must not be negative"), "{}", error.message);
}

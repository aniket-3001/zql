//! The gate G2 conformance suite for `docs/SQL-SUBSET.md`.
//!
//! §10 of that document sets three conditions for the parser gate, and this
//! file is each of them in turn:
//!
//! 1. every canonical example in §11 parses,
//! 2. every construct in §7 returns `0A000` with the feature named,
//! 3. a sweep of truncated and mangled queries produces zero panics.
//!
//! The third is the one that catches what review does not. It is the same
//! pattern as the adversarial-file sweep that proved out the image decoder:
//! cheap to write, and it fails loudly the moment an index or a slice stops
//! being checked.

use zql::error::SqlState;
use zql::sql::ast::*;
use zql::sql::parse;

/// The canonical examples, verbatim from `SQL-SUBSET.md` §11.
const CANONICAL: &[&str] = &[
    // the cold open: a file the judge also has and cannot open
    "SELECT title, url FROM sqlite('places.sqlite', 'moz_places') ORDER BY visit_count DESC LIMIT 10",
    // what the tool is actually for: one query language over unlike things
    "SELECT f.name, f.size, m.owner \
     FROM files('src') AS f \
     JOIN csv('owners.csv') AS m ON f.name = m.filename \
     WHERE f.ext = 'rs' AND f.size > 1000 \
     ORDER BY f.size DESC",
    // aggregation over the filesystem
    "SELECT ext, COUNT(*) AS n, SUM(size) AS bytes \
     FROM files \
     WHERE NOT is_dir \
     GROUP BY ext \
     HAVING COUNT(*) > 5 \
     ORDER BY bytes DESC \
     LIMIT 10",
    // three-valued logic, on purpose
    "SELECT COUNT(*) FROM sqlite('app.db','t') WHERE note IS NULL",
    // streaming; Ctrl-C must stop this
    "SELECT line FROM tail('server.log') WHERE line LIKE '%ERROR%'",
    // discoverability
    "SHOW SOURCES",
    "EXPLAIN SELECT ext, COUNT(*) FROM files GROUP BY ext",
];

// ---------------------------------------------------------------- condition 1

#[test]
fn every_canonical_example_parses() {
    for sql in CANONICAL {
        if let Err(error) = parse(sql) {
            panic!("{sql}\n  failed to parse: {error}");
        }
    }
}

#[test]
fn a_trailing_semicolon_is_optional_everywhere() {
    for sql in CANONICAL {
        let with_semicolon = format!("{sql};");
        assert_eq!(
            parse(sql).is_ok(),
            parse(&with_semicolon).is_ok(),
            "semicolon changed the outcome for: {sql}"
        );
    }
}

#[test]
fn the_join_example_binds_its_clauses_where_expected() {
    let Statement::Select(select) = parse(CANONICAL[1]).unwrap() else {
        panic!("expected a SELECT");
    };

    let from = select.from.expect("the example has a FROM clause");
    assert_eq!(from.source.name, "files");
    assert_eq!(from.alias.as_deref(), Some("f"));

    let join = from.join.expect("the example has a JOIN");
    assert_eq!(join.kind, JoinKind::Inner);
    assert_eq!(join.source.name, "csv");
    assert_eq!(join.alias.as_deref(), Some("m"));

    // `ON f.name = m.filename` — both sides qualified.
    let ExprKind::Binary { op, left, right } = &join.on.kind else {
        panic!("expected a binary comparison in ON");
    };
    assert_eq!(*op, BinaryOp::Eq);
    assert!(matches!(
        &left.kind,
        ExprKind::Column { qualifier: Some(q), name } if q == "f" && name == "name"
    ));
    assert!(matches!(
        &right.kind,
        ExprKind::Column { qualifier: Some(q), name } if q == "m" && name == "filename"
    ));

    // `WHERE f.ext = 'rs' AND f.size > 1000` — AND binds looser than `=`.
    let filter = select.filter.expect("the example has a WHERE clause");
    assert!(matches!(
        filter.kind,
        ExprKind::Binary {
            op: BinaryOp::And,
            ..
        }
    ));
}

#[test]
fn source_arguments_keep_the_case_they_were_written_with() {
    // The whole reason source arguments are strings: a SQLite table name is
    // case-sensitive as stored, and an identifier would have been folded.
    let Statement::Select(select) = parse("SELECT x FROM sqlite('App.DB', 'Users')").unwrap()
    else {
        panic!("expected a SELECT");
    };
    let args = select.from.unwrap().source.args.unwrap();
    assert_eq!(
        args,
        vec![
            Literal::String("App.DB".to_string()),
            Literal::String("Users".to_string())
        ]
    );
}

// ---------------------------------------------------------------- condition 2

/// Every row of `SQL-SUBSET.md` §7, with the phrase the refusal must contain.
const UNSUPPORTED: &[(&str, &str)] = &[
    ("INSERT INTO t VALUES (1)", "read-only"),
    ("UPDATE t SET x = 1", "read-only"),
    ("DELETE FROM t", "read-only"),
    ("CREATE TABLE t (x INT)", "read-only"),
    ("DROP TABLE t", "read-only"),
    ("SELECT * FROM t WHERE x IN (SELECT y FROM u)", "subqueries"),
    ("SELECT (SELECT 1)", "subqueries"),
    ("WITH a AS (SELECT 1) SELECT * FROM a", "common table"),
    ("SELECT row_number() OVER () FROM t", "window functions"),
    ("SELECT 1 UNION SELECT 2", "UNION"),
    ("SELECT 1 INTERSECT SELECT 2", "INTERSECT"),
    ("SELECT 1 EXCEPT SELECT 2", "EXCEPT"),
    ("SELECT * FROM a RIGHT JOIN b ON a.x = b.x", "OUTER JOIN"),
    ("SELECT * FROM a FULL OUTER JOIN b ON a.x = b.x", "OUTER JOIN"),
    (
        "SELECT * FROM a JOIN b ON a.x = b.x JOIN c ON b.y = c.y",
        "three or more",
    ),
    ("PREPARE p AS SELECT 1", "prepared statements"),
    ("DECLARE c CURSOR FOR SELECT 1", "cursors"),
    ("COPY t TO STDOUT", "COPY"),
    ("BEGIN", "transactions"),
    ("COMMIT", "transactions"),
    ("SELECT 1; SELECT 2", "more than one statement"),
];

#[test]
fn every_unsupported_construct_is_refused_by_name() {
    for (sql, expected_phrase) in UNSUPPORTED {
        let error = match parse(sql) {
            Err(error) => error,
            Ok(_) => panic!("{sql}\n  parsed, but should have been refused"),
        };

        assert_eq!(
            error.state,
            SqlState::FeatureNotSupported,
            "{sql}\n  refused with {} instead of 0A000: {}",
            error.state.code(),
            error.message
        );

        // The reason may live in DETAIL rather than the message — that is
        // where the second sentence belongs — so both count as naming it.
        let explanation = format!("{} {}", error.message, error.detail.unwrap_or_default());
        assert!(
            explanation.contains(expected_phrase),
            "{sql}\n  refusal did not name the feature: {explanation}"
        );
    }
}

#[test]
fn a_syntax_error_is_a_syntax_error_and_not_a_refusal() {
    // The two must not blur: `0A000` says "zql will not do this", `42601` says
    // "that is not SQL". A user acts differently on each.
    for sql in ["SELECT * form files", "SELECT FROM", "SELECT 1 +"] {
        let error = parse(sql).unwrap_err();
        assert_eq!(error.state, SqlState::SyntaxError, "{sql}: {error}");
    }
}

#[test]
fn the_canonical_typo_gets_a_caret_and_a_suggestion() {
    // §9: `psql` renders the position itself, so this is a large amount of
    // polish for one protocol field.
    let error = parse("SELECT * form files").unwrap_err();
    assert_eq!(error.state, SqlState::SyntaxError);
    assert!(error.position.is_some(), "no position, so psql draws no caret");
    assert_eq!(error.hint.as_deref(), Some("did you mean FROM?"));
}

// ---------------------------------------------------------------- condition 3

/// Truncation at every byte boundary of every canonical query.
///
/// A parser that indexes past the end of its token stream fails here and
/// nowhere else, because a hand-written query is almost never cut in half.
#[test]
fn truncating_every_canonical_query_at_every_byte_never_panics() {
    let mut checked = 0;
    for sql in CANONICAL {
        for end in 0..=sql.len() {
            if !sql.is_char_boundary(end) {
                continue;
            }
            // The result is irrelevant; not panicking is the assertion.
            let _ = parse(&sql[..end]);
            checked += 1;
        }
    }
    assert!(checked > 500, "the sweep did not actually run: {checked}");
}

/// Deletion, duplication and substitution at every position.
#[test]
fn mangling_every_canonical_query_never_panics() {
    let interesting = ['\'', '"', '(', ')', ',', '.', '*', '-', '/', '|', '\0', '写'];

    for sql in CANONICAL {
        let characters: Vec<char> = sql.chars().collect();
        for index in 0..characters.len() {
            let mut deleted = characters.clone();
            deleted.remove(index);
            let _ = parse(&deleted.iter().collect::<String>());

            let mut duplicated = characters.clone();
            duplicated.insert(index, characters[index]);
            let _ = parse(&duplicated.iter().collect::<String>());

            for replacement in interesting {
                let mut substituted = characters.clone();
                substituted[index] = replacement;
                let _ = parse(&substituted.iter().collect::<String>());
            }
        }
    }
}

/// Inputs chosen to break a parser specifically, rather than at random.
#[test]
fn adversarial_inputs_never_panic() {
    let inputs: Vec<String> = vec![
        String::new(),
        " ".to_string(),
        ";".to_string(),
        ";;;;".to_string(),
        "\0".to_string(),
        "SELECT".to_string(),
        "SELECT ".to_string(),
        "SELECT '".to_string(),
        "SELECT \"".to_string(),
        "SELECT /*".to_string(),
        "SELECT --".to_string(),
        "SELECT .".to_string(),
        "SELECT ..".to_string(),
        "SELECT a.".to_string(),
        "SELECT 1e".to_string(),
        "SELECT 1e+".to_string(),
        "SELECT 99999999999999999999999999".to_string(),
        "SELECT -9223372036854775808".to_string(),
        "SELECT 1 BETWEEN".to_string(),
        "SELECT 1 BETWEEN 2".to_string(),
        "SELECT 1 BETWEEN 2 AND".to_string(),
        "SELECT CASE".to_string(),
        "SELECT CASE WHEN".to_string(),
        "SELECT CASE WHEN 1 THEN".to_string(),
        "SELECT CAST(".to_string(),
        "SELECT CAST(1 AS".to_string(),
        "SELECT f(".to_string(),
        "SELECT f(,)".to_string(),
        "SELECT * FROM".to_string(),
        "SELECT * FROM t JOIN".to_string(),
        "SELECT * FROM t JOIN u".to_string(),
        "SELECT * FROM t JOIN u ON".to_string(),
        "SELECT * FROM t ORDER BY".to_string(),
        "SELECT * FROM t LIMIT".to_string(),
        "SELECT * FROM t LIMIT -1".to_string(),
        "SELECT * FROM t LIMIT 99999999999999999999".to_string(),
        "SELECT 写真 FROM 写真".to_string(),
        "SELECT '🎞'".to_string(),
        // Deep nesting: the recursive descent has to survive its own depth.
        format!("SELECT {}1{}", "(".repeat(200), ")".repeat(200)),
        format!("SELECT {}1", "-".repeat(200)),
        format!("SELECT {}1", "NOT ".repeat(200)),
        format!("SELECT 1{}", " + 1".repeat(2000)),
        "SELECT ".to_string() + &"a,".repeat(2000) + "b",
    ];

    for input in &inputs {
        let _ = parse(input);
    }
}

// ------------------------------------------------------- precedence, §2 table

#[test]
fn precedence_runs_or_and_not_comparison_concat_additive_multiplicative() {
    // `a OR b AND c` groups as `a OR (b AND c)`.
    let filter = filter_of("SELECT 1 WHERE a OR b AND c");
    let ExprKind::Binary { op, right, .. } = &filter.kind else {
        panic!("expected OR at the root");
    };
    assert_eq!(*op, BinaryOp::Or);
    assert!(matches!(
        right.kind,
        ExprKind::Binary {
            op: BinaryOp::And,
            ..
        }
    ));

    // `a + b * c` groups as `a + (b * c)`.
    let filter = filter_of("SELECT 1 WHERE a + b * c");
    let ExprKind::Binary { op, right, .. } = &filter.kind else {
        panic!("expected + at the root");
    };
    assert_eq!(*op, BinaryOp::Add);
    assert!(matches!(
        right.kind,
        ExprKind::Binary {
            op: BinaryOp::Mul,
            ..
        }
    ));

    // `a || b = c` groups as `(a || b) = c`: concatenation binds tighter.
    let filter = filter_of("SELECT 1 WHERE a || b = c");
    let ExprKind::Binary { op, left, .. } = &filter.kind else {
        panic!("expected = at the root");
    };
    assert_eq!(*op, BinaryOp::Eq);
    assert!(matches!(
        left.kind,
        ExprKind::Binary {
            op: BinaryOp::Concat,
            ..
        }
    ));
}

#[test]
fn subtraction_is_left_associative() {
    // `a - b - c` is `(a - b) - c`, not `a - (b - c)`.
    let filter = filter_of("SELECT 1 WHERE a - b - c");
    let ExprKind::Binary { op, left, .. } = &filter.kind else {
        panic!("expected - at the root");
    };
    assert_eq!(*op, BinaryOp::Sub);
    assert!(matches!(
        left.kind,
        ExprKind::Binary {
            op: BinaryOp::Sub,
            ..
        }
    ));
}

#[test]
fn not_takes_a_whole_comparison() {
    // `NOT a = b` is `NOT (a = b)` — NOT binds looser than comparison.
    let filter = filter_of("SELECT 1 WHERE NOT a = b");
    let ExprKind::Unary {
        op: UnaryOp::Not,
        expr,
    } = &filter.kind
    else {
        panic!("expected NOT at the root");
    };
    assert!(matches!(
        expr.kind,
        ExprKind::Binary {
            op: BinaryOp::Eq,
            ..
        }
    ));
}

#[test]
fn unary_minus_binds_tighter_than_multiplication() {
    // `-a * b` is `(-a) * b`.
    let filter = filter_of("SELECT 1 WHERE -a * b");
    let ExprKind::Binary { op, left, .. } = &filter.kind else {
        panic!("expected * at the root");
    };
    assert_eq!(*op, BinaryOp::Mul);
    assert!(matches!(
        left.kind,
        ExprKind::Unary {
            op: UnaryOp::Neg,
            ..
        }
    ));
}

#[test]
fn between_does_not_swallow_a_following_and() {
    // `x BETWEEN 1 AND 2 AND y` must be `(x BETWEEN 1 AND 2) AND y`.
    let filter = filter_of("SELECT 1 WHERE x BETWEEN 1 AND 2 AND y");
    let ExprKind::Binary { op, left, .. } = &filter.kind else {
        panic!("expected AND at the root, got {:?}", filter.kind);
    };
    assert_eq!(*op, BinaryOp::And);
    assert!(matches!(left.kind, ExprKind::Between { .. }));
}

#[test]
fn the_negated_predicates_all_parse() {
    for sql in [
        "SELECT 1 WHERE a NOT LIKE 'x%'",
        "SELECT 1 WHERE a NOT IN (1, 2)",
        "SELECT 1 WHERE a NOT BETWEEN 1 AND 2",
        "SELECT 1 WHERE a IS NOT NULL",
    ] {
        let negated = match filter_of(sql).kind {
            ExprKind::Like { negated, .. }
            | ExprKind::InList { negated, .. }
            | ExprKind::Between { negated, .. }
            | ExprKind::IsNull { negated, .. } => negated,
            other => panic!("{sql}: unexpected node {other:?}"),
        };
        assert!(negated, "{sql}: the NOT was lost");
    }
}

#[test]
fn count_star_and_count_distinct_are_distinguished() {
    let ExprKind::Function(star) = projection_expr("SELECT COUNT(*) FROM t").kind else {
        panic!("expected a function call");
    };
    assert!(star.star && !star.distinct && star.args.is_empty());

    let ExprKind::Function(distinct) = projection_expr("SELECT COUNT(DISTINCT x) FROM t").kind
    else {
        panic!("expected a function call");
    };
    assert!(distinct.distinct && !distinct.star && distinct.args.len() == 1);
}

#[test]
fn order_by_carries_direction_and_null_placement() {
    let Statement::Select(select) =
        parse("SELECT x FROM t ORDER BY a DESC NULLS FIRST, b ASC, c").unwrap()
    else {
        panic!("expected a SELECT");
    };

    assert_eq!(select.order_by.len(), 3);
    assert!(select.order_by[0].descending);
    assert_eq!(select.order_by[0].nulls, Some(NullsOrder::First));
    assert!(!select.order_by[1].descending);
    assert_eq!(select.order_by[1].nulls, None);
    assert!(!select.order_by[2].descending);
}

#[test]
fn limit_and_offset_are_accepted_in_either_order() {
    for sql in [
        "SELECT x FROM t LIMIT 10 OFFSET 5",
        "SELECT x FROM t OFFSET 5 LIMIT 10",
    ] {
        let Statement::Select(select) = parse(sql).unwrap() else {
            panic!("expected a SELECT");
        };
        assert_eq!(select.limit, Some(10), "{sql}");
        assert_eq!(select.offset, Some(5), "{sql}");
    }
}

#[test]
fn a_star_cannot_be_mixed_with_named_columns() {
    // The grammar admits `*` or a list, never both, and the message says so.
    let error = parse("SELECT *, name FROM files").unwrap_err();
    assert_eq!(error.state, SqlState::SyntaxError);
    assert!(error.message.contains('*'));
}

// ------------------------------------------------------- expression depth

/// `::` is not in the grammar, and the error should say which spelling is.
#[test]
fn the_postgres_cast_operator_is_refused_by_name() {
    let error = parse("SELECT size::text FROM files").unwrap_err();
    assert_eq!(error.state, SqlState::SyntaxError);
    assert!(error.message.contains("::"), "message was: {}", error.message);
    assert!(
        error.hint.unwrap_or_default().contains("CAST"),
        "the hint should point at the spelling that works"
    );
}

// ------------------------------------------------------------------- helpers

fn filter_of(sql: &str) -> Expr {
    let Statement::Select(select) = parse(sql).unwrap_or_else(|error| panic!("{sql}: {error}"))
    else {
        panic!("{sql}: expected a SELECT");
    };
    select.filter.unwrap_or_else(|| panic!("{sql}: no WHERE"))
}

fn projection_expr(sql: &str) -> Expr {
    let Statement::Select(select) = parse(sql).unwrap_or_else(|error| panic!("{sql}: {error}"))
    else {
        panic!("{sql}: expected a SELECT");
    };
    match select.projection {
        Projection::Items(mut items) if !items.is_empty() => items.remove(0).expr,
        _ => panic!("{sql}: expected a projection list"),
    }
}

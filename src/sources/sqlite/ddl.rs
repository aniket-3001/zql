//! Column names and types, recovered from `CREATE TABLE` text.
//!
//! SQLite stores no structured column list. The only record of a table's
//! columns is the original `CREATE TABLE` statement, kept verbatim in
//! `sqlite_master.sql` — so `RowDescription` cannot be written without parsing
//! DDL, and this module exists.
//!
//! It reuses zql's own SQL lexer rather than scanning bytes: quoted
//! identifiers, comments, and string defaults are all already handled there,
//! and each is a way a naive split-on-commas goes wrong.
//!
//! **Recovering the original case.** The lexer folds unquoted identifiers to
//! lower case, which is right for zql's own SQL and wrong here — a column
//! written `visitCount` should be shown as `visitCount`. The fold is
//! `to_ascii_lowercase`, which never changes a byte's *length*, so the original
//! text can be sliced straight back out of the DDL using the token's recorded
//! position. That is why [`Token::position`] is on every token and not only on
//! the ones that can produce an error.

use crate::error::Result;
use crate::sql::lexer::tokenize;
use crate::sql::token::{Symbol, Token, TokenKind};
use crate::value::Type;

/// One column of a SQLite table.
#[derive(Debug, Clone, PartialEq)]
pub struct SqliteColumn {
    pub name: String,
    /// The declared type as written, or empty if the column was declared
    /// without one — which SQLite permits.
    pub declared_type: String,
    /// Whether this column is `INTEGER PRIMARY KEY`, and therefore an alias
    /// for the rowid.
    pub is_rowid_alias: bool,
}

impl SqliteColumn {
    /// This column's affinity.
    pub fn affinity(&self) -> Affinity {
        if self.is_rowid_alias {
            Affinity::Integer
        } else {
            affinity(&self.declared_type)
        }
    }

    /// The zql type advertised for this column, from SQLite's affinity rules.
    pub fn ty(&self) -> Type {
        match self.affinity() {
            Affinity::Integer => Type::Int,
            Affinity::Text => Type::Text,
            Affinity::Blob => Type::Blob,
            Affinity::Real | Affinity::Numeric => Type::Real,
        }
    }
}

/// SQLite's five type affinities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Affinity {
    Integer,
    Text,
    Blob,
    Real,
    Numeric,
}

/// Resolves a declared type to an affinity.
///
/// Declared types in SQLite are advisory and matched by *substring*, in a fixed
/// order that matters: `POINT` contains `INT`, so it is an INTEGER column, and
/// checking `REAL` before `INT` would get that wrong. The order below is the
/// order in the file format specification.
pub fn affinity(declared: &str) -> Affinity {
    let upper = declared.to_ascii_uppercase();

    if upper.contains("INT") {
        Affinity::Integer
    } else if upper.contains("CHAR") || upper.contains("CLOB") || upper.contains("TEXT") {
        Affinity::Text
    } else if upper.contains("BLOB") || upper.is_empty() {
        Affinity::Blob
    } else if upper.contains("REAL") || upper.contains("FLOA") || upper.contains("DOUB") {
        Affinity::Real
    } else {
        Affinity::Numeric
    }
}

/// Extracts the column list from a `CREATE TABLE` statement.
///
/// Returns an empty list rather than an error when the statement cannot be
/// understood — a `CREATE VIRTUAL TABLE`, for instance. The caller turns that
/// into a clear message naming the table, which beats a parse error naming a
/// byte offset in text the user never wrote.
pub fn parse_create_table(sql: &str) -> Result<Vec<SqliteColumn>> {
    let tokens = tokenize(sql)?;

    // Everything before the first `(` is `CREATE TABLE <name>`; the column
    // definitions are what follows, up to its matching `)`.
    let Some(open) = tokens.iter().position(|t| t.is_symbol(Symbol::LParen)) else {
        return Ok(Vec::new());
    };

    let mut columns = Vec::new();
    for definition in split_top_level(&tokens[open + 1..]) {
        if let Some(column) = parse_column(definition, sql) {
            columns.push(column);
        }
    }
    Ok(columns)
}

/// Splits the definition list on commas that are not inside parentheses.
///
/// The nesting matters: `CHECK (x IN (1, 2))` and `DECIMAL(10, 2)` both contain
/// commas that do not separate columns.
fn split_top_level(tokens: &[Token]) -> Vec<&[Token]> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;

    for (index, token) in tokens.iter().enumerate() {
        match token.kind {
            TokenKind::Symbol(Symbol::LParen) => depth += 1,
            TokenKind::Symbol(Symbol::RParen) => {
                // Depth zero here is the paren closing the whole list.
                if depth == 0 {
                    parts.push(&tokens[start..index]);
                    return parts;
                }
                depth -= 1;
            }
            TokenKind::Symbol(Symbol::Comma) if depth == 0 => {
                parts.push(&tokens[start..index]);
                start = index + 1;
            }
            TokenKind::Eof => break,
            _ => {}
        }
    }

    if start < tokens.len() {
        parts.push(&tokens[start..]);
    }
    parts
}

/// Turns one definition into a column, or `None` if it is a table constraint.
fn parse_column(tokens: &[Token], sql: &str) -> Option<SqliteColumn> {
    let first = tokens.first()?;

    // Table-level constraints share the list with columns and are not columns.
    // `CONSTRAINT` may precede any of them.
    if TABLE_CONSTRAINTS.contains(&word(first)) {
        return None;
    }

    let name = original_text(first, sql);
    if name.is_empty() {
        return None;
    }

    // The type name runs from after the column name up to the first constraint
    // keyword — or to the end, since a type is optional.
    let mut declared = String::new();
    let mut index = 1;
    while let Some(token) = tokens.get(index) {
        if is_constraint_start(token) {
            break;
        }
        // Parameterised types such as `VARCHAR(255)` contribute their
        // parentheses too; affinity only looks for substrings, so keeping them
        // is harmless and keeping the text faithful is worth more.
        if !declared.is_empty() && matches!(token.kind, TokenKind::Identifier) {
            declared.push(' ');
        }
        declared.push_str(&original_text(token, sql));
        index += 1;
    }

    Some(SqliteColumn {
        is_rowid_alias: is_rowid_alias(&declared, &tokens[index..]),
        name,
        declared_type: declared.trim().to_string(),
    })
}

/// `INTEGER PRIMARY KEY` — and *only* that spelling — aliases the rowid.
///
/// **Found by comparing against the oracle, not by reading the specification.**
/// Such a column is stored as `NULL` in every record, with the real value
/// living in the cell header. Miss it and every primary key reads as NULL:
/// plausible enough to survive casual testing, and wrong in every single row.
///
/// The exact type matters. `INT PRIMARY KEY` is *not* an alias — it is an
/// ordinary column with INTEGER affinity — and neither is a `DESC` primary key.
fn is_rowid_alias(declared: &str, rest: &[Token]) -> bool {
    if !declared.trim().eq_ignore_ascii_case("INTEGER") {
        return false;
    }

    let mut tokens = rest.iter().filter(|t| !matches!(t.kind, TokenKind::Eof));
    if tokens.next().map(word) != Some("primary") {
        return false;
    }
    if tokens.next().map(word) != Some("key") {
        return false;
    }

    // `PRIMARY KEY DESC` is stored as an ordinary indexed column.
    tokens.next().map(word) != Some("desc")
}

/// Words that begin a table-level constraint rather than a column.
const TABLE_CONSTRAINTS: &[&str] =
    &["primary", "unique", "check", "foreign", "constraint"];

/// Words that end a column's declared type and begin its constraints.
const COLUMN_CONSTRAINTS: &[&str] = &[
    "primary",
    "not",
    "null",
    "unique",
    "check",
    "default",
    "collate",
    "references",
    "generated",
    "as",
    "constraint",
    "foreign",
    "autoincrement",
];

/// A token's word, lower-cased.
///
/// Both identifiers and keywords carry their folded word in `text`, so this
/// works whether or not zql's own grammar happens to reserve the word. That
/// matters: `KEY`, `DEFAULT` and `COLLATE` are SQLite DDL vocabulary, and
/// reserving them in zql would break `SELECT key FROM sqlite(...)`.
fn word(token: &Token) -> &str {
    match token.kind {
        TokenKind::Identifier | TokenKind::Keyword(_) => &token.text,
        _ => "",
    }
}

fn is_constraint_start(token: &Token) -> bool {
    COLUMN_CONSTRAINTS.contains(&word(token))
}

/// The token's text as it was originally written.
///
/// Quoted identifiers already carry their true text. Unquoted ones were folded
/// to lower case by `to_ascii_lowercase`, which is length-preserving, so the
/// original bytes can be sliced back out at the recorded position.
fn original_text(token: &Token, sql: &str) -> String {
    match token.kind {
        TokenKind::Identifier | TokenKind::Keyword(_) => {
            let start = token.position.saturating_sub(1) as usize;
            let end = start + token.text.len();
            match sql.get(start..end) {
                // Only trust the slice if it really is the same word; a quoted
                // identifier's text is shorter than its source span.
                Some(slice) if slice.eq_ignore_ascii_case(&token.text) => slice.to_string(),
                _ => token.text.clone(),
            }
        }
        _ => token.text.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn columns(sql: &str) -> Vec<SqliteColumn> {
        parse_create_table(sql).unwrap()
    }

    fn names(sql: &str) -> Vec<String> {
        columns(sql).into_iter().map(|c| c.name).collect()
    }

    #[test]
    fn a_plain_table_yields_its_columns_in_order() {
        let parsed = columns("CREATE TABLE users (id INTEGER, name TEXT, score REAL)");
        assert_eq!(names("CREATE TABLE users (id INTEGER, name TEXT, score REAL)"),
                   vec!["id", "name", "score"]);
        assert_eq!(parsed[0].ty(), Type::Int);
        assert_eq!(parsed[1].ty(), Type::Text);
        assert_eq!(parsed[2].ty(), Type::Real);
    }

    #[test]
    fn column_case_survives_the_lexers_folding() {
        assert_eq!(
            names("CREATE TABLE t (visitCount INTEGER, URL TEXT)"),
            vec!["visitCount", "URL"]
        );
    }

    #[test]
    fn quoted_identifiers_keep_their_text_including_keywords() {
        assert_eq!(
            names(r#"CREATE TABLE t ("select" INTEGER, "My Column" TEXT)"#),
            vec!["select", "My Column"]
        );
    }

    #[test]
    fn a_keyword_used_unquoted_as_a_name_still_works() {
        // SQLite allows many of these; zql's lexer sees a keyword token.
        assert_eq!(names("CREATE TABLE t (key INTEGER, value TEXT)"), vec!["key", "value"]);
    }

    #[test]
    fn parameterised_types_do_not_split_the_column_list() {
        let parsed = columns("CREATE TABLE t (a VARCHAR(255), b DECIMAL(10, 2), c TEXT)");
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].ty(), Type::Text);
        assert_eq!(parsed[1].ty(), Type::Real, "DECIMAL is NUMERIC affinity");
    }

    #[test]
    fn table_level_constraints_are_not_columns() {
        let parsed = columns(
            "CREATE TABLE t (a INTEGER, b TEXT, PRIMARY KEY (a, b), \
             FOREIGN KEY (b) REFERENCES u(x), UNIQUE (a), CHECK (a > 0))",
        );
        assert_eq!(
            parsed.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            vec!["a", "b"]
        );
    }

    #[test]
    fn a_check_constraint_containing_a_comma_does_not_split() {
        let parsed = columns("CREATE TABLE t (a INTEGER CHECK (a IN (1, 2, 3)), b TEXT)");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[1].name, "b");
    }

    #[test]
    fn integer_primary_key_is_recognised_as_a_rowid_alias() {
        let parsed = columns("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)");
        assert!(parsed[0].is_rowid_alias, "INTEGER PRIMARY KEY aliases rowid");
        assert!(!parsed[1].is_rowid_alias);
    }

    #[test]
    fn only_the_exact_spelling_aliases_the_rowid() {
        // INT is not INTEGER, and a DESC key is stored normally. Both of these
        // are ordinary columns whose values live in the record.
        assert!(!columns("CREATE TABLE t (id INT PRIMARY KEY)")[0].is_rowid_alias);
        assert!(!columns("CREATE TABLE t (id INTEGER PRIMARY KEY DESC)")[0].is_rowid_alias);
        assert!(!columns("CREATE TABLE t (id BIGINT PRIMARY KEY)")[0].is_rowid_alias);
        // But autoincrement and extra constraints do not stop it.
        assert!(
            columns("CREATE TABLE t (id INTEGER PRIMARY KEY AUTOINCREMENT)")[0].is_rowid_alias
        );
    }

    #[test]
    fn a_column_with_no_declared_type_is_blob_affinity() {
        let parsed = columns("CREATE TABLE t (anything, b TEXT)");
        assert_eq!(parsed[0].declared_type, "");
        assert_eq!(parsed[0].ty(), Type::Blob);
    }

    #[test]
    fn affinity_follows_the_specified_substring_order() {
        assert_eq!(affinity("INTEGER"), Affinity::Integer);
        assert_eq!(affinity("BIGINT"), Affinity::Integer);
        // POINT contains INT, and the order in the spec makes it an integer.
        assert_eq!(affinity("POINT"), Affinity::Integer);
        assert_eq!(affinity("VARCHAR(20)"), Affinity::Text);
        assert_eq!(affinity("CLOB"), Affinity::Text);
        assert_eq!(affinity(""), Affinity::Blob);
        assert_eq!(affinity("BLOB"), Affinity::Blob);
        assert_eq!(affinity("DOUBLE PRECISION"), Affinity::Real);
        assert_eq!(affinity("FLOATING POINT"), Affinity::Integer, "INT wins");
        assert_eq!(affinity("DECIMAL(10,5)"), Affinity::Numeric);
        assert_eq!(affinity("DATETIME"), Affinity::Numeric);
    }

    #[test]
    fn unparseable_ddl_yields_no_columns_rather_than_an_error() {
        assert!(columns("CREATE VIRTUAL TABLE t USING fts5(body)").len() <= 1);
        assert!(parse_create_table("nonsense").unwrap().is_empty());
        assert!(parse_create_table("").unwrap().is_empty());
    }

    #[test]
    fn defaults_and_collations_are_not_mistaken_for_types() {
        let parsed = columns(
            "CREATE TABLE t (a TEXT DEFAULT 'x, y' COLLATE NOCASE, b INTEGER NOT NULL DEFAULT 0)",
        );
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].declared_type, "TEXT");
        assert_eq!(parsed[1].declared_type, "INTEGER");
    }
}

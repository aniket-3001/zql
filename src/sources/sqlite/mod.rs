//! `sqlite('file.db', 'table')` — the headline source.
//!
//! Almost every application on a machine keeps its data in SQLite: browsers,
//! phones, notes apps, photo libraries. You have dozens of `.db` files and no
//! way to look inside any of them without installing something. This module is
//! why zql exists.
//!
//! It is read-only, and it never opens the file for writing.
//!
//! # A note on types
//!
//! SQLite is dynamically typed: a column's declared type is an *affinity*, a
//! strong hint about what it holds rather than a guarantee. zql advertises the
//! affinity in `RowDescription`, because that is what the schema says and it is
//! what makes `psql` right-align a number column. A value that contradicts its
//! column's affinity — a string stored in an `INTEGER` column, which SQLite
//! permits — is still returned faithfully rather than being coerced or
//! dropped. The README says so.

pub mod btree;
pub mod ddl;
pub mod pager;
pub mod record;

use std::path::{Path, PathBuf};

use crate::error::{Result, SqlState, ZqlError};
use crate::exec::RowIter;
use crate::plan::schema::{Column, Schema};
use crate::server::cancel::CancelFlag;
use crate::sources::TableSource;
use crate::value::{Row, Value};

use btree::Cursor;
use ddl::SqliteColumn;
use pager::Pager;

/// One object in `sqlite_master`.
struct SchemaEntry {
    kind: String,
    name: String,
    root_page: u32,
    sql: String,
}

/// A table inside a SQLite database file.
pub struct SqliteSource {
    path: PathBuf,
    root_page: u32,
    columns: Vec<SqliteColumn>,
    schema: Schema,
}

impl SqliteSource {
    /// Opens a database and locates one table.
    ///
    /// All of this happens at *bind* time, so a missing file, a missing table
    /// or an un-checkpointed WAL is an error before `RowDescription` is sent —
    /// rather than a half-streamed result that dies on row zero.
    pub fn open(path: &Path, table: &str) -> Result<SqliteSource> {
        let entries = read_schema(path)?;

        let entry = entries
            .iter()
            .filter(|entry| entry.kind == "table")
            .find(|entry| entry.name.eq_ignore_ascii_case(table))
            .ok_or_else(|| unknown_table(path, table, &entries))?;

        let columns = ddl::parse_create_table(&entry.sql)?;
        if columns.is_empty() {
            return Err(ZqlError::unsupported(format!(
                "the table \"{}\", whose definition zql cannot read",
                entry.name
            ))
            .with_detail(format!("its definition is: {}", truncate(&entry.sql, 120)))
            .with_hint("virtual tables such as FTS indexes are not supported"));
        }

        let schema = Schema::new(
            columns
                .iter()
                .map(|column| Column::new(&column.name, column.ty()))
                .collect(),
        );

        Ok(SqliteSource {
            path: path.to_path_buf(),
            root_page: entry.root_page,
            columns,
            schema,
        })
    }

    /// The tables in a database, for `sqlite()` called without a table name.
    pub fn table_names(path: &Path) -> Result<Vec<String>> {
        Ok(read_schema(path)?
            .into_iter()
            .filter(|entry| entry.kind == "table" && !entry.name.starts_with("sqlite_"))
            .map(|entry| entry.name)
            .collect())
    }
}

impl TableSource for SqliteSource {
    fn schema(&self) -> &Schema {
        &self.schema
    }

    fn scan(&self, _cancel: &CancelFlag) -> Result<Box<dyn RowIter>> {
        let pager = Pager::open(&self.path)?;
        let cursor = Cursor::open(pager, self.root_page)?;

        Ok(Box::new(TableScan {
            cursor,
            columns: self.columns.clone(),
        }))
    }
}

struct TableScan {
    cursor: Cursor,
    columns: Vec<SqliteColumn>,
}

impl RowIter for TableScan {
    fn next(&mut self) -> Result<Option<Row>> {
        let Some(record) = self.cursor.next()? else {
            return Ok(None);
        };

        let mut values = Vec::with_capacity(self.columns.len());
        for (index, column) in self.columns.iter().enumerate() {
            // An `INTEGER PRIMARY KEY` column is an alias for the rowid and is
            // stored as NULL in every record — the real value lives in the cell
            // header. Substituting it here is the difference between a primary
            // key column that works and one that is NULL in every single row.
            if column.is_rowid_alias {
                values.push(Value::Int(record.rowid));
                continue;
            }

            // A record may be shorter than the schema: SQLite's `ALTER TABLE
            // ADD COLUMN` does not rewrite existing rows, so rows written
            // before the column existed simply end early, and the value is
            // NULL rather than an error.
            let value = record.values.get(index).cloned().unwrap_or(Value::Null);
            values.push(apply_affinity(value, column.affinity()));
        }

        Ok(Some(Row::new(values)))
    }
}

/// Applies the one affinity rule that changes a value on the way *out*.
///
/// SQLite stores a `REAL`-affinity value with no fractional part as an
/// integer, to save space, and converts it back to a float when it is read.
/// So a `score REAL` column holding 375.0 has an *integer* 375 in the record,
/// and a reader that returns it as an integer disagrees with every other
/// SQLite client about what is in the database.
///
/// Found by the oracle, not by reading: the value looked entirely plausible.
/// It is also the reason the reader advertises `float8` for such a column and
/// must then produce something a client will parse as one.
///
/// Nothing else is coerced. SQLite is dynamically typed and a value that
/// contradicts its column's affinity — text in an `INTEGER` column, which is
/// permitted — is returned as it was stored rather than mangled into shape.
fn apply_affinity(value: Value, affinity: ddl::Affinity) -> Value {
    match (&value, affinity) {
        (Value::Int(number), ddl::Affinity::Real) => Value::Real(*number as f64),
        _ => value,
    }
}

/// Reads `sqlite_master`, which is always the b-tree rooted at page 1.
fn read_schema(path: &Path) -> Result<Vec<SchemaEntry>> {
    let pager = Pager::open(path)?;
    let mut cursor = Cursor::open(pager, 1)?;

    let mut entries = Vec::new();
    while let Some(record) = cursor.next()? {
        // sqlite_master is (type, name, tbl_name, rootpage, sql).
        let text = |index: usize| match record.values.get(index) {
            Some(Value::Text(text)) => text.clone(),
            _ => String::new(),
        };

        let root_page = match record.values.get(3) {
            Some(Value::Int(page)) => u32::try_from(*page).unwrap_or(0),
            _ => 0,
        };

        entries.push(SchemaEntry {
            kind: text(0),
            name: text(1),
            root_page,
            sql: text(4),
        });
    }

    Ok(entries)
}

/// "No such table" — with the tables that *are* there.
///
/// The single most useful error this module can produce: nobody remembers what
/// is inside `places.sqlite`, and the answer to "which table?" is right here.
fn unknown_table(path: &Path, wanted: &str, entries: &[SchemaEntry]) -> ZqlError {
    let available: Vec<&str> = entries
        .iter()
        .filter(|entry| entry.kind == "table" && !entry.name.starts_with("sqlite_"))
        .map(|entry| entry.name.as_str())
        .collect();

    let error = ZqlError::new(
        SqlState::UndefinedTable,
        format!("{} has no table named \"{wanted}\"", path.display()),
    );

    if available.is_empty() {
        error.with_hint("this database contains no ordinary tables")
    } else {
        error.with_hint(format!("it contains: {}", available.join(", ")))
    }
}

fn truncate(text: &str, limit: usize) -> String {
    let mut end = limit.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    if end < text.len() {
        format!("{}…", &text[..end])
    } else {
        text.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `SqliteSource` is not `Debug`, so `unwrap_err` is unavailable.
    fn expect_open_error(path: &str, table: &str) -> ZqlError {
        match SqliteSource::open(Path::new(path), table) {
            Err(error) => error,
            Ok(_) => panic!("expected {path} to fail to open"),
        }
    }

    #[test]
    fn a_non_sqlite_file_is_refused_without_panicking() {
        let error = expect_open_error("Cargo.toml", "anything");
        assert_eq!(error.state, SqlState::IoError);
        assert!(error.message.contains("not a SQLite database"));
    }

    #[test]
    fn a_missing_file_is_an_io_error() {
        let error = expect_open_error("no-such-file.db", "t");
        assert_eq!(error.state, SqlState::IoError);
    }

    #[test]
    fn truncation_respects_character_boundaries() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello", 3), "hel…");
        // Cutting mid-character must not panic.
        assert_eq!(truncate("héllo", 2), "h…");
        assert_eq!(truncate("写真", 1), "…");
    }
}

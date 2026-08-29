//! Table sources: the things a `FROM` clause can name.
//!
//! A source is either a bare name (`files`, `env`) or a function call
//! (`sqlite('app.db', 'users')`). Both resolve here, at *bind* time — so a bad
//! path is a `42P01` before execution starts, rather than a half-streamed
//! result that dies on row zero with a `RowDescription` already sent.

pub mod cache;
pub mod csv;
pub mod env;
pub mod files;
pub mod sqlite;
pub mod tail;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::sources::cache::FileIndexCache;

use crate::error::{Result, SqlState, ZqlError};
use crate::exec::RowIter;
use crate::plan::schema::Schema;
use crate::server::cancel::CancelFlag;
use crate::sql::ast::{Literal, Source};

/// Something rows can be read from.
pub trait TableSource: Send {
    /// The output shape, known before any row is read because
    /// `RowDescription` must precede the first `DataRow`.
    fn schema(&self) -> &Schema;

    /// Opens a fresh stream over the source.
    ///
    /// The returned iterator owns everything it needs — a materialised vector,
    /// a shared index handle, or an open file. Borrowing from `self` instead
    /// would make the plan self-referential for no benefit.
    ///
    /// # Why a source is handed the cancel flag
    ///
    /// Every operator above checks it between rows, which suffices while rows
    /// keep arriving. `tail()` breaks that assumption: between two log lines it
    /// is asleep, and a check that only runs on producing a row would never
    /// run. Finite sources ignore the argument.
    fn scan(&self, cancel: &CancelFlag) -> Result<Box<dyn RowIter>>;
}

/// What the sources need to know about the session asking.
#[derive(Clone)]
pub struct SourceConfig {
    /// The directory `files` walks when given no path of its own.
    pub dir: PathBuf,
    /// This session's filesystem index. `None` under `--no-cache`.
    pub files_cache: Option<Arc<FileIndexCache>>,
}

impl SourceConfig {
    /// A configuration with no caching, for tests and one-shot binds.
    pub fn uncached(dir: PathBuf) -> Self {
        SourceConfig {
            dir,
            files_cache: None,
        }
    }
}

/// Every source zql actually provides, with the signature shown by
/// `SHOW SOURCES` and in the "did you mean" list on an unknown name.
///
/// `json()` is deliberately absent: it was cut, and listing a source that then
/// refuses every call would make the one command whose whole job is to answer
/// "what can I query?" into a source of wrong answers.
pub const CATALOGUE: &[(&str, &str, &str)] = &[
    ("files", "files | files('path')", "the filesystem, walked recursively"),
    ("env", "env", "environment variables of the server process"),
    ("sqlite", "sqlite('file.db', 'table')", "a table in a SQLite database"),
    ("csv", "csv('file.csv')", "a CSV file with a header row"),
    ("tail", "tail('file.log')", "a log file, streamed as it grows"),
];

/// Resolves a `FROM` item to a live source.
pub fn resolve(source: &Source, config: &SourceConfig) -> Result<Box<dyn TableSource>> {
    let args = source.args.as_deref().unwrap_or(&[]);

    match source.name.as_str() {
        "files" => {
            let path = match args {
                [] => config.dir.clone(),
                [Literal::String(path)] => PathBuf::from(path),
                _ => return Err(bad_arguments(source, "files() takes one optional path")),
            };
            Ok(Box::new(files::FilesSource::open(
                path,
                config.files_cache.clone(),
            )?))
        }

        "env" => {
            if !args.is_empty() {
                return Err(bad_arguments(source, "env takes no arguments"));
            }
            Ok(Box::new(env::EnvSource::new()))
        }

        "sqlite" => match args {
            [Literal::String(path), Literal::String(table)] => {
                Ok(Box::new(sqlite::SqliteSource::open(Path::new(path), table)?))
            }
            // Nobody remembers what tables are inside `places.sqlite`, so the
            // one-argument form answers that question instead of refusing.
            [Literal::String(path)] => {
                let tables = sqlite::SqliteSource::table_names(Path::new(path))?;
                Err(ZqlError::new(
                    SqlState::SyntaxError,
                    "sqlite() needs a table name",
                )
                .at(source.position)
                .with_detail(if tables.is_empty() {
                    format!("{path} contains no ordinary tables")
                } else {
                    format!("{path} contains: {}", tables.join(", "))
                })
                .with_hint("for example: sqlite('file.db', 'table')"))
            }
            _ => Err(bad_arguments(
                source,
                "sqlite() takes a file path and a table name, both as strings",
            )),
        },

        "csv" => match args {
            [Literal::String(path)] => Ok(Box::new(csv::CsvSource::open(Path::new(path))?)),
            _ => Err(bad_arguments(source, "csv() takes one file path, as a string")),
        },

        "tail" => match args {
            [Literal::String(path)] => Ok(Box::new(tail::TailSource::open(Path::new(path))?)),
            _ => Err(bad_arguments(source, "tail() takes one file path, as a string")),
        },

        // The declared sacrifice from FEATURES.md §8: `csv()` covers the same
        // ground and the SQLite reader already shows the format craft.
        "json" => Err(ZqlError::unsupported("the json() source")
            .at(source.position)
            .with_detail("it was cut deliberately; csv() reads tabular data")),

        // Exists only when the library is compiled with `cfg(test)`, so it
        // cannot appear in the shipped binary. It is the only way to raise a
        // genuine panic inside a real connection thread, which is what the
        // `catch_unwind` boundary in `server::handle` exists to contain — and
        // an untested safety net is not a safety net.
        #[cfg(test)]
        "__panic_probe" => panic!("deliberate panic, to exercise the connection guard"),

        unknown => {
            let mut error = ZqlError::new(
                SqlState::UndefinedTable,
                format!("no source named \"{unknown}\""),
            )
            .at(source.position);

            let names: Vec<&str> = CATALOGUE.iter().map(|(name, _, _)| *name).collect();
            error = error.with_hint(format!("available sources: {}", names.join(", ")));
            Err(error)
        }
    }
}

fn bad_arguments(source: &Source, message: &str) -> ZqlError {
    ZqlError::new(SqlState::SyntaxError, message.to_string()).at(source.position)
}

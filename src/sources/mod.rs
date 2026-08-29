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

use std::path::PathBuf;
use std::sync::Arc;

use crate::sources::cache::FileIndexCache;

use crate::error::Result;
use crate::exec::RowIter;
use crate::plan::schema::Schema;
use crate::server::cancel::CancelFlag;

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

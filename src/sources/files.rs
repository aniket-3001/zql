//! `files` — the filesystem as a table.
//!
//! Columns: `path`, `name`, `ext`, `size`, `modified`, `is_dir`, `depth`.
//!
//! The walk stands in for `walkdir`, and the interesting parts are the two
//! things `walkdir` does that a naive `read_dir` recursion does not:
//!
//! - **It does not follow symbolic links.** A link pointing at an ancestor
//!   turns a recursive walk into an infinite one, and a directory that contains
//!   a link to itself is a thing that exists on real machines.
//! - **It does not abandon the walk on one unreadable directory.** A
//!   permission error deep in a tree should cost you that subtree, not your
//!   query.

use std::fs::{self, DirEntry, Metadata};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use crate::error::{Result, SqlState, ZqlError};
use crate::exec::values::ValuesIter;
use crate::exec::RowIter;
use crate::plan::schema::{Column, Schema};
use crate::sources::cache::FileIndexCache;
use crate::server::cancel::CancelFlag;
use crate::sources::TableSource;
use crate::value::{Row, Type, Value};

/// A ceiling on entries collected in one walk.
///
/// The walk materialises, so an unbounded tree is an unbounded allocation.
/// Ten million rows is far past anything a person inspects interactively and
/// far below what would exhaust a machine, and hitting it is a clear `54000`
/// rather than an out-of-memory kill.
const MAX_ENTRIES: usize = 10_000_000;

pub struct FilesSource {
    root: PathBuf,
    schema: Schema,
    /// The session's index. `None` under `--no-cache`, which re-walks the tree
    /// on every query.
    cache: Option<Arc<FileIndexCache>>,
}

impl FilesSource {
    /// Resolves and validates the root directory.
    ///
    /// Checked here, at bind time, so that `SELECT * FROM files('nope')` is an
    /// error before a single byte reaches the client.
    pub fn open(root: PathBuf, cache: Option<Arc<FileIndexCache>>) -> Result<Self> {
        let metadata = fs::metadata(&root).map_err(|err| {
            ZqlError::new(
                SqlState::IoError,
                format!("cannot read {}: {err}", root.display()),
            )
        })?;

        if !metadata.is_dir() {
            return Err(ZqlError::new(
                SqlState::IoError,
                format!("{} is not a directory", root.display()),
            ));
        }

        Ok(FilesSource {
            root,
            cache,
            schema: Schema::new(vec![
                Column::new("path", Type::Text),
                Column::new("name", Type::Text),
                Column::new("ext", Type::Text),
                Column::new("size", Type::Int),
                Column::new("modified", Type::Timestamp),
                Column::new("is_dir", Type::Bool),
                Column::new("depth", Type::Int),
            ]),
        })
    }
}

impl TableSource for FilesSource {
    fn schema(&self) -> &Schema {
        &self.schema
    }

    fn scan(&self, _cancel: &CancelFlag) -> Result<Box<dyn RowIter>> {
        let build = || {
            let mut rows = Vec::new();
            walk(&self.root, 0, &mut rows)?;
            Ok(rows)
        };

        match &self.cache {
            Some(cache) => Ok(Box::new(CachedIter::new(
                cache.get_or_build(&self.root, build)?,
            ))),
            None => Ok(Box::new(ValuesIter::new(build()?))),
        }
    }
}

/// Streams rows out of a shared, cached walk.
///
/// Each row is cloned on its way out because `RowIter` yields owned rows. That
/// is a handful of small allocations per row against a filesystem walk saved,
/// which is not a close trade — and keeping the rows owned is what lets an
/// operator hold onto one without borrowing the whole index.
struct CachedIter {
    rows: Arc<Vec<Row>>,
    index: usize,
}

impl CachedIter {
    fn new(rows: Arc<Vec<Row>>) -> Self {
        CachedIter { rows, index: 0 }
    }
}

impl RowIter for CachedIter {
    fn next(&mut self) -> Result<Option<Row>> {
        let row = self.rows.get(self.index).cloned();
        if row.is_some() {
            self.index += 1;
        }
        Ok(row)
    }
}

/// Depth-first walk, breadth-first within a directory.
///
/// Written as an explicit recursion with a depth cap rather than a worklist
/// because the shape matches the tree and the cap makes the stack bounded.
fn walk(directory: &Path, depth: i64, rows: &mut Vec<Row>) -> Result<()> {
    /// Deep enough for any real tree, shallow enough that the recursion cannot
    /// exhaust the stack. Paths on Windows top out well before this anyway.
    const MAX_DEPTH: i64 = 256;

    if depth > MAX_DEPTH {
        return Ok(());
    }

    // An unreadable directory costs its subtree and nothing else. The root is
    // different: it was validated at bind time, so a failure there is real.
    let Ok(entries) = fs::read_dir(directory) else {
        return Ok(());
    };

    for entry in entries.flatten() {
        if rows.len() >= MAX_ENTRIES {
            return Err(ZqlError::new(
                SqlState::ProgramLimitExceeded,
                format!("the walk exceeded {MAX_ENTRIES} entries"),
            )
            .with_hint("point files() at a narrower directory"));
        }

        // `symlink_metadata` describes the link itself. Following it would let
        // a link to an ancestor loop forever.
        let Ok(metadata) = entry.path().symlink_metadata() else {
            continue;
        };

        rows.push(row_for(&entry, &metadata, depth));

        if metadata.is_dir() {
            walk(&entry.path(), depth + 1, rows)?;
        }
    }

    Ok(())
}

fn row_for(entry: &DirEntry, metadata: &Metadata, depth: i64) -> Row {
    let path = entry.path();
    let name = entry.file_name().to_string_lossy().into_owned();

    // The extension without its dot, lower-cased, so `WHERE ext = 'rs'` reads
    // the way people write it. A dotfile has no extension: `.gitignore` is a
    // name, not an extension, and `Path::extension` already agrees.
    let ext = path
        .extension()
        .map(|ext| Value::Text(ext.to_string_lossy().to_lowercase()))
        .unwrap_or(Value::Null);

    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| match time.duration_since(UNIX_EPOCH) {
            Ok(elapsed) => i64::try_from(elapsed.as_secs()).ok(),
            // Before 1970 — rare, but a real value rather than an error.
            Err(err) => i64::try_from(err.duration().as_secs()).ok().map(|s| -s),
        })
        .map(Value::Timestamp)
        .unwrap_or(Value::Null);

    Row::new(vec![
        Value::Text(path.to_string_lossy().into_owned()),
        Value::Text(name),
        ext,
        Value::Int(i64::try_from(metadata.len()).unwrap_or(i64::MAX)),
        modified,
        Value::Bool(metadata.is_dir()),
        Value::Int(depth),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::collect;

    /// `FilesSource` is not `Debug`, so `unwrap_err` is unavailable.
    fn expect_open_error(root: PathBuf) -> ZqlError {
        match FilesSource::open(root, None) {
            Err(error) => error,
            Ok(_) => panic!("expected the open to fail"),
        }
    }

    fn scan_rows(root: &Path) -> Vec<Row> {
        let source = FilesSource::open(root.to_path_buf(), None).unwrap();
        let flag: CancelFlag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut scan = source.scan(&flag).unwrap();
        collect(scan.as_mut()).unwrap()
    }

    #[test]
    fn a_missing_directory_fails_at_open_not_during_the_scan() {
        let error = expect_open_error(PathBuf::from("no-such-directory-here"));
        assert_eq!(error.state, SqlState::IoError);
    }

    #[test]
    fn a_file_is_not_a_valid_root() {
        let error = expect_open_error(PathBuf::from("Cargo.toml"));
        assert_eq!(error.state, SqlState::IoError);
        assert!(error.message.contains("not a directory"));
    }

    #[test]
    fn walks_the_projects_own_source_tree() {
        let rows = scan_rows(Path::new("src"));
        assert!(rows.len() > 10, "expected the source tree, got {}", rows.len());

        let names: Vec<String> = rows
            .iter()
            .filter_map(|row| match &row.0[1] {
                Value::Text(name) => Some(name.clone()),
                _ => None,
            })
            .collect();
        assert!(names.iter().any(|name| name == "lib.rs"));

        // Nested files must appear, at a depth greater than zero.
        let nested = rows
            .iter()
            .find(|row| matches!(&row.0[1], Value::Text(name) if name == "oid.rs"))
            .expect("src/wire/oid.rs should be in the walk");
        assert!(matches!(nested.0[6], Value::Int(depth) if depth > 0));
    }

    #[test]
    fn extensions_are_lowercased_and_absent_for_directories() {
        let rows = scan_rows(Path::new("src"));

        let source_file = rows
            .iter()
            .find(|row| matches!(&row.0[1], Value::Text(name) if name == "lib.rs"))
            .expect("lib.rs");
        assert!(matches!(&source_file.0[2], Value::Text(ext) if ext == "rs"));
        assert!(matches!(source_file.0[5], Value::Bool(false)));

        let directory = rows
            .iter()
            .find(|row| matches!(&row.0[1], Value::Text(name) if name == "wire"))
            .expect("the wire module directory");
        assert!(directory.0[2].is_null(), "a directory has no extension");
        assert!(matches!(directory.0[5], Value::Bool(true)));
    }

    #[test]
    fn every_row_has_the_full_column_set() {
        for row in scan_rows(Path::new("src")) {
            assert_eq!(row.len(), 7);
        }
    }
}

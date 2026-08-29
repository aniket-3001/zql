//! The filesystem index, cached for the life of a session.
//!
//! Walking a large tree costs seconds; the spike measured 127,490 entries in
//! roughly five. Doing that again for every query is fine as proof and poor as
//! a tool — the second query should feel instant, and in a demo it is the
//! difference between "it works" and "I would install this".
//!
//! # Why per session, and not per process
//!
//! A cache is a promise that the answer has not changed, and a filesystem
//! changes. Scoping it to one session bounds the staleness to something the
//! user controls and understands: reconnect and you get a fresh view. A
//! process-wide cache would be faster still and would quietly serve a
//! week-old picture of the disk to a server that has been up for a week.
//!
//! `--no-cache` opts out entirely, for when the tree is being watched as it
//! changes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Instant;

use crate::error::Result;
use crate::value::Row;

/// One session's memory of the directories it has already walked.
///
/// Keyed by root path, because `files('src')` and `files('tests')` are
/// different walks that a single session may well do both of.
#[derive(Default)]
pub struct FileIndexCache {
    roots: RwLock<HashMap<PathBuf, Arc<Vec<Row>>>>,
    /// Counters, so the dashboard can report whether a query re-used the index
    /// or paid for it. Reporting "cached" without knowing would be a guess.
    hits: AtomicU64,
    builds: AtomicU64,
}

/// What the index did during one query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheActivity {
    pub hits: u64,
    pub builds: u64,
}

impl CacheActivity {
    /// The word the dashboard shows, or `None` if the query never touched the
    /// filesystem at all.
    pub fn describe(before: CacheActivity, after: CacheActivity) -> Option<&'static str> {
        if after.builds > before.builds {
            Some("indexed")
        } else if after.hits > before.hits {
            Some("cached")
        } else {
            None
        }
    }
}

impl FileIndexCache {
    pub fn new() -> Self {
        FileIndexCache::default()
    }

    /// Returns the walk for `root`, performing it only if this session has not
    /// already done so.
    ///
    /// An `RwLock` rather than a `Mutex` because the hit path — every query
    /// after the first — only reads, and concurrent readers should not queue
    /// behind each other.
    pub fn get_or_build(
        &self,
        root: &Path,
        build: impl FnOnce() -> Result<Vec<Row>>,
    ) -> Result<Arc<Vec<Row>>> {
        if let Some(rows) = self.read().get(root) {
            self.hits.fetch_add(1, Ordering::Relaxed);
            return Ok(Arc::clone(rows));
        }

        // The build runs outside the lock. Holding a write lock across a
        // multi-second filesystem walk would stall every other query in the
        // session, including ones for a different directory entirely.
        let started = Instant::now();
        let rows = Arc::new(build()?);
        self.builds.fetch_add(1, Ordering::Relaxed);
        eprintln!(
            "zql: indexed {} entries under {} in {} ms",
            rows.len(),
            root.display(),
            started.elapsed().as_millis()
        );

        // Two queries racing on the same new root both walk it, and the loser's
        // work is discarded. That is a rare, bounded waste; the alternative is
        // holding the lock across the walk, which is a common, unbounded stall.
        let mut roots = self.write();
        Ok(Arc::clone(
            roots.entry(root.to_path_buf()).or_insert(rows),
        ))
    }

    /// Whether this root has already been walked.
    pub fn is_cached(&self, root: &Path) -> bool {
        self.read().contains_key(root)
    }

    /// A snapshot of the counters, taken either side of a query.
    pub fn activity(&self) -> CacheActivity {
        CacheActivity {
            hits: self.hits.load(Ordering::Relaxed),
            builds: self.builds.load(Ordering::Relaxed),
        }
    }

    /// A poisoned lock means another thread panicked while holding it. The map
    /// holds plain data that a panic cannot leave inconsistent, so recovering
    /// beats refusing to serve cached rows for the rest of the session.
    fn read(&self) -> std::sync::RwLockReadGuard<'_, HashMap<PathBuf, Arc<Vec<Row>>>> {
        self.roots
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, HashMap<PathBuf, Arc<Vec<Row>>>> {
        self.roots
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::Value;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn rows(count: usize) -> Vec<Row> {
        (0..count)
            .map(|n| Row::new(vec![Value::Int(n as i64)]))
            .collect()
    }

    #[test]
    fn the_second_request_does_not_walk_again() {
        let cache = FileIndexCache::new();
        let walks = AtomicUsize::new(0);

        // Two separate closures, because `get_or_build` takes `FnOnce`; both
        // increment the same counter, so a second walk would be visible.
        let build = || {
            walks.fetch_add(1, Ordering::SeqCst);
            Ok(rows(3))
        };
        let first = cache.get_or_build(Path::new("a"), build).unwrap();

        let build_again = || {
            walks.fetch_add(1, Ordering::SeqCst);
            Ok(rows(3))
        };
        let second = cache.get_or_build(Path::new("a"), build_again).unwrap();

        assert_eq!(walks.load(Ordering::SeqCst), 1, "the tree was walked twice");
        assert_eq!(first.len(), 3);
        assert!(Arc::ptr_eq(&first, &second), "a second copy was handed out");
    }

    #[test]
    fn different_roots_are_cached_separately() {
        let cache = FileIndexCache::new();
        let a = cache.get_or_build(Path::new("a"), || Ok(rows(1))).unwrap();
        let b = cache.get_or_build(Path::new("b"), || Ok(rows(2))).unwrap();

        assert_eq!(a.len(), 1);
        assert_eq!(b.len(), 2);
        assert!(cache.is_cached(Path::new("a")));
        assert!(!cache.is_cached(Path::new("c")));
    }

    #[test]
    fn a_failed_walk_is_not_cached() {
        let cache = FileIndexCache::new();
        let failed = cache.get_or_build(Path::new("a"), || {
            Err(crate::error::ZqlError::internal("walk failed"))
        });

        assert!(failed.is_err());
        assert!(
            !cache.is_cached(Path::new("a")),
            "a failure must not be remembered as an empty directory"
        );
    }
}

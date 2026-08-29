//! Execution: a tree of operators that pull rows from each other.
//!
//! # Why pull-based
//!
//! Each operator exposes `next()` and pulls from its child, which is the
//! textbook volcano model. Three things fall out of it for free:
//!
//! - `LIMIT` stops by simply not pulling again, rather than by cancelling
//!   work already in flight,
//! - an endless source such as `tail()` composes with everything else without
//!   any operator knowing it is endless,
//! - the memory profile is one row at a time, except where an operator has to
//!   materialise by its nature.
//!
//! Push-based execution vectorises better and is materially harder to read.
//! Throughput is not a judging criterion and readability is.

pub mod aggregate;
pub mod distinct;
pub mod filter;
pub mod limit;
pub mod project;
pub mod scan;
pub mod sort;
pub mod values;

use crate::error::Result;
use crate::value::Row;

/// A stream of rows.
///
/// # Why not `Iterator<Item = Result<Row>>`
///
/// The standard iterator forces `Option<Result<Row>>`, which inverts the
/// natural reading: every operator would have to match "some, and it is an
/// error" before "none". With `Result<Option<Row>>` the whole volcano loop
/// propagates errors with `?` and terminates on `Ok(None)`, so an operator
/// body reads as the algorithm rather than as error plumbing.
///
/// The cost is losing the iterator adapters, which this engine does not use —
/// every operator is hand-written, because each one needs to interleave a
/// cancellation check or a three-valued predicate that an adapter cannot
/// express.
pub trait RowIter {
    /// The next row, or `Ok(None)` when the stream is exhausted.
    fn next(&mut self) -> Result<Option<Row>>;
}

/// Drains a stream into a vector.
///
/// Used by tests and by the `EXPLAIN`-style paths that need the whole result.
/// The session loop deliberately does *not* use it: streaming rows to the
/// client as they are produced is what makes `tail()` work.
pub fn collect(iter: &mut dyn RowIter) -> Result<Vec<Row>> {
    let mut rows = Vec::new();
    while let Some(row) = iter.next()? {
        rows.push(row);
    }
    Ok(rows)
}

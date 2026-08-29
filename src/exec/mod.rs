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
pub mod join;
pub mod limit;
pub mod project;
pub mod scan;
pub mod sort;
pub mod values;

use std::sync::Arc;

use crate::error::Result;
use crate::plan::plan::Plan;
use crate::server::cancel::CancelFlag;
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
/// Turns a plan into a running operator tree.
///
/// The cancel flag is threaded to the leaves rather than checked here: a
/// filter that rejects a million rows never yields to its caller, so a check
/// at the top of the tree alone would leave a long scan uninterruptible.
pub fn execute(plan: Plan, cancel: CancelFlag) -> Result<Box<dyn RowIter>> {
    Ok(match plan {
        Plan::Values { rows, .. } => Box::new(values::ValuesIter::new(rows)),

        Plan::Scan { source, .. } => {
            let rows = source.scan(&cancel)?;
            Box::new(scan::ScanIter::new(rows, cancel))
        }

        Plan::Filter { input, predicate } => {
            let input = execute(*input, cancel)?;
            Box::new(filter::FilterIter::new(input, predicate))
        }

        Plan::Project { input, exprs, .. } => {
            let input = execute(*input, cancel)?;
            Box::new(project::ProjectIter::new(input, exprs))
        }

        Plan::Limit {
            input,
            limit,
            offset,
        } => {
            let input = execute(*input, cancel)?;
            Box::new(limit::LimitIter::new(input, limit, offset))
        }

        Plan::Aggregate {
            input,
            keys,
            aggregates,
            ..
        } => {
            let input = execute(*input, cancel)?;
            Box::new(aggregate::AggregateIter::new(input, keys, aggregates))
        }

        Plan::Sort { input, keys } => {
            let input = execute(*input, cancel)?;
            Box::new(sort::SortIter::new(input, keys))
        }

        Plan::Distinct { input } => {
            let input = execute(*input, cancel)?;
            Box::new(distinct::DistinctIter::new(input))
        }

        Plan::Join {
            left,
            right,
            condition,
            kind,
            ..
        } => {
            let right_width = right.schema().len();
            let left = execute(*left, Arc::clone(&cancel))?;
            let right = execute(*right, cancel)?;
            Box::new(join::JoinIter::new(left, right, condition, kind, right_width))
        }
    })
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

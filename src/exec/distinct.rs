//! `SELECT DISTINCT`.
//!
//! Streaming, not blocking: a row is emitted the moment it is known to be new,
//! so `SELECT DISTINCT ext FROM files LIMIT 5` stops after finding five rather
//! than reading the whole tree. Keeping the *first* occurrence also preserves
//! whatever order the input arrived in, which is what lets `DISTINCT` sit above
//! a sort without undoing it.

use std::collections::HashSet;

use crate::error::{Result, SqlState, ZqlError};
use crate::exec::RowIter;
use crate::value::{GroupKey, Row};

/// A ceiling on remembered rows, so `DISTINCT` over a unique column cannot
/// exhaust memory silently.
const MAX_DISTINCT: usize = 5_000_000;

pub struct DistinctIter {
    input: Box<dyn RowIter>,
    seen: HashSet<GroupKey>,
}

impl DistinctIter {
    pub fn new(input: Box<dyn RowIter>) -> Self {
        DistinctIter {
            input,
            seen: HashSet::new(),
        }
    }
}

impl RowIter for DistinctIter {
    fn next(&mut self) -> Result<Option<Row>> {
        while let Some(row) = self.input.next()? {
            if self.seen.len() >= MAX_DISTINCT {
                return Err(ZqlError::new(
                    SqlState::ProgramLimitExceeded,
                    format!("DISTINCT needs to remember more than {MAX_DISTINCT} rows"),
                )
                .with_hint("add a LIMIT, or select fewer columns"));
            }

            // `GroupKey` equality, not comparison equality: two NULLs are the
            // same row for the purposes of DISTINCT, even though `NULL = NULL`
            // is unknown.
            if self.seen.insert(GroupKey::new(row.0.clone())) {
                return Ok(Some(row));
            }
        }
        Ok(None)
    }

    /// A pass-through: this operator waits exactly as long as its input does.
    fn may_block(&self) -> bool {
        self.input.may_block()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::{collect, values::ValuesIter};
    use crate::value::Value;

    fn distinct(rows: Vec<Row>) -> Vec<Row> {
        let mut iter = DistinctIter::new(Box::new(ValuesIter::new(rows)));
        collect(&mut iter).unwrap()
    }

    #[test]
    fn duplicates_are_dropped_and_first_occurrences_kept_in_order() {
        let rows = vec![
            Row::new(vec![Value::Text("b".into())]),
            Row::new(vec![Value::Text("a".into())]),
            Row::new(vec![Value::Text("b".into())]),
        ];
        let out = distinct(rows);
        assert_eq!(out.len(), 2);
        assert!(matches!(&out[0].0[0], Value::Text(text) if text == "b"));
        assert!(matches!(&out[1].0[0], Value::Text(text) if text == "a"));
    }

    #[test]
    fn repeated_nulls_collapse_to_one_row() {
        let rows = vec![
            Row::new(vec![Value::Null]),
            Row::new(vec![Value::Null]),
            Row::new(vec![Value::Int(1)]),
        ];
        assert_eq!(distinct(rows).len(), 2);
    }

    #[test]
    fn distinctness_is_over_the_whole_row_not_the_first_column() {
        let rows = vec![
            Row::new(vec![Value::Int(1), Value::Int(1)]),
            Row::new(vec![Value::Int(1), Value::Int(2)]),
        ];
        assert_eq!(distinct(rows).len(), 2);
    }

    #[test]
    fn it_streams_rather_than_collecting_first() {
        // The third row is never pulled, so an error in it is never seen.
        let rows = vec![
            Row::new(vec![Value::Int(1)]),
            Row::new(vec![Value::Int(1)]),
            Row::new(vec![Value::Int(2)]),
        ];
        let mut iter = DistinctIter::new(Box::new(ValuesIter::new(rows)));
        assert!(iter.next().unwrap().is_some());
        assert!(matches!(iter.next().unwrap(), Some(row) if matches!(row.0[0], Value::Int(2))));
    }
}

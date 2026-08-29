//! A stream over rows that are already in memory.
//!
//! Backs `SELECT 1` — a query with no `FROM`, which is how most clients test
//! that a connection is alive — and serves as the input in operator tests.

use crate::error::Result;
use crate::exec::RowIter;
use crate::value::Row;

pub struct ValuesIter {
    rows: std::vec::IntoIter<Row>,
}

impl ValuesIter {
    pub fn new(rows: Vec<Row>) -> Self {
        ValuesIter {
            rows: rows.into_iter(),
        }
    }
}

impl RowIter for ValuesIter {
    fn next(&mut self) -> Result<Option<Row>> {
        Ok(self.rows.next())
    }
}

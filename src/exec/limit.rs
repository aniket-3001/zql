//! `LIMIT` and `OFFSET`.
//!
//! In a pull-based tree this operator is almost free: it stops asking. Nothing
//! downstream needs to know it happened, and no work is started and then thrown
//! away — which is why `LIMIT 10` over an endless `tail()` terminates rather
//! than running forever.

use crate::error::Result;
use crate::exec::RowIter;
use crate::value::Row;

pub struct LimitIter {
    input: Box<dyn RowIter>,
    /// `None` means `OFFSET` with no `LIMIT`.
    limit: Option<u64>,
    /// Rows still to be discarded before the first one is returned.
    remaining_offset: u64,
    emitted: u64,
}

impl LimitIter {
    pub fn new(input: Box<dyn RowIter>, limit: Option<u64>, offset: u64) -> Self {
        LimitIter {
            input,
            limit,
            remaining_offset: offset,
            emitted: 0,
        }
    }
}

impl RowIter for LimitIter {
    fn next(&mut self) -> Result<Option<Row>> {
        if self.limit == Some(self.emitted) {
            return Ok(None);
        }

        // Skipped rows still have to be produced — there is no index to seek
        // with — but they are dropped here rather than travelling further up.
        while self.remaining_offset > 0 {
            if self.input.next()?.is_none() {
                return Ok(None);
            }
            self.remaining_offset -= 1;
        }

        let row = self.input.next()?;
        if row.is_some() {
            self.emitted += 1;
        }
        Ok(row)
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

    fn source(count: i64) -> Box<dyn RowIter> {
        Box::new(ValuesIter::new(
            (0..count).map(|n| Row::new(vec![Value::Int(n)])).collect(),
        ))
    }

    fn first_values(iter: &mut dyn RowIter) -> Vec<i64> {
        collect(iter)
            .unwrap()
            .iter()
            .map(|row| match row.0[0] {
                Value::Int(n) => n,
                _ => panic!("expected an integer"),
            })
            .collect()
    }

    #[test]
    fn limit_stops_early() {
        let mut limit = LimitIter::new(source(100), Some(3), 0);
        assert_eq!(first_values(&mut limit), vec![0, 1, 2]);
    }

    #[test]
    fn offset_skips_then_limit_applies() {
        let mut limit = LimitIter::new(source(10), Some(3), 5);
        assert_eq!(first_values(&mut limit), vec![5, 6, 7]);
    }

    #[test]
    fn offset_beyond_the_end_yields_nothing() {
        let mut limit = LimitIter::new(source(3), None, 10);
        assert!(first_values(&mut limit).is_empty());
    }

    #[test]
    fn limit_zero_returns_nothing_and_pulls_nothing() {
        let mut limit = LimitIter::new(source(10), Some(0), 0);
        assert!(first_values(&mut limit).is_empty());
    }

    #[test]
    fn offset_with_no_limit_returns_the_rest() {
        let mut limit = LimitIter::new(source(5), None, 2);
        assert_eq!(first_values(&mut limit), vec![2, 3, 4]);
    }
}

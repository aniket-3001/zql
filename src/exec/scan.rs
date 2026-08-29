//! The leaf operator: rows out of a table source.
//!
//! This is where cancellation is enforced, because it is the one operator every
//! plan has and the one that can run for a long time without yielding anything
//! upward.

use std::sync::atomic::Ordering;

use crate::error::{Result, ZqlError};
use crate::exec::RowIter;
use crate::server::cancel::CancelFlag;
use crate::value::Row;

pub struct ScanIter {
    source: Box<dyn RowIter>,
    cancel: CancelFlag,
}

impl ScanIter {
    pub fn new(source: Box<dyn RowIter>, cancel: CancelFlag) -> Self {
        ScanIter { source, cancel }
    }
}

impl RowIter for ScanIter {
    fn next(&mut self) -> Result<Option<Row>> {
        // Between rows, never inside one. A cancellation that lands halfway
        // through writing a `DataRow` desynchronises the stream and the client
        // never recovers — so the check belongs here, at the top of the loop,
        // and not anywhere that a row is partly built.
        if self.cancel.load(Ordering::SeqCst) {
            return Err(ZqlError::canceled());
        }
        self.source.next()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::values::ValuesIter;
    use crate::value::Value;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    fn rows(count: i64) -> Box<dyn RowIter> {
        Box::new(ValuesIter::new(
            (0..count).map(|n| Row::new(vec![Value::Int(n)])).collect(),
        ))
    }

    #[test]
    fn passes_rows_through_untouched() {
        let flag = Arc::new(AtomicBool::new(false));
        let mut scan = ScanIter::new(rows(3), flag);
        assert_eq!(crate::exec::collect(&mut scan).unwrap().len(), 3);
    }

    #[test]
    fn a_set_flag_stops_the_scan_with_57014() {
        let flag = Arc::new(AtomicBool::new(false));
        let mut scan = ScanIter::new(rows(1000), Arc::clone(&flag));

        assert!(scan.next().unwrap().is_some());
        flag.store(true, Ordering::SeqCst);

        let error = scan.next().unwrap_err();
        assert_eq!(error.state, crate::error::SqlState::QueryCanceled);
    }
}

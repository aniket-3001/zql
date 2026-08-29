//! `WHERE` and `HAVING`.

use crate::error::Result;
use crate::exec::RowIter;
use crate::plan::expr::CompiledExpr;
use crate::value::Row;

pub struct FilterIter {
    input: Box<dyn RowIter>,
    predicate: CompiledExpr,
}

impl FilterIter {
    pub fn new(input: Box<dyn RowIter>, predicate: CompiledExpr) -> Self {
        FilterIter { input, predicate }
    }
}

impl RowIter for FilterIter {
    fn next(&mut self) -> Result<Option<Row>> {
        while let Some(row) = self.input.next()? {
            // Three-valued logic in one place: a row survives only when the
            // predicate is *exactly* true. `NULL` — "unknown" — excludes it,
            // identically to `FALSE`; the difference between those two only
            // becomes visible under `NOT`, which the evaluator handles.
            //
            // `as_bool` rather than `is_true` so that `WHERE size` is a type
            // error instead of a filter that silently matches nothing. A
            // predicate that is not boolean is a mistake, not an empty result.
            if self.predicate.eval(&row)?.as_bool()? == Some(true) {
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

    fn input() -> Box<dyn RowIter> {
        Box::new(ValuesIter::new(vec![
            Row::new(vec![Value::Int(1)]),
            Row::new(vec![Value::Null]),
            Row::new(vec![Value::Int(2)]),
        ]))
    }

    #[test]
    fn a_null_predicate_excludes_the_row_just_as_false_does() {
        // `WHERE col > 1` over [1, NULL, 2] keeps only 2: the NULL row
        // evaluates to NULL, which is not true.
        use crate::plan::expr::CompiledExpr as E;
        use crate::sql::ast::BinaryOp;

        let predicate = E::Binary {
            op: BinaryOp::Gt,
            left: Box::new(E::Column(0)),
            right: Box::new(E::Literal(Value::Int(1))),
        };

        let mut filter = FilterIter::new(input(), predicate);
        let rows = collect(&mut filter).unwrap();
        assert_eq!(rows.len(), 1);
        assert!(matches!(rows[0].0[0], Value::Int(2)));
    }

    #[test]
    fn a_non_boolean_predicate_is_a_type_error_not_a_coercion() {
        use crate::plan::expr::CompiledExpr as E;
        let mut filter = FilterIter::new(input(), E::Column(0));
        // `WHERE 1` is not `WHERE true`.
        assert!(filter.next().is_err());
    }
}

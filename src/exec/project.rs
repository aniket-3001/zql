//! The `SELECT` list: input rows in, output rows out.

use crate::error::Result;
use crate::exec::RowIter;
use crate::plan::expr::CompiledExpr;
use crate::value::Row;

pub struct ProjectIter {
    input: Box<dyn RowIter>,
    exprs: Vec<CompiledExpr>,
}

impl ProjectIter {
    pub fn new(input: Box<dyn RowIter>, exprs: Vec<CompiledExpr>) -> Self {
        ProjectIter { input, exprs }
    }
}

impl RowIter for ProjectIter {
    fn next(&mut self) -> Result<Option<Row>> {
        let Some(row) = self.input.next()? else {
            return Ok(None);
        };

        let mut values = Vec::with_capacity(self.exprs.len());
        for expr in &self.exprs {
            values.push(expr.eval(&row)?);
        }
        Ok(Some(Row::new(values)))
    }
}

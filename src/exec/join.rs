//! `INNER JOIN` and `LEFT JOIN`.
//!
//! This is the feature the whole tool is *for*: one query language over unlike
//! things, so a SQLite table can be joined against a CSV against the
//! filesystem. Everything else zql does, something else already does better.
//!
//! # Nested loop, and why
//!
//! The right side is materialised once and scanned for each left row. A hash
//! join would be asymptotically better, but it only applies to equality
//! predicates and would need the binder to recognise them, split the condition,
//! and fall back to this anyway for everything else. At the sizes a person
//! inspects interactively the constant factors dominate, and `ON` conditions
//! here are frequently not plain equality — `ON f.name = m.filename` is, but
//! `ON f.path LIKE m.prefix || '%'` is the kind of thing this tool exists to
//! make easy. The cost is stated in the README rather than hidden.
//!
//! The left side is **not** materialised, so it streams.

use crate::error::{Result, SqlState, ZqlError};
use crate::exec::RowIter;
use crate::plan::expr::CompiledExpr;
use crate::sql::ast::JoinKind;
use crate::value::{Row, Value};

/// A ceiling on the materialised side.
const MAX_RIGHT_ROWS: usize = 2_000_000;

pub struct JoinIter {
    left: Box<dyn RowIter>,
    right: Option<Box<dyn RowIter>>,
    right_rows: Vec<Row>,
    right_width: usize,
    condition: CompiledExpr,
    kind: JoinKind,

    /// The left row currently being matched against the right side.
    current: Option<Row>,
    next_right: usize,
    matched: bool,
}

impl JoinIter {
    pub fn new(
        left: Box<dyn RowIter>,
        right: Box<dyn RowIter>,
        condition: CompiledExpr,
        kind: JoinKind,
        right_width: usize,
    ) -> Self {
        JoinIter {
            left,
            right: Some(right),
            right_rows: Vec::new(),
            right_width,
            condition,
            kind,
            current: None,
            next_right: 0,
            matched: false,
        }
    }

    fn materialise_right(&mut self) -> Result<()> {
        let Some(mut right) = self.right.take() else {
            return Ok(());
        };
        while let Some(row) = right.next()? {
            if self.right_rows.len() >= MAX_RIGHT_ROWS {
                return Err(ZqlError::new(
                    SqlState::ProgramLimitExceeded,
                    format!("the right side of the join exceeded {MAX_RIGHT_ROWS} rows"),
                )
                .with_hint("filter the joined source, or swap the two sides"));
            }
            self.right_rows.push(row);
        }
        Ok(())
    }

    /// Left columns then right columns — the layout the binder resolved every
    /// column index against.
    fn combine(left: &Row, right: &Row) -> Row {
        let mut values = Vec::with_capacity(left.len() + right.len());
        values.extend_from_slice(&left.0);
        values.extend_from_slice(&right.0);
        Row::new(values)
    }

    fn combine_with_nulls(&self, left: &Row) -> Row {
        let mut values = Vec::with_capacity(left.len() + self.right_width);
        values.extend_from_slice(&left.0);
        values.extend(std::iter::repeat_n(Value::Null, self.right_width));
        Row::new(values)
    }
}

impl RowIter for JoinIter {
    fn next(&mut self) -> Result<Option<Row>> {
        if self.right.is_some() {
            self.materialise_right()?;
        }

        loop {
            let Some(left) = self.current.clone() else {
                // Advance to the next left row and restart the inner scan.
                match self.left.next()? {
                    Some(row) => {
                        self.current = Some(row);
                        self.next_right = 0;
                        self.matched = false;
                        continue;
                    }
                    None => return Ok(None),
                }
            };

            while self.next_right < self.right_rows.len() {
                let right = &self.right_rows[self.next_right];
                self.next_right += 1;

                let combined = Self::combine(&left, right);
                // The `ON` condition is three-valued like any other predicate:
                // only exactly-true is a match, so a NULL on either side does
                // not join.
                if self.condition.eval(&combined)?.as_bool()? == Some(true) {
                    self.matched = true;
                    return Ok(Some(combined));
                }
            }

            // The right side is exhausted for this left row.
            self.current = None;
            if self.kind == JoinKind::Left && !self.matched {
                return Ok(Some(self.combine_with_nulls(&left)));
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::{collect, values::ValuesIter};
    use crate::sql::ast::BinaryOp;

    fn rows(values: &[i64]) -> Box<dyn RowIter> {
        Box::new(ValuesIter::new(
            values
                .iter()
                .map(|n| Row::new(vec![Value::Int(*n)]))
                .collect(),
        ))
    }

    /// `ON left.0 = right.0`, with the right side at index 1 of the combined row.
    fn equality() -> CompiledExpr {
        CompiledExpr::Binary {
            op: BinaryOp::Eq,
            left: Box::new(CompiledExpr::Column(0)),
            right: Box::new(CompiledExpr::Column(1)),
        }
    }

    fn join(left: &[i64], right: &[i64], kind: JoinKind) -> Vec<Row> {
        let mut iter = JoinIter::new(rows(left), rows(right), equality(), kind, 1);
        collect(&mut iter).unwrap()
    }

    #[test]
    fn an_inner_join_keeps_only_matching_pairs() {
        let out = join(&[1, 2, 3], &[2, 3, 4], JoinKind::Inner);
        assert_eq!(out.len(), 2);
        assert!(matches!(out[0].0[0], Value::Int(2)));
        assert!(matches!(out[1].0[0], Value::Int(3)));
    }

    #[test]
    fn a_left_join_keeps_unmatched_left_rows_padded_with_nulls() {
        let out = join(&[1, 2], &[2], JoinKind::Left);
        assert_eq!(out.len(), 2);

        // Row 1 has no partner: the right column is NULL, not missing.
        assert!(matches!(out[0].0[0], Value::Int(1)));
        assert!(out[0].0[1].is_null());
        assert_eq!(out[0].len(), 2, "the row must keep its full width");

        assert!(matches!(out[1].0[1], Value::Int(2)));
    }

    #[test]
    fn a_left_row_matching_several_right_rows_yields_one_output_each() {
        let out = join(&[1], &[1, 1, 1], JoinKind::Inner);
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn an_empty_right_side_drops_inner_rows_and_keeps_left_ones() {
        assert!(join(&[1, 2], &[], JoinKind::Inner).is_empty());

        let out = join(&[1, 2], &[], JoinKind::Left);
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|row| row.0[1].is_null()));
    }

    #[test]
    fn an_empty_left_side_yields_nothing_either_way() {
        assert!(join(&[], &[1], JoinKind::Inner).is_empty());
        assert!(join(&[], &[1], JoinKind::Left).is_empty());
    }

    #[test]
    fn a_null_never_joins_because_the_condition_is_unknown_not_true() {
        let left: Box<dyn RowIter> =
            Box::new(ValuesIter::new(vec![Row::new(vec![Value::Null])]));
        let right: Box<dyn RowIter> =
            Box::new(ValuesIter::new(vec![Row::new(vec![Value::Null])]));

        let mut inner = JoinIter::new(left, right, equality(), JoinKind::Inner, 1);
        assert!(
            collect(&mut inner).unwrap().is_empty(),
            "NULL = NULL is unknown, so it must not join"
        );
    }
}

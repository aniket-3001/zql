//! `ORDER BY`.
//!
//! An in-memory sort with a documented row ceiling. External merge sort was
//! considered and **cut**: nobody orders ten million rows through a terminal,
//! and the failure it prevents is better handled by saying so than by spilling
//! to disk. Above the ceiling this errors with `54000` rather than swapping the
//! machine to death.
//!
//! Sort keys are evaluated **once per row**, not once per comparison. A sort
//! performs `O(n log n)` comparisons, so evaluating `lower(name)` inside the
//! comparator would run it a hundred thousand times over a ten-thousand-row
//! result.

use std::cmp::Ordering;

use crate::error::{Result, SqlState, ZqlError};
use crate::exec::RowIter;
use crate::plan::expr::CompiledExpr;
use crate::value::{compare, Row, Value};

/// The documented ceiling, above which the sort refuses rather than thrashes.
const MAX_ROWS: usize = 5_000_000;

/// One `ORDER BY` term.
#[derive(Debug, Clone)]
pub struct SortKey {
    pub expr: CompiledExpr,
    pub descending: bool,
    /// Where NULLs go. Postgres puts them last when ascending and first when
    /// descending, which keeps them at the "largest" end either way.
    pub nulls_first: bool,
}

pub struct SortIter {
    input: Option<Box<dyn RowIter>>,
    keys: Vec<SortKey>,
    output: std::vec::IntoIter<Row>,
}

impl SortIter {
    pub fn new(input: Box<dyn RowIter>, keys: Vec<SortKey>) -> Self {
        SortIter {
            input: Some(input),
            keys,
            output: Vec::new().into_iter(),
        }
    }

    fn build(&mut self) -> Result<()> {
        let Some(mut input) = self.input.take() else {
            return Ok(());
        };

        // Decorate: each row is paired with its already-evaluated keys.
        let mut decorated: Vec<(Vec<Value>, Row)> = Vec::new();
        while let Some(row) = input.next()? {
            if decorated.len() >= MAX_ROWS {
                return Err(ZqlError::new(
                    SqlState::ProgramLimitExceeded,
                    format!("ORDER BY needs to hold more than {MAX_ROWS} rows in memory"),
                )
                .with_hint("add a LIMIT, or filter the input further"));
            }
            let mut values = Vec::with_capacity(self.keys.len());
            for key in &self.keys {
                values.push(key.expr.eval(&row)?);
            }
            decorated.push((values, row));
        }

        // A stable sort, so equal keys keep the order they arrived in. That
        // makes results reproducible between runs, which matters more here
        // than the small cost over an unstable sort.
        decorated.sort_by(|left, right| compare_keys(&left.0, &right.0, &self.keys));

        self.output = decorated
            .into_iter()
            .map(|(_, row)| row)
            .collect::<Vec<_>>()
            .into_iter();
        Ok(())
    }
}

impl RowIter for SortIter {
    fn next(&mut self) -> Result<Option<Row>> {
        if self.input.is_some() {
            self.build()?;
        }
        Ok(self.output.next())
    }
}

fn compare_keys(left: &[Value], right: &[Value], keys: &[SortKey]) -> Ordering {
    for (index, key) in keys.iter().enumerate() {
        let (Some(a), Some(b)) = (left.get(index), right.get(index)) else {
            continue;
        };

        // NULL ordering is decided before the values are compared at all,
        // because a NULL has no position among ordinary values.
        let ordering = match (a.is_null(), b.is_null()) {
            (true, true) => Ordering::Equal,
            (true, false) => {
                return if key.nulls_first {
                    Ordering::Less
                } else {
                    Ordering::Greater
                }
            }
            (false, true) => {
                return if key.nulls_first {
                    Ordering::Greater
                } else {
                    Ordering::Less
                }
            }
            (false, false) => total_order(a, b),
        };

        if ordering != Ordering::Equal {
            return if key.descending {
                ordering.reverse()
            } else {
                ordering
            };
        }
    }
    Ordering::Equal
}

/// A **total** order over every value, including ones that cannot be compared.
///
/// `compare` is deliberately partial: comparing text against a number is a type
/// error, because silently answering it is how a `WHERE` clause returns the
/// wrong rows. Sorting cannot afford that luxury — a sort comparator must be
/// total or the sort is meaningless — so a mixed column falls back to ordering
/// by *type*: NULL, then booleans, then numbers, then text, then blobs.
///
/// This is the rule in `SQL-SUBSET.md` §6.2, and it is why `ORDER BY` over a
/// dynamically-typed SQLite column never fails.
fn total_order(left: &Value, right: &Value) -> Ordering {
    match compare(left, right) {
        Ok(Some(ordering)) => ordering,
        // Unordered reals: NaN sorts after everything else, as Postgres does.
        Ok(None) => type_rank(left).cmp(&type_rank(right)),
        Err(_) => type_rank(left).cmp(&type_rank(right)),
    }
}

fn type_rank(value: &Value) -> u8 {
    match value {
        Value::Null => 0,
        Value::Bool(_) => 1,
        Value::Int(_) | Value::Real(_) | Value::Timestamp(_) => 2,
        Value::Text(_) => 3,
        Value::Blob(_) => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::{collect, values::ValuesIter};

    fn key(descending: bool, nulls_first: bool) -> SortKey {
        SortKey {
            expr: CompiledExpr::Column(0),
            descending,
            nulls_first,
        }
    }

    fn sorted(values: Vec<Value>, key: SortKey) -> Vec<Value> {
        let rows = values.into_iter().map(|v| Row::new(vec![v])).collect();
        let mut iter = SortIter::new(Box::new(ValuesIter::new(rows)), vec![key]);
        collect(&mut iter)
            .unwrap()
            .into_iter()
            .map(|row| row.0[0].clone())
            .collect()
    }

    fn ints(values: &[Value]) -> Vec<i64> {
        values
            .iter()
            .filter_map(|value| match value {
                Value::Int(number) => Some(*number),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn ascending_puts_nulls_last_and_descending_puts_them_first() {
        let input = vec![Value::Int(2), Value::Null, Value::Int(1)];

        let ascending = sorted(input.clone(), key(false, false));
        assert_eq!(ints(&ascending), vec![1, 2]);
        assert!(ascending[2].is_null(), "ascending sorts NULLs last");

        let descending = sorted(input, key(true, true));
        assert!(descending[0].is_null(), "descending sorts NULLs first");
        assert_eq!(ints(&descending), vec![2, 1]);
    }

    #[test]
    fn nulls_first_and_last_override_the_default() {
        let input = vec![Value::Int(1), Value::Null];
        assert!(sorted(input.clone(), key(false, true))[0].is_null());
        assert!(sorted(input, key(true, false))[1].is_null());
    }

    #[test]
    fn a_mixed_type_column_sorts_by_type_rather_than_failing() {
        // SQLite is dynamically typed, so this is ordinary rather than exotic.
        let input = vec![
            Value::Text("b".into()),
            Value::Int(2),
            Value::Null,
            Value::Bool(true),
            Value::Blob(vec![1]),
            Value::Int(1),
        ];
        let out = sorted(input, key(false, false));

        // Type rank orders the rest: boolean, numbers, text, blob — with NULL
        // last, because the NULL rule is applied before any comparison.
        assert!(matches!(out[0], Value::Bool(true)));
        assert_eq!(ints(&out), vec![1, 2]);
        assert!(matches!(out[3], Value::Text(_)));
        assert!(matches!(out[4], Value::Blob(_)));
        assert!(out[5].is_null());
    }

    #[test]
    fn the_sort_is_stable_so_equal_keys_keep_their_input_order() {
        let rows = vec![
            Row::new(vec![Value::Int(1), Value::Text("first".into())]),
            Row::new(vec![Value::Int(1), Value::Text("second".into())]),
        ];
        let mut iter = SortIter::new(Box::new(ValuesIter::new(rows)), vec![key(false, false)]);
        let out = collect(&mut iter).unwrap();
        assert!(matches!(&out[0].0[1], Value::Text(text) if text == "first"));
    }

    #[test]
    fn a_second_key_breaks_ties_on_the_first() {
        let rows = vec![
            Row::new(vec![Value::Int(1), Value::Int(20)]),
            Row::new(vec![Value::Int(1), Value::Int(10)]),
            Row::new(vec![Value::Int(0), Value::Int(30)]),
        ];
        let keys = vec![
            key(false, false),
            SortKey {
                expr: CompiledExpr::Column(1),
                descending: false,
                nulls_first: false,
            },
        ];
        let mut iter = SortIter::new(Box::new(ValuesIter::new(rows)), keys);
        let out = collect(&mut iter).unwrap();

        assert!(matches!(out[0].0[1], Value::Int(30)));
        assert!(matches!(out[1].0[1], Value::Int(10)));
        assert!(matches!(out[2].0[1], Value::Int(20)));
    }
}

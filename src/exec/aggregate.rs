//! `GROUP BY` and the aggregate functions.
//!
//! A hash aggregate: one pass over the input, one accumulator set per group.
//! Sorting the input first and aggregating runs of equal keys would avoid the
//! hash map but costs an `O(n log n)` sort for a job that is `O(n)`.
//!
//! # Output order
//!
//! Groups come out in **first-seen order**, not hash order. SQL guarantees no
//! ordering without `ORDER BY`, and neither does Postgres — but a query that
//! returns the same rows in a different arrangement on every run is miserable
//! to test against and worse to demo. Insertion order costs one `Vec`.
//!
//! # The asymmetry worth knowing
//!
//! `COUNT` returns 0 over no input. Every other aggregate returns `NULL`. That
//! is standard, it is frequently got wrong, and it has a test.

use std::collections::HashMap;

use crate::error::{Result, SqlState, ZqlError};
use crate::exec::RowIter;
use crate::plan::expr::{AggFn, AggSpec, CompiledExpr};
use crate::value::{compare, GroupKey, Row, Value};

use std::cmp::Ordering;
use std::collections::HashSet;

/// A ceiling on distinct groups, so a `GROUP BY` over a unique column cannot
/// exhaust memory silently. Hitting it is a clear `54000`.
const MAX_GROUPS: usize = 5_000_000;

pub struct AggregateIter {
    input: Option<Box<dyn RowIter>>,
    keys: Vec<CompiledExpr>,
    specs: Vec<AggSpec>,
    /// Filled on the first call to `next`, then drained.
    output: std::vec::IntoIter<Row>,
}

impl AggregateIter {
    pub fn new(input: Box<dyn RowIter>, keys: Vec<CompiledExpr>, specs: Vec<AggSpec>) -> Self {
        AggregateIter {
            input: Some(input),
            keys,
            specs,
            output: Vec::new().into_iter(),
        }
    }

    /// Consumes the whole input and builds every group.
    ///
    /// Aggregation is a blocking operator by nature — the first row of output
    /// cannot be known until the last row of input has been seen — so this is
    /// the one place in the engine that deliberately materialises.
    fn build(&mut self) -> Result<()> {
        let Some(mut input) = self.input.take() else {
            return Ok(());
        };

        let mut lookup: HashMap<GroupKey, usize> = HashMap::new();
        let mut groups: Vec<(GroupKey, Vec<State>)> = Vec::new();

        while let Some(row) = input.next()? {
            let mut key_values = Vec::with_capacity(self.keys.len());
            for key in &self.keys {
                key_values.push(key.eval(&row)?);
            }
            let key = GroupKey::new(key_values);

            let index = match lookup.get(&key) {
                Some(index) => *index,
                None => {
                    if groups.len() >= MAX_GROUPS {
                        return Err(ZqlError::new(
                            SqlState::ProgramLimitExceeded,
                            format!("GROUP BY produced more than {MAX_GROUPS} groups"),
                        )
                        .with_hint("group by something coarser, or add a WHERE clause"));
                    }
                    let index = groups.len();
                    let states = self.specs.iter().map(State::new).collect();
                    groups.push((key.clone(), states));
                    lookup.insert(key, index);
                    index
                }
            };

            let (_, states) = &mut groups[index];
            for (state, spec) in states.iter_mut().zip(&self.specs) {
                state.accumulate(spec, &row)?;
            }
        }

        // `SELECT count(*) FROM t` over an empty table is one row reading 0,
        // not zero rows. With a GROUP BY there is nothing to group, so it is
        // genuinely empty.
        if groups.is_empty() && self.keys.is_empty() {
            let states: Vec<State> = self.specs.iter().map(State::new).collect();
            groups.push((GroupKey::new(Vec::new()), states));
        }

        let mut rows = Vec::with_capacity(groups.len());
        for (key, states) in groups {
            // Keys first, then aggregates: the layout the binder resolved
            // every projection and HAVING reference against.
            let mut values = key.0;
            for state in states {
                values.push(state.finish()?);
            }
            rows.push(Row::new(values));
        }

        self.output = rows.into_iter();
        Ok(())
    }
}

impl RowIter for AggregateIter {
    fn next(&mut self) -> Result<Option<Row>> {
        if self.input.is_some() {
            self.build()?;
        }
        Ok(self.output.next())
    }
}

/// One aggregate's running state within one group.
struct State {
    /// Values already seen, for `DISTINCT`. `None` when not distinct.
    seen: Option<HashSet<GroupKey>>,
    accumulator: Accumulator,
}

enum Accumulator {
    Count(i64),
    /// Integer sums stay exact until a real arrives; `overflowed` records that
    /// the integer path failed so the error surfaces at the end rather than
    /// being masked by a later value.
    Sum {
        integer: i64,
        real: f64,
        is_real: bool,
        any: bool,
        overflowed: bool,
    },
    Avg {
        total: f64,
        count: i64,
    },
    Min(Option<Value>),
    Max(Option<Value>),
}

impl State {
    fn new(spec: &AggSpec) -> State {
        State {
            seen: spec.distinct.then(HashSet::new),
            accumulator: match spec.function {
                AggFn::Count => Accumulator::Count(0),
                AggFn::Sum => Accumulator::Sum {
                    integer: 0,
                    real: 0.0,
                    is_real: false,
                    any: false,
                    overflowed: false,
                },
                AggFn::Avg => Accumulator::Avg {
                    total: 0.0,
                    count: 0,
                },
                AggFn::Min => Accumulator::Min(None),
                AggFn::Max => Accumulator::Max(None),
            },
        }
    }

    fn accumulate(&mut self, spec: &AggSpec, row: &Row) -> Result<()> {
        // `COUNT(*)` has no argument and counts the row itself, NULLs and all.
        let Some(argument) = &spec.argument else {
            if let Accumulator::Count(count) = &mut self.accumulator {
                *count += 1;
            }
            return Ok(());
        };

        let value = argument.eval(row)?;

        // Every aggregate skips NULLs — including `COUNT(expr)`, which is what
        // makes it differ from `COUNT(*)`.
        if value.is_null() {
            return Ok(());
        }

        if let Some(seen) = &mut self.seen {
            if !seen.insert(GroupKey::new(vec![value.clone()])) {
                return Ok(());
            }
        }

        match &mut self.accumulator {
            Accumulator::Count(count) => *count += 1,

            Accumulator::Sum {
                integer,
                real,
                is_real,
                any,
                overflowed,
            } => {
                *any = true;
                match value {
                    Value::Int(number) if !*is_real => match integer.checked_add(number) {
                        Some(total) => *integer = total,
                        None => *overflowed = true,
                    },
                    Value::Int(number) => *real += number as f64,
                    Value::Real(number) => {
                        // The first real value converts what has accumulated so
                        // far and everything after it stays in floating point.
                        if !*is_real {
                            *is_real = true;
                            *real = *integer as f64;
                        }
                        *real += number;
                    }
                    other => return Err(not_numeric("sum", &other)),
                }
            }

            Accumulator::Avg { total, count } => {
                match value {
                    Value::Int(number) => *total += number as f64,
                    Value::Real(number) => *total += number,
                    other => return Err(not_numeric("avg", &other)),
                }
                *count += 1;
            }

            Accumulator::Min(current) => {
                if extreme(current, &value, Ordering::Less)? {
                    *current = Some(value);
                }
            }
            Accumulator::Max(current) => {
                if extreme(current, &value, Ordering::Greater)? {
                    *current = Some(value);
                }
            }
        }

        Ok(())
    }

    fn finish(self) -> Result<Value> {
        Ok(match self.accumulator {
            // The asymmetry: COUNT is the only one that reports 0 rather than
            // NULL when it saw nothing.
            Accumulator::Count(count) => Value::Int(count),

            Accumulator::Sum {
                integer,
                real,
                is_real,
                any,
                overflowed,
            } => {
                if !any {
                    Value::Null
                } else if overflowed {
                    return Err(ZqlError::new(
                        SqlState::NumericValueOutOfRange,
                        "sum overflowed a 64-bit integer",
                    )
                    .with_hint("cast the column to a real: SUM(CAST(x AS real))"));
                } else if is_real {
                    Value::Real(real)
                } else {
                    Value::Int(integer)
                }
            }

            Accumulator::Avg { total, count } => {
                if count == 0 {
                    Value::Null
                } else {
                    Value::Real(total / count as f64)
                }
            }

            Accumulator::Min(value) | Accumulator::Max(value) => value.unwrap_or(Value::Null),
        })
    }
}

/// Whether `candidate` should replace `current` for a MIN or MAX.
fn extreme(current: &Option<Value>, candidate: &Value, wanted: Ordering) -> Result<bool> {
    let Some(current) = current else {
        return Ok(true);
    };
    // A mixed-type column makes this unordered rather than an error, matching
    // how ORDER BY treats the same data.
    Ok(compare(candidate, current)
        .unwrap_or(None)
        .is_some_and(|ordering| ordering == wanted))
}

fn not_numeric(function: &str, value: &Value) -> ZqlError {
    ZqlError::new(
        SqlState::DatatypeMismatch,
        format!("{function}() needs numbers, not {}", value.type_of().name()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::{collect, values::ValuesIter};
    use crate::value::Type;

    fn spec(function: AggFn, argument: Option<CompiledExpr>, distinct: bool) -> AggSpec {
        AggSpec {
            function,
            argument,
            distinct,
            name: function.name().to_string(),
            result_type: Type::Int,
        }
    }

    fn aggregate(rows: Vec<Row>, keys: Vec<CompiledExpr>, specs: Vec<AggSpec>) -> Vec<Row> {
        let mut iter = AggregateIter::new(Box::new(ValuesIter::new(rows)), keys, specs);
        collect(&mut iter).unwrap()
    }

    fn ints(values: &[Option<i64>]) -> Vec<Row> {
        values
            .iter()
            .map(|value| {
                Row::new(vec![match value {
                    Some(number) => Value::Int(*number),
                    None => Value::Null,
                }])
            })
            .collect()
    }

    #[test]
    fn count_star_counts_rows_and_count_expr_counts_non_nulls() {
        let rows = ints(&[Some(1), None, Some(3)]);
        let out = aggregate(
            rows,
            vec![],
            vec![
                spec(AggFn::Count, None, false),
                spec(AggFn::Count, Some(CompiledExpr::Column(0)), false),
            ],
        );
        assert!(matches!(out[0].0[0], Value::Int(3)), "COUNT(*) counts rows");
        assert!(
            matches!(out[0].0[1], Value::Int(2)),
            "COUNT(expr) skips NULLs"
        );
    }

    #[test]
    fn over_no_rows_count_is_zero_and_everything_else_is_null() {
        let out = aggregate(
            vec![],
            vec![],
            vec![
                spec(AggFn::Count, None, false),
                spec(AggFn::Sum, Some(CompiledExpr::Column(0)), false),
                spec(AggFn::Avg, Some(CompiledExpr::Column(0)), false),
                spec(AggFn::Min, Some(CompiledExpr::Column(0)), false),
                spec(AggFn::Max, Some(CompiledExpr::Column(0)), false),
            ],
        );
        assert_eq!(out.len(), 1, "an ungrouped aggregate always yields one row");
        assert!(matches!(out[0].0[0], Value::Int(0)));
        for index in 1..5 {
            assert!(out[0].0[index].is_null(), "column {index} should be NULL");
        }
    }

    #[test]
    fn an_all_null_column_aggregates_to_null_except_for_count() {
        let out = aggregate(
            ints(&[None, None]),
            vec![],
            vec![
                spec(AggFn::Count, Some(CompiledExpr::Column(0)), false),
                spec(AggFn::Sum, Some(CompiledExpr::Column(0)), false),
            ],
        );
        assert!(matches!(out[0].0[0], Value::Int(0)));
        assert!(out[0].0[1].is_null());
    }

    #[test]
    fn sum_stays_integral_until_a_real_arrives() {
        let rows = vec![
            Row::new(vec![Value::Int(1)]),
            Row::new(vec![Value::Int(2)]),
        ];
        let out = aggregate(
            rows,
            vec![],
            vec![spec(AggFn::Sum, Some(CompiledExpr::Column(0)), false)],
        );
        assert!(matches!(out[0].0[0], Value::Int(3)));

        let mixed = vec![
            Row::new(vec![Value::Int(1)]),
            Row::new(vec![Value::Real(0.5)]),
        ];
        let out = aggregate(
            mixed,
            vec![],
            vec![spec(AggFn::Sum, Some(CompiledExpr::Column(0)), false)],
        );
        assert!(matches!(out[0].0[0], Value::Real(value) if (value - 1.5).abs() < 1e-9));
    }

    #[test]
    fn an_overflowing_sum_errors_rather_than_wrapping() {
        let rows = vec![
            Row::new(vec![Value::Int(i64::MAX)]),
            Row::new(vec![Value::Int(1)]),
        ];
        let mut iter = AggregateIter::new(
            Box::new(ValuesIter::new(rows)),
            vec![],
            vec![spec(AggFn::Sum, Some(CompiledExpr::Column(0)), false)],
        );
        let error = collect(&mut iter).unwrap_err();
        assert_eq!(error.state, SqlState::NumericValueOutOfRange);
    }

    #[test]
    fn avg_of_integers_is_not_an_integer() {
        let out = aggregate(
            ints(&[Some(1), Some(2)]),
            vec![],
            vec![spec(AggFn::Avg, Some(CompiledExpr::Column(0)), false)],
        );
        assert!(matches!(out[0].0[0], Value::Real(value) if (value - 1.5).abs() < 1e-9));
    }

    #[test]
    fn distinct_is_honoured_per_group() {
        let rows = ints(&[Some(1), Some(1), Some(2)]);
        let out = aggregate(
            rows,
            vec![],
            vec![
                spec(AggFn::Count, Some(CompiledExpr::Column(0)), true),
                spec(AggFn::Sum, Some(CompiledExpr::Column(0)), true),
            ],
        );
        assert!(matches!(out[0].0[0], Value::Int(2)));
        assert!(matches!(out[0].0[1], Value::Int(3)));
    }

    #[test]
    fn groups_come_out_in_first_seen_order() {
        let rows = vec![
            Row::new(vec![Value::Text("b".into())]),
            Row::new(vec![Value::Text("a".into())]),
            Row::new(vec![Value::Text("b".into())]),
        ];
        let out = aggregate(
            rows,
            vec![CompiledExpr::Column(0)],
            vec![spec(AggFn::Count, None, false)],
        );
        assert_eq!(out.len(), 2);
        assert!(matches!(&out[0].0[0], Value::Text(text) if text == "b"));
        assert!(matches!(out[0].0[1], Value::Int(2)));
        assert!(matches!(&out[1].0[0], Value::Text(text) if text == "a"));
    }

    #[test]
    fn all_nulls_land_in_one_group() {
        let out = aggregate(
            ints(&[None, None, Some(1)]),
            vec![CompiledExpr::Column(0)],
            vec![spec(AggFn::Count, None, false)],
        );
        assert_eq!(out.len(), 2, "NULLs must form a single group");
        assert!(out[0].0[0].is_null());
        assert!(matches!(out[0].0[1], Value::Int(2)));
    }

    #[test]
    fn nan_keys_do_not_produce_one_group_per_row() {
        let rows = vec![
            Row::new(vec![Value::Real(f64::NAN)]),
            Row::new(vec![Value::Real(f64::NAN)]),
        ];
        let out = aggregate(
            rows,
            vec![CompiledExpr::Column(0)],
            vec![spec(AggFn::Count, None, false)],
        );
        assert_eq!(out.len(), 1, "every NaN belongs to one group");
    }

    #[test]
    fn min_and_max_skip_nulls() {
        let out = aggregate(
            ints(&[Some(5), None, Some(2), Some(9)]),
            vec![],
            vec![
                spec(AggFn::Min, Some(CompiledExpr::Column(0)), false),
                spec(AggFn::Max, Some(CompiledExpr::Column(0)), false),
            ],
        );
        assert!(matches!(out[0].0[0], Value::Int(2)));
        assert!(matches!(out[0].0[1], Value::Int(9)));
    }
}

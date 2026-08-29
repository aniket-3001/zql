//! Compiled expressions.
//!
//! # Why compile at all
//!
//! Column references are resolved to **integer indices** during binding, so the
//! executor never looks a column up by name. The classic naive-interpreter
//! mistake is a string hash per column per row; at 127,000 rows and six columns
//! that is three quarters of a million lookups per query, all of them answering
//! a question that was already settled before execution began.
//!
//! # Why an enum and not `Box<dyn Fn>`
//!
//! Closures would be marginally faster to dispatch and impossible to inspect.
//! An enum can be printed by `EXPLAIN`, pattern-matched by a future optimiser,
//! and read in a debugger. It also avoids threading lifetimes through every
//! node.

use std::cmp::Ordering;

use crate::datetime;
use crate::error::{Result, SqlState, ZqlError};
use crate::sql::ast::{BinaryOp, UnaryOp};
use crate::value::{compare, Row, Type, Value};

/// An expression with every name already resolved.
#[derive(Debug, Clone)]
pub enum CompiledExpr {
    Literal(Value),
    /// An index into the input row, fixed at bind time.
    Column(usize),
    Unary {
        op: UnaryOp,
        expr: Box<CompiledExpr>,
    },
    Binary {
        op: BinaryOp,
        left: Box<CompiledExpr>,
        right: Box<CompiledExpr>,
    },
    IsNull {
        expr: Box<CompiledExpr>,
        negated: bool,
    },
    Like {
        expr: Box<CompiledExpr>,
        pattern: Box<CompiledExpr>,
        negated: bool,
    },
    InList {
        expr: Box<CompiledExpr>,
        list: Vec<Value>,
        negated: bool,
    },
    Between {
        expr: Box<CompiledExpr>,
        low: Box<CompiledExpr>,
        high: Box<CompiledExpr>,
        negated: bool,
    },
    Case {
        branches: Vec<(CompiledExpr, CompiledExpr)>,
        else_result: Option<Box<CompiledExpr>>,
    },
    Cast {
        expr: Box<CompiledExpr>,
        ty: Type,
    },
}

impl CompiledExpr {
    /// Evaluates against one input row.
    pub fn eval(&self, row: &Row) -> Result<Value> {
        match self {
            CompiledExpr::Literal(value) => Ok(value.clone()),

            CompiledExpr::Column(index) => row.get(*index).cloned(),

            CompiledExpr::Unary { op, expr } => eval_unary(*op, expr, row),

            CompiledExpr::Binary { op, left, right } => eval_binary(*op, left, right, row),

            CompiledExpr::IsNull { expr, negated } => {
                // The one operator that is *not* three-valued: `IS NULL` always
                // answers true or false, which is what makes it usable at all.
                let is_null = expr.eval(row)?.is_null();
                Ok(Value::Bool(is_null != *negated))
            }

            CompiledExpr::Like {
                expr,
                pattern,
                negated,
            } => {
                let (subject, pattern) = (expr.eval(row)?, pattern.eval(row)?);
                if subject.is_null() || pattern.is_null() {
                    return Ok(Value::Null);
                }
                let subject = expect_text(&subject, "LIKE")?;
                let pattern = expect_text(&pattern, "LIKE")?;
                Ok(Value::Bool(like_matches(&subject, &pattern) != *negated))
            }

            CompiledExpr::InList {
                expr,
                list,
                negated,
            } => eval_in_list(expr, list, *negated, row),

            CompiledExpr::Between {
                expr,
                low,
                high,
                negated,
            } => {
                // `x BETWEEN a AND b` is `x >= a AND x <= b`, three-valued
                // logic included — so a NULL bound makes the answer unknown
                // rather than false.
                let subject = expr.eval(row)?;
                let at_least = compare_op(BinaryOp::GtEq, &subject, &low.eval(row)?)?;
                let at_most = compare_op(BinaryOp::LtEq, &subject, &high.eval(row)?)?;
                let within = and_values(&at_least, &at_most)?;
                if *negated {
                    not_value(&within)
                } else {
                    Ok(within)
                }
            }

            CompiledExpr::Case {
                branches,
                else_result,
            } => {
                for (condition, result) in branches {
                    // Only an exactly-true branch fires; unknown falls through
                    // to the next `WHEN`, as it would in a `WHERE`.
                    if condition.eval(row)?.as_bool()? == Some(true) {
                        return result.eval(row);
                    }
                }
                match else_result {
                    Some(expr) => expr.eval(row),
                    None => Ok(Value::Null),
                }
            }

            CompiledExpr::Cast { expr, ty } => cast(&expr.eval(row)?, *ty),

        }
    }

    /// The type this expression produces, needed for `RowDescription` before
    /// any row has been seen.
    pub fn result_type(&self) -> Type {
        match self {
            CompiledExpr::Literal(value) => value.type_of(),
            // A column's type comes from the schema; the binder supplies it.
            CompiledExpr::Column(_) => Type::Unknown,
            CompiledExpr::Unary { op, expr } => match op {
                UnaryOp::Not => Type::Bool,
                UnaryOp::Neg | UnaryOp::Plus => expr.result_type(),
            },
            CompiledExpr::Binary { op, left, right } => binary_result_type(*op, left, right),
            CompiledExpr::IsNull { .. }
            | CompiledExpr::Like { .. }
            | CompiledExpr::InList { .. }
            | CompiledExpr::Between { .. } => Type::Bool,
            CompiledExpr::Case {
                branches,
                else_result,
            } => branches
                .first()
                .map(|(_, result)| result.result_type())
                .or_else(|| else_result.as_ref().map(|expr| expr.result_type()))
                .unwrap_or(Type::Unknown),
            CompiledExpr::Cast { ty, .. } => *ty,
        }
    }
}

fn binary_result_type(op: BinaryOp, left: &CompiledExpr, right: &CompiledExpr) -> Type {
    use BinaryOp::*;
    match op {
        Or | And | Eq | NotEq | Lt | LtEq | Gt | GtEq => Type::Bool,
        Concat => Type::Text,
        Add | Sub | Mul | Mod => {
            // Integer arithmetic stays integral; one real operand makes the
            // whole expression real.
            if left.result_type() == Type::Real || right.result_type() == Type::Real {
                Type::Real
            } else {
                Type::Int
            }
        }
        // Division always widens: `3 / 2` is 1.5, not 1. SQL engines disagree
        // about this and integer division is the more surprising of the two
        // answers to get from a tool you are using to inspect data.
        Div => Type::Real,
    }
}

// ------------------------------------------------------------------- unary

fn eval_unary(op: UnaryOp, expr: &CompiledExpr, row: &Row) -> Result<Value> {
    let value = expr.eval(row)?;
    match op {
        UnaryOp::Not => not_value(&value),
        UnaryOp::Plus => Ok(value),
        UnaryOp::Neg => match value {
            Value::Null => Ok(Value::Null),
            Value::Int(number) => number
                .checked_neg()
                .map(Value::Int)
                .ok_or_else(|| overflow("negation")),
            Value::Real(number) => Ok(Value::Real(-number)),
            other => Err(type_error(format!(
                "cannot negate {}",
                other.type_of().name()
            ))),
        },
    }
}

/// `NOT` under three-valued logic: `NOT NULL` is `NULL`, not `true`.
fn not_value(value: &Value) -> Result<Value> {
    Ok(match value.as_bool()? {
        Some(boolean) => Value::Bool(!boolean),
        None => Value::Null,
    })
}

// ------------------------------------------------------------------ binary

fn eval_binary(
    op: BinaryOp,
    left: &CompiledExpr,
    right: &CompiledExpr,
    row: &Row,
) -> Result<Value> {
    use BinaryOp::*;

    // `AND` and `OR` short-circuit, but only where the answer is settled
    // *regardless* of what the other side turns out to be. That is a property
    // of the truth table in SQL-SUBSET.md §6.1, not an optimisation:
    // `FALSE AND NULL` is `FALSE`, so the right side need never be evaluated.
    match op {
        And => {
            let left = left.eval(row)?;
            if left.as_bool()? == Some(false) {
                return Ok(Value::Bool(false));
            }
            return and_values(&left, &right.eval(row)?);
        }
        Or => {
            let left = left.eval(row)?;
            if left.as_bool()? == Some(true) {
                return Ok(Value::Bool(true));
            }
            return or_values(&left, &right.eval(row)?);
        }
        _ => {}
    }

    let (left, right) = (left.eval(row)?, right.eval(row)?);

    match op {
        Eq | NotEq | Lt | LtEq | Gt | GtEq => compare_op(op, &left, &right),
        Concat => concat(&left, &right),
        Add | Sub | Mul | Div | Mod => arithmetic(op, &left, &right),
        // Short-circuited above and returned there. An internal error rather
        // than `unreachable!`: this file must not contain a way to panic, and
        // "the compiler cannot see that this is impossible" is not a reason to
        // add one.
        And | Or => Err(ZqlError::internal("logical operator reached the value path")),
    }
}

/// `AND` over the full truth table: false dominates, then unknown.
fn and_values(left: &Value, right: &Value) -> Result<Value> {
    Ok(match (left.as_bool()?, right.as_bool()?) {
        (Some(false), _) | (_, Some(false)) => Value::Bool(false),
        (Some(true), Some(true)) => Value::Bool(true),
        _ => Value::Null,
    })
}

/// `OR` over the full truth table: true dominates, then unknown.
fn or_values(left: &Value, right: &Value) -> Result<Value> {
    Ok(match (left.as_bool()?, right.as_bool()?) {
        (Some(true), _) | (_, Some(true)) => Value::Bool(true),
        (Some(false), Some(false)) => Value::Bool(false),
        _ => Value::Null,
    })
}

fn compare_op(op: BinaryOp, left: &Value, right: &Value) -> Result<Value> {
    use BinaryOp::*;
    let Some(ordering) = compare(left, right)? else {
        // Either side NULL, or two unordered reals: the answer is unknown.
        return Ok(Value::Null);
    };
    Ok(Value::Bool(match op {
        Eq => ordering == Ordering::Equal,
        NotEq => ordering != Ordering::Equal,
        Lt => ordering == Ordering::Less,
        LtEq => ordering != Ordering::Greater,
        Gt => ordering == Ordering::Greater,
        GtEq => ordering != Ordering::Less,
        _ => return Err(ZqlError::internal("not a comparison operator")),
    }))
}

/// `||` is string concatenation, never logical-or, and `NULL || 'x'` is `NULL`.
fn concat(left: &Value, right: &Value) -> Result<Value> {
    if left.is_null() || right.is_null() {
        return Ok(Value::Null);
    }
    Ok(Value::Text(format!(
        "{}{}",
        text_of(left)?,
        text_of(right)?
    )))
}

fn arithmetic(op: BinaryOp, left: &Value, right: &Value) -> Result<Value> {
    use BinaryOp::*;

    if left.is_null() || right.is_null() {
        return Ok(Value::Null);
    }

    // Integers stay integers, with *checked* arithmetic: a size column summing
    // past i64 must report `22003` rather than silently wrapping into a
    // negative number that looks like a plausible answer.
    if let (Value::Int(a), Value::Int(b)) = (left, right) {
        let (a, b) = (*a, *b);
        return Ok(match op {
            Add => Value::Int(a.checked_add(b).ok_or_else(|| overflow("addition"))?),
            Sub => Value::Int(a.checked_sub(b).ok_or_else(|| overflow("subtraction"))?),
            Mul => Value::Int(
                a.checked_mul(b)
                    .ok_or_else(|| overflow("multiplication"))?,
            ),
            Mod => {
                if b == 0 {
                    return Err(division_by_zero());
                }
                // `checked_rem` also guards i64::MIN % -1, which overflows.
                Value::Int(a.checked_rem(b).ok_or_else(|| overflow("modulo"))?)
            }
            Div => {
                if b == 0 {
                    return Err(division_by_zero());
                }
                Value::Real(a as f64 / b as f64)
            }
            _ => return Err(ZqlError::internal("not an arithmetic operator")),
        });
    }

    let (a, b) = (numeric(left, op)?, numeric(right, op)?);
    Ok(match op {
        Add => Value::Real(a + b),
        Sub => Value::Real(a - b),
        Mul => Value::Real(a * b),
        Div => {
            if b == 0.0 {
                return Err(division_by_zero());
            }
            Value::Real(a / b)
        }
        Mod => {
            if b == 0.0 {
                return Err(division_by_zero());
            }
            Value::Real(a % b)
        }
        _ => return Err(ZqlError::internal("not an arithmetic operator")),
    })
}

fn eval_in_list(
    expr: &CompiledExpr,
    list: &[Value],
    negated: bool,
    row: &Row,
) -> Result<Value> {
    let subject = expr.eval(row)?;
    if subject.is_null() {
        return Ok(Value::Null);
    }

    // `x IN (1, NULL)` is `true` when x is 1 and `NULL` otherwise — never
    // `false` — because a NULL in the list means "there might be a match here".
    let mut saw_null = false;
    for candidate in list {
        match compare(&subject, candidate)? {
            Some(Ordering::Equal) => return Ok(Value::Bool(!negated)),
            Some(_) => {}
            None => saw_null = true,
        }
    }

    Ok(if saw_null {
        Value::Null
    } else {
        Value::Bool(negated)
    })
}

// -------------------------------------------------------------------- LIKE

/// `%` matches any run of characters, `_` matches exactly one.
///
/// Case-**sensitive**, which is Postgres semantics and differs from SQLite's
/// default. The README says which was chosen and why.
///
/// This is a two-pointer backtracking matcher rather than a recursive one:
/// a pattern like `%a%a%a%a%a%b` against a long non-matching subject drives a
/// recursive matcher into deep recursion, and the standard library gives no way
/// to grow the stack. Backtracking to the last `%` keeps the memory flat and
/// makes the worst case slow rather than fatal.
fn like_matches(subject: &str, pattern: &str) -> bool {
    let subject: Vec<char> = subject.chars().collect();
    let pattern: Vec<char> = pattern.chars().collect();

    let (mut s, mut p) = (0usize, 0usize);
    // Where to resume if the current `%` guess turns out to be wrong.
    let mut star: Option<usize> = None;
    let mut star_subject = 0usize;

    while s < subject.len() {
        match pattern.get(p) {
            Some('%') => {
                star = Some(p);
                star_subject = s;
                p += 1; // first try matching zero characters
            }
            Some('_') => {
                s += 1;
                p += 1;
            }
            Some(literal) if *literal == subject[s] => {
                s += 1;
                p += 1;
            }
            // Mismatch: let the last `%` swallow one more character.
            _ => match star {
                Some(star_position) => {
                    p = star_position + 1;
                    star_subject += 1;
                    s = star_subject;
                }
                None => return false,
            },
        }
    }

    // Trailing `%`s may still match the empty remainder.
    pattern[p..].iter().all(|character| *character == '%')
}

// ------------------------------------------------------------------- casts

fn cast(value: &Value, ty: Type) -> Result<Value> {
    if value.is_null() {
        return Ok(Value::Null);
    }
    Ok(match ty {
        Type::Text => Value::Text(text_of(value)?),
        Type::Int => match value {
            Value::Int(number) => Value::Int(*number),
            Value::Timestamp(seconds) => Value::Int(*seconds),
            Value::Bool(boolean) => Value::Int(i64::from(*boolean)),
            Value::Real(number) => {
                // Truncation toward zero, and a range check: `f64 as i64`
                // saturates silently in Rust, which would turn a nonsense
                // value into a plausible one.
                let truncated = number.trunc();
                if !truncated.is_finite() || truncated.abs() >= 9.223_372_036_854_776e18 {
                    return Err(overflow("cast to integer"));
                }
                Value::Int(truncated as i64)
            }
            Value::Text(text) => text
                .trim()
                .parse::<i64>()
                .map(Value::Int)
                .map_err(|_| cast_error(text, "integer"))?,
            other => return Err(cast_error(&text_of(other)?, "integer")),
        },
        Type::Real => match value {
            Value::Real(number) => Value::Real(*number),
            Value::Int(number) => Value::Real(*number as f64),
            Value::Text(text) => text
                .trim()
                .parse::<f64>()
                .map(Value::Real)
                .map_err(|_| cast_error(text, "real"))?,
            other => return Err(cast_error(&text_of(other)?, "real")),
        },
        Type::Bool => match value {
            Value::Bool(boolean) => Value::Bool(*boolean),
            Value::Int(number) => Value::Bool(*number != 0),
            Value::Text(text) => match text.trim().to_ascii_lowercase().as_str() {
                "t" | "true" | "yes" | "on" | "1" => Value::Bool(true),
                "f" | "false" | "no" | "off" | "0" => Value::Bool(false),
                _ => return Err(cast_error(text, "boolean")),
            },
            other => return Err(cast_error(&text_of(other)?, "boolean")),
        },
        Type::Timestamp => match value {
            Value::Timestamp(seconds) => Value::Timestamp(*seconds),
            Value::Int(seconds) => Value::Timestamp(*seconds),
            other => return Err(cast_error(&text_of(other)?, "timestamp")),
        },
        Type::Blob | Type::Unknown => {
            return Err(ZqlError::unsupported(format!("casting to {}", ty.name())))
        }
    })
}

// ----------------------------------------------------------------- helpers

/// Renders a value as text for `||`, `CAST` and the string functions.
fn text_of(value: &Value) -> Result<String> {
    Ok(match value {
        Value::Null => String::new(),
        Value::Text(text) => text.clone(),
        Value::Int(number) => number.to_string(),
        Value::Real(number) => number.to_string(),
        Value::Bool(boolean) => if *boolean { "true" } else { "false" }.to_string(),
        Value::Timestamp(seconds) => datetime::format_timestamp(*seconds),
        Value::Blob(_) => {
            return Err(type_error(
                "a blob has no text form; use CAST or a comparison instead",
            ))
        }
    })
}

fn expect_text(value: &Value, context: &str) -> Result<String> {
    match value {
        Value::Text(text) => Ok(text.clone()),
        other => Err(type_error(format!(
            "{context} needs text, not {}",
            other.type_of().name()
        ))),
    }
}

fn numeric(value: &Value, op: BinaryOp) -> Result<f64> {
    match value {
        Value::Int(number) => Ok(*number as f64),
        Value::Real(number) => Ok(*number),
        Value::Timestamp(seconds) => Ok(*seconds as f64),
        other => Err(type_error(format!(
            "operator {} does not accept {}",
            op.as_str(),
            other.type_of().name()
        ))),
    }
}

fn type_error(message: impl Into<String>) -> ZqlError {
    ZqlError::new(SqlState::DatatypeMismatch, message)
}

fn cast_error(text: &str, target: &str) -> ZqlError {
    ZqlError::new(
        SqlState::DatatypeMismatch,
        format!("cannot cast '{text}' to {target}"),
    )
}

fn overflow(operation: &str) -> ZqlError {
    ZqlError::new(
        SqlState::NumericValueOutOfRange,
        format!("{operation} overflowed a 64-bit integer"),
    )
}

fn division_by_zero() -> ZqlError {
    ZqlError::new(SqlState::DivisionByZero, "division by zero")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(expr: &CompiledExpr) -> Value {
        expr.eval(&Row::new(vec![])).expect("evaluation failed")
    }

    fn literal(value: Value) -> Box<CompiledExpr> {
        Box::new(CompiledExpr::Literal(value))
    }

    fn binary(op: BinaryOp, left: Value, right: Value) -> Value {
        eval(&CompiledExpr::Binary {
            op,
            left: literal(left),
            right: literal(right),
        })
    }

    /// The truth table from SQL-SUBSET.md §6.1, verbatim.
    #[test]
    fn three_valued_logic_matches_the_specified_table() {
        use BinaryOp::{And, Or};
        let t = || Value::Bool(true);
        let f = || Value::Bool(false);
        let n = || Value::Null;

        assert!(binary(Or, t(), n()).is_true(), "TRUE OR NULL is TRUE");
        assert!(binary(Or, f(), n()).is_null(), "FALSE OR NULL is NULL");
        assert!(
            matches!(binary(And, f(), n()), Value::Bool(false)),
            "FALSE AND NULL is FALSE"
        );
        assert!(binary(And, t(), n()).is_null(), "TRUE AND NULL is NULL");

        // Both directions, because short-circuiting makes them different code.
        assert!(binary(Or, n(), t()).is_true());
        assert!(matches!(binary(And, n(), f()), Value::Bool(false)));

        assert!(eval(&CompiledExpr::Unary {
            op: UnaryOp::Not,
            expr: literal(Value::Null)
        })
        .is_null());
    }

    #[test]
    fn null_equality_is_unknown_but_is_null_is_not() {
        assert!(binary(BinaryOp::Eq, Value::Null, Value::Null).is_null());
        assert!(binary(BinaryOp::NotEq, Value::Null, Value::Null).is_null());

        let is_null = CompiledExpr::IsNull {
            expr: literal(Value::Null),
            negated: false,
        };
        assert!(eval(&is_null).is_true());
    }

    #[test]
    fn integer_arithmetic_is_checked_not_wrapping() {
        let error = CompiledExpr::Binary {
            op: BinaryOp::Add,
            left: literal(Value::Int(i64::MAX)),
            right: literal(Value::Int(1)),
        }
        .eval(&Row::new(vec![]))
        .unwrap_err();
        assert_eq!(error.state, SqlState::NumericValueOutOfRange);
    }

    #[test]
    fn division_by_zero_is_an_error_not_an_infinity() {
        for (left, right) in [
            (Value::Int(1), Value::Int(0)),
            (Value::Real(1.0), Value::Real(0.0)),
        ] {
            let error = CompiledExpr::Binary {
                op: BinaryOp::Div,
                left: literal(left),
                right: literal(right),
            }
            .eval(&Row::new(vec![]))
            .unwrap_err();
            assert_eq!(error.state, SqlState::DivisionByZero);
        }
    }

    #[test]
    fn division_widens_rather_than_truncating() {
        assert!(matches!(
            binary(BinaryOp::Div, Value::Int(3), Value::Int(2)),
            Value::Real(value) if (value - 1.5).abs() < f64::EPSILON
        ));
    }

    #[test]
    fn concat_is_not_logical_or_and_propagates_null() {
        assert!(matches!(
            binary(BinaryOp::Concat, Value::Text("a".into()), Value::Int(1)),
            Value::Text(text) if text == "a1"
        ));
        assert!(binary(BinaryOp::Concat, Value::Null, Value::Text("x".into())).is_null());
    }

    #[test]
    fn in_list_with_a_null_is_unknown_rather_than_false() {
        let in_list = |subject: Value| {
            eval(&CompiledExpr::InList {
                expr: literal(subject),
                list: vec![Value::Int(1), Value::Null],
                negated: false,
            })
        };
        assert!(in_list(Value::Int(1)).is_true(), "a match still matches");
        assert!(in_list(Value::Int(9)).is_null(), "no match, but a NULL hides");
    }

    #[test]
    fn like_handles_the_wildcards_and_backtracks() {
        assert!(like_matches("server.log", "%.log"));
        assert!(like_matches("ERROR: disk full", "%ERROR%"));
        assert!(like_matches("abc", "a_c"));
        assert!(!like_matches("abc", "a_"));
        assert!(like_matches("abc", "abc"));
        assert!(like_matches("abc", "%"));
        assert!(like_matches("", "%"));
        assert!(!like_matches("", "_"));
        // Backtracking: the first `%` must give back what it took.
        assert!(like_matches("aaa", "%a"));
        assert!(like_matches("abcabc", "%abc"));
        assert!(!like_matches("abcabd", "%abc"));
        // Case-sensitive, per Postgres semantics.
        assert!(!like_matches("ABC", "abc"));
    }

    #[test]
    fn a_pathological_like_pattern_terminates_without_recursing() {
        let subject = "a".repeat(2000);
        let pattern = format!("{}b", "%a".repeat(50));
        assert!(!like_matches(&subject, &pattern));
    }




    #[test]
    fn casting_a_huge_float_to_an_integer_errors_rather_than_saturating() {
        let error = cast(&Value::Real(1e30), Type::Int).unwrap_err();
        assert_eq!(error.state, SqlState::NumericValueOutOfRange);
    }
}

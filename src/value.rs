//! Values, types, and the three-valued logic that holds them together.
//!
//! `Type` lives here rather than beside `Schema` because a value must be able
//! to report its own type without the schema layer existing; putting them in
//! separate modules would make `value` depend on `plan` and `plan` depend on
//! `value`.
//!
//! **The trap in this file is NULL.** SQL's `NULL` is *unknown*, not *absent*:
//! `NULL = NULL` is `NULL`, not `true`. Comparison therefore returns
//! `Option<Ordering>` and every caller has to decide what `None` means, which
//! is exactly the point — a hand-written engine is most likely to be quietly
//! wrong here, so the compiler is made to ask the question at each site.

use std::cmp::Ordering;

use crate::error::{Result, SqlState, ZqlError};

/// The types zql can put on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Type {
    Bool,
    Int,
    Real,
    Text,
    Blob,
    /// Seconds since the Unix epoch, always UTC. Distinct from `Int` so that
    /// the OID and the rendering live in one place and cannot drift apart.
    Timestamp,
    /// The type of a bare `NULL` literal before anything constrains it.
    /// Advertised to clients as `text`, which is what Postgres does with an
    /// unknown-typed literal in a result column.
    Unknown,
}

impl Type {
    /// The name reported by `typeof()` and used in error messages.
    pub fn name(self) -> &'static str {
        match self {
            Type::Bool => "boolean",
            Type::Int => "integer",
            Type::Real => "real",
            Type::Text => "text",
            Type::Blob => "blob",
            Type::Timestamp => "timestamp",
            Type::Unknown => "unknown",
        }
    }

    pub fn is_numeric(self) -> bool {
        matches!(self, Type::Int | Type::Real)
    }
}

/// A single cell.
#[derive(Debug, Clone)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
    /// Seconds since the Unix epoch, UTC.
    Timestamp(i64),
}

impl Value {
    pub fn type_of(&self) -> Type {
        match self {
            Value::Null => Type::Unknown,
            Value::Bool(_) => Type::Bool,
            Value::Int(_) => Type::Int,
            Value::Real(_) => Type::Real,
            Value::Text(_) => Type::Text,
            Value::Blob(_) => Type::Blob,
            Value::Timestamp(_) => Type::Timestamp,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// `WHERE` and `HAVING` admit a row only when the predicate is *exactly*
    /// true. `NULL` excludes, identically to `FALSE` — the difference between
    /// them only shows up under `NOT`.
    pub fn is_true(&self) -> bool {
        matches!(self, Value::Bool(true))
    }

    /// The value as a boolean under three-valued logic: `None` means unknown.
    pub fn as_bool(&self) -> Result<Option<bool>> {
        match self {
            Value::Null => Ok(None),
            Value::Bool(b) => Ok(Some(*b)),
            other => Err(ZqlError::new(
                SqlState::DatatypeMismatch,
                format!(
                    "argument of a logical operator must be boolean, not {}",
                    other.type_of().name()
                ),
            )),
        }
    }

    /// Numeric widening for arithmetic and comparison. `Int` promotes to
    /// `Real` when the other side is `Real`; text never coerces to a number.
    fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Int(i) => Some(*i as f64),
            Value::Real(r) => Some(*r),
            Value::Timestamp(t) => Some(*t as f64),
            _ => None,
        }
    }
}

/// Three-valued comparison.
///
/// `Ok(None)` is SQL `NULL` — "the answer is unknown" — and is returned
/// whenever either side is `NULL`, or when two `Real`s are unordered because
/// one of them is `NaN`. An `Err` means the two values are not comparable at
/// all, which is a type error rather than an unknown answer.
pub fn compare(left: &Value, right: &Value) -> Result<Option<Ordering>> {
    use Value::*;

    if left.is_null() || right.is_null() {
        return Ok(None);
    }

    let ordering = match (left, right) {
        (Int(a), Int(b)) => a.cmp(b),
        (Timestamp(a), Timestamp(b)) => a.cmp(b),
        (Bool(a), Bool(b)) => a.cmp(b),
        (Text(a), Text(b)) => a.cmp(b),
        (Blob(a), Blob(b)) => a.cmp(b),

        // Mixed numerics widen to f64. NaN makes the pair unordered, which is
        // `NULL` rather than an error: the values are comparable in principle.
        _ if left.type_of().is_numeric() && right.type_of().is_numeric() => {
            let (a, b) = (
                left.as_f64().unwrap_or(f64::NAN),
                right.as_f64().unwrap_or(f64::NAN),
            );
            match a.partial_cmp(&b) {
                Some(ordering) => ordering,
                None => return Ok(None),
            }
        }

        // Everything else is a genuine type error. Being strict here is both
        // more correct than silently coercing and less code; `CAST` is the
        // explicit escape hatch.
        _ => {
            return Err(ZqlError::new(
                SqlState::DatatypeMismatch,
                format!(
                    "cannot compare {} with {}",
                    left.type_of().name(),
                    right.type_of().name()
                ),
            )
            .with_hint("use CAST to convert one side explicitly"))
        }
    };

    Ok(Some(ordering))
}

/// One output row. A newtype so the representation can change without
/// touching every operator.
///
/// Columnar batching was considered and rejected: at the ~10^5 row scale zql
/// targets it buys nothing measurable and costs a great deal of readability.
#[derive(Debug, Clone, Default)]
pub struct Row(pub Vec<Value>);

impl Row {
    pub fn new(values: Vec<Value>) -> Self {
        Row(values)
    }

    /// Checked column access. Operators index rows with indices resolved at
    /// bind time, so an out-of-range index is a bug in the binder rather than
    /// bad user input — but it still must not panic.
    pub fn get(&self, index: usize) -> Result<&Value> {
        self.0
            .get(index)
            .ok_or_else(|| ZqlError::internal(format!("column index {index} out of range")))
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_compares_as_unknown_to_everything_including_null() {
        assert!(compare(&Value::Null, &Value::Null).unwrap().is_none());
        assert!(compare(&Value::Null, &Value::Int(1)).unwrap().is_none());
        assert!(compare(&Value::Int(1), &Value::Null).unwrap().is_none());
    }

    #[test]
    fn ints_and_reals_compare_across_the_type_boundary() {
        assert_eq!(
            compare(&Value::Int(2), &Value::Real(2.5)).unwrap(),
            Some(Ordering::Less)
        );
        assert_eq!(
            compare(&Value::Real(2.0), &Value::Int(2)).unwrap(),
            Some(Ordering::Equal)
        );
    }

    #[test]
    fn nan_is_unordered_rather_than_an_error() {
        assert!(compare(&Value::Real(f64::NAN), &Value::Int(1))
            .unwrap()
            .is_none());
    }

    #[test]
    fn text_never_silently_compares_against_a_number() {
        let err = compare(&Value::Text("42".into()), &Value::Int(42)).unwrap_err();
        assert_eq!(err.state, SqlState::DatatypeMismatch);
    }

    #[test]
    fn only_exactly_true_passes_a_filter() {
        assert!(Value::Bool(true).is_true());
        assert!(!Value::Bool(false).is_true());
        assert!(!Value::Null.is_true());
        assert!(!Value::Int(1).is_true());
    }
}

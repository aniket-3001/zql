//! Output schemas — the named, typed shape of a result set.
//!
//! The wire protocol writes `RowDescription` *before* the first `DataRow`, so
//! the full output schema has to be known before execution begins. That single
//! constraint is why binding is a distinct phase rather than something the
//! operators discover as they run.

use crate::value::Type;

/// One output column.
#[derive(Debug, Clone)]
pub struct Column {
    /// The name as the client will see it, after any `AS` alias.
    pub name: String,
    pub ty: Type,
    /// The source alias this column came from, if any (`f` in `files AS f`).
    /// Used only to resolve qualified references at bind time.
    pub qualifier: Option<String>,
}

impl Column {
    pub fn new(name: impl Into<String>, ty: Type) -> Self {
        Column {
            name: name.into(),
            ty,
            qualifier: None,
        }
    }

    pub fn qualified(name: impl Into<String>, ty: Type, qualifier: impl Into<String>) -> Self {
        Column {
            name: name.into(),
            ty,
            qualifier: Some(qualifier.into()),
        }
    }
}

/// An ordered list of columns.
#[derive(Debug, Clone, Default)]
pub struct Schema {
    pub columns: Vec<Column>,
}

impl Schema {
    pub fn new(columns: Vec<Column>) -> Self {
        Schema { columns }
    }

    pub fn len(&self) -> usize {
        self.columns.len()
    }

    pub fn is_empty(&self) -> bool {
        self.columns.is_empty()
    }

    /// Resolves an unqualified column name to its index.
    ///
    /// Unquoted identifiers have already been folded to lower case by the
    /// lexer, so this is a plain comparison rather than a case-insensitive one.
    /// Returns `None` for both "no such column" and "ambiguous" — the binder
    /// distinguishes them, because only it can produce the better message.
    pub fn index_of(&self, name: &str) -> Option<usize> {
        let mut found = None;
        for (index, column) in self.columns.iter().enumerate() {
            if column.name == name {
                if found.is_some() {
                    return None; // ambiguous
                }
                found = Some(index);
            }
        }
        found
    }

    /// Resolves a qualified reference such as `f.size`.
    pub fn index_of_qualified(&self, qualifier: &str, name: &str) -> Option<usize> {
        self.columns.iter().position(|column| {
            column.name == name && column.qualifier.as_deref() == Some(qualifier)
        })
    }

    /// How many columns carry this name. The binder uses it to tell
    /// "unknown column" apart from "ambiguous column".
    pub fn count_named(&self, name: &str) -> usize {
        self.columns.iter().filter(|c| c.name == name).count()
    }
}

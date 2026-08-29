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

    /// Resolves a name ignoring case, for the one source that can produce a
    /// column whose name is not already lower case.
    ///
    /// Every other source lower-cases its own column names, so this only ever
    /// fires for `sqlite()`: the DDL parser deliberately preserves the case a
    /// column was declared with, so `visitCount` is displayed as written — but
    /// the lexer folds an unquoted reference to `visitcount`, and the two would
    /// never meet. SQLite itself treats column names case-insensitively, so
    /// requiring `SELECT "visitCount"` would be zql inventing a restriction the
    /// underlying file does not have.
    ///
    /// Tried only *after* an exact match fails, so this can never change what
    /// an already-working query means — it only resolves ones that used to be
    /// an error. `None` covers both "no such column" and "more than one match",
    /// which the binder tells apart with [`count_named_ignoring_case`].
    ///
    /// [`count_named_ignoring_case`]: Schema::count_named_ignoring_case
    pub fn index_of_ignoring_case(&self, name: &str) -> Option<usize> {
        let mut found = None;
        for (index, column) in self.columns.iter().enumerate() {
            if column.name.eq_ignore_ascii_case(name) {
                if found.is_some() {
                    return None; // ambiguous
                }
                found = Some(index);
            }
        }
        found
    }

    pub fn count_named_ignoring_case(&self, name: &str) -> usize {
        self.columns
            .iter()
            .filter(|c| c.name.eq_ignore_ascii_case(name))
            .count()
    }

    /// The qualified form of [`index_of_ignoring_case`], for `t.visitCount`.
    ///
    /// [`index_of_ignoring_case`]: Schema::index_of_ignoring_case
    pub fn index_of_qualified_ignoring_case(
        &self,
        qualifier: &str,
        name: &str,
    ) -> Option<usize> {
        let mut found = None;
        for (index, column) in self.columns.iter().enumerate() {
            let matches = column.name.eq_ignore_ascii_case(name)
                && column
                    .qualifier
                    .as_deref()
                    .is_some_and(|q| q.eq_ignore_ascii_case(qualifier));
            if matches {
                if found.is_some() {
                    return None;
                }
                found = Some(index);
            }
        }
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> Schema {
        Schema::new(vec![
            Column::qualified("visitCount", Type::Int, "p"),
            Column::qualified("url", Type::Text, "p"),
        ])
    }

    #[test]
    fn an_exact_match_still_wins() {
        assert_eq!(schema().index_of("visitCount"), Some(0));
        assert_eq!(schema().index_of("url"), Some(1));
    }

    /// The lexer folds an unquoted reference, so this is the only way an
    /// unquoted `visitCount` can ever reach a case-preserved SQLite column.
    #[test]
    fn a_folded_reference_finds_a_case_preserved_column() {
        assert_eq!(schema().index_of("visitcount"), None, "exact lookup must not match");
        assert_eq!(schema().index_of_ignoring_case("visitcount"), Some(0));
        assert_eq!(schema().count_named_ignoring_case("visitcount"), 1);
        assert_eq!(
            schema().index_of_qualified_ignoring_case("p", "visitcount"),
            Some(0)
        );
    }

    #[test]
    fn a_name_matching_two_columns_only_by_case_stays_ambiguous() {
        let ambiguous = Schema::new(vec![
            Column::new("Value", Type::Int),
            Column::new("value", Type::Text),
        ]);
        // An exact match is unambiguous and must keep working.
        assert_eq!(ambiguous.index_of("value"), Some(1));
        // Ignoring case, both match, so the binder is told to say so.
        assert_eq!(ambiguous.count_named_ignoring_case("VALUE"), 2);
        assert_eq!(ambiguous.index_of_ignoring_case("VALUE"), None);
    }

    #[test]
    fn an_unknown_name_is_still_unknown_either_way() {
        assert_eq!(schema().index_of("nope"), None);
        assert_eq!(schema().index_of_ignoring_case("nope"), None);
        assert_eq!(schema().count_named_ignoring_case("nope"), 0);
    }

    #[test]
    fn a_qualifier_must_still_match() {
        assert_eq!(
            schema().index_of_qualified_ignoring_case("other", "visitcount"),
            None
        );
    }
}

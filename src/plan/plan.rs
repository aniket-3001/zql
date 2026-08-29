//! The plan tree.
//!
//! # Why there is only one
//!
//! Textbook engines carry a logical plan and a physical plan. The second exists
//! to *choose* — between an index scan and a sequential scan, between a hash
//! join and a merge join, between join orders. zql has no indexes, no join
//! reordering, and exactly one implementation of each operator, so there is
//! precisely one physical plan for every logical plan and the second tree would
//! be a transformation from a thing to an identical thing.
//!
//! This is written down rather than left implicit because deliberately-absent
//! structure with a stated reason reads differently from absent structure.

use crate::exec::sort::SortKey;
use crate::plan::expr::{AggSpec, CompiledExpr};
use crate::plan::schema::Schema;
use crate::sources::TableSource;
use crate::sql::ast::JoinKind;
use crate::value::Row;

pub enum Plan {
    /// Rows already in memory — a `SELECT` with no `FROM`.
    Values { schema: Schema, rows: Vec<Row> },

    Scan {
        source: Box<dyn TableSource>,
        schema: Schema,
    },

    Filter {
        input: Box<Plan>,
        predicate: CompiledExpr,
    },

    Project {
        input: Box<Plan>,
        exprs: Vec<CompiledExpr>,
        schema: Schema,
    },

    Limit {
        input: Box<Plan>,
        limit: Option<u64>,
        offset: u64,
    },

    /// `GROUP BY` and the aggregates over it.
    ///
    /// The output is the group keys followed by the aggregate results, which
    /// is the layout every projection and `HAVING` reference is bound against.
    Aggregate {
        input: Box<Plan>,
        keys: Vec<CompiledExpr>,
        aggregates: Vec<AggSpec>,
        schema: Schema,
    },

    Sort {
        input: Box<Plan>,
        keys: Vec<SortKey>,
    },

    Distinct {
        input: Box<Plan>,
    },

    Join {
        left: Box<Plan>,
        right: Box<Plan>,
        condition: CompiledExpr,
        kind: JoinKind,
        schema: Schema,
    },
}

impl Plan {
    /// The shape of the rows this plan produces.
    ///
    /// Only `Project`, `Scan` and `Values` introduce a schema; everything else
    /// passes its input's through unchanged, which is what makes a filter
    /// invisible to the client.
    pub fn schema(&self) -> &Schema {
        match self {
            Plan::Values { schema, .. }
            | Plan::Scan { schema, .. }
            | Plan::Project { schema, .. }
            | Plan::Aggregate { schema, .. }
            | Plan::Join { schema, .. } => schema,

            Plan::Filter { input, .. }
            | Plan::Limit { input, .. }
            | Plan::Sort { input, .. }
            | Plan::Distinct { input } => input.schema(),
        }
    }
}

impl Plan {
    /// Renders the plan tree, one line per operator, for `EXPLAIN`.
    ///
    /// Cheap to write because the plan really is a tree of named operators —
    /// which is the point worth showing. An interpreter that walked the AST
    /// directly would have nothing to print here.
    pub fn explain(&self) -> Vec<String> {
        let mut lines = Vec::new();
        self.describe(0, &mut lines);
        lines
    }

    fn describe(&self, depth: usize, lines: &mut Vec<String>) {
        let indent = "  ".repeat(depth);
        let (label, children): (String, Vec<&Plan>) = match self {
            Plan::Values { rows, .. } => (format!("Values ({} rows)", rows.len()), vec![]),
            Plan::Scan { schema, .. } => {
                let source = schema
                    .columns
                    .first()
                    .and_then(|column| column.qualifier.clone())
                    .unwrap_or_else(|| "?".to_string());
                (format!("Scan on {source}"), vec![])
            }
            Plan::Filter { input, .. } => ("Filter".to_string(), vec![input.as_ref()]),
            Plan::Project { exprs, .. } => {
                (format!("Project ({} columns)", exprs.len()), vec![])
            }
            Plan::Limit { input, limit, offset } => {
                let mut label = "Limit".to_string();
                if let Some(limit) = limit {
                    label.push_str(&format!(" {limit}"));
                }
                if *offset > 0 {
                    label.push_str(&format!(" offset {offset}"));
                }
                (label, vec![input.as_ref()])
            }
            Plan::Aggregate { input, keys, aggregates, .. } => (
                format!(
                    "Aggregate ({} keys, {} aggregates)",
                    keys.len(),
                    aggregates.len()
                ),
                vec![input.as_ref()],
            ),
            Plan::Sort { input, keys } => {
                (format!("Sort ({} keys)", keys.len()), vec![input.as_ref()])
            }
            Plan::Distinct { input } => ("Distinct".to_string(), vec![input.as_ref()]),
            Plan::Join { left, right, kind, .. } => (
                format!("{kind:?} Join (nested loop)"),
                vec![left.as_ref(), right.as_ref()],
            ),
        };

        lines.push(format!("{indent}{label}"));

        // `Project` holds its input like everything else; it is listed here
        // rather than above only to keep the match arm readable.
        if let Plan::Project { input, .. } = self {
            input.describe(depth + 1, lines);
        }
        for child in children {
            child.describe(depth + 1, lines);
        }
    }
}

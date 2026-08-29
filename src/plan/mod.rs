//! Planning: names in, indices out.
//!
//! The binder is the only place that knows both the AST and the catalogue.
//! Above it everything is text the user wrote; below it everything is a column
//! index and a resolved source, which is what lets the executor run without
//! ever performing a lookup by name.

pub mod binder;
pub mod expr;
#[allow(clippy::module_inception)]
// `plan::plan::Plan` trips clippy's module_inception on stable, which did not
// fire on the pinned 1.97.1 this was developed against. Kept rather than
// renamed: `plan/` is the planning *phase* — binder, expressions, schemas —
// and `plan.rs` is the operator tree it produces. Flattening the tree into
// `mod.rs` would bury 160 lines in a file whose job is to declare modules, and
// `plan/tree.rs` would name the file after its shape rather than its meaning.
#[allow(clippy::module_inception)]
pub mod plan;
pub mod schema;

pub use binder::bind;

//! Turning an AST into something runnable.
//!
//! Binding is a distinct phase because `RowDescription` must be written before
//! the first `DataRow`: the full output schema has to be known before execution
//! begins, so names and types are resolved here rather than discovered by the
//! operators as they run.

pub mod binder;
pub mod expr;
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

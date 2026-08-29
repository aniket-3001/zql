//! Turning an AST into something runnable.
//!
//! Binding is a distinct phase because `RowDescription` must be written before
//! the first `DataRow`: the full output schema has to be known before execution
//! begins, so names and types are resolved here rather than discovered by the
//! operators as they run.

pub mod expr;
pub mod schema;

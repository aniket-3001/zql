//! zql — open any `.db` file and query it with SQL.
//!
//! A read-only query engine that speaks the PostgreSQL v3 wire protocol, so
//! every Postgres client already on the machine can talk to it — but with files
//! behind it instead of a database. Built against an empty `[dependencies]`.
//!
//! The crate is split into a library and a thin binary so that the golden-query
//! and adversarial-input corpora in `tests/` can drive the engine directly,
//! rather than only through a socket.
//!
//! # Shape
//!
//! ```text
//! TCP :5432 → server → sql (lex, parse) → plan (bind) → exec → wire
//! ```
//!
//! The invariant that shapes all of it: `RowDescription` must be written
//! *before* the first `DataRow`, so the full output schema has to be known
//! before execution begins. That is why binding is a distinct phase and not
//! something the operators discover as they run.

pub mod datetime;
pub mod error;
pub mod plan;
pub mod server;
pub mod sql;
pub mod value;
pub mod wire;

//! SQL text to an abstract syntax tree.
//!
//! The grammar this implements is frozen in `docs/SQL-SUBSET.md` and is
//! deliberately small. A SQL parser is not hard; it is *unbounded*, and every
//! feature suggests two more. The subset is the schedule.

pub mod ast;
pub mod lexer;
pub mod parser;
pub mod token;

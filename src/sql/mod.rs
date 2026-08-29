//! SQL text to an abstract syntax tree.
//!
//! The grammar this implements is frozen in `docs/SQL-SUBSET.md` and is
//! deliberately small. A SQL parser is not hard; it is *unbounded*, and every
//! feature suggests two more. The subset is the schedule.

pub mod ast;
pub mod lexer;
pub mod parser;
pub mod token;

pub use parser::parse;

use crate::error::Result;
use crate::sql::token::TokenKind;

/// Whether a query carries no statement at all — it is empty, whitespace, or
/// only a comment.
///
/// This is a real case rather than an edge one: `psql` sends the text of a
/// bare `;` or a lone `-- note` straight to the server, and the correct answer
/// is `EmptyQueryResponse`, not a syntax error.
pub fn is_blank(sql: &str) -> Result<bool> {
    let tokens = lexer::tokenize(sql)?;
    Ok(match tokens.as_slice() {
        [only] => only.kind == TokenKind::Eof,
        [first, second] => {
            first.kind == TokenKind::Symbol(token::Symbol::Semicolon)
                && second.kind == TokenKind::Eof
        }
        _ => false,
    })
}

//! The one error type, and its SQLSTATE mapping.
//!
//! `ZqlError` is shaped so that it maps *directly* onto the fields of a
//! PostgreSQL `ErrorResponse` message. There is deliberately no translation
//! layer between "an error in the engine" and "an error on the wire": every
//! field here is a field `psql` knows how to render, including the caret line,
//! which comes free once `position` is populated.

use std::fmt;

/// A five-character SQLSTATE code.
///
/// Only the codes zql can actually produce are listed. Keeping this a closed
/// enum rather than a `&str` means an unmapped error is a compile error rather
/// than a typo that reaches a client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqlState {
    /// `42601` — the parser could not make sense of the input.
    SyntaxError,
    /// `42P01` — no such table or table function.
    UndefinedTable,
    /// `42703` — no such column.
    UndefinedColumn,
    /// `42883` — no such function.
    UndefinedFunction,
    /// `42804` — the types do not line up and zql will not guess.
    DatatypeMismatch,
    /// `0A000` — valid SQL that zql deliberately does not implement.
    FeatureNotSupported,
    /// `22012` — division (or modulo) by zero.
    DivisionByZero,
    /// `22003` — an arithmetic result does not fit in an `i64`.
    NumericValueOutOfRange,
    /// `54000` — an in-memory operator hit its documented row ceiling.
    ProgramLimitExceeded,
    /// `54001` — the query nests more deeply than zql will walk.
    StatementTooComplex,
    /// `58030` — a source file could not be read, or is not what it claims.
    IoError,
    /// `57014` — the client sent a CancelRequest.
    QueryCanceled,
    /// `XX000` — a bug in zql. Should never reach a client.
    InternalError,
}

impl SqlState {
    /// The five-character code as it goes into the `C` field of `ErrorResponse`.
    pub fn code(self) -> &'static str {
        match self {
            SqlState::SyntaxError => "42601",
            SqlState::UndefinedTable => "42P01",
            SqlState::UndefinedColumn => "42703",
            SqlState::UndefinedFunction => "42883",
            SqlState::DatatypeMismatch => "42804",
            SqlState::FeatureNotSupported => "0A000",
            SqlState::DivisionByZero => "22012",
            SqlState::NumericValueOutOfRange => "22003",
            SqlState::ProgramLimitExceeded => "54000",
            SqlState::StatementTooComplex => "54001",
            SqlState::IoError => "58030",
            SqlState::QueryCanceled => "57014",
            SqlState::InternalError => "XX000",
        }
    }
}

/// An error on its way to becoming an `ErrorResponse`.
#[derive(Debug, Clone)]
pub struct ZqlError {
    pub state: SqlState,
    pub message: String,
    /// Goes into the `D` field. Use for the second sentence, not the first.
    pub detail: Option<String>,
    /// Goes into the `H` field. Reserved for actionable suggestions.
    pub hint: Option<String>,
    /// 1-based byte offset into the query text. `psql` renders this as the
    /// `LINE 1: ... ^` caret block, so populating it is worth one field.
    pub position: Option<u32>,
}

impl ZqlError {
    pub fn new(state: SqlState, message: impl Into<String>) -> Self {
        ZqlError {
            state,
            message: message.into(),
            detail: None,
            hint: None,
            position: None,
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// `position` is 1-based, as the protocol requires.
    pub fn at(mut self, position: u32) -> Self {
        self.position = Some(position);
        self
    }

    pub fn syntax(message: impl Into<String>) -> Self {
        ZqlError::new(SqlState::SyntaxError, message)
    }

    /// The refusal path for valid SQL zql does not implement. The message
    /// always names the feature, so the limitation reads as a decision.
    ///
    /// Phrased "zql does not support X" rather than "X is not supported"
    /// because the feature names are a mix of singular and plural — "a
    /// subquery", "window functions" — and only this word order agrees with
    /// both.
    pub fn unsupported(feature: impl fmt::Display) -> Self {
        ZqlError::new(
            SqlState::FeatureNotSupported,
            format!("zql does not support {feature}"),
        )
    }

    pub fn internal(message: impl Into<String>) -> Self {
        ZqlError::new(SqlState::InternalError, message)
    }

    pub fn canceled() -> Self {
        ZqlError::new(
            SqlState::QueryCanceled,
            "canceling statement due to user request",
        )
    }

    /// Source-file IO. Deliberately *not* a blanket `From<io::Error>`: socket
    /// errors and data-file errors are different things, and conflating them
    /// would let a dead client masquerade as a corrupt database.
    pub fn io(path: &str, err: &std::io::Error) -> Self {
        ZqlError::new(SqlState::IoError, format!("cannot read {path}: {err}"))
    }
}

impl fmt::Display for ZqlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ERROR: {} ({})", self.message, self.state.code())
    }
}

impl std::error::Error for ZqlError {}

/// Every fallible path in the engine returns this.
pub type Result<T> = std::result::Result<T, ZqlError>;

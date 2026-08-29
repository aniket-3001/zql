//! Tokens and the keyword table.
//!
//! Keywords are reserved, including the ones zql does not implement. That is
//! deliberate: reserving `INSERT` means the parser can answer "INSERT is not
//! supported by zql, which is read-only" instead of "syntax error near
//! INSERT", and the difference between those two messages is the difference
//! between a stated boundary and a broken program. A quoted identifier can
//! still shadow any of them.

/// A reserved word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keyword {
    // --- Implemented ---
    Select,
    Distinct,
    From,
    Where,
    Group,
    By,
    Having,
    Order,
    Limit,
    Offset,
    As,
    Join,
    Inner,
    Left,
    Outer,
    On,
    Asc,
    Desc,
    Nulls,
    First,
    Last,
    And,
    Or,
    Not,
    Like,
    In,
    Between,
    Is,
    Null,
    True,
    False,
    Case,
    When,
    Then,
    Else,
    End,
    Cast,
    Show,
    Explain,

    // --- Reserved so that the refusal can name the feature ---
    Insert,
    Update,
    Delete,
    Create,
    Drop,
    Alter,
    Truncate,
    With,
    Recursive,
    Union,
    Intersect,
    Except,
    Right,
    Full,
    Cross,
    Natural,
    Using,
    Over,
    Partition,
    Window,
    Begin,
    Commit,
    Rollback,
    Transaction,
    Prepare,
    Execute,
    Declare,
    Fetch,
    Cursor,
    Copy,
    Values,
    Into,
}

impl Keyword {
    /// Looks up an already-lowercased word.
    pub fn lookup(word: &str) -> Option<Keyword> {
        use Keyword::*;
        Some(match word {
            "select" => Select,
            "distinct" => Distinct,
            "from" => From,
            "where" => Where,
            "group" => Group,
            "by" => By,
            "having" => Having,
            "order" => Order,
            "limit" => Limit,
            "offset" => Offset,
            "as" => As,
            "join" => Join,
            "inner" => Inner,
            "left" => Left,
            "outer" => Outer,
            "on" => On,
            "asc" => Asc,
            "desc" => Desc,
            "nulls" => Nulls,
            "first" => First,
            "last" => Last,
            "and" => And,
            "or" => Or,
            "not" => Not,
            "like" => Like,
            "in" => In,
            "between" => Between,
            "is" => Is,
            "null" => Null,
            "true" => True,
            "false" => False,
            "case" => Case,
            "when" => When,
            "then" => Then,
            "else" => Else,
            "end" => End,
            "cast" => Cast,
            "show" => Show,
            "explain" => Explain,

            "insert" => Insert,
            "update" => Update,
            "delete" => Delete,
            "create" => Create,
            "drop" => Drop,
            "alter" => Alter,
            "truncate" => Truncate,
            "with" => With,
            "recursive" => Recursive,
            "union" => Union,
            "intersect" => Intersect,
            "except" => Except,
            "right" => Right,
            "full" => Full,
            "cross" => Cross,
            "natural" => Natural,
            "using" => Using,
            "over" => Over,
            "partition" => Partition,
            "window" => Window,
            "begin" => Begin,
            "commit" => Commit,
            "rollback" => Rollback,
            "transaction" => Transaction,
            "prepare" => Prepare,
            "execute" => Execute,
            "declare" => Declare,
            "fetch" => Fetch,
            "cursor" => Cursor,
            "copy" => Copy,
            "values" => Values,
            "into" => Into,

            _ => return None,
        })
    }

    /// The word as it is spelled in an error message.
    pub fn as_str(self) -> &'static str {
        use Keyword::*;
        match self {
            Select => "SELECT",
            Distinct => "DISTINCT",
            From => "FROM",
            Where => "WHERE",
            Group => "GROUP",
            By => "BY",
            Having => "HAVING",
            Order => "ORDER",
            Limit => "LIMIT",
            Offset => "OFFSET",
            As => "AS",
            Join => "JOIN",
            Inner => "INNER",
            Left => "LEFT",
            Outer => "OUTER",
            On => "ON",
            Asc => "ASC",
            Desc => "DESC",
            Nulls => "NULLS",
            First => "FIRST",
            Last => "LAST",
            And => "AND",
            Or => "OR",
            Not => "NOT",
            Like => "LIKE",
            In => "IN",
            Between => "BETWEEN",
            Is => "IS",
            Null => "NULL",
            True => "TRUE",
            False => "FALSE",
            Case => "CASE",
            When => "WHEN",
            Then => "THEN",
            Else => "ELSE",
            End => "END",
            Cast => "CAST",
            Show => "SHOW",
            Explain => "EXPLAIN",
            Insert => "INSERT",
            Update => "UPDATE",
            Delete => "DELETE",
            Create => "CREATE",
            Drop => "DROP",
            Alter => "ALTER",
            Truncate => "TRUNCATE",
            With => "WITH",
            Recursive => "RECURSIVE",
            Union => "UNION",
            Intersect => "INTERSECT",
            Except => "EXCEPT",
            Right => "RIGHT",
            Full => "FULL",
            Cross => "CROSS",
            Natural => "NATURAL",
            Using => "USING",
            Over => "OVER",
            Partition => "PARTITION",
            Window => "WINDOW",
            Begin => "BEGIN",
            Commit => "COMMIT",
            Rollback => "ROLLBACK",
            Transaction => "TRANSACTION",
            Prepare => "PREPARE",
            Execute => "EXECUTE",
            Declare => "DECLARE",
            Fetch => "FETCH",
            Cursor => "CURSOR",
            Copy => "COPY",
            Values => "VALUES",
            Into => "INTO",
        }
    }

    /// If this word begins a statement zql deliberately does not implement,
    /// the feature it names and why it is absent.
    ///
    /// The reason travels separately because it belongs in the `DETAIL` field
    /// rather than the message: a refusal that explains itself reads as a scope
    /// decision, and one that does not reads as an unfinished corner.
    pub fn unsupported_feature(self) -> Option<Unsupported> {
        use Keyword::*;
        let (feature, reason) = match self {
            Insert | Update | Delete => ("data modification", Some(READ_ONLY)),
            Create | Drop | Alter | Truncate => ("schema changes", Some(READ_ONLY)),
            With | Recursive => ("common table expressions", None),
            Union => ("UNION", Some(SCHEMA_UNIFICATION)),
            Intersect => ("INTERSECT", Some(SCHEMA_UNIFICATION)),
            Except => ("EXCEPT", Some(SCHEMA_UNIFICATION)),
            Over | Partition | Window => ("window functions", None),
            Begin | Commit | Rollback | Transaction => (
                "transactions",
                Some("zql is read-only, so there is nothing to transact"),
            ),
            Prepare | Execute => ("prepared statements", Some(SIMPLE_QUERY_ONLY)),
            Declare | Fetch | Cursor => ("cursors", Some(SIMPLE_QUERY_ONLY)),
            Copy => ("COPY", None),
            Values => ("VALUES lists", None),
            Into => ("SELECT INTO", Some(READ_ONLY)),
            _ => return None,
        };
        Some(Unsupported { feature, reason })
    }
}

const READ_ONLY: &str = "zql is read-only and never writes to the files it reads";
const SCHEMA_UNIFICATION: &str = "unifying the schemas of two result sets is not implemented";
const SIMPLE_QUERY_ONLY: &str = "zql implements the simple query protocol only";

/// A feature zql deliberately does not implement, and why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unsupported {
    pub feature: &'static str,
    pub reason: Option<&'static str>,
}

impl Unsupported {
    /// Builds the `0A000` error, attaching the reason as `DETAIL`.
    pub fn into_error(self, position: u32) -> crate::error::ZqlError {
        let error = crate::error::ZqlError::unsupported(self.feature).at(position);
        match self.reason {
            Some(reason) => error.with_detail(reason),
            None => error,
        }
    }
}

/// The keywords zql implements, for the "did you mean" hint.

/// Operators and punctuation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Symbol {
    Comma,
    LParen,
    RParen,
    Dot,
    Semicolon,
    Star,
    Plus,
    Minus,
    Slash,
    Percent,
    /// `||` — string concatenation, never logical-or.
    Concat,
    Eq,
    NotEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
}

impl Symbol {
    pub fn as_str(self) -> &'static str {
        use Symbol::*;
        match self {
            Comma => ",",
            LParen => "(",
            RParen => ")",
            Dot => ".",
            Semicolon => ";",
            Star => "*",
            Plus => "+",
            Minus => "-",
            Slash => "/",
            Percent => "%",
            Concat => "||",
            Eq => "=",
            NotEq => "<>",
            Lt => "<",
            LtEq => "<=",
            Gt => ">",
            GtEq => ">=",
        }
    }
}

/// What a token is.
#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    Keyword(Keyword),
    /// An identifier. Unquoted ones arrive folded to lower case; quoted ones
    /// keep the case they were written with.
    Identifier,
    Number,
    String,
    Symbol(Symbol),
    Eof,
}

/// A token, with enough context to build an error message that points at it.
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    /// The payload: an identifier's name, a string's *decoded* content, or a
    /// number's digits. Empty for symbols and keywords.
    pub text: String,
    /// 1-based offset into the query, which is what the `P` field of
    /// `ErrorResponse` expects and what `psql` turns into a caret line.
    pub position: u32,
}

impl Token {
    /// How this token is referred to in an error message.
    pub fn describe(&self) -> String {
        match &self.kind {
            TokenKind::Keyword(keyword) => keyword.as_str().to_string(),
            TokenKind::Identifier => format!("\"{}\"", self.text),
            TokenKind::Number => self.text.clone(),
            TokenKind::String => format!("'{}'", self.text),
            TokenKind::Symbol(symbol) => symbol.as_str().to_string(),
            TokenKind::Eof => "end of input".to_string(),
        }
    }

    pub fn is_keyword(&self, keyword: Keyword) -> bool {
        self.kind == TokenKind::Keyword(keyword)
    }

    pub fn is_symbol(&self, symbol: Symbol) -> bool {
        self.kind == TokenKind::Symbol(symbol)
    }

    pub fn is_eof(&self) -> bool {
        self.kind == TokenKind::Eof
    }
}

//! The abstract syntax tree.
//!
//! This is a faithful shape of the frozen grammar in `docs/SQL-SUBSET.md` §2
//! and nothing more. Anything the grammar does not admit has no node here,
//! which is what keeps "while I'm here, let me also support…" from becoming a
//! rewrite: adding a feature means changing this file first, deliberately.

/// A complete statement. One per `Query` message.
///
/// `Select` is boxed because it dwarfs the other two arms — a `SHOW SOURCES`
/// would otherwise carry several hundred bytes of unused `Select` around with
/// it, and every `Statement` moved would copy them.
#[derive(Debug, Clone, PartialEq)]
pub enum Statement {
    Select(Box<Select>),
    /// `SHOW SOURCES` — the discoverability answer to "what can I even query?"
    ShowSources,
    /// `EXPLAIN <select>` — prints the plan tree as rows.
    Explain(Box<Select>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Select {
    pub distinct: bool,
    pub projection: Projection,
    /// `SELECT 1` has no `FROM`, which is how a client tests a connection.
    pub from: Option<FromItem>,
    pub filter: Option<Expr>,
    pub group_by: Vec<Expr>,
    pub having: Option<Expr>,
    pub order_by: Vec<OrderKey>,
    pub limit: Option<u64>,
    pub offset: Option<u64>,
}

/// The grammar admits `*` **or** a list of expressions, not a mixture of the
/// two. Modelling that as an enum rather than a list containing a wildcard
/// makes the restriction unrepresentable rather than merely unchecked.
#[derive(Debug, Clone, PartialEq)]
pub enum Projection {
    Wildcard,
    Items(Vec<ProjectionItem>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProjectionItem {
    pub expr: Expr,
    pub alias: Option<String>,
}

/// A `FROM` clause: one source, optionally joined to one other.
///
/// Exactly one join is representable, which is the documented limit. A third
/// source needs a nested query, and zql has no subqueries — so the shape of
/// this type states the limitation without needing a check.
#[derive(Debug, Clone, PartialEq)]
pub struct FromItem {
    pub source: Source,
    pub alias: Option<String>,
    pub join: Option<Join>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Join {
    pub kind: JoinKind,
    pub source: Source,
    pub alias: Option<String>,
    pub on: Expr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinKind {
    Inner,
    Left,
}

/// A bare name (a virtual table) or a table function call.
#[derive(Debug, Clone, PartialEq)]
pub struct Source {
    pub name: String,
    /// `None` for a bare name such as `files`; `Some` for a call such as
    /// `csv('x.csv')`, including a call with no arguments.
    pub args: Option<Vec<Literal>>,
    pub position: u32,
}

/// Source arguments are literals, not general expressions.
///
/// That is not a simplification — it is the reason case folding never becomes
/// a problem. A SQLite table name is case-sensitive as stored, so it arrives as
/// a string literal, which the lexer leaves alone, rather than as an identifier,
/// which it would fold to lower case.
#[derive(Debug, Clone, PartialEq)]
pub enum Literal {
    Null,
    Bool(bool),
    Int(i64),
    Real(f64),
    String(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct OrderKey {
    pub expr: Expr,
    pub descending: bool,
    /// `None` means the default: ascending sorts NULLs last, descending sorts
    /// them first, matching Postgres.
    pub nulls: Option<NullsOrder>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NullsOrder {
    First,
    Last,
}

/// An expression, carrying the position of the token that started it.
///
/// The position is on every node rather than only the ones that can fail,
/// because "unknown column" and "cannot compare" are both worth a caret and
/// which node reports an error is not known when the tree is built.
#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub position: u32,
}

impl Expr {
    pub fn new(kind: ExprKind, position: u32) -> Self {
        Expr { kind, position }
    }

    pub fn boxed(self) -> Box<Expr> {
        Box::new(self)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
    Literal(Literal),
    Column {
        /// The `f` in `f.size`.
        qualifier: Option<String>,
        name: String,
    },
    Unary {
        op: UnaryOp,
        expr: Box<Expr>,
    },
    Binary {
        op: BinaryOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    IsNull {
        expr: Box<Expr>,
        negated: bool,
    },
    Like {
        expr: Box<Expr>,
        pattern: Box<Expr>,
        negated: bool,
    },
    InList {
        expr: Box<Expr>,
        list: Vec<Literal>,
        negated: bool,
    },
    Between {
        expr: Box<Expr>,
        low: Box<Expr>,
        high: Box<Expr>,
        negated: bool,
    },
    Function(FunctionCall),
    Cast {
        expr: Box<Expr>,
        ty: String,
    },
    Case {
        /// `WHEN condition THEN result`, in order.
        branches: Vec<CaseBranch>,
        else_result: Option<Box<Expr>>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct CaseBranch {
    pub condition: Expr,
    pub result: Expr,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FunctionCall {
    /// Folded to lower case; the function set is closed and all-ASCII.
    pub name: String,
    pub args: Vec<Expr>,
    /// `COUNT(DISTINCT x)`.
    pub distinct: bool,
    /// `COUNT(*)`, which is the only place a star may appear as an argument.
    pub star: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    /// Unary `+`, which is a no-op kept for symmetry with what users type.
    Plus,
    Not,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Or,
    And,
    Eq,
    NotEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    /// `||` — string concatenation.
    Concat,
    Add,
    Sub,
    Mul,
    Div,
    Mod,
}

impl BinaryOp {
    pub fn as_str(self) -> &'static str {
        use BinaryOp::*;
        match self {
            Or => "OR",
            And => "AND",
            Eq => "=",
            NotEq => "<>",
            Lt => "<",
            LtEq => "<=",
            Gt => ">",
            GtEq => ">=",
            Concat => "||",
            Add => "+",
            Sub => "-",
            Mul => "*",
            Div => "/",
            Mod => "%",
        }
    }
}

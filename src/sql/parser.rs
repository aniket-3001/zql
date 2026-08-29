//! The statement parser and the Pratt expression parser.
//!
//! # Why Pratt
//!
//! Precedence climbing puts the whole of the expression grammar — eleven
//! precedence levels — into one function and a binding-power table. The
//! recursive-descent alternative is one function per level, each calling the
//! next, and changing the precedence of an operator means moving code between
//! functions. Here it means editing a number in [`infix_binding_power`]. The
//! table carries the complexity; the code does not.
//!
//! # The refusal path is a feature
//!
//! Everything in `docs/SQL-SUBSET.md` §7 is parsed far enough to be *named* and
//! then refused with `0A000`. `INSERT` does not produce "syntax error near
//! INSERT"; it produces "data modification (zql is read-only) is not supported
//! by zql". That is the difference between a stated boundary and an unfinished
//! program, and it costs a keyword table entry.

use crate::error::{Result, ZqlError};
use crate::sql::ast::*;
use crate::sql::lexer::tokenize;
use crate::sql::token::{did_you_mean, Keyword, Symbol, Token, TokenKind};

/// Parses one statement. A trailing semicolon is optional; a *second*
/// statement is refused, because the protocol gives one `CommandComplete` per
/// query and two result sets have nowhere to go.
pub fn parse(sql: &str) -> Result<Statement> {
    let tokens = tokenize(sql)?;
    let mut parser = Parser::new(tokens);
    let statement = parser.parse_statement()?;

    // Whether a semicolon was consumed decides which of two very different
    // answers the leftovers deserve: text after `;` is a second statement,
    // which zql refuses; text with no `;` in front of it is a typo.
    let terminated = parser.eat_symbol(Symbol::Semicolon);
    if !parser.peek().is_eof() {
        return Err(if terminated {
            ZqlError::unsupported("more than one statement per query")
                .at(parser.peek().position)
                .with_hint("send each statement as its own query")
        } else {
            parser.unexpected("end of statement")
        });
    }
    Ok(statement)
}

/// Returned when the cursor somehow outruns the token vector. Never expected;
/// existing means the parser has no way to panic.
static END_OF_INPUT: Token = Token {
    kind: TokenKind::Eof,
    text: String::new(),
    position: 1,
};

struct Parser {
    tokens: Vec<Token>,
    index: usize,
}

impl Parser {
    fn new(tokens: Vec<Token>) -> Self {
        Parser { tokens, index: 0 }
    }

    // ---------------------------------------------------------------- cursor

    /// The token under the cursor.
    ///
    /// `tokenize` always ends its output with `Eof`, so the vector is never
    /// empty and the cursor is clamped by `advance` — but rather than stake a
    /// panic on that, an exhausted cursor yields a synthetic `Eof`, which every
    /// caller already handles as "the input ended here".
    fn peek(&self) -> &Token {
        self.tokens
            .get(self.index)
            .or_else(|| self.tokens.last())
            .unwrap_or(&END_OF_INPUT)
    }

    fn advance(&mut self) -> Token {
        let token = self.peek().clone();
        if self.index < self.tokens.len().saturating_sub(1) {
            self.index += 1;
        }
        token
    }

    fn eat_keyword(&mut self, keyword: Keyword) -> bool {
        if self.peek().is_keyword(keyword) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn eat_symbol(&mut self, symbol: Symbol) -> bool {
        if self.peek().is_symbol(symbol) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn expect_keyword(&mut self, keyword: Keyword) -> Result<()> {
        if self.eat_keyword(keyword) {
            Ok(())
        } else {
            Err(self.unexpected(keyword.as_str()))
        }
    }

    fn expect_symbol(&mut self, symbol: Symbol) -> Result<()> {
        if self.eat_symbol(symbol) {
            Ok(())
        } else {
            Err(self.unexpected(symbol.as_str()))
        }
    }

    /// The one place a syntax error is built, so every one of them carries a
    /// position and, where it can, a suggestion.
    fn unexpected(&self, expected: &str) -> ZqlError {
        let token = self.peek();
        let mut error = ZqlError::syntax(format!("syntax error at or near {}", token.describe()))
            .at(token.position)
            .with_detail(format!("expected {expected}"));

        // `SELECT * form files` is the canonical typo, and the hint costs one
        // edit-distance check against the keyword table.
        if token.kind == TokenKind::Identifier {
            if let Some(suggestion) = did_you_mean(&token.text) {
                error = error.with_hint(format!("did you mean {suggestion}?"));
            }
        } else if let TokenKind::Keyword(keyword) = token.kind {
            error = error.with_hint(format!(
                "{} is a reserved word; write it as \"{}\" to use it as an identifier",
                keyword.as_str(),
                token.text
            ));
        }
        error
    }

    // ------------------------------------------------------------ statements

    fn parse_statement(&mut self) -> Result<Statement> {
        let token = self.peek().clone();

        if let TokenKind::Keyword(keyword) = token.kind {
            // Refuse by name before trying to parse. A `WITH` that fails as a
            // syntax error tells the user nothing about why it will never work.
            if let Some(unsupported) = keyword.unsupported_feature() {
                return Err(unsupported.into_error(token.position));
            }

            match keyword {
                Keyword::Select => {
                    return Ok(Statement::Select(Box::new(self.parse_select()?)))
                }
                Keyword::Show => return self.parse_show(),
                Keyword::Explain => {
                    self.advance();
                    let select = self.parse_select()?;
                    return Ok(Statement::Explain(Box::new(select)));
                }
                _ => {}
            }
        }

        Err(self.unexpected("SELECT, SHOW or EXPLAIN"))
    }

    /// `SHOW SOURCES`. `SOURCES` is not reserved — it is matched as an
    /// identifier, so nobody loses the word as a column name.
    fn parse_show(&mut self) -> Result<Statement> {
        self.advance(); // SHOW
        let token = self.peek().clone();
        if token.kind == TokenKind::Identifier && token.text == "sources" {
            self.advance();
            Ok(Statement::ShowSources)
        } else {
            Err(self.unexpected("SOURCES"))
        }
    }

    fn parse_select(&mut self) -> Result<Select> {
        self.expect_keyword(Keyword::Select)?;
        let distinct = self.eat_keyword(Keyword::Distinct);
        let projection = self.parse_projection()?;

        let from = if self.eat_keyword(Keyword::From) {
            Some(self.parse_from_item()?)
        } else {
            None
        };

        let filter = if self.eat_keyword(Keyword::Where) {
            Some(self.parse_expr(0)?)
        } else {
            None
        };

        let group_by = if self.eat_keyword(Keyword::Group) {
            self.expect_keyword(Keyword::By)?;
            self.parse_expr_list()?
        } else {
            Vec::new()
        };

        let having = if self.eat_keyword(Keyword::Having) {
            Some(self.parse_expr(0)?)
        } else {
            None
        };

        let order_by = if self.eat_keyword(Keyword::Order) {
            self.expect_keyword(Keyword::By)?;
            self.parse_order_keys()?
        } else {
            Vec::new()
        };

        // Postgres accepts these in either order; so does zql, because a user
        // who writes them backwards has made no mistake worth an error.
        let mut limit = None;
        let mut offset = None;
        loop {
            if limit.is_none() && self.eat_keyword(Keyword::Limit) {
                limit = Some(self.parse_unsigned_integer("LIMIT")?);
            } else if offset.is_none() && self.eat_keyword(Keyword::Offset) {
                offset = Some(self.parse_unsigned_integer("OFFSET")?);
            } else {
                break;
            }
        }

        // A set operator here parses cleanly and means something zql does not
        // do, so it is named rather than reported as a stray word.
        if let TokenKind::Keyword(keyword) = self.peek().kind {
            if let Some(unsupported) = keyword.unsupported_feature() {
                return Err(unsupported.into_error(self.peek().position));
            }
        }

        Ok(Select {
            distinct,
            projection,
            from,
            filter,
            group_by,
            having,
            order_by,
            limit,
            offset,
        })
    }

    fn parse_projection(&mut self) -> Result<Projection> {
        if self.peek().is_symbol(Symbol::Star) {
            self.advance();
            if self.peek().is_symbol(Symbol::Comma) {
                return Err(ZqlError::syntax(
                    "`*` cannot be combined with other output columns",
                )
                .at(self.peek().position)
                .with_hint("select `*` on its own, or name every column"));
            }
            return Ok(Projection::Wildcard);
        }

        let mut items = Vec::new();
        loop {
            let expr = self.parse_expr(0)?;
            let alias = self.parse_optional_alias()?;
            items.push(ProjectionItem { expr, alias });
            if !self.eat_symbol(Symbol::Comma) {
                break;
            }
        }
        Ok(Projection::Items(items))
    }

    /// `[AS] identifier`. Without `AS` an alias is just an identifier, and no
    /// ambiguity arises because every clause that could follow begins with a
    /// reserved word.
    fn parse_optional_alias(&mut self) -> Result<Option<String>> {
        if self.eat_keyword(Keyword::As) {
            let token = self.advance();
            return match token.kind {
                TokenKind::Identifier => Ok(Some(token.text)),
                _ => Err(ZqlError::syntax(format!(
                    "expected an alias after AS, found {}",
                    token.describe()
                ))
                .at(token.position)),
            };
        }

        if self.peek().kind == TokenKind::Identifier {
            return Ok(Some(self.advance().text));
        }
        Ok(None)
    }

    fn parse_from_item(&mut self) -> Result<FromItem> {
        let source = self.parse_source()?;
        let alias = self.parse_optional_alias()?;
        let join = self.parse_optional_join()?;

        if self.peek().is_keyword(Keyword::Join) {
            return Err(ZqlError::unsupported("joining three or more sources")
                .at(self.peek().position)
                .with_hint("zql joins exactly two sources in one query"));
        }

        Ok(FromItem {
            source,
            alias,
            join,
        })
    }

    fn parse_optional_join(&mut self) -> Result<Option<Join>> {
        let token = self.peek().clone();
        let kind = match token.kind {
            TokenKind::Keyword(Keyword::Join) => JoinKind::Inner,
            TokenKind::Keyword(Keyword::Inner) => {
                self.advance();
                JoinKind::Inner
            }
            TokenKind::Keyword(Keyword::Left) => {
                self.advance();
                self.eat_keyword(Keyword::Outer);
                JoinKind::Left
            }
            // These are all real SQL and all deliberately absent. `RIGHT` in
            // particular has a one-line workaround worth putting in the hint.
            TokenKind::Keyword(Keyword::Right | Keyword::Full) => {
                return Err(ZqlError::unsupported(format!(
                    "{} OUTER JOIN",
                    token.describe()
                ))
                .at(token.position)
                .with_hint("swap the two sources and use LEFT JOIN"))
            }
            TokenKind::Keyword(Keyword::Cross | Keyword::Natural) => {
                return Err(
                    ZqlError::unsupported(format!("{} JOIN", token.describe())).at(token.position)
                )
            }
            _ => return Ok(None),
        };

        self.expect_keyword(Keyword::Join)?;
        let source = self.parse_source()?;
        let alias = self.parse_optional_alias()?;

        if self.peek().is_keyword(Keyword::Using) {
            return Err(ZqlError::unsupported("JOIN ... USING")
                .at(self.peek().position)
                .with_hint("write the equality out with ON"));
        }
        self.expect_keyword(Keyword::On)?;
        let on = self.parse_expr(0)?;

        Ok(Some(Join {
            kind,
            source,
            alias,
            on,
        }))
    }

    /// A bare name, or a name with a parenthesised argument list.
    fn parse_source(&mut self) -> Result<Source> {
        let token = self.advance();
        if token.kind != TokenKind::Identifier {
            return Err(ZqlError::syntax(format!(
                "expected a table or table function, found {}",
                token.describe()
            ))
            .at(token.position));
        }

        let args = if self.eat_symbol(Symbol::LParen) {
            let mut args = Vec::new();
            if !self.peek().is_symbol(Symbol::RParen) {
                loop {
                    args.push(self.parse_source_argument()?);
                    if !self.eat_symbol(Symbol::Comma) {
                        break;
                    }
                }
            }
            self.expect_symbol(Symbol::RParen)?;
            Some(args)
        } else {
            None
        };

        Ok(Source {
            name: token.text,
            args,
            position: token.position,
        })
    }

    /// Source arguments are literals. See [`Literal`] for why that is a design
    /// decision about case folding rather than a shortcut.
    fn parse_source_argument(&mut self) -> Result<Literal> {
        let token = self.peek().clone();
        match self.parse_literal() {
            Some(literal) => literal,
            None => Err(ZqlError::syntax(format!(
                "source arguments must be literals, found {}",
                token.describe()
            ))
            .at(token.position)
            .with_hint("file and table names are written as 'quoted strings'")),
        }
    }

    fn parse_expr_list(&mut self) -> Result<Vec<Expr>> {
        let mut exprs = vec![self.parse_expr(0)?];
        while self.eat_symbol(Symbol::Comma) {
            exprs.push(self.parse_expr(0)?);
        }
        Ok(exprs)
    }

    fn parse_order_keys(&mut self) -> Result<Vec<OrderKey>> {
        let mut keys = Vec::new();
        loop {
            let expr = self.parse_expr(0)?;
            let descending = if self.eat_keyword(Keyword::Desc) {
                true
            } else {
                self.eat_keyword(Keyword::Asc);
                false
            };
            let nulls = if self.eat_keyword(Keyword::Nulls) {
                if self.eat_keyword(Keyword::First) {
                    Some(NullsOrder::First)
                } else if self.eat_keyword(Keyword::Last) {
                    Some(NullsOrder::Last)
                } else {
                    return Err(self.unexpected("FIRST or LAST"));
                }
            } else {
                None
            };
            keys.push(OrderKey {
                expr,
                descending,
                nulls,
            });
            if !self.eat_symbol(Symbol::Comma) {
                break;
            }
        }
        Ok(keys)
    }

    /// `LIMIT` and `OFFSET` take a plain integer, per the grammar — not an
    /// expression, because there is nothing sensible to evaluate it against.
    fn parse_unsigned_integer(&mut self, clause: &str) -> Result<u64> {
        // Caught before the number: a leading `-` is its own token, so without
        // this the message reads "LIMIT needs a number, found -".
        if self.peek().is_symbol(Symbol::Minus) {
            return Err(ZqlError::syntax(format!("{clause} must not be negative"))
                .at(self.peek().position));
        }
        let token = self.advance();
        if token.kind != TokenKind::Number {
            return Err(ZqlError::syntax(format!(
                "{clause} needs a number, found {}",
                token.describe()
            ))
            .at(token.position));
        }
        token.text.parse::<u64>().map_err(|_| {
            ZqlError::syntax(format!("{clause} needs a non-negative whole number"))
                .at(token.position)
        })
    }

    // ----------------------------------------------------------- expressions

    /// The Pratt loop.
    ///
    /// `min_bp` is the binding power the caller has already claimed: the loop
    /// keeps absorbing operators that bind tighter than that and stops at the
    /// first one that does not, which is what makes `a + b * c` and
    /// `a * b + c` come out differently from the same code.
    fn parse_expr(&mut self, min_bp: u8) -> Result<Expr> {
        let mut left = self.parse_prefix()?;

        loop {
            let token = self.peek().clone();

            // `NOT` in an infix position introduces one of the negated forms.
            if token.is_keyword(Keyword::Not) {
                let (lbp, _) = PREDICATE_BP;
                if lbp <= min_bp {
                    break;
                }
                self.advance();
                left = self.parse_negated_predicate(left)?;
                continue;
            }

            if token.is_keyword(Keyword::Is) {
                let (lbp, _) = PREDICATE_BP;
                if lbp <= min_bp {
                    break;
                }
                self.advance();
                let negated = self.eat_keyword(Keyword::Not);
                self.expect_keyword(Keyword::Null)?;
                let position = left.position;
                left = Expr::new(
                    ExprKind::IsNull {
                        expr: left.boxed(),
                        negated,
                    },
                    position,
                );
                continue;
            }

            if matches!(
                token.kind,
                TokenKind::Keyword(Keyword::Like | Keyword::In | Keyword::Between)
            ) {
                let (lbp, _) = PREDICATE_BP;
                if lbp <= min_bp {
                    break;
                }
                left = self.parse_predicate(left, false)?;
                continue;
            }

            let Some(op) = binary_op(&token) else { break };
            let Some((lbp, rbp)) = infix_binding_power(op) else {
                break;
            };
            if lbp <= min_bp {
                break;
            }

            self.advance();
            let right = self.parse_expr(rbp)?;
            let position = left.position;
            left = Expr::new(
                ExprKind::Binary {
                    op,
                    left: left.boxed(),
                    right: right.boxed(),
                },
                position,
            );
        }

        Ok(left)
    }

    fn parse_negated_predicate(&mut self, left: Expr) -> Result<Expr> {
        if matches!(
            self.peek().kind,
            TokenKind::Keyword(Keyword::Like | Keyword::In | Keyword::Between)
        ) {
            self.parse_predicate(left, true)
        } else {
            Err(self.unexpected("LIKE, IN or BETWEEN"))
        }
    }

    /// `LIKE`, `IN` and `BETWEEN`, each optionally negated.
    fn parse_predicate(&mut self, left: Expr, negated: bool) -> Result<Expr> {
        let position = left.position;
        let token = self.advance();

        let kind = match token.kind {
            TokenKind::Keyword(Keyword::Like) => {
                // The pattern binds tighter than a comparison, so `a LIKE b ||
                // '%'` concatenates before matching.
                let pattern = self.parse_expr(PREDICATE_BP.1)?;
                ExprKind::Like {
                    expr: left.boxed(),
                    pattern: pattern.boxed(),
                    negated,
                }
            }

            TokenKind::Keyword(Keyword::In) => {
                self.expect_symbol(Symbol::LParen)?;
                if self.peek().is_keyword(Keyword::Select) {
                    return Err(ZqlError::unsupported("subqueries").at(self.peek().position));
                }
                let mut list = Vec::new();
                loop {
                    let item = self.peek().clone();
                    match self.parse_literal() {
                        Some(literal) => list.push(literal?),
                        None => {
                            return Err(ZqlError::syntax(format!(
                                "IN takes a list of literals, found {}",
                                item.describe()
                            ))
                            .at(item.position))
                        }
                    }
                    if !self.eat_symbol(Symbol::Comma) {
                        break;
                    }
                }
                self.expect_symbol(Symbol::RParen)?;
                ExprKind::InList {
                    expr: left.boxed(),
                    list,
                    negated,
                }
            }

            TokenKind::Keyword(Keyword::Between) => {
                // Both bounds parse above `AND`'s binding power, so the `AND`
                // that separates them is never mistaken for a boolean one.
                let low = self.parse_expr(PREDICATE_BP.0)?;
                self.expect_keyword(Keyword::And)?;
                let high = self.parse_expr(PREDICATE_BP.0)?;
                ExprKind::Between {
                    expr: left.boxed(),
                    low: low.boxed(),
                    high: high.boxed(),
                    negated,
                }
            }

            _ => return Err(self.unexpected("LIKE, IN or BETWEEN")),
        };

        Ok(Expr::new(kind, position))
    }

    fn parse_prefix(&mut self) -> Result<Expr> {
        let token = self.peek().clone();

        match token.kind {
            TokenKind::Keyword(Keyword::Not) => {
                self.advance();
                let expr = self.parse_expr(NOT_BP)?;
                Ok(Expr::new(
                    ExprKind::Unary {
                        op: UnaryOp::Not,
                        expr: expr.boxed(),
                    },
                    token.position,
                ))
            }
            TokenKind::Symbol(Symbol::Minus) => {
                self.advance();
                let expr = self.parse_expr(UNARY_BP)?;
                Ok(Expr::new(
                    ExprKind::Unary {
                        op: UnaryOp::Neg,
                        expr: expr.boxed(),
                    },
                    token.position,
                ))
            }
            TokenKind::Symbol(Symbol::Plus) => {
                self.advance();
                let expr = self.parse_expr(UNARY_BP)?;
                Ok(Expr::new(
                    ExprKind::Unary {
                        op: UnaryOp::Plus,
                        expr: expr.boxed(),
                    },
                    token.position,
                ))
            }
            _ => self.parse_atom(),
        }
    }

    fn parse_atom(&mut self) -> Result<Expr> {
        let token = self.peek().clone();

        if let Some(literal) = self.parse_literal() {
            return Ok(Expr::new(ExprKind::Literal(literal?), token.position));
        }

        match token.kind {
            TokenKind::Symbol(Symbol::LParen) => {
                self.advance();
                if self.peek().is_keyword(Keyword::Select) {
                    return Err(ZqlError::unsupported("subqueries")
                        .at(self.peek().position)
                        .with_hint("zql evaluates one SELECT per query"));
                }
                let expr = self.parse_expr(0)?;
                self.expect_symbol(Symbol::RParen)?;
                Ok(expr)
            }

            TokenKind::Keyword(Keyword::Cast) => self.parse_cast(),
            TokenKind::Keyword(Keyword::Case) => self.parse_case(),

            TokenKind::Identifier => {
                self.advance();
                if self.peek().is_symbol(Symbol::LParen) {
                    return self.parse_function_call(token);
                }
                // `f.size` — a qualified column reference.
                if self.peek().is_symbol(Symbol::Dot) {
                    self.advance();
                    let name = self.advance();
                    if name.kind != TokenKind::Identifier {
                        return Err(ZqlError::syntax(format!(
                            "expected a column name after `.`, found {}",
                            name.describe()
                        ))
                        .at(name.position));
                    }
                    return Ok(Expr::new(
                        ExprKind::Column {
                            qualifier: Some(token.text),
                            name: name.text,
                        },
                        token.position,
                    ));
                }
                Ok(Expr::new(
                    ExprKind::Column {
                        qualifier: None,
                        name: token.text,
                    },
                    token.position,
                ))
            }

            _ => Err(self.unexpected("an expression")),
        }
    }

    fn parse_cast(&mut self) -> Result<Expr> {
        let token = self.advance(); // CAST
        self.expect_symbol(Symbol::LParen)?;
        let expr = self.parse_expr(0)?;
        self.expect_keyword(Keyword::As)?;

        let ty = self.advance();
        if ty.kind != TokenKind::Identifier {
            return Err(
                ZqlError::syntax(format!("expected a type name, found {}", ty.describe()))
                    .at(ty.position),
            );
        }
        self.expect_symbol(Symbol::RParen)?;

        Ok(Expr::new(
            ExprKind::Cast {
                expr: expr.boxed(),
                ty: ty.text,
            },
            token.position,
        ))
    }

    fn parse_case(&mut self) -> Result<Expr> {
        let token = self.advance(); // CASE

        // `CASE expr WHEN ...` — the form that compares against a subject — is
        // not in the grammar. Naming it beats a confusing syntax error.
        if !self.peek().is_keyword(Keyword::When) {
            return Err(ZqlError::unsupported("CASE with a subject expression")
                .at(self.peek().position)
                .with_hint("write CASE WHEN <condition> THEN ... instead"));
        }

        let mut branches = Vec::new();
        while self.eat_keyword(Keyword::When) {
            let condition = self.parse_expr(0)?;
            self.expect_keyword(Keyword::Then)?;
            let result = self.parse_expr(0)?;
            branches.push(CaseBranch { condition, result });
        }

        let else_result = if self.eat_keyword(Keyword::Else) {
            Some(self.parse_expr(0)?.boxed())
        } else {
            None
        };
        self.expect_keyword(Keyword::End)?;

        Ok(Expr::new(
            ExprKind::Case {
                branches,
                else_result,
            },
            token.position,
        ))
    }

    fn parse_function_call(&mut self, name: Token) -> Result<Expr> {
        self.expect_symbol(Symbol::LParen)?;
        let distinct = self.eat_keyword(Keyword::Distinct);

        let mut args = Vec::new();
        let mut star = false;

        if self.peek().is_symbol(Symbol::Star) {
            // `COUNT(*)` is the only place a star is an argument.
            self.advance();
            star = true;
        } else if !self.peek().is_symbol(Symbol::RParen) {
            loop {
                args.push(self.parse_expr(0)?);
                if !self.eat_symbol(Symbol::Comma) {
                    break;
                }
            }
        }
        self.expect_symbol(Symbol::RParen)?;

        if self.peek().is_keyword(Keyword::Over) {
            return Err(ZqlError::unsupported("window functions").at(self.peek().position));
        }

        Ok(Expr::new(
            ExprKind::Function(FunctionCall {
                name: name.text,
                args,
                distinct,
                star,
            }),
            name.position,
        ))
    }

    /// Consumes a literal if the next token is one.
    ///
    /// The doubled `Option<Result<_>>` distinguishes "not a literal, try
    /// something else" from "a literal that does not fit in an i64", which are
    /// different answers to different questions.
    fn parse_literal(&mut self) -> Option<Result<Literal>> {
        let token = self.peek().clone();
        let literal = match token.kind {
            TokenKind::Number => {
                self.advance();
                parse_number(&token)
            }
            TokenKind::String => {
                self.advance();
                Ok(Literal::String(token.text))
            }
            TokenKind::Keyword(Keyword::Null) => {
                self.advance();
                Ok(Literal::Null)
            }
            TokenKind::Keyword(Keyword::True) => {
                self.advance();
                Ok(Literal::Bool(true))
            }
            TokenKind::Keyword(Keyword::False) => {
                self.advance();
                Ok(Literal::Bool(false))
            }
            _ => return None,
        };
        Some(literal)
    }
}

/// A whole number stays a whole number; anything with a point or an exponent is
/// a `Real`. An integer too large for `i64` widens to `Real` rather than
/// failing, which is what Postgres does with an over-large literal.
fn parse_number(token: &Token) -> Result<Literal> {
    let text = &token.text;
    let looks_real = text.contains('.') || text.contains('e') || text.contains('E');

    if !looks_real {
        if let Ok(value) = text.parse::<i64>() {
            return Ok(Literal::Int(value));
        }
    }
    text.parse::<f64>()
        .map(Literal::Real)
        .map_err(|_| ZqlError::syntax(format!("`{text}` is not a valid number")).at(token.position))
}

// ------------------------------------------------------------ binding powers

/// Comparison, `LIKE`, `IN`, `BETWEEN` and `IS` all sit at one level.
const PREDICATE_BP: (u8, u8) = (8, 9);
/// `NOT` binds looser than comparison, so `NOT a = b` is `NOT (a = b)`, but
/// tighter than `AND`, so `NOT a AND b` is `(NOT a) AND b`.
const NOT_BP: u8 = 6;
/// Unary minus binds tightest of all, so `-a * b` is `(-a) * b`.
const UNARY_BP: u8 = 16;

/// The precedence table, lowest to highest, exactly as §2 fixes it:
/// `OR` → `AND` → `NOT` → comparison → `||` → `+ -` → `* / %` → unary minus.
///
/// **Levels are spaced two apart, not one, and the spacing is load-bearing.**
/// A level's right power sits one above its left, which is what makes every
/// operator left-associative — `a - b - c` is `(a - b) - c`. That leaves the
/// next level up needing a left power *strictly greater* than the level below's
/// right power, or it never gets absorbed. Packed one apart, `OR` ends at 2 and
/// `AND` starts at 2, and `a OR b AND c` silently parses as `(a OR b) AND c`.
fn infix_binding_power(op: BinaryOp) -> Option<(u8, u8)> {
    use BinaryOp::*;
    Some(match op {
        Or => (2, 3),
        And => (4, 5),
        // NOT_BP = 6 sits here.
        Eq | NotEq | Lt | LtEq | Gt | GtEq => PREDICATE_BP,
        Concat => (10, 11),
        Add | Sub => (12, 13),
        Mul | Div | Mod => (14, 15),
        // UNARY_BP = 16 sits here.
    })
}

fn binary_op(token: &Token) -> Option<BinaryOp> {
    Some(match token.kind {
        TokenKind::Keyword(Keyword::Or) => BinaryOp::Or,
        TokenKind::Keyword(Keyword::And) => BinaryOp::And,
        TokenKind::Symbol(symbol) => match symbol {
            Symbol::Eq => BinaryOp::Eq,
            Symbol::NotEq => BinaryOp::NotEq,
            Symbol::Lt => BinaryOp::Lt,
            Symbol::LtEq => BinaryOp::LtEq,
            Symbol::Gt => BinaryOp::Gt,
            Symbol::GtEq => BinaryOp::GtEq,
            Symbol::Concat => BinaryOp::Concat,
            Symbol::Plus => BinaryOp::Add,
            Symbol::Minus => BinaryOp::Sub,
            Symbol::Star => BinaryOp::Mul,
            Symbol::Slash => BinaryOp::Div,
            Symbol::Percent => BinaryOp::Mod,
            _ => return None,
        },
        _ => return None,
    })
}

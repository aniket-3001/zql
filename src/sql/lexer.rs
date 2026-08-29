//! Text to tokens.
//!
//! The lexer scans bytes rather than chars. That is safe because every
//! structural character in the grammar — quotes, parentheses, operators — is
//! ASCII, and a UTF-8 continuation byte can never be mistaken for one. String
//! contents are sliced out whole and only then interpreted as text, so an
//! identifier called `写真` or a literal full of emoji passes through intact.
//!
//! Every token carries a 1-based position. That single field is what lets
//! `psql` draw its own caret line under the offending word, which is a large
//! amount of polish for very little code.

use crate::error::{Result, ZqlError};
use crate::sql::token::{Keyword, Symbol, Token, TokenKind};

/// Turns a query into tokens, always ending with [`TokenKind::Eof`].
pub fn tokenize(sql: &str) -> Result<Vec<Token>> {
    Lexer::new(sql).run()
}

struct Lexer<'a> {
    input: &'a [u8],
    /// The original text, for slicing out string and identifier contents.
    source: &'a str,
    offset: usize,
}

impl<'a> Lexer<'a> {
    fn new(source: &'a str) -> Self {
        Lexer {
            input: source.as_bytes(),
            source,
            offset: 0,
        }
    }

    fn run(mut self) -> Result<Vec<Token>> {
        let mut tokens = Vec::new();
        loop {
            self.skip_trivia()?;
            let position = self.position();
            let Some(byte) = self.peek() else {
                tokens.push(Token {
                    kind: TokenKind::Eof,
                    text: String::new(),
                    position,
                });
                return Ok(tokens);
            };

            let token = match byte {
                b'0'..=b'9' => self.lex_number()?,
                b'\'' => self.lex_string()?,
                b'"' => self.lex_quoted_identifier()?,
                byte if is_identifier_start(byte) => self.lex_word(),
                _ => self.lex_symbol()?,
            };
            tokens.push(token);
        }
    }

    /// 1-based, as the protocol's `P` field requires.
    fn position(&self) -> u32 {
        // Saturating rather than wrapping: a query long enough to overflow a
        // u32 position is already refused by the wire layer's length cap, and
        // a wrong caret is better than a panic.
        u32::try_from(self.offset + 1).unwrap_or(u32::MAX)
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.offset).copied()
    }

    fn peek_at(&self, ahead: usize) -> Option<u8> {
        self.input.get(self.offset + ahead).copied()
    }

    fn advance(&mut self) {
        self.offset += 1;
    }

    /// Whitespace and both comment forms. Block comments do not nest, per §3.
    fn skip_trivia(&mut self) -> Result<()> {
        loop {
            match self.peek() {
                Some(byte) if byte.is_ascii_whitespace() => self.advance(),

                Some(b'-') if self.peek_at(1) == Some(b'-') => {
                    while let Some(byte) = self.peek() {
                        if byte == b'\n' {
                            break;
                        }
                        self.advance();
                    }
                }

                Some(b'/') if self.peek_at(1) == Some(b'*') => {
                    let start = self.position();
                    self.advance();
                    self.advance();
                    loop {
                        match self.peek() {
                            None => {
                                return Err(ZqlError::syntax("unterminated /* comment").at(start))
                            }
                            Some(b'*') if self.peek_at(1) == Some(b'/') => {
                                self.advance();
                                self.advance();
                                break;
                            }
                            Some(_) => self.advance(),
                        }
                    }
                }

                _ => return Ok(()),
            }
        }
    }

    /// An identifier or a keyword. Unquoted words fold to lower case, which is
    /// Postgres behaviour and the reason source arguments are strings rather
    /// than identifiers.
    fn lex_word(&mut self) -> Token {
        let position = self.position();
        let start = self.offset;
        while matches!(self.peek(), Some(byte) if is_identifier_continue(byte)) {
            self.advance();
        }
        let word = self.source[start..self.offset].to_ascii_lowercase();

        match Keyword::lookup(&word) {
            Some(keyword) => Token {
                kind: TokenKind::Keyword(keyword),
                text: word,
                position,
            },
            None => Token {
                kind: TokenKind::Identifier,
                text: word,
                position,
            },
        }
    }

    /// `"quoted"` — case preserved, `""` escapes one quote.
    fn lex_quoted_identifier(&mut self) -> Result<Token> {
        let position = self.position();
        let text = self.lex_delimited(b'"', "quoted identifier")?;
        if text.is_empty() {
            return Err(ZqlError::syntax("zero-length quoted identifier").at(position));
        }
        Ok(Token {
            kind: TokenKind::Identifier,
            text,
            position,
        })
    }

    /// `'string'` — `''` escapes one quote. There are deliberately no
    /// backslash escapes: that is what `standard_conforming_strings = on`
    /// promises the client, and a Windows path in a query would otherwise be
    /// mangled on its way in.
    fn lex_string(&mut self) -> Result<Token> {
        let position = self.position();
        let text = self.lex_delimited(b'\'', "string literal")?;
        Ok(Token {
            kind: TokenKind::String,
            text,
            position,
        })
    }

    /// Shared body of the two quoted forms: read to the closing delimiter,
    /// treating a doubled delimiter as one literal character.
    fn lex_delimited(&mut self, quote: u8, what: &str) -> Result<String> {
        let position = self.position();
        self.advance(); // opening quote
        let mut text = String::new();
        let mut chunk_start = self.offset;

        loop {
            match self.peek() {
                None => {
                    return Err(ZqlError::syntax(format!("unterminated {what}")).at(position));
                }
                Some(byte) if byte == quote => {
                    text.push_str(&self.source[chunk_start..self.offset]);
                    self.advance();
                    if self.peek() == Some(quote) {
                        // A doubled quote is one literal quote character.
                        text.push(quote as char);
                        self.advance();
                        chunk_start = self.offset;
                    } else {
                        return Ok(text);
                    }
                }
                Some(_) => self.advance(),
            }
        }
    }

    /// `123`, `1.5`, `1e10`. A leading `-` is always the unary operator and
    /// never part of the literal, so `1-2` lexes as three tokens.
    fn lex_number(&mut self) -> Result<Token> {
        let position = self.position();
        let start = self.offset;

        while matches!(self.peek(), Some(byte) if byte.is_ascii_digit()) {
            self.advance();
        }
        if self.peek() == Some(b'.') {
            self.advance();
            while matches!(self.peek(), Some(byte) if byte.is_ascii_digit()) {
                self.advance();
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            // Only consume the `e` if what follows really is an exponent —
            // otherwise `1east` would lex as a broken number rather than a
            // number followed by an identifier.
            let mut ahead = 1;
            if matches!(self.peek_at(ahead), Some(b'+' | b'-')) {
                ahead += 1;
            }
            if matches!(self.peek_at(ahead), Some(byte) if byte.is_ascii_digit()) {
                for _ in 0..ahead {
                    self.advance();
                }
                while matches!(self.peek(), Some(byte) if byte.is_ascii_digit()) {
                    self.advance();
                }
            }
        }

        // `123abc` is a typo, not a number followed by a name.
        if matches!(self.peek(), Some(byte) if is_identifier_start(byte)) {
            return Err(ZqlError::syntax("trailing characters after a number").at(position));
        }

        Ok(Token {
            kind: TokenKind::Number,
            text: self.source[start..self.offset].to_string(),
            position,
        })
    }

    fn lex_symbol(&mut self) -> Result<Token> {
        let position = self.position();
        let byte = self.peek().unwrap_or(0);
        self.advance();

        let symbol = match byte {
            b',' => Symbol::Comma,
            b'(' => Symbol::LParen,
            b')' => Symbol::RParen,
            b'.' => Symbol::Dot,
            b';' => Symbol::Semicolon,
            b'*' => Symbol::Star,
            b'+' => Symbol::Plus,
            b'-' => Symbol::Minus,
            b'/' => Symbol::Slash,
            b'%' => Symbol::Percent,
            b'=' => Symbol::Eq,

            b'|' => {
                if self.peek() == Some(b'|') {
                    self.advance();
                    Symbol::Concat
                } else {
                    return Err(ZqlError::syntax("`|` is not an operator; did you mean `||`?")
                        .at(position));
                }
            }
            b'!' => {
                if self.peek() == Some(b'=') {
                    self.advance();
                    Symbol::NotEq
                } else {
                    return Err(ZqlError::syntax("`!` is not an operator; did you mean `!=`?")
                        .at(position));
                }
            }
            // `x::text` is the first thing a Postgres user types, and the
            // grammar in docs/SQL-SUBSET.md deliberately has only the standard
            // spelling. Naming the one that exists costs a line and saves the
            // reader working out which of the two colons upset it.
            b':' if self.peek() == Some(b':') => {
                self.advance();
                return Err(ZqlError::syntax("zql does not have the `::` cast operator")
                    .at(position)
                    .with_hint("write it as CAST(x AS text)"));
            }
            b'<' => match self.peek() {
                Some(b'=') => {
                    self.advance();
                    Symbol::LtEq
                }
                Some(b'>') => {
                    self.advance();
                    Symbol::NotEq
                }
                _ => Symbol::Lt,
            },
            b'>' => {
                if self.peek() == Some(b'=') {
                    self.advance();
                    Symbol::GtEq
                } else {
                    Symbol::Gt
                }
            }

            other => {
                // Report the whole character, not the byte, or a multi-byte
                // character produces mojibake in the error message.
                let character = self.source[self.offset - 1..]
                    .chars()
                    .next()
                    .unwrap_or(other as char);
                // Resynchronise past the rest of the character.
                self.offset = self.offset - 1 + character.len_utf8();
                return Err(
                    ZqlError::syntax(format!("unexpected character `{character}`")).at(position)
                );
            }
        };

        Ok(Token {
            kind: TokenKind::Symbol(symbol),
            text: String::new(),
            position,
        })
    }
}

fn is_identifier_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

fn is_identifier_continue(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(sql: &str) -> Vec<TokenKind> {
        tokenize(sql)
            .unwrap()
            .into_iter()
            .map(|token| token.kind)
            .collect()
    }

    fn texts(sql: &str) -> Vec<String> {
        tokenize(sql)
            .unwrap()
            .into_iter()
            .map(|token| token.text)
            .collect()
    }

    #[test]
    fn keywords_are_case_insensitive_and_identifiers_fold_down() {
        assert_eq!(
            kinds("SeLeCt Name"),
            vec![
                TokenKind::Keyword(Keyword::Select),
                TokenKind::Identifier,
                TokenKind::Eof
            ]
        );
        assert_eq!(texts("SeLeCt Name")[1], "name");
    }

    #[test]
    fn quoted_identifiers_keep_their_case() {
        let tokens = tokenize("\"MixedCase\"").unwrap();
        assert_eq!(tokens[0].kind, TokenKind::Identifier);
        assert_eq!(tokens[0].text, "MixedCase");
    }

    #[test]
    fn a_quoted_identifier_shadows_a_keyword() {
        let tokens = tokenize("\"select\"").unwrap();
        assert_eq!(tokens[0].kind, TokenKind::Identifier);
    }

    #[test]
    fn doubled_quotes_escape_within_both_quoted_forms() {
        assert_eq!(tokenize("'it''s'").unwrap()[0].text, "it's");
        assert_eq!(tokenize("\"a\"\"b\"").unwrap()[0].text, "a\"b");
    }

    #[test]
    fn backslashes_are_literal_not_escapes() {
        // A Windows path must survive being written in a query.
        assert_eq!(tokenize(r"'D:\db\app.db'").unwrap()[0].text, r"D:\db\app.db");
    }

    #[test]
    fn strings_carry_unicode_intact() {
        assert_eq!(tokenize("'héllo wörld 🎞'").unwrap()[0].text, "héllo wörld 🎞");
    }

    #[test]
    fn numbers_stop_before_a_minus_sign() {
        assert_eq!(
            kinds("1-2"),
            vec![
                TokenKind::Number,
                TokenKind::Symbol(Symbol::Minus),
                TokenKind::Number,
                TokenKind::Eof
            ]
        );
    }

    #[test]
    fn exponents_lex_but_words_after_digits_do_not() {
        assert_eq!(texts("1e10")[0], "1e10");
        assert_eq!(texts("1.5e-3")[0], "1.5e-3");
        assert!(tokenize("1east").is_err());
    }

    #[test]
    fn comments_are_trivia() {
        assert_eq!(
            kinds("SELECT -- a comment\n 1 /* and another */ "),
            vec![
                TokenKind::Keyword(Keyword::Select),
                TokenKind::Number,
                TokenKind::Eof
            ]
        );
    }

    #[test]
    fn unterminated_things_are_errors_with_a_position() {
        for sql in ["'unterminated", "\"unterminated", "/* unterminated"] {
            let err = tokenize(sql).unwrap_err();
            assert!(err.position.is_some(), "{sql} should report a position");
        }
    }

    #[test]
    fn two_character_operators_are_preferred_over_one() {
        assert_eq!(
            kinds("a<=b<>c||d>=e!=f"),
            vec![
                TokenKind::Identifier,
                TokenKind::Symbol(Symbol::LtEq),
                TokenKind::Identifier,
                TokenKind::Symbol(Symbol::NotEq),
                TokenKind::Identifier,
                TokenKind::Symbol(Symbol::Concat),
                TokenKind::Identifier,
                TokenKind::Symbol(Symbol::GtEq),
                TokenKind::Identifier,
                TokenKind::Symbol(Symbol::NotEq),
                TokenKind::Identifier,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn a_lone_pipe_suggests_the_operator_that_exists() {
        let err = tokenize("a | b").unwrap_err();
        assert!(err.message.contains("||"));
    }

    #[test]
    fn positions_are_one_based_and_point_at_the_token() {
        let tokens = tokenize("SELECT name").unwrap();
        assert_eq!(tokens[0].position, 1);
        assert_eq!(tokens[1].position, 8);
    }

    #[test]
    fn an_unexpected_character_is_reported_whole() {
        let err = tokenize("SELECT 写").unwrap_err();
        assert!(err.message.contains('写'), "got: {}", err.message);
    }
}

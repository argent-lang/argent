//! Canonical source text and tokens for an Argent entry body.

use crate::error::Result;
use crate::lexer::{Span, Token, TokenKind, lex};

/// Keeps one token stream shared by body analysis and lowering.
#[derive(Debug, Clone)]
pub struct EntryBody {
    text: String,
    tokens: Vec<Token>,
}

impl EntryBody {
    pub fn new(text: impl Into<String>) -> Result<Self> {
        let text = text.into();
        let tokens = lex(&text)?;
        Ok(Self { text, tokens })
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn tokens(&self) -> &[Token] {
        &self.tokens
    }

    pub(crate) fn cursor(&self) -> EntryBodyCursor<'_> {
        EntryBodyCursor { body: self, pos: 0 }
    }
}

impl Default for EntryBody {
    fn default() -> Self {
        Self::new(String::new()).expect("empty entry body lexes")
    }
}

/// Traverses a body's shared tokens while retaining access to their source text.
pub(crate) struct EntryBodyCursor<'a> {
    body: &'a EntryBody,
    pos: usize,
}

impl<'a> EntryBodyCursor<'a> {
    pub(crate) fn current(&self) -> &Token {
        &self.body.tokens[self.pos]
    }

    pub(crate) fn peek_kind(&self, offset: usize) -> Option<&TokenKind> {
        self.body.tokens.get(self.pos + offset).map(|token| &token.kind)
    }

    pub(crate) fn advance(&mut self) {
        if !self.is_eof() {
            self.pos += 1;
        }
    }

    pub(crate) fn is_eof(&self) -> bool {
        matches!(self.current().kind, TokenKind::Eof)
    }

    pub(crate) fn check_ident(&self, expected: &str) -> bool {
        matches!(&self.current().kind, TokenKind::Ident(actual) if actual == expected)
    }

    pub(crate) fn consume_ident(&mut self, expected: &str) -> bool {
        if self.check_ident(expected) {
            self.advance();
            true
        } else {
            false
        }
    }

    pub(crate) fn check_symbol(&self, expected: char) -> bool {
        matches!(self.current().kind, TokenKind::Symbol(actual) if actual == expected)
    }

    pub(crate) fn consume_symbol(&mut self, expected: char) -> bool {
        if self.check_symbol(expected) {
            self.advance();
            true
        } else {
            false
        }
    }

    /// Takes the source span inside a group after its opening token was consumed.
    pub(crate) fn take_balanced_after_open(&mut self, open: char, close: char) -> Option<Span> {
        let start = self.current().span.start;
        let mut depth = 1usize;
        while !self.is_eof() {
            match self.current().kind {
                TokenKind::Symbol(symbol) if symbol == open => {
                    depth += 1;
                    self.advance();
                }
                TokenKind::Symbol(symbol) if symbol == close => {
                    depth -= 1;
                    if depth == 0 {
                        let end = self.current().span.start;
                        self.advance();
                        return Some(Span { start, end });
                    }
                    self.advance();
                }
                _ => self.advance(),
            }
        }
        None
    }

    pub(crate) fn span_text(&self, span: Span) -> &'a str {
        &self.body.text[span.start..span.end]
    }

    pub(crate) fn byte_offset(&self) -> usize {
        self.current().span.start
    }

    pub(crate) fn remaining_text(&self) -> &'a str {
        self.body.text.get(self.byte_offset()..).unwrap_or("")
    }
}

#[cfg(test)]
mod tests {
    use super::EntryBody;

    #[test]
    fn cursor_takes_nested_balanced_source() {
        let body = EntryBody::new("if (outer(inner)) become next <- Done(state);").expect("body lexes");
        let mut cursor = body.cursor();

        assert!(cursor.consume_ident("if"));
        assert!(cursor.consume_symbol('('));
        let span = cursor.take_balanced_after_open('(', ')').expect("condition closes");
        assert_eq!(cursor.span_text(span), "outer(inner)");
        assert!(cursor.check_ident("become"));
    }

    #[test]
    fn cursor_returns_none_for_an_unterminated_group() {
        let body = EntryBody::new("(state").expect("body lexes");
        let mut cursor = body.cursor();

        assert!(cursor.consume_symbol('('));
        assert_eq!(cursor.take_balanced_after_open('(', ')'), None);
        assert!(cursor.is_eof());
    }
}

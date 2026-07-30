//! Canonical source text and tokens for an Argent entry body.

use crate::error::Result;
use crate::language::word;
use crate::lexer::{Span, Token, TokenKind, lex};

/// Keeps source, tokens, and structural body syntax together.
#[derive(Debug, Clone)]
pub struct EntryBody {
    text: String,
    tokens: Vec<Token>,
    statements: Vec<EntryStatement>,
}

impl EntryBody {
    pub fn new(text: impl Into<String>) -> Result<Self> {
        let text = text.into();
        let tokens = lex(&text)?;
        let mut body = Self { text, tokens, statements: Vec::new() };
        body.statements = EntryStatementParser { cursor: body.cursor() }.parse_sequence(None)?;
        Ok(body)
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn tokens(&self) -> &[Token] {
        &self.tokens
    }

    pub(crate) fn statements(&self) -> &[EntryStatement] {
        &self.statements
    }

    pub(crate) fn span_text(&self, span: Span) -> &str {
        &self.text[span.start..span.end]
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

/// The structural statements Argent needs to understand before full Sil parsing.
#[derive(Debug, Clone)]
pub(crate) enum EntryStatement {
    If {
        // Kept for the lowering pass that will consume this structure next.
        #[allow(dead_code)]
        condition: Span,
        then_branch: Box<EntryStatement>,
        else_branch: Option<Box<EntryStatement>>,
        span: Span,
    },
    Block {
        statements: Vec<EntryStatement>,
        span: Span,
    },
    Become {
        routes: Vec<EntryRoute>,
        span: Span,
    },
    Plain {
        span: Span,
    },
}

impl EntryStatement {
    pub(crate) fn span(&self) -> Span {
        match self {
            Self::If { span, .. } | Self::Block { span, .. } | Self::Become { span, .. } | Self::Plain { span } => *span,
        }
    }
}

/// One parsed route in a `become` statement.
#[derive(Debug, Clone)]
pub(crate) struct EntryRoute {
    pub(crate) output: String,
    pub(crate) actor: String,
    pub(crate) state: Span,
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
        self.body.span_text(span)
    }

    pub(crate) fn span_to_current(&self, start: usize) -> Span {
        Span { start, end: self.byte_offset() }
    }

    fn consumed_span_from(&self, start: usize) -> Span {
        let end = self.pos.checked_sub(1).and_then(|previous| self.body.tokens.get(previous)).map_or(start, |token| token.span.end);
        Span { start, end }
    }

    pub(crate) fn byte_offset(&self) -> usize {
        self.current().span.start
    }

    pub(crate) fn remaining_text(&self) -> &'a str {
        self.body.text.get(self.byte_offset()..).unwrap_or("")
    }
}

struct EntryStatementParser<'a> {
    cursor: EntryBodyCursor<'a>,
}

impl EntryStatementParser<'_> {
    fn parse_sequence(&mut self, end: Option<char>) -> Result<Vec<EntryStatement>> {
        let mut statements = Vec::new();
        while !self.cursor.is_eof() && !end.is_some_and(|symbol| self.cursor.check_symbol(symbol)) {
            if self.cursor.consume_symbol(';') {
                continue;
            }
            statements.push(self.parse_statement()?);
        }
        Ok(statements)
    }

    fn parse_statement(&mut self) -> Result<EntryStatement> {
        let start = self.cursor.byte_offset();
        if self.cursor.consume_ident(word::IF) {
            self.expect_symbol('(')?;
            let condition = self.cursor.take_balanced_after_open('(', ')').ok_or_else(|| self.error("unterminated `(` group"))?;
            let then_branch = Box::new(self.parse_block_or_statement()?);
            let else_branch =
                if self.cursor.consume_ident(word::ELSE) { Some(Box::new(self.parse_block_or_statement()?)) } else { None };
            let end = else_branch.as_deref().unwrap_or(&then_branch).span().end;
            Ok(EntryStatement::If { condition, then_branch, else_branch, span: Span { start, end } })
        } else if self.cursor.consume_ident(word::BECOME) {
            let routes = self.parse_become_tail()?;
            Ok(EntryStatement::Become { routes, span: self.cursor.consumed_span_from(start) })
        } else if self.cursor.consume_symbol('{') {
            self.parse_block_after_open(start)
        } else {
            self.skip_until_statement_end()?;
            Ok(EntryStatement::Plain { span: self.cursor.consumed_span_from(start) })
        }
    }

    fn parse_block_or_statement(&mut self) -> Result<EntryStatement> {
        if self.cursor.check_symbol('{') {
            let start = self.cursor.byte_offset();
            self.cursor.advance();
            self.parse_block_after_open(start)
        } else {
            self.parse_statement()
        }
    }

    fn parse_block_after_open(&mut self, start: usize) -> Result<EntryStatement> {
        let statements = self.parse_sequence(Some('}'))?;
        self.expect_symbol('}')?;
        Ok(EntryStatement::Block { statements, span: self.cursor.consumed_span_from(start) })
    }

    fn parse_become_tail(&mut self) -> Result<Vec<EntryRoute>> {
        if self.cursor.consume_symbol('{') {
            let mut routes = Vec::new();
            while !self.cursor.check_symbol('}') && !self.cursor.is_eof() {
                if self.cursor.check_ident(word::BECOME) {
                    return Err(self.error("nested `become` blocks are not supported yet"));
                }
                routes.push(self.parse_route()?);
                self.expect_list_separator_or_end('}')?;
            }
            self.expect_symbol('}')?;
            self.cursor.consume_symbol(';');
            return Ok(routes);
        }

        let route = self.parse_route()?;
        self.cursor.consume_symbol(';');
        Ok(vec![route])
    }

    fn parse_route(&mut self) -> Result<EntryRoute> {
        let output = self.expect_any_ident()?;
        if !self.consume_left_arrow() {
            return Err(self.error("every `become` route must name its output with `output <- Actor(state)`"));
        }
        let actor = self.parse_actor_name()?;

        self.expect_symbol('(')?;
        let state = self.cursor.take_balanced_after_open('(', ')').ok_or_else(|| self.error("unterminated route state expression"))?;
        Ok(EntryRoute { output, actor, state })
    }

    fn parse_actor_name(&mut self) -> Result<String> {
        let first = self.expect_any_ident()?;
        if !self.cursor.consume_symbol(':') {
            return Ok(first);
        }
        self.expect_symbol(':')?;
        Ok(format!("{first}::{}", self.expect_any_ident()?))
    }

    fn consume_left_arrow(&mut self) -> bool {
        match self.cursor.current().kind {
            TokenKind::LeftArrow => {
                self.cursor.advance();
                true
            }
            TokenKind::Symbol('<') if matches!(self.cursor.peek_kind(1), Some(TokenKind::Symbol('-'))) => {
                self.cursor.advance();
                self.cursor.advance();
                true
            }
            _ => false,
        }
    }

    fn expect_any_ident(&mut self) -> Result<String> {
        match self.cursor.current().kind.clone() {
            TokenKind::Ident(name) => {
                self.cursor.advance();
                Ok(name)
            }
            _ => Err(self.error("expected identifier in `become` route")),
        }
    }

    fn skip_until_statement_end(&mut self) -> Result<()> {
        while !self.cursor.is_eof() {
            match self.cursor.current().kind {
                TokenKind::Symbol(';') => {
                    self.cursor.advance();
                    return Ok(());
                }
                TokenKind::Symbol('{') => {
                    self.cursor.advance();
                    self.skip_balanced_after_open('{', '}')?;
                }
                TokenKind::Symbol('(') => {
                    self.cursor.advance();
                    self.skip_balanced_after_open('(', ')')?;
                }
                TokenKind::Symbol('[') => {
                    self.cursor.advance();
                    self.skip_balanced_after_open('[', ']')?;
                }
                TokenKind::Symbol('}') => return Ok(()),
                _ => self.cursor.advance(),
            }
        }
        Ok(())
    }

    fn skip_balanced_after_open(&mut self, open: char, close: char) -> Result<()> {
        if self.cursor.take_balanced_after_open(open, close).is_some() {
            Ok(())
        } else {
            Err(self.error(format!("unterminated `{open}` group")))
        }
    }

    fn expect_symbol(&mut self, expected: char) -> Result<()> {
        if self.cursor.consume_symbol(expected) { Ok(()) } else { Err(self.error(format!("expected `{expected}`"))) }
    }

    fn expect_list_separator_or_end(&mut self, end: char) -> Result<()> {
        if self.cursor.consume_symbol(',') || self.cursor.check_symbol(end) {
            Ok(())
        } else {
            Err(self.error(format!("expected `,` or `{end}`")))
        }
    }

    fn error(&self, message: impl Into<String>) -> crate::error::ArgentError {
        let preview = self.cursor.remaining_text().lines().next().unwrap_or("").trim().chars().take(80).collect::<String>();
        crate::error::ArgentError::new(format!("{} at body byte {} near `{}`", message.into(), self.cursor.byte_offset(), preview))
    }
}

#[cfg(test)]
mod tests {
    use super::{EntryBody, EntryStatement, lex};

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
        let text = "(state".to_string();
        let body = EntryBody { tokens: lex(&text).expect("body lexes"), text, statements: Vec::new() };
        let mut cursor = body.cursor();

        assert!(cursor.consume_symbol('('));
        assert_eq!(cursor.take_balanced_after_open('(', ')'), None);
        assert!(cursor.is_eof());
    }

    #[test]
    fn statements_keep_structure_and_source_spans() {
        let body = EntryBody::new(
            r#"
            int next = value;
            if (done) {
                become output <- Done(next);
            } else {
                become output <- Live(next);
            }
            "#,
        )
        .expect("body lexes");

        let [EntryStatement::Plain { span: plain }, EntryStatement::If { condition, then_branch, else_branch, .. }] =
            body.statements()
        else {
            panic!("expected one plain statement followed by an if");
        };
        assert_eq!(body.span_text(*plain).trim(), "int next = value;");
        assert_eq!(body.span_text(*condition), "done");
        assert!(matches!(then_branch.as_ref(), EntryStatement::Block { .. }));
        assert!(matches!(else_branch.as_deref(), Some(EntryStatement::Block { .. })));
    }
}

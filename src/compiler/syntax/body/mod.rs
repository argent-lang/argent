//! Canonical source text and tokens for an Argent entry body.

use super::lexer::{Span, Token, TokenKind, lex};
use super::word;
use crate::error::Result;

pub mod routes;

/// Keeps source, tokens, and structural body syntax together.
#[derive(Debug, Clone)]
pub struct EntryBody {
    text: String,
    tokens: Vec<Token>,
    statements: Vec<EntryStatement>,
}

#[cfg(test)]
mod tests;

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

    /// Return initialized local declarations in source order, including nested scopes.
    pub(crate) fn local_declarations(&self) -> Vec<&EntryLocalDecl> {
        let mut locals = Vec::new();
        for statement in &self.statements {
            statement.collect_local_declarations(&mut locals);
        }
        locals
    }

    pub(crate) fn span_text(&self, span: Span) -> &str {
        &self.text[span.start..span.end]
    }

    fn cursor(&self) -> EntryBodyCursor<'_> {
        EntryBodyCursor { body: self, pos: 0 }
    }
}

impl Default for EntryBody {
    fn default() -> Self {
        Self::new(String::new()).expect("empty entry body lexes")
    }
}

/// The structural statements Argent understands without parsing ordinary Sil statements.
#[derive(Debug, Clone)]
pub(crate) enum EntryStatement {
    If {
        condition: Span,
        then_branch: Box<EntryStatement>,
        else_branch: Option<Box<EntryStatement>>,
        span: Span,
    },
    For {
        binding: EntryBinding,
        header: Span,
        body: Box<EntryStatement>,
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
    ValidateOutputsBecome {
        group: String,
        routes: Vec<EntryRoute>,
        span: Span,
    },
    /// A directly declared local variable with an initializer.
    Local {
        declaration: EntryLocalDecl,
        span: Span,
    },
    /// An ordinary Sil statement kept opaque except for introduced bindings and
    /// the layout boundary on typed struct destructuring.
    Plain {
        bindings: Vec<EntryBinding>,
        /// Layout-relevant spans on a typed Sil struct destructuring assignment.
        destructuring: Option<EntryStructDestructure>,
        span: Span,
    },
}

impl EntryStatement {
    pub(crate) fn span(&self) -> Span {
        match self {
            Self::If { span, .. }
            | Self::For { span, .. }
            | Self::Block { span, .. }
            | Self::Become { span, .. }
            | Self::ValidateOutputsBecome { span, .. }
            | Self::Local { span, .. }
            | Self::Plain { span, .. } => *span,
        }
    }

    fn collect_local_declarations<'a>(&'a self, locals: &mut Vec<&'a EntryLocalDecl>) {
        match self {
            Self::If { then_branch, else_branch, .. } => {
                then_branch.collect_local_declarations(locals);
                if let Some(else_branch) = else_branch {
                    else_branch.collect_local_declarations(locals);
                }
            }
            Self::For { body, .. } => body.collect_local_declarations(locals),
            Self::Block { statements, .. } => {
                for statement in statements {
                    statement.collect_local_declarations(locals);
                }
            }
            Self::Local { declaration, .. } => locals.push(declaration),
            Self::Become { .. } | Self::ValidateOutputsBecome { .. } | Self::Plain { .. } => {}
        }
    }
}

/// One directly declared local variable whose initializer Argent may lower.
#[derive(Debug, Clone)]
pub(crate) struct EntryLocalDecl {
    pub(crate) binding: EntryBinding,
    /// The source type and any following declaration qualifiers.
    pub(crate) declared_type: Span,
    pub(crate) initializer: Span,
}

/// One lexically scoped value introduced by ordinary Sil syntax.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EntryBinding {
    pub(crate) name: String,
    pub(crate) source_type: String,
    /// Inner state when the declared type is a scalar `actor_type<State>`.
    pub(crate) actor_type_state: Option<String>,
}

/// The written type and source value of a typed Sil struct destructuring.
#[derive(Debug, Clone, Copy)]
pub(crate) struct EntryStructDestructure {
    pub(crate) declared_type: Span,
    pub(crate) value: Span,
}

/// One parsed route in a `become` statement.
#[derive(Debug, Clone)]
pub(crate) struct EntryRoute {
    pub(crate) output: String,
    pub(crate) actor: Span,
    pub(crate) state: Span,
}

/// Traverses a body's shared tokens while retaining access to their source text.
struct EntryBodyCursor<'a> {
    body: &'a EntryBody,
    pos: usize,
}

impl<'a> EntryBodyCursor<'a> {
    fn current(&self) -> &Token {
        &self.body.tokens[self.pos]
    }

    fn peek_kind(&self, offset: usize) -> Option<&TokenKind> {
        self.body.tokens.get(self.pos + offset).map(|token| &token.kind)
    }

    fn advance(&mut self) {
        if !self.is_eof() {
            self.pos += 1;
        }
    }

    fn is_eof(&self) -> bool {
        matches!(self.current().kind, TokenKind::Eof)
    }

    fn check_ident(&self, expected: &str) -> bool {
        matches!(&self.current().kind, TokenKind::Ident(actual) if actual == expected)
    }

    fn consume_ident(&mut self, expected: &str) -> bool {
        if self.check_ident(expected) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn check_symbol(&self, expected: char) -> bool {
        matches!(self.current().kind, TokenKind::Symbol(actual) if actual == expected)
    }

    fn consume_symbol(&mut self, expected: char) -> bool {
        if self.check_symbol(expected) {
            self.advance();
            true
        } else {
            false
        }
    }

    /// Takes the source span inside a group after its opening token was consumed.
    fn take_balanced_after_open(&mut self, open: char, close: char) -> Option<Span> {
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

    fn span_to_current(&self, start: usize) -> Span {
        Span { start, end: self.byte_offset() }
    }

    fn consumed_span_from(&self, start: usize) -> Span {
        let end = self.pos.checked_sub(1).and_then(|previous| self.body.tokens.get(previous)).map_or(start, |token| token.span.end);
        Span { start, end }
    }

    fn byte_offset(&self) -> usize {
        self.current().span.start
    }

    fn remaining_text(&self) -> &'a str {
        self.body.text.get(self.byte_offset()..).unwrap_or("")
    }

    /// Distinguishes Sil destructuring assignments from standalone blocks.
    fn check_braced_assignment_start(&self) -> bool {
        if !self.check_symbol('{') {
            return false;
        }
        let mut depth = 0usize;
        for (offset, token) in self.body.tokens[self.pos..].iter().enumerate() {
            match token.kind {
                TokenKind::Symbol('{') => depth += 1,
                TokenKind::Symbol('}') => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return matches!(
                            self.body.tokens.get(self.pos + offset + 1).map(|token| &token.kind),
                            Some(TokenKind::Symbol('='))
                        );
                    }
                }
                TokenKind::Eof => return false,
                _ => {}
            }
        }
        false
    }
}

struct EntryStatementParser<'a> {
    cursor: EntryBodyCursor<'a>,
}

impl EntryStatementParser<'_> {
    fn parse_sequence(&mut self, end: Option<char>) -> Result<Vec<EntryStatement>> {
        let mut statements = Vec::new();
        while !self.cursor.is_eof() && !end.is_some_and(|symbol| self.cursor.check_symbol(symbol)) {
            if self.cursor.check_symbol('}') {
                return Err(self.error("unexpected `}`"));
            }
            if self.cursor.consume_symbol(';') {
                continue;
            }
            statements.push(self.parse_statement()?);
        }
        Ok(statements)
    }

    fn parse_statement(&mut self) -> Result<EntryStatement> {
        let token_start = self.cursor.pos;
        let start = self.cursor.byte_offset();
        if self.cursor.consume_ident(word::IF) {
            self.expect_symbol('(')?;
            let condition = self.cursor.take_balanced_after_open('(', ')').ok_or_else(|| self.error("unterminated `(` group"))?;
            let then_branch = Box::new(self.parse_block_or_statement()?);
            let else_branch =
                if self.cursor.consume_ident(word::ELSE) { Some(Box::new(self.parse_block_or_statement()?)) } else { None };
            let end = else_branch.as_deref().unwrap_or(&then_branch).span().end;
            Ok(EntryStatement::If { condition, then_branch, else_branch, span: Span { start, end } })
        } else if self.cursor.consume_ident(word::FOR) {
            self.expect_symbol('(')?;
            let binding = match self.cursor.current().kind.clone() {
                TokenKind::Ident(name) => EntryBinding { name, source_type: "int".to_string(), actor_type_state: None },
                _ => return Err(self.error("expected loop binding identifier")),
            };
            let header = self.cursor.take_balanced_after_open('(', ')').ok_or_else(|| self.error("unterminated `(` group"))?;
            let body = Box::new(self.parse_block_or_statement()?);
            let span = Span { start, end: body.span().end };
            Ok(EntryStatement::For { binding, header, body, span })
        } else if self.cursor.consume_ident(word::BECOME) {
            let routes = self.parse_become_tail()?;
            Ok(EntryStatement::Become { routes, span: self.cursor.consumed_span_from(start) })
        } else if self.check_outputs_become_start() {
            let (group, routes) = self.parse_outputs_become()?;
            Ok(EntryStatement::ValidateOutputsBecome { group, routes, span: self.cursor.consumed_span_from(start) })
        } else if self.cursor.check_braced_assignment_start() {
            self.skip_until_statement_end()?;
            Ok(self.plain_statement(token_start, start))
        } else if self.cursor.consume_symbol('{') {
            self.parse_block_after_open(start)
        } else {
            self.skip_until_statement_end()?;
            Ok(self.plain_statement(token_start, start))
        }
    }

    fn plain_statement(&self, token_start: usize, byte_start: usize) -> EntryStatement {
        let span = self.cursor.consumed_span_from(byte_start);
        let parsed = PlainBindingParser::new(self.cursor.body, &self.cursor.body.tokens[token_start..self.cursor.pos]).parse();
        match parsed {
            ParsedPlain::Local(declaration) => EntryStatement::Local { declaration, span },
            ParsedPlain::Bindings { bindings, destructuring } => EntryStatement::Plain { bindings, destructuring, span },
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

    fn parse_outputs_become(&mut self) -> Result<(String, Vec<EntryRoute>)> {
        self.expect_ident(word::REQUIRE)?;
        let group = self.expect_any_ident()?;
        self.expect_symbol('.')?;
        self.expect_ident(word::OUTPUTS)?;
        self.expect_ident(word::BECOME)?;
        Ok((group, self.parse_become_tail()?))
    }

    fn parse_route(&mut self) -> Result<EntryRoute> {
        let output = self.expect_any_ident()?;
        if !self.consume_left_arrow() {
            return Err(self.error("every `become` route must name its output with `output <- Actor(state)`"));
        }
        let actor = self.take_route_actor_expr()?;

        self.expect_symbol('(')?;
        let state = self.cursor.take_balanced_after_open('(', ')').ok_or_else(|| self.error("unterminated route state expression"))?;
        Ok(EntryRoute { output, actor, state })
    }

    fn take_route_actor_expr(&mut self) -> Result<Span> {
        let start = self.cursor.byte_offset();
        let mut depth = 0usize;
        while !self.cursor.is_eof() {
            match self.cursor.current().kind {
                TokenKind::Symbol('(') if depth == 0 => {
                    if self.cursor.byte_offset() == start {
                        return Err(self.error("become target is empty"));
                    }
                    return Ok(self.cursor.span_to_current(start));
                }
                TokenKind::Symbol('{') | TokenKind::Symbol('[') | TokenKind::Symbol('<') => {
                    depth += 1;
                    self.cursor.advance();
                }
                TokenKind::Symbol('}') | TokenKind::Symbol(']') | TokenKind::Symbol('>') if depth > 0 => {
                    depth -= 1;
                    self.cursor.advance();
                }
                TokenKind::Symbol(',')
                | TokenKind::Symbol(';')
                | TokenKind::Symbol(')')
                | TokenKind::Symbol('}')
                | TokenKind::Symbol(']')
                | TokenKind::Symbol('>')
                    if depth == 0 =>
                {
                    return Err(self.error("expected `(` after become target"));
                }
                _ => self.cursor.advance(),
            }
        }
        Err(self.error("unterminated become target"))
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

    fn expect_ident(&mut self, expected: &str) -> Result<()> {
        if self.cursor.consume_ident(expected) { Ok(()) } else { Err(self.error(format!("expected `{expected}`"))) }
    }

    fn expect_list_separator_or_end(&mut self, end: char) -> Result<()> {
        if self.cursor.consume_symbol(',') || self.cursor.check_symbol(end) {
            Ok(())
        } else {
            Err(self.error(format!("expected `,` or `{end}`")))
        }
    }

    fn check_outputs_become_start(&self) -> bool {
        matches!(&self.cursor.current().kind, TokenKind::Ident(actual) if actual == word::REQUIRE)
            && matches!(self.cursor.peek_kind(1), Some(TokenKind::Ident(_)))
            && matches!(self.cursor.peek_kind(2), Some(TokenKind::Symbol('.')))
            && matches!(self.cursor.peek_kind(3), Some(TokenKind::Ident(actual)) if actual == word::OUTPUTS)
            && matches!(self.cursor.peek_kind(4), Some(TokenKind::Ident(actual)) if actual == word::BECOME)
    }

    fn error(&self, message: impl Into<String>) -> crate::error::ArgentError {
        let preview = self.cursor.remaining_text().lines().next().unwrap_or("").trim().chars().take(80).collect::<String>();
        crate::error::ArgentError::new(format!("{} at body byte {} near `{}`", message.into(), self.cursor.byte_offset(), preview))
    }
}

/// Recognizes the binding surfaces of ordinary Sil statements while leaving
/// their expressions and semantics to Silverscript.
struct PlainBindingParser<'a> {
    body: &'a EntryBody,
    tokens: &'a [Token],
    pos: usize,
}

enum ParsedPlain {
    Local(EntryLocalDecl),
    Bindings { bindings: Vec<EntryBinding>, destructuring: Option<EntryStructDestructure> },
}

impl Default for ParsedPlain {
    fn default() -> Self {
        Self::Bindings { bindings: Vec::new(), destructuring: None }
    }
}

struct ParsedBindingType {
    source: String,
    actor_type_state: Option<String>,
}

struct ParsedVariableBinding {
    binding: EntryBinding,
    declared_type: Span,
}

impl<'a> PlainBindingParser<'a> {
    fn new(body: &'a EntryBody, tokens: &'a [Token]) -> Self {
        Self { body, tokens, pos: 0 }
    }

    fn parse(mut self) -> ParsedPlain {
        if self.consume_symbol('{') {
            return self.parse_struct_bindings(None).unwrap_or_default();
        }
        if self.consume_symbol('(') {
            return self.parse_parenthesized_bindings().unwrap_or_default();
        }
        let start = self.pos;
        let type_start = self.current().map(|token| token.span.start);
        if self.parse_type().is_some() {
            let type_end = self.tokens.get(self.pos.saturating_sub(1)).map(|token| token.span.end);
            if self.consume_symbol('{')
                && let (Some(type_start), Some(type_end)) = (type_start, type_end)
            {
                let declared_type = Span { start: type_start, end: type_end };
                return self.parse_struct_bindings(Some(declared_type)).unwrap_or_default();
            }
        }
        self.pos = start;
        self.parse_leading_bindings().unwrap_or_default()
    }

    fn parse_struct_bindings(&mut self, declared_type: Option<Span>) -> Option<ParsedPlain> {
        let mut bindings = Vec::new();
        loop {
            self.take_ident()?;
            self.consume_symbol(':').then_some(())?;
            bindings.push(self.parse_typed_binding()?);
            if self.consume_symbol('}') {
                break;
            }
            self.consume_symbol(',').then_some(())?;
            if self.consume_symbol('}') {
                break;
            }
        }
        self.consume_symbol('=').then_some(())?;
        let destructuring = match declared_type {
            Some(declared_type) => Some(EntryStructDestructure { declared_type, value: self.remaining_initializer_span()? }),
            None => None,
        };
        Some(ParsedPlain::Bindings { bindings, destructuring })
    }

    fn parse_parenthesized_bindings(&mut self) -> Option<ParsedPlain> {
        let mut bindings = vec![self.parse_typed_binding()?];
        while self.consume_symbol(',') {
            if self.check_symbol(')') {
                break;
            }
            bindings.push(self.parse_typed_binding()?);
        }
        self.consume_symbol(')').then_some(())?;
        self.consume_symbol('=').then_some(())?;
        Some(ParsedPlain::Bindings { bindings, destructuring: None })
    }

    fn parse_leading_bindings(&mut self) -> Option<ParsedPlain> {
        if self.check_ident("return") {
            return None;
        }
        let first = self.parse_variable_binding()?;
        if self.consume_symbol(',') {
            let second = self.parse_typed_binding()?;
            self.consume_symbol('=').then_some(())?;
            return Some(ParsedPlain::Bindings { bindings: vec![first.binding, second], destructuring: None });
        }
        if self.consume_symbol('=') {
            return self.remaining_initializer_span().map(|initializer| {
                ParsedPlain::Local(EntryLocalDecl { binding: first.binding, declared_type: first.declared_type, initializer })
            });
        }
        self.check_symbol(';').then_some(())?;
        Some(ParsedPlain::Bindings { bindings: vec![first.binding], destructuring: None })
    }

    fn parse_variable_binding(&mut self) -> Option<ParsedVariableBinding> {
        let declared_type_start = self.current()?.span.start;
        let ty = self.parse_type()?;
        while self.consume_ident("constant") {}
        let declared_type_end = self.tokens.get(self.pos.checked_sub(1)?)?.span.end;
        let name = self.take_ident()?;
        Some(ParsedVariableBinding {
            binding: EntryBinding { name, source_type: ty.source, actor_type_state: ty.actor_type_state },
            declared_type: Span { start: declared_type_start, end: declared_type_end },
        })
    }

    fn parse_typed_binding(&mut self) -> Option<EntryBinding> {
        let ty = self.parse_type()?;
        let name = self.take_ident()?;
        Some(EntryBinding { name, source_type: ty.source, actor_type_state: ty.actor_type_state })
    }

    fn parse_type(&mut self) -> Option<ParsedBindingType> {
        let start = self.current()?.span.start;
        let base = self.take_ident()?;
        let actor_type_state = if base == word::ACTOR_TYPE && self.consume_symbol('<') {
            let state = self.take_ident()?;
            self.consume_symbol('>').then_some(())?;
            Some(state)
        } else {
            None
        };
        let mut has_array_dimension = false;
        while self.consume_symbol('[') {
            has_array_dimension = true;
            while !self.check_symbol(']') {
                self.advance()?;
            }
            self.consume_symbol(']').then_some(())?;
        }
        let end = self.tokens.get(self.pos.checked_sub(1)?)?.span.end;
        Some(ParsedBindingType {
            source: self.body.text[start..end].to_string(),
            actor_type_state: actor_type_state.filter(|_| !has_array_dimension),
        })
    }

    fn remaining_initializer_span(&self) -> Option<Span> {
        let first = self.tokens.get(self.pos)?;
        let last =
            self.tokens[self.pos..].iter().rev().find(|token| !matches!(token.kind, TokenKind::Symbol(';') | TokenKind::Eof))?;
        (first.span.start < last.span.end).then_some(Span { start: first.span.start, end: last.span.end })
    }

    fn current(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn advance(&mut self) -> Option<()> {
        self.current()?;
        self.pos += 1;
        Some(())
    }

    fn check_ident(&self, expected: &str) -> bool {
        matches!(self.current().map(|token| &token.kind), Some(TokenKind::Ident(actual)) if actual == expected)
    }

    fn consume_ident(&mut self, expected: &str) -> bool {
        if self.check_ident(expected) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn take_ident(&mut self) -> Option<String> {
        let TokenKind::Ident(name) = &self.current()?.kind else {
            return None;
        };
        let name = name.clone();
        self.pos += 1;
        Some(name)
    }

    fn check_symbol(&self, expected: char) -> bool {
        matches!(self.current().map(|token| &token.kind), Some(TokenKind::Symbol(actual)) if *actual == expected)
    }

    fn consume_symbol(&mut self, expected: char) -> bool {
        if self.check_symbol(expected) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
}

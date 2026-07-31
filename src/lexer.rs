use crate::error::{ArgentError, Result};
use crate::language::word;

pub const RESERVED_GENERATED_PREFIX: &str = "gen__";
pub const RESERVED_GENERATED_TYPE_PREFIX: &str = "Gen__";

#[derive(Debug, Clone)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKind {
    Ident(String),
    Number(String),
    Str(String),
    Arrow,
    LeftArrow,
    Symbol(char),
    Eof,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl std::fmt::Display for Span {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}..{}", self.start, self.end)
    }
}

pub fn lex(source: &str) -> Result<Vec<Token>> {
    lex_with_policy(source, IdentifierPolicy::Any)
}

pub fn lex_argent_source(source: &str) -> Result<Vec<Token>> {
    lex_with_policy(source, IdentifierPolicy::ArgentSource)
}

fn lex_with_policy(source: &str, identifier_policy: IdentifierPolicy) -> Result<Vec<Token>> {
    let mut lexer = Lexer { source, bytes: source.as_bytes(), pos: 0, tokens: Vec::new(), identifier_policy };
    lexer.run()?;
    Ok(lexer.tokens)
}

#[derive(Clone, Copy)]
enum IdentifierPolicy {
    Any,
    ArgentSource,
}

struct Lexer<'a> {
    source: &'a str,
    bytes: &'a [u8],
    pos: usize,
    tokens: Vec<Token>,
    identifier_policy: IdentifierPolicy,
}

impl Lexer<'_> {
    fn run(&mut self) -> Result<()> {
        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            match b {
                b' ' | b'\t' | b'\r' | b'\n' => self.pos += 1,
                b'/' if self.peek_byte(1) == Some(b'/') => self.skip_line_comment(),
                b'/' if self.peek_byte(1) == Some(b'*') => self.skip_block_comment()?,
                b'"' => self.lex_string()?,
                b'0'..=b'9' => self.lex_number(),
                b'a'..=b'z' | b'A'..=b'Z' | b'_' => self.lex_ident()?,
                b'-' if self.peek_byte(1) == Some(b'>') => {
                    let start = self.pos;
                    self.pos += 2;
                    self.push(TokenKind::Arrow, start, self.pos);
                }
                b'<' if self.peek_byte(1) == Some(b'-') => {
                    let start = self.pos;
                    self.pos += 2;
                    self.push(TokenKind::LeftArrow, start, self.pos);
                }
                b'{' | b'}' | b'(' | b')' | b'[' | b']' | b';' | b':' | b',' | b'|' | b'&' | b'!' | b'=' | b'+' | b'-' | b'*'
                | b'/' | b'%' | b'<' | b'>' | b'.' => {
                    let start = self.pos;
                    self.pos += 1;
                    self.push(TokenKind::Symbol(b as char), start, self.pos);
                }
                _ => {
                    let unexpected = self.source[self.pos..].chars().next().expect("lexer position is within source");
                    return Err(self.error_at(self.pos, format!("unexpected character `{unexpected}`")));
                }
            }
        }
        self.push(TokenKind::Eof, self.pos, self.pos);
        Ok(())
    }

    fn peek_byte(&self, offset: usize) -> Option<u8> {
        self.bytes.get(self.pos + offset).copied()
    }

    fn skip_line_comment(&mut self) {
        while self.pos < self.bytes.len() && self.bytes[self.pos] != b'\n' {
            self.pos += 1;
        }
    }

    fn skip_block_comment(&mut self) -> Result<()> {
        let start = self.pos;
        self.pos += 2;
        let mut depth = 1;

        while self.pos < self.bytes.len() {
            match (self.peek_byte(0), self.peek_byte(1)) {
                (Some(b'/'), Some(b'*')) => {
                    depth += 1;
                    self.pos += 2;
                }
                (Some(b'*'), Some(b'/')) => {
                    depth -= 1;
                    self.pos += 2;
                    if depth == 0 {
                        return Ok(());
                    }
                }
                _ => self.pos += 1,
            }
        }

        Err(self.error_at(start, "unterminated block comment"))
    }

    fn lex_string(&mut self) -> Result<()> {
        let start = self.pos;
        self.pos += 1;
        let mut out = String::new();
        while self.pos < self.bytes.len() {
            match self.bytes[self.pos] {
                b'"' => {
                    self.pos += 1;
                    self.push(TokenKind::Str(out), start, self.pos);
                    return Ok(());
                }
                b'\\' => {
                    self.pos += 1;
                    let escaped = *self.bytes.get(self.pos).ok_or_else(|| self.error_at(self.pos, "unterminated string escape"))?;
                    out.push(escaped as char);
                    self.pos += 1;
                }
                b => {
                    out.push(b as char);
                    self.pos += 1;
                }
            }
        }
        Err(self.error_at(start, "unterminated string"))
    }

    fn lex_number(&mut self) {
        let start = self.pos;
        while matches!(self.peek_byte(0), Some(b'0'..=b'9')) {
            self.pos += 1;
        }
        self.push(TokenKind::Number(self.source[start..self.pos].to_string()), start, self.pos);
    }

    fn lex_ident(&mut self) -> Result<()> {
        let start = self.pos;
        while matches!(self.peek_byte(0), Some(b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_')) {
            self.pos += 1;
        }
        let ident = self.source[start..self.pos].to_string();
        if matches!(self.identifier_policy, IdentifierPolicy::ArgentSource) {
            if ident == word::LEGACY_COVENANT_ID {
                return Err(self.error_at(start, format!("`{}` was renamed to `{}`", word::LEGACY_COVENANT_ID, word::COVENANT_ID)));
            }
            let generated_prefix =
                [RESERVED_GENERATED_PREFIX, RESERVED_GENERATED_TYPE_PREFIX].into_iter().find(|prefix| ident.starts_with(prefix));
            if let Some(generated_prefix) = generated_prefix {
                return Err(
                    self.error_at(start, format!("identifier `{ident}` uses reserved generated namespace `{generated_prefix}`"))
                );
            }
        }
        self.push(TokenKind::Ident(ident), start, self.pos);
        Ok(())
    }

    fn error_at(&self, byte_offset: usize, message: impl Into<String>) -> ArgentError {
        ArgentError::in_source(self.source, byte_offset, message)
    }

    fn push(&mut self, kind: TokenKind, start: usize, end: usize) {
        self.tokens.push(Token { kind, span: Span { start, end } });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_nested_block_comments() {
        let tokens = lex("before /* outer /* inner */ outer */ after").expect("nested block comments must lex");
        let identifiers = tokens
            .iter()
            .filter_map(|token| match &token.kind {
                TokenKind::Ident(value) => Some(value.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(identifiers, ["before", "after"]);
    }

    #[test]
    fn reports_unterminated_block_comment_location() {
        let err = lex("before\n  /* never closed").expect_err("unterminated block comment must be rejected");

        assert_eq!(err.to_string(), "2:3: unterminated block comment");
    }

    #[test]
    fn rejects_reserved_generated_namespace_identifier() {
        for source in ["state gen__state {}", "state Gen__State {}"] {
            let err = lex_argent_source(source).expect_err("reserved generated namespace must be rejected");
            assert!(err.to_string().contains("reserved generated namespace"), "unexpected error: {err}");
        }
    }

    #[test]
    fn rejects_legacy_covenant_id_keyword() {
        let err = lex_argent_source("covid value;").expect_err("the legacy covenant id keyword must be rejected");
        assert!(err.to_string().contains("`covid` was renamed to `cov_id`"), "unexpected error: {err}");
    }

    #[test]
    fn internal_lexing_accepts_generated_identifiers() {
        lex("tx.outputs[gen__next_output_idx].value").expect("generated lowering text lexes");
    }
}

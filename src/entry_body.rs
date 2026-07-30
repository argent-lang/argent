//! Canonical source text and tokens for an Argent entry body.

use crate::error::Result;
use crate::lexer::{Token, lex};

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
}

impl Default for EntryBody {
    fn default() -> Self {
        Self::new(String::new()).expect("empty entry body lexes")
    }
}

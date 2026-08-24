//! Current token-based discovery of variable identifiers in function bodies.

use crate::compiler::syntax::lexer::{Span, Token, TokenKind, lex};
use crate::error::Result;

use super::FunctionNames;

// TODO: Replace token classification with structured Sil function parsing
// once that representation is available.
pub(super) fn variable_occurrences(body: &str, names: &FunctionNames) -> Result<Vec<Span>> {
    let tokens = lex(body)?;
    Ok(tokens
        .iter()
        .enumerate()
        .filter_map(|(pos, token)| match &token.kind {
            TokenKind::Ident(name) if is_variable_identifier(&tokens, pos, name, names) => Some(token.span),
            _ => None,
        })
        .collect())
}

fn is_variable_identifier(tokens: &[Token], pos: usize, name: &str, names: &FunctionNames) -> bool {
    if is_language_identifier(name) || names.constants.contains(name) || is_type_position(tokens, pos, name, names) {
        return false;
    }

    let previous = kind(tokens, pos.checked_sub(1));
    let next = kind(tokens, Some(pos + 1));
    let starts_qualified_name = is_symbol(next, ':') && is_symbol(kind(tokens, Some(pos + 2)), ':');
    let ends_qualified_name = is_symbol(previous, ':') && is_symbol(kind(tokens, pos.checked_sub(2)), ':');
    if is_symbol(previous, '.') || is_symbol(next, '(') || starts_qualified_name || ends_qualified_name {
        return false;
    }

    // Struct field labels follow `{` or `,`. A ternary value before `:` does
    // not, so it remains a variable occurrence once the lexer accepts `?`.
    let follows_entry_separator = is_symbol(previous, '{') || is_symbol(previous, ',');
    let is_field_label = follows_entry_separator && is_symbol(next, ':');
    !is_field_label
}

fn is_type_position(tokens: &[Token], pos: usize, name: &str, names: &FunctionNames) -> bool {
    if !names.types.contains(name) {
        return false;
    }
    if matches!(kind(tokens, pos.checked_sub(1)), Some(TokenKind::Ident(previous)) if previous == "as" || previous == "new") {
        return true;
    }

    let after_type = after_array_suffixes(tokens, pos + 1);
    matches!(kind(tokens, Some(after_type)), Some(TokenKind::Ident(_)) | Some(TokenKind::Symbol('{')))
}

fn after_array_suffixes(tokens: &[Token], mut pos: usize) -> usize {
    while is_symbol(kind(tokens, Some(pos)), '[') {
        let mut depth = 0usize;
        while let Some(token) = tokens.get(pos) {
            match token.kind {
                TokenKind::Symbol('[') => depth += 1,
                TokenKind::Symbol(']') => {
                    depth -= 1;
                    if depth == 0 {
                        pos += 1;
                        break;
                    }
                }
                TokenKind::Eof => return pos,
                _ => {}
            }
            pos += 1;
        }
    }
    pos
}

fn kind(tokens: &[Token], pos: Option<usize>) -> Option<&TokenKind> {
    pos.and_then(|pos| tokens.get(pos)).map(|token| &token.kind)
}

fn is_symbol(kind: Option<&TokenKind>, expected: char) -> bool {
    matches!(kind, Some(TokenKind::Symbol(actual)) if *actual == expected)
}

fn is_language_identifier(name: &str) -> bool {
    matches!(
        name,
        "if" | "else"
            | "for"
            | "return"
            | "require"
            | "new"
            | "as"
            | "constant"
            | "true"
            | "false"
            | "int"
            | "temporal"
            | "bool"
            | "string"
            | "pubkey"
            | "sig"
            | "datasig"
            | "byte"
            | "tx"
            | "this"
            | "console"
            | "r0"
            | "g16"
            | "date"
            | "litras"
            | "grains"
            | "kas"
            | "seconds"
            | "minutes"
            | "hours"
            | "days"
            | "weeks"
    )
}

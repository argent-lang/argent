//! Token-aware helpers for inspecting and rewriting source references.
//!
//! Matches are selected from the original token stream and replacements are
//! applied once, so generated text is never reconsidered as source.

use std::collections::BTreeMap;

use crate::error::Result;
use crate::lexer::{Span, Token, TokenKind, lex};

enum RefToken<'a> {
    Ident(&'a str),
    Dot,
}

impl RefToken<'_> {
    fn matches(&self, token: &TokenKind) -> bool {
        match (self, token) {
            (Self::Ident(reference_ident), TokenKind::Ident(input_ident)) => reference_ident == input_ident,
            (Self::Dot, TokenKind::Symbol('.')) => true,
            _ => false,
        }
    }
}

/// A dotted identifier path such as `self.value` or `remote.inputs.asset.state`.
struct QualifiedRef<'a> {
    tokens: Vec<RefToken<'a>>,
}

impl<'a> QualifiedRef<'a> {
    fn parse(reference: &'a str) -> Option<Self> {
        let mut tokens = Vec::new();
        for (idx, segment) in reference.split('.').enumerate() {
            if segment.is_empty() {
                return None;
            }
            if idx > 0 {
                tokens.push(RefToken::Dot);
            }
            tokens.push(RefToken::Ident(segment));
        }
        (!tokens.is_empty()).then_some(Self { tokens })
    }

    fn root(&self) -> &'a str {
        let Some(RefToken::Ident(root)) = self.tokens.first() else {
            unreachable!("a parsed qualified reference starts with an identifier");
        };
        root
    }

    fn match_at(&self, tokens: &[Token], start: usize) -> Option<(usize, Span)> {
        // A suffix of another access path is not rooted at this identifier.
        if start > 0 && matches!(tokens[start - 1].kind, TokenKind::Symbol('.')) {
            return None;
        }

        let token_len = self.tokens.len();
        let window = tokens.get(start..start + token_len)?;
        let matches = self.tokens.iter().zip(window).all(|(reference_token, input_token)| reference_token.matches(&input_token.kind));
        matches.then_some((token_len, Span { start: window[0].span.start, end: window[token_len - 1].span.end }))
    }
}

/// Count rooted occurrences of one dotted reference in an existing token stream.
pub(crate) fn count_qualified_ref(tokens: &[Token], reference: &str) -> usize {
    let Some(reference) = QualifiedRef::parse(reference) else {
        return 0;
    };
    (0..tokens.len()).filter(|start| reference.match_at(tokens, *start).is_some()).count()
}

/// Rewrite dotted references selected from the original source tokens.
pub(crate) fn rewrite_qualified_refs<'a>(input: &str, replacements: impl IntoIterator<Item = (&'a str, &'a str)>) -> Result<String> {
    // Index paths by their root so each input token checks only candidates that can match it.
    let mut replacements_by_root = BTreeMap::<_, Vec<_>>::new();
    for (reference, replacement) in replacements {
        let Some(reference) = QualifiedRef::parse(reference) else {
            continue;
        };
        replacements_by_root.entry(reference.root()).or_default().push((reference, replacement));
    }
    for candidates in replacements_by_root.values_mut() {
        // The first matching candidate must be the most specific path.
        candidates.sort_by(|(left, _), (right, _)| right.tokens.len().cmp(&left.tokens.len()));
    }
    if replacements_by_root.is_empty() {
        return Ok(input.to_string());
    }

    let tokens = lex(input)?;
    let mut out = String::with_capacity(input.len());
    let mut cursor = 0usize;
    let mut pos = 0usize;
    while pos < tokens.len() {
        let selected = match &tokens[pos].kind {
            TokenKind::Ident(root) => replacements_by_root.get(root.as_str()).and_then(|candidates| {
                candidates
                    .iter()
                    .find_map(|(reference, replacement)| reference.match_at(&tokens, pos).map(|(len, span)| (len, span, *replacement)))
            }),
            _ => None,
        };
        let Some((token_len, span, replacement)) = selected else {
            pos += 1;
            continue;
        };
        out.push_str(&input[cursor..span.start]);
        out.push_str(replacement);
        cursor = span.end;
        pos += token_len;
    }
    out.push_str(&input[cursor..]);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_only_references_rooted_at_the_matched_identifier() {
        let tokens = lex(r#"next.value + parent.next.value + next.value_note + "next.value" /* next.value */"#).expect("source lexes");

        assert_eq!(count_qualified_ref(&tokens, "next.value"), 1);
    }

    #[test]
    fn rewrites_only_token_matched_references() {
        let source = r#"self.value + foo.self.value + self.value_note + "self.value" /* self.value */"#;
        let out = rewrite_qualified_refs(source, [("self.value", "active_value")]).expect("references rewrite");

        assert_eq!(out, r#"active_value + foo.self.value + self.value_note + "self.value" /* self.value */"#);
    }

    #[test]
    fn selects_the_most_specific_reference_at_each_source_position() {
        let source = "remote.inputs.asset.state + remote.inputs.count";
        let out = rewrite_qualified_refs(source, [("remote.inputs", "group"), ("remote.inputs.asset.state", "asset_state")])
            .expect("references rewrite");

        assert_eq!(out, "asset_state + group.count");
    }

    #[test]
    fn does_not_reconsider_replacement_text() {
        let out = rewrite_qualified_refs("next.value + self.value", [("next.value", "self.value"), ("self.value", "active_value")])
            .expect("references rewrite");

        assert_eq!(out, "self.value + active_value");
    }
}

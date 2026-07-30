//! Token-aware helpers for inspecting and rewriting source references.
//!
//! Matches are selected from the original token stream and replacements are
//! applied once, so generated text is never reconsidered as source.

use std::collections::BTreeMap;

use crate::error::{ArgentError, Result};
use crate::lexer::{Span, Token, TokenKind, lex};
use crate::naming::is_identifier;

enum RefToken {
    Ident(String),
    Dot,
}

impl RefToken {
    fn matches(&self, token: &TokenKind) -> bool {
        match (self, token) {
            (Self::Ident(reference_ident), TokenKind::Ident(input_ident)) => reference_ident == input_ident,
            (Self::Dot, TokenKind::Symbol('.')) => true,
            _ => false,
        }
    }
}

/// A dotted identifier path such as `self.value` or `remote.inputs.asset.state`.
struct QualifiedRef {
    tokens: Vec<RefToken>,
}

impl QualifiedRef {
    fn parse(reference: &str) -> Option<Self> {
        let mut tokens = Vec::new();
        for (idx, segment) in reference.split('.').enumerate() {
            if !is_identifier(segment) {
                return None;
            }
            if idx > 0 {
                tokens.push(RefToken::Dot);
            }
            tokens.push(RefToken::Ident(segment.to_string()));
        }
        (!tokens.is_empty()).then_some(Self { tokens })
    }

    fn root(&self) -> &str {
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

struct RefReplacement {
    reference: QualifiedRef,
    text: String,
}

/// Indexes replacements by root identifier so matching only inspects viable paths.
pub(crate) struct RefReplacements {
    by_root: BTreeMap<String, Vec<RefReplacement>>,
}

struct RefReplacementMatch<'a> {
    token_len: usize,
    span: Span,
    text: &'a str,
}

impl RefReplacements {
    pub(crate) fn new<R, T>(replacements: impl IntoIterator<Item = (R, T)>) -> Result<Self>
    where
        R: Into<String>,
        T: Into<String>,
    {
        let mut by_root = BTreeMap::<_, Vec<_>>::new();
        for (reference, text) in replacements {
            let reference_text = reference.into();
            let reference = QualifiedRef::parse(&reference_text)
                .ok_or_else(|| ArgentError::new(format!("invalid qualified reference `{reference_text}` in replacement plan")))?;
            by_root.entry(reference.root().to_string()).or_default().push(RefReplacement { reference, text: text.into() });
        }
        for candidates in by_root.values_mut() {
            // The first matching candidate must be the most specific path.
            candidates.sort_by(|left, right| right.reference.tokens.len().cmp(&left.reference.tokens.len()));
        }
        Ok(Self { by_root })
    }

    fn is_empty(&self) -> bool {
        self.by_root.is_empty()
    }

    fn match_at(&self, tokens: &[Token], pos: usize) -> Option<RefReplacementMatch<'_>> {
        let TokenKind::Ident(root) = &tokens.get(pos)?.kind else {
            return None;
        };
        self.by_root.get(root.as_str())?.iter().find_map(|candidate| {
            candidate.reference.match_at(tokens, pos).map(|(token_len, span)| RefReplacementMatch {
                token_len,
                span,
                text: &candidate.text,
            })
        })
    }

    /// Rewrite dotted references selected from the original input tokens.
    pub(crate) fn rewrite(&self, input: &str) -> Result<String> {
        if self.is_empty() {
            return Ok(input.to_string());
        }

        let tokens = lex(input)?;
        let mut out = String::with_capacity(input.len());
        let mut cursor = 0usize;
        let mut pos = 0usize;
        while pos < tokens.len() {
            let Some(replacement) = self.match_at(&tokens, pos) else {
                pos += 1;
                continue;
            };
            out.push_str(&input[cursor..replacement.span.start]);
            out.push_str(replacement.text);
            cursor = replacement.span.end;
            pos += replacement.token_len;
        }
        out.push_str(&input[cursor..]);
        Ok(out)
    }
}

/// Count rooted occurrences of one dotted reference in an existing token stream.
pub(crate) fn count_qualified_ref(tokens: &[Token], reference: &str) -> usize {
    let Some(reference) = QualifiedRef::parse(reference) else {
        return 0;
    };
    (0..tokens.len()).filter(|start| reference.match_at(tokens, *start).is_some()).count()
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
        let out = RefReplacements::new([("self.value", "active_value")])
            .expect("replacement plan is valid")
            .rewrite(source)
            .expect("references rewrite");

        assert_eq!(out, r#"active_value + foo.self.value + self.value_note + "self.value" /* self.value */"#);
    }

    #[test]
    fn selects_the_most_specific_reference_at_each_source_position() {
        let source = "remote.inputs.asset.state + remote.inputs.count";
        let out = RefReplacements::new([("remote.inputs", "group"), ("remote.inputs.asset.state", "asset_state")])
            .expect("replacement plan is valid")
            .rewrite(source)
            .expect("references rewrite");

        assert_eq!(out, "asset_state + group.count");
    }

    #[test]
    fn does_not_reconsider_replacement_text() {
        let out = RefReplacements::new([("next.value", "self.value"), ("self.value", "active_value")])
            .expect("replacement plan is valid")
            .rewrite("next.value + self.value")
            .expect("references rewrite");

        assert_eq!(out, "self.value + active_value");
    }

    #[test]
    fn rejects_invalid_replacement_references() {
        for reference in ["self..value", "self-value", "self.0"] {
            let err = match RefReplacements::new([(reference, "active_value")]) {
                Ok(_) => panic!("invalid replacement plan must fail"),
                Err(err) => err,
            };

            assert!(err.to_string().contains(&format!("invalid qualified reference `{reference}`")), "unexpected error: {err}");
        }
    }
}

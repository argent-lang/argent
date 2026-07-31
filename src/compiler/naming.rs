//! Compiler naming helpers shared by modeling and emission.

/// Return whether `input` is a source identifier.
pub(crate) fn is_identifier(input: &str) -> bool {
    let mut chars = input.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_') && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

/// Convert a source name to snake case while preserving acronym runs.
pub(crate) fn to_snake(input: &str) -> String {
    let mut out = String::new();
    let chars = input.chars().collect::<Vec<_>>();
    for (idx, ch) in chars.iter().copied().enumerate() {
        if ch.is_ascii_uppercase() {
            let prev = idx.checked_sub(1).and_then(|prev| chars.get(prev)).copied();
            let next = chars.get(idx + 1).copied();
            let starts_new_word = prev.is_some_and(|prev| {
                prev != '_'
                    && (prev.is_ascii_lowercase() || prev.is_ascii_digit() || next.is_some_and(|next| next.is_ascii_lowercase()))
            });
            if starts_new_word && !out.ends_with('_') {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

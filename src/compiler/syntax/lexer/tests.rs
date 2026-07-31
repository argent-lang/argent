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

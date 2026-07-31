use super::{EntryBinding, EntryBody, EntryStatement, lex};

fn binding(name: &str, source_type: &str) -> EntryBinding {
    EntryBinding { name: name.to_string(), source_type: source_type.to_string() }
}

#[test]
fn cursor_takes_nested_balanced_source() {
    let body = EntryBody::new("if (outer(inner)) become next <- Done(state);").expect("body lexes");
    let mut cursor = body.cursor();

    assert!(cursor.consume_ident("if"));
    assert!(cursor.consume_symbol('('));
    let span = cursor.take_balanced_after_open('(', ')').expect("condition closes");
    assert_eq!(body.span_text(span), "outer(inner)");
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

    let [EntryStatement::Plain { span: plain, .. }, EntryStatement::If { condition, then_branch, else_branch, .. }] =
        body.statements()
    else {
        panic!("expected one plain statement followed by an if");
    };
    assert_eq!(body.span_text(*plain).trim(), "int next = value;");
    assert_eq!(body.span_text(*condition), "done");
    assert!(matches!(then_branch.as_ref(), EntryStatement::Block { .. }));
    assert!(matches!(else_branch.as_deref(), Some(EntryStatement::Block { .. })));
}

#[test]
fn statements_keep_for_structure_and_the_following_boundary() {
    let body = EntryBody::new(
        r#"
            for (i, 0, count, MAX_COUNT) {
                check(i);
            }
            require(done);
            "#,
    )
    .expect("body lexes");

    let [EntryStatement::For { binding: loop_binding, header, body: loop_body, .. }, EntryStatement::Plain { span: following, .. }] =
        body.statements()
    else {
        panic!("expected one for loop followed by one plain statement");
    };
    assert_eq!(loop_binding, &binding("i", "int"));
    assert_eq!(body.span_text(*header), "i, 0, count, MAX_COUNT");
    assert!(matches!(loop_body.as_ref(), EntryStatement::Block { .. }));
    assert_eq!(body.span_text(*following).trim(), "require(done);");
}

#[test]
fn plain_statements_record_sil_bindings() {
    let body = EntryBody::new(
        r#"
            int value = 1;
            byte[32] constant hash = digest;
            (int left, bool right,) = pair();
            int first, int second = pair();
            {item: int unpacked, hash: byte[32] unpacked_hash,} = state;
            require(value == first);
            value = second;
            return value;
            "#,
    )
    .expect("body parses");

    let expected = [
        vec![binding("value", "int")],
        vec![binding("hash", "byte[32]")],
        vec![binding("left", "int"), binding("right", "bool")],
        vec![binding("first", "int"), binding("second", "int")],
        vec![binding("unpacked", "int"), binding("unpacked_hash", "byte[32]")],
        vec![],
        vec![],
        vec![],
    ];
    assert_eq!(body.statements().len(), expected.len());
    for (statement, expected) in body.statements().iter().zip(expected) {
        let EntryStatement::Plain { bindings, .. } = statement else {
            panic!("expected a plain statement");
        };
        assert_eq!(bindings, &expected);
    }
}

#[test]
fn statements_distinguish_brace_assignments_from_standalone_blocks() {
    let body = EntryBody::new(
        r#"
            {left: int first, right: int second} = pair;
            {count: int current} = readInputState(index);
            {
                require(first == current);
            }
            "#,
    )
    .expect("body lexes");

    let [
        EntryStatement::Plain { span: destructure, .. },
        EntryStatement::Plain { span: state_read, .. },
        EntryStatement::Block { .. },
    ] = body.statements()
    else {
        panic!("expected two brace assignments followed by one standalone block");
    };
    assert_eq!(body.span_text(*destructure).trim(), "{left: int first, right: int second} = pair;");
    assert_eq!(body.span_text(*state_read).trim(), "{count: int current} = readInputState(index);");
}

#[test]
fn statements_keep_output_validation_and_dynamic_route_targets() {
    let body = EntryBody::new(
        r#"
            require children.outputs become {
                child <- self.child_type(next_child),
            };
            become next <- target(next_state);
            "#,
    )
    .expect("body parses");

    let [
        EntryStatement::ValidateOutputsBecome { group, routes: validation_routes, .. },
        EntryStatement::Become { routes: become_routes, .. },
    ] = body.statements()
    else {
        panic!("expected output validation followed by become");
    };
    assert_eq!(group, "children");
    assert_eq!(body.span_text(validation_routes[0].actor).trim(), "self.child_type");
    assert_eq!(body.span_text(validation_routes[0].state).trim(), "next_child");
    assert_eq!(body.span_text(become_routes[0].actor).trim(), "target");
    assert_eq!(body.span_text(become_routes[0].state).trim(), "next_state");
}

#[test]
fn route_target_does_not_cross_a_route_separator() {
    let err = EntryBody::new(
        r#"
            become {
                first <- A,
                second <- B(next),
            };
            "#,
    )
    .expect_err("a route target without state arguments must not consume the next route");

    assert!(err.to_string().contains("expected `(` after become target"), "unexpected error: {err}");
}

#[test]
fn unexpected_closing_brace_is_rejected() {
    let err = EntryBody::new("}").expect_err("an unmatched closing brace must be rejected");

    assert!(err.to_string().contains("unexpected `}`"), "unexpected error: {err}");
}

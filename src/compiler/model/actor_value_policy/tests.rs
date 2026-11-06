use crate::compile_inline;

fn source(globals: &str, helpers: &str, body: &str) -> String {
    format!(
        r#"
        state Game {{ int n; }}
        actor A owns Game {{ entry check() emits none {{ require(n >= 0); }} }}
        actor B owns Game {{ entry check() emits none {{ require(n > 0); }} }}
        actor enum Move {{ A; B; }}
        const Move SELECTED = Move::B;
        fn enum_value() -> Move {{ return Move::B; }}
        {globals}
        actor Mux owns Game {{
            {helpers}
            entry check(Move selected, actor_type<Game> external) emits none {{ {body} }}
        }}
        app Test {{ actor A; actor B; actor Mux; }}
    "#
    )
}

fn rejects(source: String, expected: &str) {
    let error = compile_inline("actor-value-policy.ag", source).expect_err("actor enum values are not external handles");
    assert!(error.to_string().contains(expected), "{error}");
    assert!(!error.to_string().contains("generated Silverscript"), "Argent must reject before Sil lowering: {error}");
}

#[test]
fn enum_initializers_require_explicit_enum_types_in_each_body_context() {
    for expression in ["Move::B", "Move[0]", "selected", "SELECTED", "enum_value()"] {
        let statement = format!("actor_type<Game> target = {expression}; require(target == external);");
        for context in ["entry", "global", "actor"] {
            let (globals, helpers, body) = match context {
                "global" => (format!("fn invalid(Move selected, actor_type<Game> external) {{ {statement} }}"), String::new(), ""),
                "actor" => (String::new(), format!("fn invalid(Move selected, actor_type<Game> external) {{ {statement} }}"), ""),
                _ => (String::new(), String::new(), statement.as_str()),
            };
            rejects(source(&globals, &helpers, body), "actor_type<State> is an external actor handle, not an actor-enum selector");
        }
    }
    rejects(source("", "", "actor_type<Game> target = Move::B;"), "declare `Move target = Move::B;` instead");
}

#[test]
fn constants_returns_assignments_and_call_arguments_cannot_launder_enums() {
    for expression in ["Move::B", "Move[0]", "SELECTED", "enum_value()"] {
        rejects(
            source(&format!("const actor_type<Game> INVALID = {expression};"), "", ""),
            "actor_type<State> is an external actor handle",
        );
    }
    for expression in ["Move::B", "Move[0]", "selected", "SELECTED", "enum_value()"] {
        for (globals, helpers, body) in [
            (format!("fn invalid(Move selected) -> actor_type<Game> {{ return {expression}; }}"), String::new(), String::new()),
            (String::new(), format!("fn invalid(Move selected) -> actor_type<Game> {{ return {expression}; }}"), String::new()),
            (String::new(), String::new(), format!("external = {expression};")),
            (
                "fn sink(actor_type<Game> handle) { require(handle == handle); }".to_string(),
                String::new(),
                format!("sink({expression});"),
            ),
            (
                String::new(),
                "fn sink(actor_type<Game> handle) { require(handle == handle); }".to_string(),
                format!("sink({expression});"),
            ),
        ] {
            rejects(source(&globals, &helpers, &body), "actor_type<State> is an external actor handle");
        }
    }
    rejects(
        source("fn invalid() -> actor_type<Game> { Move copied = Move::B; return copied; }", "", ""),
        "actor_type<State> is an external actor handle",
    );
    rejects(
        source("", "fn enum_member() -> Move { return Move::B; }", "actor_type<Game> target = enum_member();"),
        "actor_type<State> is an external actor handle",
    );
    rejects(source("", "", "(actor_type<Game> target) = enum_value();"), "actor_type<State> is an external actor handle");
}

#[test]
fn bare_actor_declarations_are_not_handle_values() {
    for (globals, helpers, body) in [
        ("", "", "actor_type<Game> target = B;"),
        ("fn invalid() { actor_type<Game> target = B; }", "", ""),
        ("", "fn invalid() { actor_type<Game> target = B; }", ""),
        ("const actor_type<Game> INVALID = B;", "", ""),
        ("fn invalid() -> actor_type<Game> { return B; }", "", ""),
    ] {
        rejects(source(globals, helpers, body), "in-app actor `B` can only be referenced as a value through an actor enum group");
    }
}

#[test]
fn explicit_enum_declarations_and_returns_still_compile() {
    compile_inline(
        "explicit-enum-values.ag",
        source(
            "fn copy(Move original) -> Move { Move copied = original; return copied; }",
            "fn local_copy(Move original) -> Move { Move copied = original; return copy(copied); }",
            "Move original = Move::B; Move copied = original; require(copied == selected); require(local_copy(selected) == selected); unrestricted(next.value); Game next_state = { n: n + 1 }; become next <- copied(next_state);",
        ).replace("entry check(Move selected, actor_type<Game> external) emits none", "entry check(Move selected, actor_type<Game> external) emits next: Move"),
    )
    .expect("explicit enum values remain in their declared enum domain");
}

#[test]
fn qualified_enum_diagnostics_use_a_source_namespace_path() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(
        temp.path().join("library.ag"),
        "state Game { int n; } actor A owns Game { entry check() emits none { require(n >= 0); } } actor B owns Game { entry check() emits none { require(n > 0); } } actor enum Move { A; B; } app RemoteApp { actor A; actor B; }",
    )
    .unwrap();
    for expression in ["lib::Move::B", "lib::Move[0]"] {
        std::fs::write(
            temp.path().join("root.ag"),
            format!(
                r#"
                import "./library.ag" as lib;
                actor Mux owns lib::Game {{ entry check() emits none {{ actor_type<lib::Game> target = {expression}; }} }}
                app Test {{ actor lib::A; actor lib::B; actor Mux; }}
            "#
            ),
        )
        .unwrap();
        let error = crate::build_file(temp.path().join("root.ag"), temp.path().join("out"))
            .expect_err("qualified enum choices are still not exported handles");
        assert!(error.to_string().contains(&format!("declare `lib::Move target = {expression};` instead")), "{error}");
        assert!(!error.to_string().contains("gen__"), "suggestions must remain authored source names: {error}");
    }
    std::fs::write(
        temp.path().join("root.ag"),
        r#"
        import "./library.ag" as lib;
        actor Mux owns lib::Game { entry check() emits none { actor_type<lib::Game> target = lib::RemoteApp::Move[0]; } }
        app Test { actor lib::A; actor lib::B; actor Mux; }
    "#,
    )
    .unwrap();
    let error = crate::build_file(temp.path().join("root.ag"), temp.path().join("out"))
        .expect_err("app-qualified names currently export actors, not enum groups");
    assert!(error.to_string().contains("unresolved qualified reference `lib::RemoteApp::Move`"), "{error}");
    std::fs::write(
        temp.path().join("root.ag"),
        r#"
        import "./library.ag" as lib;
        actor Mux owns lib::Game { entry check() emits none { actor_type<lib::Game> target = lib::B; } }
        app Test { actor lib::A; actor lib::B; actor Mux; }
    "#,
    )
    .unwrap();
    let error =
        crate::build_file(temp.path().join("root.ag"), temp.path().join("out")).expect_err("qualified bare actors are not handles");
    assert!(error.to_string().contains("in-app actor `B` can only be referenced as a value through an actor enum group"), "{error}");
}

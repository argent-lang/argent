use super::*;

fn byte_occurrences(haystack: &[u8], needle: &[u8]) -> usize {
    if needle.is_empty() { 0 } else { haystack.windows(needle.len()).filter(|window| *window == needle).count() }
}

const COUNTER_APP: &str = r#"
state CounterState {
    int count;
}

actor Counter owns CounterState {
    entry bump(int delta) emits next: Counter {
        unrestricted(next.value);
        CounterState next_state = {
            count: count + delta,
        };

        become next <- Counter(next_state);
    }
}

app CounterApp {
    actor Counter;
}
"#;

const INVOCATION_UID_APP: &str = r#"
import "std::core";

state IssuerState {
    byte[32] last_uid;
}

actor Issuer owns IssuerState {
    entry issue(byte[] domain) emits next: Issuer {
        unrestricted(next.value);
        byte[32] uid = invocation_uid(domain);
        require(uid == invocation_uid(domain));

        IssuerState next_state = {
            last_uid: uid,
        };
        become next <- Issuer(next_state);
    }
}

app IssuerApp {
    actor Issuer;
}
"#;

const TWO_APPS: &str = r#"
state LeftState {
    int amount;
}

actor Left owns LeftState {
    entry bump() emits next: Left {
        unrestricted(next.value);
        LeftState next_state = {
            amount: amount + 1,
        };
        become next <- Left(next_state);
    }
}

state RightState {
    int amount;
}

actor Right owns RightState {
    entry bump() emits next: Right {
        unrestricted(next.value);
        RightState next_state = {
            amount: amount + 1,
        };
        become next <- Right(next_state);
    }
}

actor RightAlt owns RightState {
    entry bump() emits next: RightAlt {
        unrestricted(next.value);
        RightState next_state = {
            amount: amount + 2,
        };
        become next <- RightAlt(next_state);
    }
}

actor enum RightKind {
    Right;
    RightAlt;
}

app LeftApp {
    actor Left;
}

app RightApp {
    actor Right;
    actor RightAlt;
}
"#;

#[test]
fn compile_inline_returns_artifact_without_a_user_output_dir() {
    let artifact = compile_inline("counter.ag", COUNTER_APP).expect("inline app compiles");
    assert_eq!(artifact.app, "CounterApp");
    assert!(artifact.sil_abi.contract("Counter").is_some());
}

#[test]
fn compiles_separated_range_bound_and_exponent_constant() {
    let source = include_str!("../tests/fixtures/emit/entry_range_outputs/app.ag")
        .replace("const int MAX_ACCOUNTS = 3;", "const int MAX_ACCOUNTS = 3e0;")
        .replace("Account[1..=MAX_ACCOUNTS]", "Account[0_1..=MAX_ACCOUNTS]");
    let artifact = compile_inline("entry-range-literals.ag", source).expect("range literals compile");
    assert_eq!(artifact.app, "EntryRangeOutputs");
}

#[test]
fn compiles_bound_arithmetic_range_constant() {
    let source = include_str!("../tests/fixtures/emit/entry_range_outputs/app.ag")
        .replace("const int MAX_ACCOUNTS = 3;", "const int MAX_ACCOUNTS = 1 + 2;");
    let artifact = compile_inline("entry-range-expression.ag", source).expect("bound constant expression compiles");
    assert_eq!(artifact.app, "EntryRangeOutputs");
}

#[test]
fn negative_range_literal_reaches_semantic_validation() {
    let source = include_str!("../tests/fixtures/emit/entry_range_outputs/app.ag")
        .replace("Account[1..=MAX_ACCOUNTS]", "Account[-1..=MAX_ACCOUNTS]");
    let error = compile_inline("negative-range.ag", source).expect_err("negative bounds are invalid");
    assert!(error.to_string().contains("must have non-negative bounds"), "unexpected error: {error}");
}

#[test]
fn aliased_helpers_keep_their_defining_module_bindings() {
    let temp = std::env::temp_dir().join(format!("argent-module-helper-bindings-{}", std::process::id()));
    std::fs::create_dir_all(&temp).expect("test directory created");
    let library = r#"
        const int LIMIT = 1;
        fn source_limit() -> int { return LIMIT; }
        fn limit() -> int { return source_limit(); }
    "#;
    for root_constant in ["", "const int LIMIT = 2;"] {
        let root = format!(
            r#"
            import "./library.ag" as lib;
            {root_constant}
            fn source_limit() -> int {{ return 2; }}
            state S {{}}
            actor A owns S {{
                entry check(int value) emits none {{ require(value == lib::limit()); }}
            }}
            app Test {{ actor A; }}
        "#
        );
        std::fs::write(temp.join("root.ag"), root).expect("root source written");
        std::fs::write(temp.join("library.ag"), library).expect("library source written");
        let actual = build_file(temp.join("root.ag"), temp.join("actual")).expect("module-local dependency closure compiles");

        std::fs::write(temp.join("library.ag"), library.replace("return LIMIT;", "return 1;")).expect("reference source written");
        let expected = build_file(temp.join("root.ag"), temp.join("expected")).expect("literal reference compiles");
        assert_eq!(
            actual.sil_abi.contract("A").unwrap().compiled.bytecode,
            expected.sil_abi.contract("A").unwrap().compiled.bytecode,
            "lib::limit() must return its own module's 1, with or without a root LIMIT"
        );
    }
    std::fs::remove_dir_all(temp).expect("test directory removed");
}

#[test]
fn qualified_library_names_do_not_capture_actor_or_function_locals() {
    let temp = std::env::temp_dir().join(format!("argent-qualified-name-capture-{}", std::process::id()));
    std::fs::create_dir_all(&temp).unwrap();
    std::fs::write(
        temp.join("library.ag"),
        r#"
        const int LIMIT = 1;
        fn limit() -> int { return LIMIT; }
        fn identity(int ROOT_LIMIT) -> int { return ROOT_LIMIT; }
    "#,
    )
    .unwrap();
    std::fs::write(
        temp.join("root.ag"),
        r#"
        import "./library.ag" as lib;
        const int ROOT_LIMIT = 3;
        state S {}
        actor A owns S {
            fn limit() -> int { return 2; }
            entry check(int LIMIT) emits none {
                require(LIMIT == lib::LIMIT);
                require(lib::limit() == 1);
                require(limit() == 2);
                require(lib::identity(ROOT_LIMIT) == 3);
            }
        }
        app Test { actor A; }
    "#,
    )
    .unwrap();
    let actual = build_file(temp.join("root.ag"), temp.join("actual")).expect("qualified library references remain hygienic");
    let expected = compile_inline(
        "literal-reference.ag",
        r#"
        fn library_limit() -> int { return 1; }
        fn identity(int x) -> int { return x; }
        state S {}
        actor A owns S {
            fn limit() -> int { return 2; }
            entry check(int LIMIT) emits none {
                require(LIMIT == 1);
                require(library_limit() == 1);
                require(limit() == 2);
                require(identity(3) == 3);
            }
        }
        app Test { actor A; }
    "#,
    )
    .expect("literal reference compiles");
    assert_eq!(actual.sil_abi.contract("A").unwrap().compiled.bytecode, expected.sil_abi.contract("A").unwrap().compiled.bytecode);
    std::fs::remove_dir_all(temp).unwrap();
}

#[test]
fn importing_module_cannot_supply_an_unresolved_library_identifier() {
    let temp = std::env::temp_dir().join(format!("argent-unresolved-library-name-{}", std::process::id()));
    std::fs::create_dir_all(&temp).unwrap();
    std::fs::write(temp.join("library.ag"), "fn limit() -> int { return LIMIT; }").unwrap();
    std::fs::write(
        temp.join("root.ag"),
        r#"
        import "./library.ag" as lib;
        const int LIMIT = 2;
        state S {}
        actor A owns S { entry check(int value) emits none { require(value == lib::limit()); } }
        app Test { actor A; }
    "#,
    )
    .unwrap();
    let error = build_file(temp.join("root.ag"), temp.join("out")).expect_err("root constants are not in the library's scope");
    assert!(error.to_string().contains("unknown reference `LIMIT`"), "{error}");
    std::fs::remove_dir_all(temp).unwrap();
}

#[test]
fn unused_import_does_not_change_linked_app_artifacts() {
    let temp = std::env::temp_dir().join(format!("argent-stable-app-exports-{}", std::process::id()));
    std::fs::create_dir_all(&temp).expect("test directory created");
    // Both libraries define Asset and AssetState, but only actor.ag is used.
    std::fs::write(temp.join("actor.ag"), "state AssetState { int n; } actor Asset owns AssetState { entry check() emits none {} }")
        .unwrap();
    std::fs::write(
        temp.join("unused.ag"),
        "state AssetState { bool n; } actor Asset owns AssetState { entry unused() emits none {} }",
    )
    .unwrap();
    std::fs::write(
        temp.join("consumer.ag"),
        r#"
        import "./asset.ag" as asset;
        state ConsumerState {}
        actor Consumer owns ConsumerState {
            entry check(cov_id id)
            observes asset by id { inputs { source: asset::AssetApp::Asset, } }
            emits none { require(1 == 1); }
        }
        app ConsumerApp { actor Consumer; }
    "#,
    )
    .unwrap();
    let source = r#"import "./actor.ag" as actor; app AssetApp { actor actor::Asset; }"#;
    std::fs::write(temp.join("asset.ag"), source).unwrap();
    let baseline = build_file_app_bundle(temp.join("consumer.ag"), "ConsumerApp", temp.join("baseline")).expect("baseline links");
    let baseline_asset = baseline.app("AssetApp").unwrap();
    assert_eq!(baseline_asset.argent.actors.iter().map(|actor| actor.name.as_str()).collect::<Vec<_>>(), ["Asset"]);
    assert!(baseline_asset.sil_abi.contract("Asset").is_some());

    // Adding the unused import in either position must preserve both apps' artifacts.
    let unused_import = r#"import "./unused.ag" as unused;"#;
    for changed_source in [format!("{unused_import} {source}"), format!("{source} {unused_import}")] {
        std::fs::write(temp.join("asset.ag"), changed_source).unwrap();
        let changed = build_file_app_bundle(temp.join("consumer.ag"), "ConsumerApp", temp.join("changed"))
            .expect("AssetApp::Asset still links after an unused name collision");
        // Artifact IDs cover exported names, interfaces, and compiled code.
        assert_eq!(changed.app("AssetApp").unwrap().id, baseline_asset.id, "asset artifact changed");
        assert_eq!(changed.primary().id, baseline.primary().id, "consumer artifact changed");
    }
    std::fs::remove_dir_all(temp).expect("test directory removed");
}

#[test]
fn static_cross_app_actor_values_use_shared_expression_lowering() {
    let temp = tempfile::tempdir().expect("test directory created");
    std::fs::write(
        temp.path().join("foreign.ag"),
        r#"
        state ForeignState { int n; }
        actor Foreign owns ForeignState {
            entry check() emits none { require(n >= 0); }
        }
        app RemoteApp { actor Foreign; }
    "#,
    )
    .unwrap();
    let cases = [
        ("entry", "", "", "remote::RemoteApp::Foreign"),
        (
            "global-helper",
            "fn expected() -> actor_type<remote::ForeignState> { return remote::RemoteApp::Foreign; }",
            "",
            "expected()",
        ),
        ("actor-helper", "", "fn expected() -> actor_type<remote::ForeignState> { return remote::RemoteApp::Foreign; }", "expected()"),
        (
            "helper-local",
            "fn expected() -> actor_type<remote::ForeignState> { actor_type<remote::ForeignState> result = remote::RemoteApp::Foreign; return result; }",
            "",
            "expected()",
        ),
        ("constant", "const actor_type<remote::ForeignState> EXPECTED = remote::RemoteApp::Foreign;", "", "EXPECTED"),
    ];
    for (name, globals, helpers, expected) in cases {
        std::fs::write(
            temp.path().join("host.ag"),
            format!(
                r#"
                import "./foreign.ag" as remote;
                {globals}
                state HostState {{ actor_type<remote::ForeignState> token_type; }}
                actor Host owns HostState {{
                    {helpers}
                    entry check() emits none {{ require(token_type == {expected}); }}
                }}
                state OtherState {{ int n; }}
                actor Other owns OtherState {{ entry check() emits none {{}} }}
                app HostApp {{ actor Host; actor Other; }}
            "#
            ),
        )
        .unwrap();
        let output = temp.path().join(name);
        let bundle = build_file_bundle(temp.path().join("host.ag"), &output)
            .unwrap_or_else(|error| panic!("static actor value in {name} compiles: {error}"));
        let artifact = bundle.primary();
        assert_eq!(bundle.apps().map(|(app, _)| app).collect::<Vec<_>>(), ["HostApp", "RemoteApp"]);
        assert_eq!(artifact.argent.interfaces.imports.len(), 1);
        assert_eq!(artifact.argent.interfaces.imports[0].app, "RemoteApp");
        assert_eq!(artifact.argent.interfaces.imports[0].actor, "Foreign");
        let exported = &bundle.app("RemoteApp").unwrap().argent.template_plan.templates[0].actor_type_handle.template.hash;
        let contract = artifact.sil_abi.contract("Host").unwrap();
        assert!(byte_occurrences(&contract.compiled.bytecode, exported) > 0);
        assert!(contract.entry("check").unwrap().params.is_empty(), "value-only imports add no witness parameters");
        assert_eq!(contract.runtime_state.fields.len(), 1, "value-only imports add no route fields");
        assert_eq!(artifact.argent.actors.len(), 2, "linked actors do not enter the app actor domain");

        let host_sil = std::fs::read_to_string(output.join("sil/Host.sil")).unwrap();
        let other_sil = std::fs::read_to_string(output.join("sil/Other.sil")).unwrap();
        let constant = "gen__remote_app__foreign_template_const";
        assert!(host_sil.contains(constant), "{name}: {host_sil}");
        assert_eq!(
            other_sil.contains(constant),
            matches!(name, "global-helper" | "helper-local" | "constant"),
            "only globally emitted uses require constants in unrelated actors: {name}"
        );
    }
    for statement in ["remote::RemoteApp::Foreign();", "remote::RemoteApp::Foreign invalid;"] {
        std::fs::write(
            temp.path().join("misuse.ag"),
            format!(
                r#"
                import "./foreign.ag" as remote;
                state HostState {{}}
                actor Host owns HostState {{ entry check() emits none {{ {statement} }} }}
                app HostApp {{ actor Host; }}
            "#
            ),
        )
        .unwrap();
        build_file(temp.path().join("misuse.ag"), temp.path().join("invalid"))
            .expect_err("an actor value is not a function or a state type");
    }
}

#[test]
fn static_cross_app_actor_values_distinguish_same_named_exports() {
    let temp = tempfile::tempdir().unwrap();
    for (file, app, condition) in [("first.ag", "FirstApp", "n >= 0"), ("second.ag", "SecondApp", "n > 0")] {
        std::fs::write(
            temp.path().join(file),
            format!(
                "state ForeignState {{ int n; }} actor Foreign owns ForeignState {{ entry check() emits none {{ require({condition}); }} }} app {app} {{ actor Foreign; }}"
            ),
        )
        .unwrap();
    }
    std::fs::write(
        temp.path().join("host.ag"),
        r#"
        import "./first.ag" as first;
        import "./second.ag" as second;
        state HostState {}
        actor Host owns HostState {
            entry check() emits none {
                require(first::FirstApp::Foreign != second::SecondApp::Foreign);
            }
        }
        app HostApp { actor Host; }
    "#,
    )
    .unwrap();
    let bundle = build_file_bundle(temp.path().join("host.ag"), temp.path().join("out")).expect("distinct exports compile");
    let artifact = bundle.primary();
    assert_eq!(artifact.argent.interfaces.imports.len(), 2);
    assert_eq!(artifact.dependencies.len(), 2);
    let first = &bundle.app("FirstApp").unwrap().argent.template_plan.templates[0].actor_type_handle.template.hash;
    let second = &bundle.app("SecondApp").unwrap().argent.template_plan.templates[0].actor_type_handle.template.hash;
    assert_ne!(first, second);
    let bytecode = &artifact.sil_abi.contract("Host").unwrap().compiled.bytecode;
    assert!(byte_occurrences(bytecode, first) > 0);
    assert!(byte_occurrences(bytecode, second) > 0);
}

#[test]
fn compile_inline_supports_helpers_without_return_types() {
    let source = r#"
        fn authorize(int value) {
            require(value > 0);
        }

        state CounterState {
            int count;
        }

        actor Counter owns CounterState {
            entry bump(int delta) emits next: Counter {
                authorize(delta);
                unrestricted(next.value);
                CounterState next_state = {
                    count: count + delta,
                };
                become next <- Counter(next_state);
            }
        }

        app CounterApp {
            actor Counter;
        }
    "#;

    let artifact = compile_inline("void-helper.ag", source).expect("void helper compiles through Silverscript");
    assert!(artifact.sil_abi.contract("Counter").is_some());
}

#[test]
fn build_inline_writes_outputs_and_returns_artifact() {
    let out_dir = std::env::temp_dir().join(format!("argent-build-inline-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out_dir);

    let artifact = build_inline("counter.ag", COUNTER_APP, &out_dir).expect("inline app builds");

    assert_eq!(artifact.app, "CounterApp");
    assert!(out_dir.join("artifact.json").exists());
    assert!(out_dir.join("sil").join("Counter.sil").exists());

    let _ = std::fs::remove_dir_all(out_dir);
}

#[test]
fn build_inline_loads_explicit_standard_module() {
    let out_dir = std::env::temp_dir().join(format!("argent-build-inline-std-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out_dir);

    let artifact = build_inline("issuer.ag", INVOCATION_UID_APP, &out_dir).expect("inline app imports std::core");
    let sil = std::fs::read_to_string(out_dir.join("sil/Issuer.sil")).expect("generated Issuer Sil exists");

    assert!(artifact.modules.iter().any(|module| module == "std::core"));
    assert!(sil.contains("function invocation_uid(byte[] gen__glob_domain): byte[32]"), "{sil}");
    assert!(sil.contains("return(blake2bWithKey(byte[](gen__glob_outpoint), gen__glob_domain));"), "{sil}");
    assert!(sil.contains("byte[32] uid = invocation_uid(domain);"), "{sil}");

    let _ = std::fs::remove_dir_all(out_dir);
}

#[test]
fn global_helpers_support_declared_invocation_uid_and_runtime_introspection() {
    let source = r#"
        import "std::core";
        fn shared(byte[] domain) -> byte[32] { return invocation_uid(domain); }
        fn input_count() -> int {
            return this.activeInputIndex + tx.version + tx.inputs.length + tx.outputs.length
                + tx.inputs[0].value + tx.outputs[0].value;
        }
        state S { int count; }
        actor A owns S {
            entry check(byte[] domain) emits none {
                require(shared(domain).length == 32);
                require(input_count() >= count);
            }
        }
        app Test { actor A; }
    "#;
    compile_inline("global-runtime.ag", source).expect("declared library calls and qualified runtime introspection compile");
}

#[test]
fn helper_source_collisions_are_rejected() {
    for (global, params, body, expected) in [
        (true, "int value", "int value = 7; return value;", "already defined"),
        (false, "int value", "int value = 7; return value;", "already defined"),
        (true, "int value, int value", "return value;", "duplicate parameter name"),
        (false, "int count", "return count;", "variable 'count' is already defined"),
        (false, "", "int count = 7; return count;", "variable 'count' is already defined"),
        (true, "int value", "{ int value = 7; } return value;", "already defined"),
        (false, "int value", "for (value, 0, 1, 1) {} return value;", "already defined"),
        (false, "int value", "int value, int other = 0x0001.split(1); return other;", "already defined"),
        (false, "int INDEX", "int INDEX = 7; return INDEX;", "already defined"),
    ] {
        let helper = format!("fn echo({params}) -> int {{ {body} }}");
        let args = params.split(',').filter(|param| !param.is_empty()).map(|_| "count").collect::<Vec<_>>().join(", ");
        let source = format!(
            "const int INDEX = 3; state S {{ int count; }} {} actor A owns S {{ {} \
             entry check() emits none {{ require(echo({args}) >= 0); }} }} app Test {{ actor A; }}",
            if global { &helper } else { "" },
            if global { "" } else { &helper },
        );
        let error = compile_inline("helper-collision.ag", source).expect_err("source collisions must reach Silverscript validation");
        assert!(error.to_string().contains(expected), "{helper}: {error}");
    }

    let source = r#"
        state count { int n; }
        state S { int count; }
        actor A owns S {
            fn echo(int count) -> int { return count; }
            entry check(count other) emits none { require(echo(other.n) >= 0); }
        }
        app Test { actor A; }
    "#;
    let error = compile_inline("helper-field-collision.ag", source).expect_err("type-name hygiene cannot hide actor field shadowing");
    assert!(error.to_string().contains("variable 'count' is already defined"), "{error}");
}

#[test]
fn helpers_lower_co_spent_from_bound_source_ast() {
    let out_dir = std::env::temp_dir().join(format!("argent-helper-co-spent-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out_dir);
    let source = r#"
        state GuardState { cov_id guard; }
        fn globally_present(cov_id id) -> bool {
            cov_id local = id;
            return id.co_spent() && local.co_spent();
        }
        actor Guard owns GuardState {
            fn locally_present() -> bool { return guard.co_spent(); }
            entry check() emits none {
                require(globally_present(guard) && locally_present());
            }
        }
        app GuardApp { actor Guard; }
    "#;
    build_inline("guard.ag", source, &out_dir).expect("co-spend helpers compile");
    let sil = std::fs::read_to_string(out_dir.join("sil/Guard.sil")).expect("generated Guard Sil exists");
    assert!(sil.contains("OpCovInputCount(gen__glob_id) > 0"), "{sil}");
    assert!(sil.contains("OpCovInputCount(gen__glob_local) > 0"), "{sil}");
    assert!(sil.contains("OpCovInputCount(guard) > 0"), "{sil}");
    let _ = std::fs::remove_dir_all(out_dir);
}

#[test]
fn helper_co_spent_rejects_non_covenant_id_receivers() {
    let source = r#"
        state GuardState { byte[32] guard; }
        fn globally_present(byte[32] id) -> bool { return id.co_spent(); }
        actor Guard owns GuardState {
            entry check() emits none { require(globally_present(guard)); }
        }
        app GuardApp { actor Guard; }
    "#;
    let err = compile_inline("guard.ag", source).expect_err("ordinary bytes are not covenant IDs");
    assert!(err.to_string().contains("`.co_spent()` requires one `cov_id` receiver"), "{err}");
}

#[test]
fn helper_co_spent_accepts_an_explicit_covenant_id_cast() {
    let source = r#"
        state GuardState { byte[32] guard; }
        fn globally_present(byte[32] id) -> bool { return cov_id(id).co_spent(); }
        actor Guard owns GuardState {
            entry check() emits none { require(globally_present(guard)); }
        }
        app GuardApp { actor Guard; }
    "#;
    compile_inline("guard.ag", source).expect("explicit covenant-ID cast compiles in a helper");
}

#[test]
fn helper_co_spent_accepts_field_and_call_receivers_by_type() {
    let source = r#"
        state GuardState { cov_id guard; }
        fn same(cov_id id) -> cov_id { return id; }
        actor Guard owns GuardState {
            fn present() -> bool { return self.guard.co_spent() && same(guard).co_spent(); }
            entry check() emits none { require(present()); }
        }
        app GuardApp { actor Guard; }
    "#;
    compile_inline("guard.ag", source).expect("covenant-ID fields and helper results are valid receivers");
}

#[test]
fn helper_local_names_preserve_disjoint_scopes() {
    let out_dir = std::env::temp_dir().join(format!("argent-helper-local-ids-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out_dir);
    let source = r#"
        state GuardState { cov_id guard; }
        actor Guard owns GuardState {
            fn present(bool flag, cov_id id) -> bool {
                if (flag) {
                    cov_id copy = id;
                    require(copy.co_spent());
                }
                cov_id copy = id;
                return copy.co_spent() && self.guard.co_spent();
            }
            entry check() emits none { require(present(true, guard)); }
        }
        app GuardApp { actor Guard; }
    "#;
    build_inline("guard.ag", source, &out_dir).expect("distinct bound locals compile");
    let sil = std::fs::read_to_string(out_dir.join("sil/Guard.sil")).expect("generated contract exists");
    assert_eq!(sil.matches("byte[32] copy = id;").count(), 2, "{sil}");
    assert!(sil.contains("OpCovInputCount(guard) > 0"), "{sil}");
    let _ = std::fs::remove_dir_all(out_dir);
}

#[test]
fn co_spent_method_binding_is_independent_of_a_local_name() {
    let source = r#"
        state GuardState { cov_id guard; }
        fn globally_present(cov_id id) -> bool {
            int co_spent = 1;
            return id.co_spent() && co_spent > 0;
        }
        actor Guard owns GuardState {
            entry check() emits none { require(globally_present(guard)); }
        }
        app GuardApp { actor Guard; }
    "#;
    compile_inline("guard.ag", source).expect("method namespace remains separate from local names");
}

#[test]
fn retained_helper_ast_lowers_state_types() {
    let out_dir = std::env::temp_dir().join(format!("argent-helper-state-ast-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out_dir);
    let artifact = build_file(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/state_layout/function_contexts/app.ag"), &out_dir)
        .expect("state-valued helpers compile from retained ASTs");
    let aligned = std::fs::read_to_string(out_dir.join("sil/Aligned.sil")).expect("aligned contract exists");
    assert!(aligned.contains("function global_identity(State gen__glob_value): State"), "{aligned}");
    assert!(aligned.contains("State {left: int gen__glob_copied_left"), "{aligned}");
    assert!(!aligned.contains("SharedState"), "{aligned}");
    let aligned_template =
        artifact.argent.template_plan.templates.iter().find(|template| template.actor == "Aligned").expect("Aligned template exists");
    assert_eq!(
        crate::codec::encode_hex(&aligned_template.sil_template_hash),
        "93509ef29827d95b79f405cf30f2c651fadbd01c06f0a7c73088678a10dfb4ef",
    );
    let _ = std::fs::remove_dir_all(out_dir);
}

#[test]
fn retained_helper_ast_lowers_bound_actor_enum_variants() {
    let source = r#"
        state SharedState { int counter; }
        actor First owns SharedState {
            entry check() emits none { require(second_variant() == 1); }
        }
        actor Second owns SharedState {
            entry check() emits none { require(counter >= 0); }
        }
        actor enum Choice { First; Second; }
        fn second_variant() -> int { return Choice::Second; }
        app Test { actor First; actor Second; }
    "#;
    compile_inline("enum-helper.ag", source).expect("bound enum variant lowers to its ordinal");
}

#[test]
fn helper_co_spent_rejects_method_arguments() {
    let source = r#"
        state GuardState { cov_id guard; }
        actor Guard owns GuardState {
            fn locally_present() -> bool { return guard.co_spent(guard); }
            entry check() emits none { require(locally_present()); }
        }
        app GuardApp { actor Guard; }
    "#;
    let err = compile_inline("guard.ag", source).expect_err("co-spent does not take method arguments");
    assert!(err.to_string().contains("unsupported method call `co_spent`"), "{err}");
}

#[test]
fn source_helper_ast_keeps_global_constant_bindings_isolated() {
    let source = r#"
        const int LIMIT = 4;
        state CounterState { int count; }
        fn invalid() -> int { int LIMIT = 2; return LIMIT; }
        actor Counter owns CounterState {
            entry check() emits none { require(invalid() > 0); }
        }
        app CounterApp { actor Counter; }
    "#;
    let err = compile_inline("counter.ag", source).expect_err("helper local shadows a shared constant");
    assert!(err.to_string().contains("binding `LIMIT` shadows a shared constant"), "{err}");
}

#[test]
fn retained_global_helper_ast_renames_bound_variables() {
    let out_dir = std::env::temp_dir().join(format!("argent-global-helper-bindings-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out_dir);
    let source = r#"
        const int LIMIT = 4;
        state CounterState { int count; }
        fn summarize(int value) -> int {
            int result = value + LIMIT;
            for (index, 0, 2, LIMIT) {
                result = result + index;
            }
            return result;
        }
        actor Counter owns CounterState {
            entry check() emits none { require(summarize(count) >= 0); }
        }
        app CounterApp { actor Counter; }
    "#;
    build_inline("counter.ag", source, &out_dir).expect("bound global helper compiles from its AST");
    let sil = std::fs::read_to_string(out_dir.join("sil/Counter.sil")).expect("generated contract exists");
    assert!(sil.contains("function summarize(int gen__glob_value): int"), "{sil}");
    assert!(sil.contains("int gen__glob_result = gen__glob_value + LIMIT;"), "{sil}");
    assert!(sil.contains("for (gen__glob_index, 0, 2, LIMIT)"), "{sil}");
    assert!(sil.contains("gen__glob_result = gen__glob_result + gen__glob_index;"), "{sil}");
    let _ = std::fs::remove_dir_all(out_dir);
}

#[test]
fn retained_global_helper_ast_rejects_parameter_shadowing_a_shared_constant() {
    let source = r#"
        const int LIMIT = 4;
        state CounterState { int count; }
        fn invalid(int LIMIT) -> int { return LIMIT; }
        actor Counter owns CounterState {
            entry check() emits none { require(invalid(count) > 0); }
        }
        app CounterApp { actor Counter; }
    "#;
    let err = compile_inline("counter.ag", source).expect_err("helper parameter shadows a shared constant");
    assert!(err.to_string().contains("binding `LIMIT` shadows a shared constant"), "{err}");
}

#[test]
fn retained_global_helper_ast_rejects_loop_binding_shadowing_a_shared_constant() {
    let source = r#"
        const int LIMIT = 4;
        state CounterState { int count; }
        fn invalid() -> int {
            int result = 0;
            for (LIMIT, 0, 2, 2) { result = result + LIMIT; }
            return result;
        }
        actor Counter owns CounterState {
            entry check() emits none { require(invalid() >= 0); }
        }
        app CounterApp { actor Counter; }
    "#;
    let err = compile_inline("counter.ag", source).expect_err("helper loop binding shadows a shared constant");
    assert!(err.to_string().contains("binding `LIMIT` shadows a shared constant"), "{err}");
}

#[test]
fn entry_co_spent_lowers_in_initializers_and_conditions() {
    let source = r#"
        state GuardState { cov_id guard; }
        actor Guard owns GuardState {
            entry check() emits none {
                bool present = guard.co_spent();
                require(present && guard.co_spent());
                if (guard.co_spent()) { require(true); }
            }
        }
        app GuardApp { actor Guard; }
    "#;
    compile_inline("guard.ag", source).expect("entry co-spend compiles from the authored call site");
}

#[test]
fn entry_co_spent_accepts_field_and_call_receivers_by_type() {
    let source = r#"
        state GuardState { cov_id guard; }
        fn same(cov_id id) -> cov_id { return id; }
        actor Guard owns GuardState {
            entry check() emits none {
                require(self.guard.co_spent() && same(guard).co_spent());
            }
        }
        app GuardApp { actor Guard; }
    "#;
    compile_inline("guard.ag", source).expect("entry receiver types use bound AST lowering");
}

#[test]
fn co_spent_rejects_a_call_returning_ordinary_bytes() {
    let source = r#"
        state GuardState { byte[32] guard; }
        fn same(byte[32] id) -> byte[32] { return id; }
        actor Guard owns GuardState {
            entry check() emits none { require(same(guard).co_spent()); }
        }
        app GuardApp { actor Guard; }
    "#;
    let err = compile_inline("guard.ag", source).expect_err("ordinary byte results are not covenant IDs");
    assert!(err.to_string().contains("requires one `cov_id` receiver"), "{err}");
}

#[test]
fn build_inline_does_not_load_standard_module_without_import() {
    let out_dir = std::env::temp_dir().join(format!("argent-build-inline-no-std-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out_dir);
    let source = INVOCATION_UID_APP.replace("import \"std::core\";", "");

    let error = build_inline("issuer.ag", source, &out_dir).expect_err("standard function requires an explicit import");
    assert!(error.to_string().contains("unknown reference `invocation_uid`"), "unexpected error: {error}");

    let _ = std::fs::remove_dir_all(out_dir);
}

#[test]
fn build_file_writes_outputs_and_returns_artifact() {
    let temp = std::env::temp_dir().join(format!("argent-build-file-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(&temp).expect("temp dir created");

    let input = temp.join("counter.ag");
    let out_dir = temp.join("build");
    std::fs::write(&input, COUNTER_APP).expect("source written");

    let artifact = build_file(&input, &out_dir).expect("file app builds");

    assert_eq!(artifact.app, "CounterApp");
    assert!(out_dir.join("artifact.json").exists());
    assert!(out_dir.join("sil").join("Counter.sil").exists());

    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn build_file_loads_explicit_standard_module() {
    let temp = std::env::temp_dir().join(format!("argent-build-file-std-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(&temp).expect("temp dir created");

    let input = temp.join("issuer.ag");
    let out_dir = temp.join("build");
    std::fs::write(&input, INVOCATION_UID_APP).expect("source written");

    let artifact = build_file(&input, &out_dir).expect("file app imports std::core");
    assert!(artifact.modules.iter().any(|module| module == "std::core"));
    assert!(out_dir.join("sil/Issuer.sil").exists());

    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn build_file_app_selects_one_root_app() {
    let temp = std::env::temp_dir().join(format!("argent-build-file-app-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(&temp).expect("temp dir created");

    let input = temp.join("pair.ag");
    std::fs::write(&input, TWO_APPS).expect("source written");

    let left = build_file_app(&input, "LeftApp", temp.join("left")).expect("left app builds");
    assert_eq!(left.app, "LeftApp");
    assert!(left.sil_abi.contract("Left").is_some());
    assert!(left.sil_abi.contract("Right").is_none());

    let right = build_file_app(&input, "RightApp", temp.join("right")).expect("right app builds");
    assert_eq!(right.app, "RightApp");
    assert!(right.sil_abi.contract("Right").is_some());
    assert!(right.sil_abi.contract("RightAlt").is_some());
    assert!(right.sil_abi.contract("Left").is_none());
    assert!(right.argent.actor_enums.iter().any(|actor_enum| actor_enum.name == "RightKind"));

    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn app_qualified_actor_imports_keep_each_selected_app_compilation() {
    let temp = std::env::temp_dir().join(format!("argent-app-qualified-actor-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(&temp).expect("temp dir created");

    std::fs::write(
        temp.join("shared.ag"),
        r#"
state SharedState {
    int count;
}

actor Shared owns SharedState {
    entry merge()
    consumes {
        other: Shared,
    }
    emits next: Shared {
        unrestricted(next.value);
        SharedState next_state = {
            count: count + other.count,
        };
        become next <- Shared(next_state);
    }
}

state GuardState {
    int marker;
}

actor Guard owns GuardState {
    entry hold() emits next: Guard {
        unrestricted(next.value);
        become next <- self;
    }
}

app SoloApp {
    actor Shared;
}

app CohortApp {
    actor Shared;
    actor Guard;
}
"#,
    )
    .expect("shared actor source written");

    std::fs::write(
        temp.join("controller.ag"),
        r#"
import "./shared.ag";

state CtrlState {
    int marker;
}

actor Ctrl owns CtrlState {
    entry inspect(cov_id solo_id, cov_id cohort_id)
    observes solo by solo_id {
        inputs {
            src: SoloApp::Shared,
        }
    }
    observes cohort by cohort_id {
        inputs {
            src: CohortApp::Shared,
        }
    }
    emits none {
        SharedState solo_state = state(solo.inputs.src);
        SharedState cohort_state = state(cohort.inputs.src);
        require(solo_state.count >= 0);
        require(cohort_state.count >= 0);
    }
}

app CtrlApp {
    actor Ctrl;
}
"#,
    )
    .expect("controller source written");

    let compiled = build_file_app_bundle(temp.join("controller.ag"), "CtrlApp", temp.join("build"))
        .expect("both app-qualified identities compile in one bundle");
    let solo_actor = compiled
        .app("SoloApp")
        .expect("solo dependency exists")
        .argent
        .template_plan
        .templates
        .iter()
        .find(|template| template.actor == "Shared")
        .expect("solo Shared template exists");
    let cohort_actor = compiled
        .app("CohortApp")
        .expect("cohort dependency exists")
        .argent
        .template_plan
        .templates
        .iter()
        .find(|template| template.actor == "Shared")
        .expect("cohort Shared template exists");
    assert_ne!(
        solo_actor.sil_template_hash, cohort_actor.sil_template_hash,
        "one source actor must compile in each selected app context"
    );
    assert_ne!(
        solo_actor.actor_type_handle.template.hash, cohort_actor.actor_type_handle.template.hash,
        "each app-qualified actor must export its own handle"
    );

    let ctrl_script = &compiled.primary().sil_abi.contract("Ctrl").expect("controller contract exists").compiled.bytecode;
    assert_eq!(byte_occurrences(ctrl_script, &solo_actor.actor_type_handle.template.hash), 1);
    assert_eq!(byte_occurrences(ctrl_script, &cohort_actor.actor_type_handle.template.hash), 1);

    let solo_sil = std::fs::read_to_string(temp.join("build/apps/SoloApp/sil/Shared.sil")).expect("solo Shared Sil exists");
    let cohort_sil = std::fs::read_to_string(temp.join("build/apps/CohortApp/sil/Shared.sil")).expect("cohort Shared Sil exists");
    assert!(solo_sil.contains("State gen__other_state = readInputState("), "{solo_sil}");
    assert!(cohort_sil.contains("State gen__other_state = readInputStateWithTemplate("), "{cohort_sil}");

    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn build_file_app_removes_stale_selected_app_contracts() {
    let temp = std::env::temp_dir().join(format!("argent-build-file-app-clean-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(&temp).expect("temp dir created");

    let input = temp.join("pair.ag");
    let out_dir = temp.join("build");
    std::fs::write(&input, TWO_APPS).expect("source written");

    build_file_app(&input, "LeftApp", &out_dir).expect("left app builds");
    assert!(out_dir.join("sil/Left.sil").exists());

    build_file_app(&input, "RightApp", &out_dir).expect("right app builds");
    assert!(!out_dir.join("sil/Left.sil").exists());
    assert!(out_dir.join("sil/Right.sil").exists());
    assert!(out_dir.join("sil/RightAlt.sil").exists());

    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn app_bundle_compiles_transitive_imports_from_dependency_artifacts() {
    let temp = std::env::temp_dir().join(format!("argent-build-transitive-apps-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(&temp).expect("temp dir created");

    std::fs::write(
        temp.join("leaf.ag"),
        r#"
state LeafState {
    int n;
}

actor Leaf owns LeafState {
    entry update() emits next: Leaf {
        unrestricted(next.value);
        LeafState next_state = {
            n: n + 1,
        };
        become next <- Leaf(next_state);
    }
}

app LeafApp {
    actor Leaf;
}
"#,
    )
    .expect("leaf source written");
    std::fs::write(
        temp.join("middle.ag"),
        r#"
import "./leaf.ag";

state MiddleState {
    int n;
}

actor Middle owns MiddleState {
    entry update(cov_id leaf_id)
    observes leaf by leaf_id {
        inputs {
            src: LeafApp::Leaf,
        }
        outputs {
            leaf_output: LeafApp::Leaf,
        }
    }
    emits next: Middle {
        unrestricted(next.value);
        LeafState next_leaf = state(leaf.inputs.src);
        require leaf.outputs become {
            leaf_output <- LeafApp::Leaf(next_leaf),
        };
        MiddleState next_state = {
            n: n + 1,
        };
        become next <- Middle(next_state);
    }
}

app MiddleApp {
    actor Middle;
}
"#,
    )
    .expect("middle source written");
    std::fs::write(
        temp.join("root.ag"),
        r#"
import "./middle.ag";

state RootState {
    int n;
}

actor Root owns RootState {
    entry update(cov_id middle_id)
    observes middle by middle_id {
        inputs {
            src: MiddleApp::Middle,
        }
        outputs {
            middle_output: MiddleApp::Middle,
        }
    }
    emits next: Root {
        unrestricted(next.value);
        MiddleState next_middle = state(middle.inputs.src);
        require middle.outputs become {
            middle_output <- MiddleApp::Middle(next_middle),
        };
        RootState next_state = {
            n: n + 1,
        };
        become next <- Root(next_state);
    }
}

app RootApp {
    actor Root;
}
"#,
    )
    .expect("root source written");

    let out_dir = temp.join("build");
    let compiled = build_file_app_bundle(temp.join("root.ag"), "RootApp", &out_dir).expect("transitive app dependency graph compiles");

    assert_eq!(compiled.apps().map(|(app, _)| app).collect::<Vec<_>>(), ["LeafApp", "MiddleApp", "RootApp"]);
    assert!(out_dir.join("apps/LeafApp/artifact.json").is_file());
    assert!(out_dir.join("apps/MiddleApp/artifact.json").is_file());
    let leaf = compiled.app("LeafApp").expect("bundle contains LeafApp");
    let middle = compiled.app("MiddleApp").expect("bundle contains MiddleApp");
    assert!(leaf.dependencies.is_empty());
    assert_eq!(middle.dependencies, [artifact::AppDependencyArtifact { app: "LeafApp".to_string(), artifact_id: leaf.id.clone() }]);
    assert_eq!(
        compiled.primary().dependencies,
        [artifact::AppDependencyArtifact { app: "MiddleApp".to_string(), artifact_id: middle.id.clone() }]
    );
    let middle_handle = compiled
        .app("MiddleApp")
        .expect("bundle contains MiddleApp")
        .argent
        .template_plan
        .templates
        .iter()
        .find(|template| template.actor == "Middle")
        .map(|template| &template.actor_type_handle)
        .expect("Middle exports a source-state handle that contains its Leaf dependency");
    let root_script = &compiled.primary().sil_abi.contract("Root").expect("Root contract exists").compiled.bytecode;
    assert_eq!(
        byte_occurrences(root_script, &middle_handle.template.hash),
        1,
        "Root embeds the exported Middle source-state template once"
    );
    let runtime_bundle = compiled.runtime_bundle().expect("all transitive artifacts form one runtime bundle");
    builder::TxBuilder::from_bundle(&runtime_bundle).expect("all direct dependency ids match transitively");

    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn linked_authored_state_declarations_follow_the_contract_value_plan() {
    let temp = std::env::temp_dir().join(format!("argent-linked-authored-state-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(&temp).expect("temp dir created");

    std::fs::write(
        temp.join("child.ag"),
        r#"
state ChildStorage {
    int amount;
    virtual detail;
}

state ChildDetail {
    int count;
}

state ChildState expands ChildStorage {
    detail: ChildDetail;
}

state IdentityState {
    int amount;
}

actor Child owns ChildState {
    entry hold() emits none {
        require(amount >= 0);
    }
}

actor Identity owns IdentityState {
    entry hold() emits none {
        require(amount >= 0);
    }
}

app ChildApp {
    actor Child;
    actor Identity;
}
"#,
    )
    .expect("child source written");

    let cases = [
        (
            "entry-params",
            "",
            "",
            r#"
    entry hold(ChildState scalar, ChildState[2] fixed, ChildState[] dynamic) emits none {
        require(scalar.amount >= 0);
        require(fixed[0].amount >= 0);
        require(dynamic.length >= 0);
    }
"#,
            ["ChildState", "ChildDetail"].as_slice(),
            ["ChildState scalar", "ChildState[2] fixed", "ChildState[] dynamic"].as_slice(),
        ),
        (
            "function-signatures",
            r#"
fn global_amount(ChildState value) -> int {
    return value.amount;
}
"#,
            r#"
    fn actor_amount(ChildState value) -> int {
        return value.amount;
    }
"#,
            r#"
    entry hold() emits none {
        require(global_amount(ChildState { amount: 1, detail: ChildDetail { count: 1 } }) == 1);
        require(actor_amount(ChildState { amount: 2, detail: ChildDetail { count: 2 } }) == 2);
    }
"#,
            ["ChildState", "ChildDetail"].as_slice(),
            ["function global_amount(ChildState", "function actor_amount(ChildState"].as_slice(),
        ),
        (
            "global-function-body",
            r#"
fn global_amount(int amount) -> int {
    ChildState value = ChildState {
        amount: amount,
        detail: ChildDetail { count: amount },
    };
    return value.amount;
}
"#,
            "",
            r#"
    entry hold() emits none {
        require(global_amount(1) == 1);
    }
"#,
            ["ChildState", "ChildDetail"].as_slice(),
            ["function global_amount(int", "ChildState gen__glob_value = ChildState"].as_slice(),
        ),
        (
            "actor-function-body",
            "",
            r#"
    fn actor_amount(int amount) -> int {
        ChildState value = ChildState {
            amount: amount,
            detail: ChildDetail { count: amount },
        };
        return value.amount;
    }
"#,
            r#"
    entry hold() emits none {
        require(actor_amount(2) == 2);
    }
"#,
            ["ChildState", "ChildDetail"].as_slice(),
            ["function actor_amount(int", "ChildState value = ChildState"].as_slice(),
        ),
        (
            "constant",
            r#"
const ChildState INITIAL_CHILD = ChildState {
    amount: 1,
    detail: ChildDetail { count: 1 },
};
"#,
            "",
            r#"
    entry hold() emits none {
        require(INITIAL_CHILD.amount == 1);
    }
"#,
            ["ChildState", "ChildDetail"].as_slice(),
            ["ChildState constant INITIAL_CHILD"].as_slice(),
        ),
        (
            "body-local",
            "",
            "",
            r#"
    entry hold() emits none {
        ChildState value = ChildState {
            amount: 1,
            detail: ChildDetail { count: 1 },
        };
        require(value.amount == 1);
    }
"#,
            ["ChildState", "ChildDetail"].as_slice(),
            ["ChildState value = ChildState"].as_slice(),
        ),
        (
            "observed-input",
            "",
            "",
            r#"
    entry hold(cov_id child_id)
    observes source by child_id {
        inputs {
            child: ChildApp::Identity,
        }
    }
    emits none {
        IdentityState value = state(source.inputs.child);
        require(value.amount >= 0);
    }
"#,
            ["IdentityState"].as_slice(),
            ["IdentityState value = gen__source_child_state;"].as_slice(),
        ),
        (
            "inline-route",
            "",
            "",
            r#"
    entry send()
    spawns children by child_id {
        outputs {
            child: ChildApp::Child,
        }
    }
    emits none {
        unrestricted(children.outputs.child.value);
        require children.outputs become {
            child <- ChildApp::Child(ChildState {
                amount: 1,
                detail: ChildDetail { count: 1 },
            }),
        };
    }
"#,
            ["ChildState", "ChildDetail"].as_slice(),
            ["validateOutputStateWithTemplate(gen__children_child_output_idx,"].as_slice(),
        ),
    ];

    for (name, global, member, entry, declarations, expected) in cases {
        let source = format!(
            r#"
import "./child.ag";

{global}

state LocalState {{
    int nonce;
}}

actor Local owns LocalState {{
{member}
{entry}
}}

app LocalApp {{
    actor Local;
}}
"#
        );
        let local_path = temp.join(format!("local-{name}.ag"));
        std::fs::write(&local_path, source).expect("local source written");

        let out_dir = temp.join(format!("build-{name}"));
        build_file_app_bundle(&local_path, "LocalApp", &out_dir)
            .unwrap_or_else(|err| panic!("linked state used only by {name} must compile: {err}"));
        let sil = std::fs::read_to_string(out_dir.join("sil/Local.sil")).expect("Local Sil exists");
        for declaration in declarations {
            assert!(sil.contains(&format!("struct {declaration} {{")), "{name}: {sil}");
        }
        for expected in expected {
            assert!(sil.contains(expected), "{name} is missing `{expected}`: {sil}");
        }
    }

    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn app_bundle_compiles_a_diamond_dependency_once() {
    let temp = std::env::temp_dir().join(format!("argent-build-diamond-apps-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(&temp).expect("temp dir created");

    std::fs::write(
        temp.join("shared.ag"),
        r#"
state SharedState {
    int n;
}

actor Shared owns SharedState {
    entry hold() emits none {
        require(n >= 0);
    }
}

app SharedApp {
    actor Shared;
}
"#,
    )
    .expect("shared source written");
    std::fs::write(
        temp.join("left.ag"),
        r#"
import "./shared.ag";

state LeftState {
    int n;
}

actor Left owns LeftState {
    entry inspect(cov_id shared_id)
    observes shared by shared_id {
        inputs {
            src: SharedApp::Shared,
        }
    }
    emits none {
        SharedState current = state(shared.inputs.src);
        require(current.n >= 0);
    }
}

app LeftApp {
    actor Left;
}
"#,
    )
    .expect("left source written");
    std::fs::write(
        temp.join("right.ag"),
        r#"
import "./shared.ag";

state RightState {
    byte tag;
}

actor Right owns RightState {
    entry inspect(cov_id shared_id)
    observes shared by shared_id {
        inputs {
            src: SharedApp::Shared,
        }
    }
    emits none {
        SharedState current = state(shared.inputs.src);
        require(current.n >= 0);
    }
}

app RightApp {
    actor Right;
}
"#,
    )
    .expect("right source written");
    std::fs::write(
        temp.join("root.ag"),
        r#"
import "./left.ag" as left;
import "./right.ag" as right;

state RootState {}

actor Root owns RootState {
    entry inspect(cov_id left_id, cov_id right_id)
    observes left by left_id {
        inputs {
            src: left::LeftApp::Left,
        }
    }
    observes right by right_id {
        inputs {
            src: right::RightApp::Right,
        }
    }
    emits none {
        require(1 == 1);
    }
}

app RootApp {
    actor Root;
}
"#,
    )
    .expect("root source written");

    let out_dir = temp.join("build");
    let compiled = build_file_app_bundle(temp.join("root.ag"), "RootApp", &out_dir).expect("diamond app dependency graph compiles");
    assert_eq!(compiled.apps().count(), 4);
    for app in ["SharedApp", "LeftApp", "RightApp", "RootApp"] {
        assert!(compiled.app(app).is_some(), "compiled bundle is missing `{app}`");
    }
    assert!(out_dir.join("apps/SharedApp/artifact.json").is_file());
    assert!(out_dir.join("apps/LeftApp/artifact.json").is_file());
    assert!(out_dir.join("apps/RightApp/artifact.json").is_file());

    let shared_handle = compiled
        .app("SharedApp")
        .expect("shared dependency exists")
        .argent
        .template_plan
        .templates
        .iter()
        .find(|template| template.actor == "Shared")
        .map(|template| template.actor_type_handle.template.hash)
        .expect("shared actor handle exists");
    for (app, actor) in [("LeftApp", "Left"), ("RightApp", "Right")] {
        let branch_script = &compiled
            .app(app)
            .expect("diamond branch artifact exists")
            .sil_abi
            .contract(actor)
            .expect("diamond branch contract exists")
            .compiled
            .bytecode;
        assert_eq!(byte_occurrences(branch_script, &shared_handle), 1);
    }
    compiled.runtime_bundle().expect("all four diamond artifacts form one runtime bundle");

    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn module_import_links_qualified_app_actors_and_shares_constants() {
    let temp = std::env::temp_dir().join(format!("argent-build-module-app-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(&temp).expect("temp dir created");

    std::fs::write(
        temp.join("asset.ag"),
        r#"
const byte ASSET_TAG = 0x01;

state AssetState {
    byte tag;
}

actor Asset owns AssetState {
    entry keep() emits next: Asset {
        unrestricted(next.value);
        become next <- self;
    }
}

app AssetApp {
    actor Asset;
}
"#,
    )
    .expect("asset source written");
    std::fs::write(
        temp.join("controller.ag"),
        r#"
import "./asset.ag";

state ControllerState {
    cov_id asset_id;
}

actor Controller owns ControllerState {
    entry update()
    observes asset by self.asset_id {
        inputs {
            src: AssetApp::Asset,
        }
        outputs {
            asset_next: AssetApp::Asset,
        }
    }
    emits next: Controller {
        unrestricted(next.value);
        AssetState current = state(asset.inputs.src);
        require(current.tag == ASSET_TAG);
        require asset.outputs become {
            asset_next <- AssetApp::Asset(current),
        };
        become next <- self;
    }
}

app ControllerApp {
    actor Controller;
}
"#,
    )
    .expect("controller source written");

    let out_dir = temp.join("build");
    let compiled =
        build_file_app_bundle(temp.join("controller.ag"), "ControllerApp", &out_dir).expect("module app dependency compiles");

    assert_eq!(compiled.apps().map(|(app, _)| app).collect::<Vec<_>>(), ["AssetApp", "ControllerApp"]);
    assert!(out_dir.join("apps/AssetApp/artifact.json").is_file());
    let controller_sil = std::fs::read_to_string(out_dir.join("sil/Controller.sil")).expect("controller Sil exists");
    assert!(controller_sil.contains("byte constant ASSET_TAG = 1;"), "{controller_sil}");
    let observed =
        &compiled.primary().argent.actors.iter().find(|actor| actor.name == "Controller").expect("controller artifact exists").entries
            [0]
        .observes[0]
            .inputs[0];
    assert!(matches!(
        &observed.target,
        artifact::ObservedTargetArtifact::StaticActor { app, actor }
            if app == "AssetApp" && actor == "Asset"
    ));

    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn build_file_requires_selection_for_multiple_root_apps() {
    let temp = std::env::temp_dir().join(format!("argent-build-file-ambiguous-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(&temp).expect("temp dir created");

    let input = temp.join("pair.ag");
    std::fs::write(&input, TWO_APPS).expect("source written");

    let error = build_file(&input, temp.join("build")).expect_err("app selection is required");
    assert!(error.to_string().contains("select one with `--app <name>`"));

    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn aliased_signature_type_must_be_qualified() {
    let temp = std::env::temp_dir().join(format!("argent-aliased-signature-{}", std::process::id()));
    std::fs::create_dir_all(&temp).unwrap();
    std::fs::write(temp.join("asset.ag"), "state S { int n; } actor A owns S {}").unwrap();
    std::fs::write(
        temp.join("root.ag"),
        r#"
        import "./asset.ag" as asset;
        state R { int nonce; }
        actor Root owns R { entry check(S value) emits none { require(value.n >= 0); } }
        app Test { actor Root; actor asset::A; }
    "#,
    )
    .unwrap();

    let error = build_file(temp.join("root.ag"), temp.join("out"))
        .expect_err("selecting asset::A must not make its state S visible by bare name");
    assert!(error.to_string().contains("unknown export `S`"), "{error}");
    std::fs::remove_dir_all(temp).unwrap();
}

#[test]
fn aliased_observation_actor_must_be_qualified() {
    let temp = std::env::temp_dir().join(format!("argent-aliased-observation-{}", std::process::id()));
    std::fs::create_dir_all(&temp).unwrap();
    std::fs::write(temp.join("asset.ag"), "state S { int n; } actor A owns S {}").unwrap();
    std::fs::write(
        temp.join("root.ag"),
        r#"
        import "./asset.ag" as asset;
        state R { int nonce; }
        actor Root owns R {
            entry check(cov_id id) observes source by id { inputs { a: A, } } emits none {}
        }
        app Test { actor Root; actor asset::A; }
    "#,
    )
    .unwrap();

    let error = build_file(temp.join("root.ag"), temp.join("out"))
        .expect_err("selecting asset::A must not make bare A visible in observations");
    assert!(error.to_string().contains("unknown export `A`"), "{error}");
    std::fs::remove_dir_all(temp).unwrap();
}

#[test]
fn body_app_reference_must_include_import_alias() {
    let temp = std::env::temp_dir().join(format!("argent-body-app-alias-{}", std::process::id()));
    std::fs::create_dir_all(&temp).unwrap();
    std::fs::write(
        temp.join("asset.ag"),
        "state S { int n; } actor A owns S { entry hold() emits none {} } app AssetApp { actor A; }",
    )
    .unwrap();
    std::fs::write(
        temp.join("root.ag"),
        r#"
        import "./asset.ag" as asset;
        state R { int nonce; }
        actor Root owns R {
            entry send(cov_id id)
            observes source by id {
                inputs { a: asset::AssetApp::A, }
                outputs { a: asset::AssetApp::A, }
            }
            emits none {
                require source.outputs become { a <- AssetApp::A(state(source.inputs.a)), };
            }
        }
        app Test { actor Root; }
    "#,
    )
    .unwrap();

    let error = build_file(temp.join("root.ag"), temp.join("out"))
        .expect_err("a linker-generated app name must not resolve an authored successor");
    assert!(error.to_string().contains("unresolved qualified reference `AssetApp::A`"), "{error}");
    std::fs::remove_dir_all(temp).unwrap();
}

#[test]
fn local_state_does_not_conflict_with_linked_state() {
    let temp = std::env::temp_dir().join(format!("argent-local-linked-state-{}", std::process::id()));
    std::fs::create_dir_all(&temp).unwrap();
    std::fs::write(
        temp.join("asset.ag"),
        "state S { int amount; } actor A owns S { entry hold() emits none {} } app AssetApp { actor A; }",
    )
    .unwrap();
    std::fs::write(
        temp.join("root.ag"),
        r#"
        import "./asset.ag" as asset;
        state S { bool active; }
        actor Root owns S {
            entry check(cov_id id)
            observes source by id { inputs { a: asset::AssetApp::A, } }
            emits none { require(state(source.inputs.a).amount >= 0); }
        }
        app Test { actor Root; }
    "#,
    )
    .unwrap();

    build_file(temp.join("root.ag"), temp.join("out")).expect("unrelated local and linked S declarations may have different layouts");
    std::fs::remove_dir_all(temp).unwrap();
}

#[test]
fn co_spend_uses_imported_storage_type_despite_local_name_collision() {
    let temp = std::env::temp_dir().join(format!("argent-linked-co-spend-type-{}", std::process::id()));
    std::fs::create_dir_all(&temp).unwrap();
    std::fs::write(
        temp.join("asset.ag"),
        "state S { byte[32] guard; } actor A owns S { entry hold() emits none {} } app AssetApp { actor A; }",
    )
    .unwrap();
    std::fs::write(
        temp.join("root.ag"),
        r#"
        import "./asset.ag" as asset;
        state S { cov_id guard; }
        actor Root owns asset::S {
            entry check() emits none { require(guard.co_spent()); }
        }
        app Test { actor Root; }
        "#,
    )
    .unwrap();

    let error = build_file(temp.join("root.ag"), temp.join("out")).expect_err("the local S must not supply the imported S field type");
    assert!(error.to_string().contains("requires one `cov_id` receiver"), "{error}");
    std::fs::write(
        temp.join("asset.ag"),
        "state S { cov_id guard; } actor A owns S { entry hold() emits none {} } app AssetApp { actor A; }",
    )
    .unwrap();
    std::fs::write(
        temp.join("root.ag"),
        r#"
        import "./asset.ag" as asset;
        state S { byte[32] guard; }
        actor Root owns asset::S {
            entry check() emits none { require(guard.co_spent()); }
        }
        app Test { actor Root; }
        "#,
    )
    .unwrap();
    build_file(temp.join("root.ag"), temp.join("out-valid"))
        .expect("the imported S cov_id must remain valid despite the local S type");
    std::fs::remove_dir_all(temp).unwrap();
}

#[test]
fn dependencies_can_have_distinct_states_named_s() {
    let temp = std::env::temp_dir().join(format!("argent-dependency-state-collision-{}", std::process::id()));
    std::fs::create_dir_all(&temp).unwrap();
    std::fs::write(
        temp.join("left.ag"),
        "state S { int amount; } actor A owns S { entry hold() emits none {} } app LeftApp { actor A; }",
    )
    .unwrap();
    std::fs::write(
        temp.join("right.ag"),
        "state S { bool active; } actor A owns S { entry hold() emits none {} } app RightApp { actor A; }",
    )
    .unwrap();
    std::fs::write(
        temp.join("root.ag"),
        r#"
        import "./left.ag" as left;
        import "./right.ag" as right;
        state R { int nonce; }
        actor Root owns R {
            entry check(cov_id left_id, cov_id right_id)
            observes left by left_id { inputs { a: left::LeftApp::A, } }
            observes right by right_id { inputs { a: right::RightApp::A, } }
            emits none {
                require(state(left.inputs.a).amount >= 0);
                require(state(right.inputs.a).active);
            }
        }
        app Test { actor Root; }
    "#,
    )
    .unwrap();

    let artifact = build_file(temp.join("root.ag"), temp.join("out")).expect("each dependency keeps its own S layout");
    let linked_sources = artifact
        .sil_abi
        .structs
        .iter()
        .filter(|(_, structure)| structure.fields.len() == 1 && matches!(structure.fields[0].name.as_str(), "amount" | "active"))
        .map(|(name, structure)| (name.as_str(), structure.fields[0].name.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(linked_sources.len(), 2, "distinct dependency states must retain separate ABI structs: {linked_sources:?}");
    assert_ne!(linked_sources[0].0, linked_sources[1].0);
    std::fs::remove_dir_all(temp).unwrap();
}

#[test]
fn actor_handle_accepts_imported_state_with_local_name_collision() {
    let temp = std::env::temp_dir().join(format!("argent-shared-state-handle-{}", std::process::id()));
    std::fs::create_dir_all(&temp).unwrap();
    std::fs::write(
        temp.join("asset.ag"),
        "state S { int amount; } actor A owns S { entry hold() emits none {} } app AssetApp { actor A; }",
    )
    .unwrap();
    std::fs::write(
        temp.join("root.ag"),
        r#"
        import "./asset.ag" as asset;
        state S { byte tag; }
        actor Root owns S {
            entry send(cov_id id, actor_type<asset::S> target)
            observes source by id { inputs { a: asset::AssetApp::A, } }
            spawns children by child_id { outputs { a: target, } }
            emits none {
                asset::S current = state(source.inputs.a);
                unrestricted(children.outputs.a.value);
                require children.outputs become { a <- target(current), };
            }
        }
        app Test { actor Root; }
    "#,
    )
    .unwrap();

    // The local S forces asset::S to use a different name from its dependency build.
    build_file(temp.join("root.ag"), temp.join("out")).expect("the observed source state must still match actor_type<asset::S>");
    std::fs::remove_dir_all(temp).unwrap();
}

#[test]
fn linking_same_actor_enum_through_different_apps_is_rejected() {
    let temp = std::env::temp_dir().join(format!("argent-shared-app-enum-{}", std::process::id()));
    std::fs::create_dir_all(&temp).unwrap();
    let shared = r#"
        state S { int n; }
        actor A owns S { entry hold() emits none { require(n >= 0); } }
        actor B owns S { entry hold() emits none { require(n > 1); } }
        actor C owns S { entry hold() emits none { require(n > 2); } }
        app SmallApp { actor A; actor B; }
        app LargeApp { actor A; actor B; actor C; }
    "#;
    std::fs::write(temp.join("shared.ag"), shared).unwrap();
    std::fs::write(
        temp.join("root.ag"),
        r#"
        import "./shared.ag" as shared;
        state R { int nonce; }
        actor Root owns R {
            entry check(cov_id small_id, cov_id large_id)
            observes small by small_id { inputs { a: shared::SmallApp::A, } }
            observes large by large_id { inputs { a: shared::LargeApp::A, } }
            emits none {}
        }
        app Test { actor Root; }
    "#,
    )
    .unwrap();

    build_file_app_bundle(temp.join("root.ag"), "Test", temp.join("without-enum"))
        .expect("actors from both apps link without the enum");

    // The restriction also applies when the root never references the enum.
    std::fs::write(temp.join("shared.ag"), format!("{shared}\nactor enum Kind {{ A; B; }}\n")).unwrap();
    let result = build_file_app_bundle(temp.join("root.ag"), "Test", temp.join("with-enum"));
    std::fs::remove_dir_all(temp).unwrap();
    let err = result.expect_err("linking the same enum declaration through different apps is unsupported");
    assert_eq!(
        err.message,
        "actor enum `Kind` is provided through both `LargeApp` and `SmallApp`; \
         linking the same enum declaration through different apps is not supported"
    );
}

#[test]
fn transitive_linked_enum_variants_keep_their_defining_app() {
    let temp = std::env::temp_dir().join(format!("argent-transitive-enum-{}", std::process::id()));
    std::fs::create_dir_all(&temp).unwrap();
    std::fs::write(
        temp.join("asset.ag"),
        r#"
        state S { int n; }
        actor A owns S { entry hold() emits none { require(n >= 0); } }
        actor B owns S { entry hold() emits none { require(n > 1); } }
        actor enum Kind { A; B; }
        app AssetApp { actor A; actor B; }
    "#,
    )
    .unwrap();
    std::fs::write(
        temp.join("middle.ag"),
        r#"
        import "./asset.ag" as asset;
        state M { int count; }
        actor Middle owns M {
            entry check(cov_id id)
            observes source by id { inputs { a: asset::AssetApp::A, } }
            emits none {}
        }
        app MiddleApp { actor Middle; }
    "#,
    )
    .unwrap();
    std::fs::write(
        temp.join("root.ag"),
        r#"
        import "./asset.ag" as asset;
        import "./middle.ag" as middle;
        state R { int nonce; }
        actor Root owns R {
            entry check(cov_id id, cov_id middle_id)
            observes source by id { inputs { a: asset::AssetApp::A, } }
            observes bridge by middle_id { inputs { a: middle::MiddleApp::Middle, } }
            emits none {}
        }
        app Test { actor Root; }
    "#,
    )
    .unwrap();

    let artifact = build_file(temp.join("root.ag"), temp.join("out")).unwrap();
    assert_eq!(artifact.argent.actor_enums[0].variants, ["AssetApp::A", "AssetApp::B"]);
    std::fs::remove_dir_all(temp).unwrap();
}

#[test]
fn aliased_actor_enum_variant_compiles() {
    let temp = std::env::temp_dir().join(format!("argent-aliased-enum-variant-{}", std::process::id()));
    std::fs::create_dir_all(&temp).unwrap();
    std::fs::write(
        temp.join("asset.ag"),
        r#"
        state S { int n; }
        actor A owns S { entry hold() emits none { require(n >= 0); } }
        actor B owns S { entry hold() emits none { require(n > 1); } }
        actor enum Kind { A; B; }
    "#,
    )
    .unwrap();
    std::fs::write(
        temp.join("root.ag"),
        r#"
        import "./asset.ag" as asset;
        actor Root owns asset::S {
            entry choose() emits next: asset::Kind {
                asset::Kind selected = asset::Kind::A;
                unrestricted(next.value);
                become next <- selected(state(self));
            }
        }
        app Test { actor Root; actor asset::A; actor asset::B; }
    "#,
    )
    .unwrap();

    build_file(temp.join("root.ag"), temp.join("out")).expect("the qualified enum variant resolves through its alias");
    std::fs::remove_dir_all(temp).unwrap();
}

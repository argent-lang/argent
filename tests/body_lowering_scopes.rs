use std::{fs, path::PathBuf};

use argent::builder::{Artifact, EntryCall, TxBuilder, TxContext, actor, args, state};
use kaspa_consensus_core::{
    Hash,
    tx::{CovenantBinding, TransactionId, TransactionOutpoint},
};

const APP_SOURCE: &str = r#"
state CounterState {
    int count;
    cov_id guard;
}

state ShadowState {
    bool marker;
}

actor Counter owns CounterState {
    entry bump(bool choose, int iterations) emits next: Counter {
        unrestricted(next.value);

        __BODY__
    }
}

actor Shadow owns ShadowState {
    entry inspect() emits none {
        require(marker);
    }
}

app ScopeMatrix {
    actor Counter;
    actor Shadow;
}
"#;

const SELECTOR_APP_SOURCE: &str = r#"
state BoardState {
    int count;
}

actor enum NextActor {
    Alpha;
    Beta;
}

actor Router owns BoardState {
    entry choose(NextActor target, bool first_branch) emits next: NextActor {
        unrestricted(next.value);
        BoardState next_state = {
            count: count + 1,
        };

        if (first_branch) {
            become next <- target(next_state);
        } else {
            become next <- target(next_state);
        }
    }
}

actor Alpha owns BoardState {
    entry inspect() emits none {
        require(count >= 0);
    }
}

actor Beta owns BoardState {
    entry inspect() emits none {
        require(count >= 1);
    }
}

app SelectorScope {
    actor Router;
    actor Alpha;
    actor Beta;
}
"#;

const SCOPED_SELECTOR_SOURCE: &str = r#"
state BoardState {
    int count;
}

actor enum NextActor {
    Alpha;
    Beta;
}

actor Router owns BoardState {
    entry choose(bool choose) emits next: NextActor {
        unrestricted(next.value);
        BoardState next_state = {
            count: count + 1,
        };

        __SELECTOR_SCOPE__

        become next <- target(next_state);
    }
}

actor Alpha owns BoardState {
    entry inspect() emits none {
        require(count >= 0);
    }
}

actor Beta owns BoardState {
    entry inspect() emits none {
        require(count >= 1);
    }
}

app ScopedSelector {
    actor Router;
    actor Alpha;
    actor Beta;
}
"#;

const SELECTOR_SHADOW_SOURCE: &str = r#"
state BoardState {
    int count;
}

state PairState {
    int item;
}

actor enum NextActor {
    Alpha;
    Beta;
}

fn identity(int value) -> int {
    return value;
}

actor Router owns BoardState {
    entry choose(NextActor target, byte[] data) emits next: NextActor {
        unrestricted(next.value);
        BoardState next_state = {
            count: count + 1,
        };
        PairState pair = {
            item: count,
        };

        __SHADOW__
    }
}

actor Alpha owns BoardState {
    entry inspect() emits none {
        require(count >= 0);
    }
}

actor Beta owns BoardState {
    entry inspect() emits none {
        require(count >= 1);
    }
}

app SelectorShadow {
    actor Router;
    actor Alpha;
    actor Beta;
}
"#;

#[test]
fn state_binding_remains_visible_after_a_standalone_block() {
    execute_scope_flows(
        "state-standalone-block",
        r#"
        CounterState next_state = {
            count: count + 1,
            guard: guard,
        };
        {
            ShadowState probe_state = {
                marker: true,
            };
            require(probe_state.marker);
        }

        become next <- Counter(next_state);
        "#,
        &[(true, 1)],
    );
}

#[test]
fn state_binding_remains_visible_after_both_if_paths() {
    execute_scope_flows(
        "state-if-branch",
        r#"
        CounterState next_state = {
            count: count + 1,
            guard: guard,
        };
        if (choose) {
            ShadowState probe_state = {
                marker: true,
            };
            require(probe_state.marker);
        }

        become next <- Counter(next_state);
        "#,
        &[(false, 1), (true, 1)],
    );
}

#[test]
fn state_binding_remains_visible_after_zero_and_nonzero_loop_iterations() {
    execute_scope_flows(
        "state-for-body",
        r#"
        CounterState next_state = {
            count: count + 1,
            guard: guard,
        };
        for (i, 0, iterations, 8) {
            ShadowState probe_state = {
                marker: i >= 0,
            };
            require(probe_state.marker);
        }

        become next <- Counter(next_state);
        "#,
        &[(true, 0), (true, 3)],
    );
}

#[test]
fn cov_id_binding_remains_visible_after_a_standalone_block() {
    execute_scope_flows(
        "cov-id-standalone-block",
        r#"
        cov_id relevant = guard;
        {
            int probe = 1;
            require(probe == 1);
        }
        require(relevant.co_spent());

        CounterState next_state = {
            count: count + 1,
            guard: guard,
        };
        become next <- Counter(next_state);
        "#,
        &[(true, 1)],
    );
}

#[test]
fn cov_id_binding_remains_visible_after_both_if_paths() {
    execute_scope_flows(
        "cov-id-if-branch",
        r#"
        cov_id relevant = guard;
        if (choose) {
            int probe = 1;
            require(probe == 1);
        }
        require(relevant.co_spent());

        CounterState next_state = {
            count: count + 1,
            guard: guard,
        };
        become next <- Counter(next_state);
        "#,
        &[(false, 1), (true, 1)],
    );
}

#[test]
fn cov_id_binding_remains_visible_after_zero_and_nonzero_loop_iterations() {
    execute_scope_flows(
        "cov-id-for-body",
        r#"
        cov_id relevant = guard;
        for (i, 0, iterations, 8) {
            int probe = i;
            require(probe >= 0);
        }
        require(relevant.co_spent());

        CounterState next_state = {
            count: count + 1,
            guard: guard,
        };
        become next <- Counter(next_state);
        "#,
        &[(true, 0), (true, 3)],
    );
}

#[test]
fn selector_materialization_is_independent_between_if_branches() {
    let artifact = compile_app("selector-if-branches", SELECTOR_APP_SOURCE);
    let builder = TxBuilder::new(&artifact).expect("builder accepts selector scope fixture");
    let covenant_id = Hash::from_bytes([0x61; 32]);
    let input_value = 1_000;

    for (index, (target, first_branch)) in [("Alpha", false), ("Alpha", true), ("Beta", false), ("Beta", true)].into_iter().enumerate()
    {
        let initial_state = state! { count: 4 };
        let input_utxo = builder
            .covenant_utxo("Router", initial_state.clone(), input_value, 0, false, Some(covenant_id))
            .expect("Router UTXO builds");
        let context = TxContext::new()
            .actor_input(
                "Router",
                initial_state,
                EntryCall::new("choose").args(args![actor(target), first_branch]),
                TransactionOutpoint::new(TransactionId::from_bytes([0x70 + index as u8; 32]), 0),
                input_utxo,
                0,
            )
            .actor_output(target, state! { count: 5 }, CovenantBinding::new(0, covenant_id), input_value);

        builder
            .build(&context)
            .unwrap_or_else(|err| panic!("selector scope flow target={target}, first_branch={first_branch} failed: {err}"));
    }
}

#[test]
fn selector_binding_does_not_escape_a_standalone_block() {
    assert_selector_scope_rejected(
        "selector-standalone-block",
        r#"
        {
            NextActor target = NextActor::Alpha;
        }
        "#,
    );
}

#[test]
fn selector_binding_does_not_escape_an_if_branch() {
    assert_selector_scope_rejected(
        "selector-if-branch",
        r#"
        if (choose) {
            NextActor target = NextActor::Alpha;
        }
        "#,
    );
}

#[test]
fn selector_binding_does_not_escape_a_for_body() {
    assert_selector_scope_rejected(
        "selector-for-body",
        r#"
        for (i, 0, 1, 1) {
            NextActor target = NextActor::Alpha;
        }
        "#,
    );
}

#[test]
fn nested_actor_enum_cannot_shadow_a_same_named_integer_parameter() {
    let source = SCOPED_SELECTOR_SOURCE.replace("entry choose(bool choose)", "entry choose(int target, bool choose)").replace(
        "__SELECTOR_SCOPE__",
        r#"
            if (choose) {
                NextActor target = NextActor::Alpha;
            }
            "#,
    );
    assert_selector_source_rejected(
        "selector-int-parameter-collision",
        &source,
        "entry binding `target` collides with entry parameter of the same name",
    );
}

#[test]
fn typed_local_shadows_an_outer_selector() {
    assert_selector_shadow_rejected(
        "selector-typed-local-shadow",
        r#"
        {
            int target = 0;
            become next <- target(next_state);
        }
        "#,
    );
}

#[test]
fn destructured_local_shadows_an_outer_selector() {
    assert_selector_shadow_rejected(
        "selector-destructured-local-shadow",
        r#"
        {
            {item: int target} = pair;
            become next <- target(next_state);
        }
        "#,
    );
}

#[test]
fn tuple_local_shadows_an_outer_selector() {
    assert_selector_shadow_rejected(
        "selector-tuple-local-shadow",
        r#"
        {
            (byte[] target, byte[] remainder) = data.split(1);
            become next <- target(next_state);
        }
        "#,
    );
}

#[test]
fn unparenthesized_tuple_local_shadows_an_outer_selector() {
    assert_selector_shadow_rejected(
        "selector-unparenthesized-tuple-local-shadow",
        r#"
        {
            byte[] target, byte[] remainder = data.split(1);
            become next <- target(next_state);
        }
        "#,
    );
}

#[test]
fn function_result_local_shadows_an_outer_selector() {
    assert_selector_shadow_rejected(
        "selector-function-result-local-shadow",
        r#"
        {
            (int target) = identity(0);
            become next <- target(next_state);
        }
        "#,
    );
}

#[test]
fn outer_selector_remains_visible_after_a_loop() {
    let source = SCOPED_SELECTOR_SOURCE.replace("entry choose(bool choose)", "entry choose(NextActor target)").replace(
        "__SELECTOR_SCOPE__",
        r#"
        for (i, 0, 1, 1) {
            require(i >= 0);
        }
        "#,
    );
    compile_app("selector-loop-binding-scope", &source);
}

#[test]
fn lexical_shadowing_is_rejected_by_sil() {
    let source = APP_SOURCE.replace(
        "__BODY__",
        r#"
        int value = 1;
        {
            int value = 2;
            require(value == 2);
        }

        CounterState next_state = {
            count: count + 1,
            guard: guard,
        };
        become next <- Counter(next_state);
        "#,
    );
    assert_source_rejected("lexical-shadowing", &source, "variable 'value' is already defined");
}

#[test]
fn actor_helper_bindings_cannot_shadow_constants() {
    let source = r#"
        const int INDEX = 3;
        const int INDEX__1 = 4;
        const int INDEX__2 = 5;
        state S { int count; }

        actor A owns S {
            fn echo(__PARAMS__) -> int { __BODY__ }

            entry check() emits none {
                require(echo(__ARGS__) == 1);
            }
        }
        app Test { actor A; }
    "#;
    for (name, params, args, body) in [
        ("parameter", "int INDEX", "count", "return INDEX;"),
        ("local", "", "", "int INDEX = 1; return INDEX;"),
        ("loop", "", "", "int result = 0; for (INDEX, 0, 2, 2) { result = result + INDEX; } return result;"),
        ("nested-local", "", "", "{ int INDEX = 1; require(INDEX == 1); } return 1;"),
        ("destructure", "", "", "S { count: int INDEX } = S { count: 1 }; return INDEX;"),
        ("tuple", "", "", "byte[] INDEX, byte[] rest = 0x0001.split(1); return INDEX.length + rest.length;"),
    ] {
        let source = source.replace("__PARAMS__", params).replace("__ARGS__", args).replace("__BODY__", body);
        assert_source_rejected(&format!("actor-helper-constant-shadow-{name}"), &source, "variable 'INDEX' is already defined");
    }
}

#[test]
fn current_and_spawned_output_values_lower_to_sil() {
    let source = include_str!("fixtures/runtime/context_genesis_spawn/app.ag")
        .split("app PairApp {")
        .next()
        .expect("controller fixture has one selected app")
        .replace("unrestricted(new_pair.outputs.left.value);", "require(new_pair.outputs.left.value >= 0);")
        .replace("unrestricted(next.value);", "require(next.value >= 0);");
    let out_dir = std::env::temp_dir().join(format!("argent-output-value-sites-{}", std::process::id()));
    if out_dir.exists() {
        fs::remove_dir_all(&out_dir).expect("old output-value fixture is removed");
    }
    argent::build_inline(PathBuf::from("output-value-sites.ag"), source, &out_dir)
        .expect("bound output-value sites compile without test-only lowering fallbacks");
    let sil = fs::read_to_string(out_dir.join("sil").join("Controller.sil")).expect("compiled controller Sil");
    assert!(sil.contains("require(tx.outputs[gen__new_pair_left_output_idx].value >= 0);"), "{sil}");
    assert!(sil.contains("require(tx.outputs[gen__next_output_idx].value >= 0);"), "{sil}");
    fs::remove_dir_all(out_dir).expect("output-value fixture is removed");
}

fn assert_selector_shadow_rejected(name: &str, shadow: &str) {
    let source = SELECTOR_SHADOW_SOURCE.replace("__SHADOW__", shadow);
    assert_selector_source_rejected(name, &source, "entry binding `target` collides with entry parameter of the same name");
}

fn assert_selector_scope_rejected(name: &str, selector_scope: &str) {
    let source = SCOPED_SELECTOR_SOURCE.replace("__SELECTOR_SCOPE__", selector_scope);
    assert_selector_source_rejected(name, &source, "actor handle `target` is not visible in this scope");
}

fn assert_selector_source_rejected(name: &str, source: &str, expected: &str) {
    assert_source_rejected(name, source, expected);
}

fn assert_source_rejected(name: &str, source: &str, expected: &str) {
    let out_dir = std::env::temp_dir().join(format!("argent-{name}-scope-test-{}", std::process::id()));
    if out_dir.exists() {
        fs::remove_dir_all(&out_dir).expect("old scope test output is removed");
    }

    let err = match argent::build_inline(PathBuf::from(format!("{name}.ag")), source, &out_dir) {
        Ok(_) => panic!("scope fixture `{name}` must be rejected"),
        Err(err) => err,
    };
    if out_dir.exists() {
        fs::remove_dir_all(&out_dir).expect("scope test output is removed");
    }
    assert!(err.to_string().contains(expected), "unexpected scope error: {err}");
}

fn execute_scope_flows(name: &str, body: &str, flows: &[(bool, i64)]) {
    let artifact = compile_scope_app(name, body);
    let builder = TxBuilder::new(&artifact).expect("builder accepts scope fixture");
    let covenant_id = Hash::from_bytes([0x41; 32]);
    let input_value = 1_000;

    for (index, &(choose, iterations)) in flows.iter().enumerate() {
        let initial_state = state! {
            count: 4,
            guard: covenant_id,
        };
        let input_utxo = builder
            .covenant_utxo("Counter", initial_state.clone(), input_value, 0, false, Some(covenant_id))
            .expect("Counter UTXO builds");
        let context = TxContext::new()
            .actor_input(
                "Counter",
                initial_state,
                EntryCall::new("bump").args(args![choose, iterations]),
                TransactionOutpoint::new(TransactionId::from_bytes([0x50 + index as u8; 32]), 0),
                input_utxo,
                0,
            )
            .actor_output(
                "Counter",
                state! {
                    count: 5,
                    guard: covenant_id,
                },
                CovenantBinding::new(0, covenant_id),
                input_value,
            );

        builder.build(&context).unwrap_or_else(|err| panic!("scope flow choose={choose}, iterations={iterations} failed: {err}"));
    }
}

fn compile_scope_app(name: &str, body: &str) -> Artifact {
    let source = APP_SOURCE.replace("__BODY__", body);
    compile_app(name, &source)
}

fn compile_app(name: &str, source: &str) -> Artifact {
    let out_dir = std::env::temp_dir().join(format!("argent-{name}-scope-test-{}", std::process::id()));
    if out_dir.exists() {
        fs::remove_dir_all(&out_dir).expect("old scope test output is removed");
    }

    let result = argent::build_inline(PathBuf::from(format!("{name}.ag")), source, &out_dir);
    if out_dir.exists() {
        fs::remove_dir_all(&out_dir).expect("scope test output is removed");
    }
    result.unwrap_or_else(|err| panic!("scope fixture `{name}` must compile: {err}"))
}

#[test]
fn ast_lowering_reconstructs_expanded_active_state() {
    let source = r#"
        state Capsule { int nonce; virtual detail; }
        state Details { int count; }
        state Expanded expands Capsule { detail: Details; }

        actor Vault owns Expanded {
            entry verify() emits none {
                Expanded snapshot = state(self);
                byte[32] direct = digest(state(self));
                byte[32] local = digest(snapshot);
                require(direct == local);
            }
        }

        app Test { actor Vault; }
    "#;
    compile_app("ast-expanded-active-state", source);
}

#[test]
fn ast_lowering_authenticates_observed_covenants_from_both_source_kinds() {
    let state_field_source = include_str!("fixtures/emit/observed_template_witnesses/app.ag");
    compile_app("ast-observed-state-field", state_field_source);

    let argument_source = state_field_source
        .replace("entry step()", "entry step(cov_id observed_id)")
        .replace("observes asset by self.target_id", "observes asset by observed_id");
    compile_app("ast-observed-entry-argument", &argument_source);
}

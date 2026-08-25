use std::{
    fs,
    path::{Path, PathBuf},
};

use kaspa_txscript::opcodes::codes::OpPushData1;

use super::*;
use crate::routing::{CommitmentNode, RouteGraph, SelectorRequirement};

#[test]
fn rejects_route_outside_named_output_union() {
    let mut program = test_program();
    program.modules[0].actors[0].entries[0].routes =
        vec![RouteCall { output: "next".to_string(), actor: "Game".to_string(), state: "next_game".to_string() }];

    let err = Model::from_program(&program).expect_err("route must be rejected");
    assert!(err.to_string().contains("routes output `next` to `Game`"), "unexpected error: {err}");
}

#[test]
fn accepts_route_inside_named_output_union() {
    let mut program = test_program();
    program.modules[0].actors[0].entries[0].emits = EmitSpec::Outputs(vec![EmitOutput {
        name: "next".to_string(),
        actors: vec!["Player".to_string(), "Game".to_string()],
        auth_index: 0,
    }]);
    let route = RouteCall { output: "next".to_string(), actor: "Game".to_string(), state: "next_game".to_string() };
    program.modules[0].actors[0].entries[0].routes = vec![route.clone()];
    program.modules[0].actors[0].entries[0].terminal_route_sets = vec![vec![route]];

    Model::from_program(&program).expect("route should be accepted");
}

#[test]
fn planning_uses_declared_emit_domain_not_body_routes() {
    let path = PathBuf::from("declared-emit-domain.ag");
    let module = crate::compiler::syntax::parser::parse_module(
        path.clone(),
        r#"
            state SourceState {
                int nonce;
            }

            state TargetState {
                int nonce;
            }

            actor Source owns SourceState {
                entry choose_a() emits next: A | B {
                    unrestricted(next.value);
                    TargetState next = {
                        nonce: nonce,
                    };
                    become next <- A(next);
                }
            }

            actor A owns TargetState {}
            actor B owns TargetState {}

            app DeclaredEmitDomain {
                actor Source;
                actor A;
                actor B;
            }
            "#
        .to_string(),
    )
    .expect("source parses");
    let program = Program { root: path, modules: vec![module] };

    let model = Model::from_program(&program).expect("declared emit domain plans");
    let source = model.actor("Source").expect("Source exists");
    let entry = &source.entries[0];
    assert_eq!(
        model
            .entry_model(source, entry)
            .expect("entry model exists")
            .expanded_routes()
            .iter()
            .map(|route| route.actor.as_str())
            .collect::<Vec<_>>(),
        ["A"]
    );
    assert!(model.route_transitions.contains_key(&("Source".to_string(), "A".to_string())));
    assert!(model.route_transitions.contains_key(&("Source".to_string(), "B".to_string())));
    assert_eq!(model.route_leaves_by_actor["Source"], [RouteRootLeaf::Actor("A".to_string()), RouteRootLeaf::Actor("B".to_string())]);
}

#[test]
fn rejects_user_state_named_state() {
    let err = parse_and_validate(
        r#"
            state State {}

            actor Foo owns State {
                entry hold() emits none {
                    require(1 == 1);
                }
            }

            app Test {
                actor Foo;
            }
            "#,
    )
    .expect_err("source `State` must be reserved");

    assert!(err.to_string().contains("reserved for generated Silverscript state"), "unexpected error: {err}");
}

#[test]
fn rejects_missing_named_output_coverage() {
    let mut program = test_program();
    program.modules[0].actors[0].entries[0].emits = EmitSpec::Outputs(vec![
        EmitOutput { name: "a".to_string(), actors: vec!["Player".to_string()], auth_index: 0 },
        EmitOutput { name: "b".to_string(), actors: vec!["Player".to_string()], auth_index: 1 },
    ]);
    let route = RouteCall { output: "a".to_string(), actor: "Player".to_string(), state: "next_a".to_string() };
    program.modules[0].actors[0].entries[0].routes = vec![route.clone()];
    program.modules[0].actors[0].entries[0].terminal_route_sets = vec![vec![route]];

    let err = Model::from_program(&program).expect_err("missing output coverage must be rejected");
    assert!(err.to_string().contains("does not validate output `b`"), "unexpected error: {err}");
}

#[test]
fn rejects_source_with_missing_named_output_coverage() {
    let module = crate::compiler::syntax::parser::parse_module(
        PathBuf::from("test.ag"),
        r#"
            state FooState {}

            actor Foo owns FooState {
                entry step() emits {
                    a: Foo,
                    b: Foo,
                } {
                    unrestricted(a.value);
                    unrestricted(b.value);
                    become a <- Foo(next_a);
                }
            }

            app Test {
                actor Foo;
            }
            "#
        .to_string(),
    )
    .expect("source parses");
    let program = Program { root: PathBuf::from("test.ag"), modules: vec![module] };

    let err = Model::from_program(&program).expect_err("missing output coverage must be rejected");
    assert!(err.to_string().contains("does not validate output `b`"), "unexpected error: {err}");
}

#[test]
fn rejects_explicit_auth_output_index_syntax() {
    let err = parse_and_validate(
        r#"
            state FooState {
                int amount;
            }

            actor Foo owns FooState {
                entry bump() emits {
                    next: Foo at auth[0],
                } {
                    unrestricted(next.value);
                    State next_state = {
                        amount: amount + 1,
                    };

                    become next <- Foo(next_state);
                }
            }

            app Test {
                actor Foo;
            }
            "#,
    )
    .expect_err("explicit auth output indexes must not be source syntax");

    assert!(err.to_string().contains("expected `,` or `}`"), "unexpected error: {err}");
}

#[test]
fn rejects_duplicate_named_output_coverage() {
    let mut program = test_program();
    let first = RouteCall { output: "next".to_string(), actor: "Player".to_string(), state: "next_player".to_string() };
    let second = RouteCall { output: "next".to_string(), actor: "Player".to_string(), state: "other_player".to_string() };
    program.modules[0].actors[0].entries[0].routes = vec![first.clone(), second.clone()];
    program.modules[0].actors[0].entries[0].terminal_route_sets = vec![vec![first, second]];

    let err = Model::from_program(&program).expect_err("duplicate output coverage must be rejected");
    assert!(err.to_string().contains("validates output `next` more than once"), "unexpected error: {err}");
}

#[test]
fn rejects_delegate_become() {
    let mut program = test_program();
    program.modules[0].actors[0].entries[0].kind = EntryKind::Delegate;
    program.modules[0].actors[0].entries[0].consumes.push(ConsumeDecl { name: "leader".to_string(), actor: "Player".to_string() });
    program.modules[0].actors[0].entries[0].emits = EmitSpec::None;
    program.modules[0].actors[0].entries[0].routes =
        vec![RouteCall { output: "next".to_string(), actor: "Player".to_string(), state: "next_player".to_string() }];

    let err = Model::from_program(&program).expect_err("delegate become must be rejected");
    assert!(err.to_string().contains("cannot use `become`"), "unexpected error: {err}");
}

#[test]
fn rejects_delegate_without_a_declared_leader() {
    let err = parse_and_validate(
        r#"
            state WorkerState {}

            actor Worker owns WorkerState {
                delegate assist() {
                    require(1 == 1);
                }
            }

            app Test {
                actor Worker;
            }
            "#,
    )
    .expect_err("delegates must name a leader");

    assert!(err.to_string().contains("must declare its leader as the first `consumes` actor"), "unexpected error: {err}");
}

#[test]
fn leader_actors_close_all_leader_input_groups() {
    let source = r#"
            state LeaderState {
                int amount;
            }

            state WorkerState {
                int amount;
            }

            state UnrelatedState {
                int amount;
            }

            actor Leader owns LeaderState {
                entry standalone() emits next: Leader {
                    unrestricted(next.value);
                    become next <- Leader(self.state);
                }

                entry coordinated() consumes {
                    worker: Worker,
                } emits next: Leader {
                    unrestricted(next.value);
                    require(worker.value >= 0);
                    become next <- Leader(self.state);
                }
            }

            actor Worker owns WorkerState {
                delegate assist() consumes {
                    leader: Leader,
                } {
                    require(leader.value >= 0);
                }
            }

            actor Unrelated owns UnrelatedState {
                entry standalone() emits next: Unrelated {
                    unrestricted(next.value);
                    become next <- Unrelated(self.state);
                }
            }

            app Test {
                actor Leader;
                actor Worker;
                actor Unrelated;
            }
        "#;
    let path = PathBuf::from("test.ag");
    let module = crate::compiler::syntax::parser::parse_module(path.clone(), source.to_string()).expect("source parses");
    let program = Program { root: path, modules: vec![module] };
    let model = Model::from_program(&program).expect("model validates");

    let leader_sil = emit_actor(model.actor("Leader").expect("Leader exists"), &model).expect("Leader emits");
    assert!(leader_sil.contains("// :: leader entry (1:N)\n    entry standalone("), "{leader_sil}");
    assert!(leader_sil.contains("// :: leader entry (M:N)\n    entry coordinated("), "{leader_sil}");
    assert!(leader_sil.contains("require(OpCovInputCount(gen__cov_id) == 1);"), "{leader_sil}");
    assert!(leader_sil.contains("require(OpCovInputCount(gen__cov_id) == 2);"), "{leader_sil}");

    let worker_sil = emit_actor(model.actor("Worker").expect("Worker exists"), &model).expect("Worker emits");
    assert!(worker_sil.contains("// :: delegate entry\n    entry assist("), "{worker_sil}");

    let unrelated_sil = emit_actor(model.actor("Unrelated").expect("Unrelated exists"), &model).expect("Unrelated emits");
    assert!(!unrelated_sil.contains("OpCovInputCount"), "{unrelated_sil}");

    let actor_sil = actor_sil_for_model(&model);
    let artifact = emit_artifact(&program, &model, &actor_sil).expect("artifact emits");
    let leader = artifact.argent.actors.iter().find(|actor| actor.name == "Leader").expect("Leader artifact exists");
    assert_eq!(leader.leader_for, vec![EntryRefArtifact { actor: "Worker".to_string(), entry: "assist".to_string() }]);
    let unrelated = artifact.argent.actors.iter().find(|actor| actor.name == "Unrelated").expect("Unrelated artifact exists");
    assert!(unrelated.leader_for.is_empty());
}

#[test]
fn rejects_duplicate_state_declarations() {
    let mut program = test_program();
    let mut duplicate = empty_module("second.ag");
    duplicate.states.push(StateDecl { name: "PlayerState".to_string(), fields: Vec::new(), expansion: None });
    program.modules.push(duplicate);

    let err = Model::from_program(&program).expect_err("duplicate state declaration must be rejected");
    assert_duplicate_top_level_error(&err, "state", "PlayerState");
}

#[test]
fn rejects_duplicate_actor_declarations() {
    let mut program = test_program();
    let mut duplicate = empty_module("second.ag");
    duplicate.actors.push(ActorDecl {
        name: "Player".to_string(),
        state: "PlayerState".to_string(),
        functions: Vec::new(),
        entries: Vec::new(),
    });
    program.modules.push(duplicate);

    let err = Model::from_program(&program).expect_err("duplicate actor declaration must be rejected");
    assert_duplicate_top_level_error(&err, "actor", "Player");
}

#[test]
fn rejects_duplicate_const_declarations() {
    let mut program = test_program();
    program.modules[0].consts.push(ConstDecl { ty: TypeRef::new("int"), name: "Limit".to_string(), value: "1".to_string() });
    let mut duplicate = empty_module("second.ag");
    duplicate.consts.push(ConstDecl { ty: TypeRef::new("int"), name: "Limit".to_string(), value: "2".to_string() });
    program.modules.push(duplicate);

    let err = Model::from_program(&program).expect_err("duplicate const declaration must be rejected");
    assert_duplicate_top_level_error(&err, "const", "Limit");
}

#[test]
fn rejects_duplicate_function_declarations() {
    let mut program = test_program();
    program.modules[0].functions.push(FunctionDecl {
        name: "helper".to_string(),
        params: Vec::new(),
        return_ty: Some(TypeRef::new("int")),
        body: "1".to_string(),
    });
    let mut duplicate = empty_module("second.ag");
    duplicate.functions.push(FunctionDecl {
        name: "helper".to_string(),
        params: Vec::new(),
        return_ty: Some(TypeRef::new("int")),
        body: "2".to_string(),
    });
    program.modules.push(duplicate);

    let err = Model::from_program(&program).expect_err("duplicate function declaration must be rejected");
    assert_duplicate_top_level_error(&err, "fn", "helper");
}

#[test]
fn rejects_function_named_unrestricted() {
    let mut program = test_program();
    program.modules[0].functions.push(FunctionDecl {
        name: word::UNRESTRICTED.to_string(),
        params: Vec::new(),
        return_ty: Some(TypeRef::new("int")),
        body: "0".to_string(),
    });

    let err = Model::from_program(&program).expect_err("the output-value declaration must not be shadowed");
    assert!(
        err.to_string().contains("function identifier `unrestricted` is reserved for output-value declarations"),
        "unexpected error: {err}"
    );
}

#[test]
fn rejects_global_and_actor_functions_with_the_same_name() {
    let mut program = test_program();
    program.modules[0].actors[0].entries.clear();
    program.modules[0].functions.push(FunctionDecl {
        name: "helper".to_string(),
        params: Vec::new(),
        return_ty: Some(TypeRef::new("int")),
        body: "return 1;".to_string(),
    });
    program.modules[0].actors[0].functions.push(FunctionDecl {
        name: "helper".to_string(),
        params: Vec::new(),
        return_ty: Some(TypeRef::new("int")),
        body: "return 2;".to_string(),
    });

    let err = Model::from_program(&program).expect_err("global and actor functions share the generated contract namespace");

    assert_eq!(err.message, "actor `Player` function `helper` conflicts with a global function of the same name");
}

#[test]
fn rejects_global_calls_to_actor_functions() {
    let mut program = test_program();
    program.modules[0].actors[0].entries.clear();
    program.modules[0].functions.push(FunctionDecl {
        name: "global_helper".to_string(),
        params: Vec::new(),
        return_ty: Some(TypeRef::new("int")),
        body: "return actor_helper();".to_string(),
    });
    program.modules[0].actors[1].functions.push(FunctionDecl {
        name: "actor_helper".to_string(),
        params: Vec::new(),
        return_ty: Some(TypeRef::new("int")),
        body: "return 2;".to_string(),
    });
    let model = Model::from_program(&program).expect("function declarations form distinct namespaces");

    let err = emit_actor(model.actor("Player").expect("Player exists"), &model)
        .expect_err("global functions must not depend on actor functions");

    assert_eq!(err.message, "global function `global_helper` cannot call actor function `actor_helper`");
}

#[test]
fn allows_the_same_function_name_on_different_actors() {
    let mut program = test_program();
    for actor in &mut program.modules[0].actors {
        actor.entries.clear();
        actor.functions.push(FunctionDecl {
            name: "helper".to_string(),
            params: Vec::new(),
            return_ty: Some(TypeRef::new("int")),
            body: "return 1;".to_string(),
        });
    }

    Model::from_program(&program).expect("actor-local function names may repeat across contracts");
}

#[test]
fn emits_actor_functions_only_in_their_owning_contract() {
    let path = PathBuf::from("actor-functions.ag");
    let module = crate::compiler::syntax::parser::parse_module(
        path.clone(),
        r#"
            const int BIAS = 1;

            state CounterState {
                int cycles;
            }

            state OtherState {
                int amount;
            }

            fn add_bias(int value) -> int {
                return value + BIAS;
            }

            actor Counter owns CounterState {
                fn current() -> int {
                    return cycles;
                }

                fn adjusted(int delta) -> int {
                    return add_bias(current() + delta);
                }

                fn ensure_nonnegative() {
                    require(current() >= 0);
                }

                entry check() emits none {
                    ensure_nonnegative();
                    require(adjusted(1) == cycles + 2);
                }
            }

            actor Other owns OtherState {
                fn adjusted(int delta) -> int {
                    return amount - delta;
                }

                entry check() emits none {
                    require(adjusted(1) == amount - 1);
                }
            }

            app FunctionsApp {
                actor Counter;
                actor Other;
            }
            "#
        .to_string(),
    )
    .expect("actor-function source parses");
    let program = Program { root: path, modules: vec![module] };
    let out_dir = std::env::temp_dir().join(format!("argent-actor-functions-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&out_dir);

    emit_build(&program, &out_dir).expect("actor functions compile in their owning contracts");
    let counter = fs::read_to_string(out_dir.join("sil/Counter.sil")).expect("generated Counter Sil exists");
    let other = fs::read_to_string(out_dir.join("sil/Other.sil")).expect("generated Other Sil exists");

    assert!(counter.contains("// :: actor functions"), "{counter}");
    assert!(counter.contains("function current() : int"), "{counter}");
    assert!(counter.contains("return cycles;"), "{counter}");
    assert!(counter.contains("function adjusted(int delta) : int"), "{counter}");
    assert!(counter.contains("return add_bias(current() + delta);"), "{counter}");
    assert!(counter.contains("function ensure_nonnegative()"), "{counter}");
    assert!(!other.contains("function current()"), "{other}");
    assert!(other.contains("return amount - delta;"), "{other}");

    let _ = fs::remove_dir_all(out_dir);
}

#[test]
fn global_function_variables_do_not_collide_with_actor_fields() {
    let program = global_function_program(
        r#"
            fn increment(int cycles) -> int {
                int result = cycles + 1;
                return result;
            }
        "#,
        "increment(cycles) > cycles",
    );
    let out_dir = std::env::temp_dir().join(format!("argent-global-function-scope-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&out_dir);

    emit_build(&program, &out_dir).expect("global function with actor-field name collisions builds");
    let sil = fs::read_to_string(out_dir.join("sil/Counter.sil")).expect("generated Counter Sil exists");
    assert!(sil.contains("function increment(int gen__glob_cycles) : int"), "{sil}");
    assert!(sil.contains("int gen__glob_result = gen__glob_cycles + 1;"), "{sil}");
    assert!(sil.contains("return gen__glob_result;"), "{sil}");
    assert!(sil.contains("require(increment(cycles) > cycles);"), "{sil}");

    let _ = fs::remove_dir_all(out_dir);
}

#[test]
fn global_function_bare_names_do_not_capture_actor_fields() {
    let program = global_function_program(
        r#"
            fn read_cycles() -> int {
                return cycles;
            }
        "#,
        "read_cycles() >= 0",
    );
    let model = Model::from_program(&program).expect("program models");
    let counter = model.actor("Counter").expect("Counter exists");
    let err = emit_actor(counter, &model).expect_err("undeclared global-function name must not capture an actor field");
    assert!(
        err.to_string().contains("global function `read_cycles` cannot access unresolved identifier `cycles`"),
        "unexpected error: {err}"
    );
}

#[test]
fn global_function_assignments_do_not_capture_actor_fields() {
    let program = global_function_program(
        r#"
            fn write_cycles() {
                cycles = 1;
            }
        "#,
        "true",
    );
    let model = Model::from_program(&program).expect("program models");
    let counter = model.actor("Counter").expect("Counter exists");
    let err = emit_actor(counter, &model).expect_err("global-function assignment must not capture an actor field");
    assert!(
        err.to_string().contains("global function `write_cycles` assigns unresolved identifier `cycles`"),
        "unexpected error: {err}"
    );
}

#[test]
fn global_function_contextual_names_and_literals_are_lowered_by_sil_syntax() {
    let program = global_function_program(
        r#"
            fn echo(int seconds) -> int {
                byte[2] marker = byte[_](0xaabb);
                int grouped = 1_000;
                require(marker == byte[_](0xaabb));
                return seconds + grouped - 1_000;
            }
        "#,
        "echo(seconds) >= seconds",
    );
    let out_dir = std::env::temp_dir().join(format!("argent-global-function-syntax-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&out_dir);

    emit_build(&program, &out_dir).expect("contextual names and complete Sil literals build");
    let sil = fs::read_to_string(out_dir.join("sil/Counter.sil")).expect("generated Counter Sil exists");
    assert!(sil.contains("function echo(int gen__glob_seconds) : int"), "{sil}");
    assert!(sil.contains("byte[2] gen__glob_marker = byte[_](0xaabb);"), "{sil}");
    assert!(sil.contains("int gen__glob_grouped = 1_000;"), "{sil}");
    assert!(sil.contains("return gen__glob_seconds + gen__glob_grouped - 1_000;"), "{sil}");

    let _ = fs::remove_dir_all(out_dir);
}

fn global_function_program(function: &str, requirement: &str) -> Program {
    let path = PathBuf::from("global-function-scope.ag");
    let source = format!(
        r#"
            state CounterState {{
                int cycles;
                int seconds;
            }}

            {function}

            actor Counter owns CounterState {{
                entry check() emits none {{
                    require({requirement});
                }}
            }}

            app CounterApp {{
                actor Counter;
            }}
        "#
    );
    let module = crate::compiler::syntax::parser::parse_module(path.clone(), source).expect("scope test source parses");
    Program { root: path, modules: vec![module] }
}

#[test]
fn rejects_duplicate_app_declarations() {
    let mut program = test_program();
    let mut duplicate = empty_module("second.ag");
    duplicate.apps.push(AppDecl { name: "Test".to_string(), actors: vec!["Player".to_string()] });
    program.modules.push(duplicate);

    let err = Model::from_program(&program).expect_err("duplicate app declaration must be rejected");
    assert_duplicate_top_level_error(&err, "app", "Test");
}

#[test]
fn rejects_reserved_state_field_from_model() {
    let mut program = test_program();
    program.modules[0].states[0].fields.push(FieldDecl {
        ty: TypeRef::new("int"),
        name: "gen__player_template".to_string(),
        virtual_slot: false,
    });

    let err = Model::from_program(&program).expect_err("reserved state field must be rejected");
    assert!(err.to_string().contains("reserved generated namespace"), "unexpected error: {err}");
}

#[test]
fn rejects_reserved_self_members_on_owned_state_surface() {
    for member in word::RESERVED_SELF_MEMBERS {
        let source = format!(
            r#"
                state WalletState {{
                    int {member};
                }}

                actor Wallet owns WalletState {{}}

                app Test {{
                    actor Wallet;
                }}
                "#
        );
        let err = parse_and_validate(&source).expect_err("reserved self member must be rejected on an owned state");
        assert!(
            err.to_string().contains(&format!("actor `Wallet` owned state `WalletState` exposes field `{member}` as `self.{member}`")),
            "unexpected error for `{member}`: {err}"
        );
    }
}

#[test]
fn allows_reserved_self_member_names_in_nested_state_values() {
    parse_and_validate(
        r#"
            state Payload {
                int value;
                int ref;
            }

            state WalletState {
                Payload payload;
            }

            actor Wallet owns WalletState {}

            app Test {
                actor Wallet;
            }
            "#,
    )
    .expect("reserved self member names are valid below the actor state surface");
}

#[test]
fn rejects_reserved_self_member_in_expanded_base_state() {
    let err = parse_and_validate(
        r#"
            state Capsule {
                virtual state;
            }

            state Memory {
                int counter;
            }

            state Expanded expands Capsule {
                state: Memory;
            }

            actor Worker owns Expanded {}

            app Test {
                actor Worker;
            }
            "#,
    )
    .expect_err("expanded base fields remain on the owned state surface");

    assert!(
        err.to_string().contains("actor `Worker` owned state `Expanded` exposes field `state` as `self.state`"),
        "unexpected error: {err}"
    );
}

#[test]
fn rejects_reserved_entry_parameter_from_model() {
    let mut program = test_program();
    program.modules[0].actors[0].entries[0]
        .params
        .push(ParamDecl { name: "gen__next_output_idx".to_string(), ty: TypeRef::new("int") });

    let err = Model::from_program(&program).expect_err("reserved entry parameter must be rejected");
    assert!(err.to_string().contains("reserved generated namespace"), "unexpected error: {err}");
}

#[test]
fn rejects_reserved_output_handle_from_model() {
    let mut program = test_program();
    program.modules[0].actors[0].entries[0].emits =
        EmitSpec::Outputs(vec![EmitOutput { name: "gen__next".to_string(), actors: vec!["Player".to_string()], auth_index: 0 }]);
    let route = RouteCall { output: "gen__next".to_string(), actor: "Player".to_string(), state: "next_player".to_string() };
    program.modules[0].actors[0].entries[0].routes = vec![route.clone()];
    program.modules[0].actors[0].entries[0].terminal_route_sets = vec![vec![route]];

    let err = Model::from_program(&program).expect_err("reserved output handle must be rejected");
    assert!(err.to_string().contains("reserved generated namespace"), "unexpected error: {err}");
}

#[test]
fn rejects_template_actor_snake_case_collision() {
    let mut program = test_program();
    program.modules[0].actors[0].name = "FooBar".to_string();
    program.modules[0].actors[1].name = "Foo_Bar".to_string();
    program.modules[0].actors[0].entries.clear();
    program.modules[0].apps[0].actors = vec!["FooBar".to_string(), "Foo_Bar".to_string()];

    let err = Model::from_program(&program).expect_err("snake-case generated names must not collide");
    assert!(err.to_string().contains("both map to generated suffix `foo_bar`"), "unexpected error: {err}");
}

#[test]
fn allows_legacy_template_like_user_field_after_namespace_move() {
    let module = crate::compiler::syntax::parser::parse_module(
        PathBuf::from("test.ag"),
        r#"
            state FooState {
                int template_foo;
            }

            actor Foo owns FooState {}

            app Test {
                actor Foo;
            }
            "#
        .to_string(),
    )
    .expect("source parses");
    let program = Program { root: PathBuf::from("test.ag"), modules: vec![module] };

    Model::from_program(&program).expect("ordinary template-like names should be legal");
}

#[test]
fn emits_reserved_generated_namespace_names() {
    let module = crate::compiler::syntax::parser::parse_module(
        PathBuf::from("test.ag"),
        r#"
            state FooState {}

            actor Foo owns FooState {
                entry step() emits next: Foo {
                    require(next.value == self.value);
                    become next <- Foo(self.state);
                }
            }

            app Test {
                actor Foo;
            }
            "#
        .to_string(),
    )
    .expect("source parses");
    let program = Program { root: PathBuf::from("test.ag"), modules: vec![module] };
    let model = Model::from_program(&program).expect("model validates");
    let actor = model.actor("Foo").expect("actor exists");
    let sil = emit_actor(actor, &model).expect("actor emits");
    let manifest = emit_manifest(&program, &model);

    assert!(!sil.contains("byte[32] gen__init_foo_template"), "{sil}");
    assert!(!sil.contains("byte[32] gen__foo_template = gen__init_foo_template;"), "{sil}");
    assert!(sil.contains("int gen__next_output_idx = OpAuthOutputIdx"), "{sil}");
    assert!(sil.contains("tx.outputs[gen__next_output_idx].value"), "{sil}");
    assert!(sil.contains("tx.outputs[gen__next_output_idx].scriptPubKey"), "{sil}");
    assert!(sil.contains("== tx.inputs[this.activeInputIndex].scriptPubKey"), "{sil}");
    assert!(manifest.contains(r#""symbol": "gen__foo_template""#), "{manifest}");
    assert!(!sil.contains("byte[32] init_template_foo"), "{sil}");
    assert!(!sil.contains("int next_output_idx ="), "{sil}");
    assert!(!sil.contains("byte[] foo_prefix"), "{sil}");
    assert!(!sil.contains("byte[] gen__foo_prefix"), "{sil}");
    assert!(!sil.contains("gen__state_foo_state"), "{sil}");
    assert!(!sil.contains("__argent_"), "{sil}");
}

#[test]
fn rejects_emit_output_without_value_policy() {
    let err = emit_inline_error(
        r#"
            state FooState {}

            actor Foo owns FooState {
                entry step() emits next: Foo {
                    become next <- Foo(self.state);
                }
            }

            app Test {
                actor Foo;
            }
            "#,
    );

    let message = err.to_string();
    assert!(message.contains("must reference output value `next.value`"), "unexpected error: {err}");
    assert!(message.contains("if intentionally unrestricted, add `unrestricted(next.value);`"), "unexpected error: {err}");
}

#[test]
fn commented_output_value_does_not_satisfy_the_reference_check() {
    let err = emit_inline_error(
        r#"
            state FooState {}

            actor Foo owns FooState {
                entry step() emits next: Foo {
                    require(1 == 1 /* next.value */);
                    become next <- Foo(self.state);
                }
            }

            app Test {
                actor Foo;
            }
            "#,
    );

    assert!(err.to_string().contains("must reference output value `next.value`"), "unexpected error: {err}");
}

#[test]
fn output_value_reference_anywhere_in_the_entry_satisfies_the_check() {
    let source = r#"
            state FooState {}

            actor Foo owns FooState {
                entry step() emits next: Foo {
                    int output_value = next.value;
                    become next <- Foo(self.state);
                }
            }

            app Test {
                actor Foo;
            }
        "#;
    let path = PathBuf::from("test.ag");
    let module = crate::compiler::syntax::parser::parse_module(path.clone(), source.to_string()).expect("source parses");
    let program = Program { root: path, modules: vec![module] };
    let model = Model::from_program(&program).expect("model validates");
    let sil = emit_actor(model.actor("Foo").expect("actor exists"), &model).expect("actor emits");

    assert!(sil.contains("int output_value = tx.outputs[gen__next_output_idx].value;"), "{sil}");
}

#[test]
fn unrestricted_output_value_policy_is_compile_time_only() {
    let source = r#"
            state FooState {}

            actor Foo owns FooState {
                entry allow() emits next: Foo {
                    unrestricted(next.value);
                    become next <- Foo(self.state);
                }
            }

            app Test {
                actor Foo;
            }
        "#;
    let path = PathBuf::from("test.ag");
    let module = crate::compiler::syntax::parser::parse_module(path.clone(), source.to_string()).expect("source parses");
    let program = Program { root: path, modules: vec![module] };
    let model = Model::from_program(&program).expect("model validates");
    let sil = emit_actor(model.actor("Foo").expect("actor exists"), &model).expect("actor emits");

    assert!(!sil.contains(word::UNRESTRICTED), "{sil}");
}

#[test]
fn unrestricted_output_value_policy_requires_a_current_output_handle() {
    let err = emit_inline_error(
        r#"
            state FooState {}

            actor Foo owns FooState {
                entry step() emits next: Foo {
                    unrestricted(self.value);
                    become next <- Foo(self.state);
                }
            }

            app Test {
                actor Foo;
            }
            "#,
    );

    assert!(
        err.to_string()
            .contains("`unrestricted(...)` expects exactly one current emit or spawn output value; `self.value` is not one"),
        "unexpected error: {err}"
    );
}

#[test]
fn spawn_output_value_can_be_constrained_by_qualified_handle() {
    let source = r#"
            state LauncherState {}
            state ChildState {}

            actor Launcher owns LauncherState {
                entry launch()
                spawns children by children_id {
                    outputs {
                        child: Child,
                    }
                }
                emits next: Launcher {
                    unrestricted(next.value);
                    require(children.outputs.child.value > 0);
                    require children.outputs become {
                        child <- Child(ChildState {}),
                    };
                    become next <- Launcher(self.state);
                }
            }

            actor Child owns ChildState {}

            app Test {
                actor Launcher;
                actor Child;
            }
        "#;
    let path = PathBuf::from("test.ag");
    let module = crate::compiler::syntax::parser::parse_module(path.clone(), source.to_string()).expect("source parses");
    let program = Program { root: path, modules: vec![module] };
    let model = Model::from_program(&program).expect("model validates");
    let sil = emit_actor(model.actor("Launcher").expect("actor exists"), &model).expect("actor emits");

    assert!(sil.contains("require(tx.outputs[gen__children_child_output_idx].value > 0);"), "{sil}");
}

#[test]
fn rejects_spawn_output_without_value_policy() {
    let err = emit_inline_error(
        r#"
            state LauncherState {}
            state ChildState {}

            actor Launcher owns LauncherState {
                entry launch()
                spawns children by children_id {
                    outputs {
                        child: Child,
                    }
                }
                emits next: Launcher {
                    unrestricted(next.value);
                    require children.outputs become {
                        child <- Child(ChildState {}),
                    };
                    become next <- Launcher(self.state);
                }
            }

            actor Child owns ChildState {}

            app Test {
                actor Launcher;
                actor Child;
            }
            "#,
    );

    assert!(err.to_string().contains("must reference output value `children.outputs.child.value`"), "unexpected error: {err}");
}

#[test]
fn qualified_spawn_value_does_not_cover_same_named_emit_value() {
    let err = emit_inline_error(
        r#"
            state LauncherState {}
            state ChildState {}

            actor Launcher owns LauncherState {
                entry launch()
                spawns children by children_id {
                    outputs {
                        child: Child,
                    }
                }
                emits child: Launcher {
                    require(children.outputs.child.value > 0);
                    require children.outputs become {
                        child <- Child(ChildState {}),
                    };
                    become child <- Launcher(self.state);
                }
            }

            actor Child owns ChildState {}

            app Test {
                actor Launcher;
                actor Child;
            }
            "#,
    );

    let message = err.to_string();
    assert!(message.contains("must reference output value `child.value`"), "unexpected error: {err}");
    assert!(!message.contains("`children.outputs.child.value`,"), "unexpected error: {err}");
}

#[test]
fn observed_output_value_is_the_emitter_contracts_responsibility() {
    let source = r#"
            state ObserverState {}
            state AssetState {}

            actor Observer owns ObserverState {
                entry follow(cov_id remote_id)
                observes remote by remote_id {
                    outputs {
                        asset: Asset,
                    }
                }
                emits none {
                    require remote.outputs become {
                        asset <- Asset(AssetState {}),
                    };
                }
            }

            actor Asset owns AssetState {}

            app Test {
                actor Observer;
                actor Asset;
            }
        "#;
    let path = PathBuf::from("test.ag");
    let module = crate::compiler::syntax::parser::parse_module(path.clone(), source.to_string()).expect("source parses");
    let program = Program { root: path, modules: vec![module] };
    let model = Model::from_program(&program).expect("model validates");

    emit_actor(model.actor("Observer").expect("actor exists"), &model).expect("observed output needs no local value policy");
}

#[test]
fn self_cov_id_lowers_to_the_active_input_covenant_id() {
    let source = r#"
            state FooState {}

            actor Foo owns FooState {
                entry step() emits next: Foo {
                    unrestricted(next.value);
                    require(self.cov_id == self.cov_id);
                    become next <- Foo(self.state);
                }
            }

            app Test {
                actor Foo;
            }
        "#;
    let path = PathBuf::from("test.ag");
    let module = crate::compiler::syntax::parser::parse_module(path.clone(), source.to_string()).expect("source parses");
    let program = Program { root: path, modules: vec![module] };
    let model = Model::from_program(&program).expect("model validates");
    let sil = emit_actor(model.actor("Foo").expect("actor exists"), &model).expect("actor emits");

    assert!(sil.contains("require(OpInputCovenantId(this.activeInputIndex) == OpInputCovenantId(this.activeInputIndex));"), "{sil}");
}

#[test]
fn self_member_prefixes_remain_state_field_refs() {
    let source = r#"
            state FooState {
                int value_note;
                int cov_id_note;
            }
            state PeerState {}

            actor Foo owns FooState {
                entry step()
                consumes {
                    foo_self: Peer,
                }
                emits next: Foo {
                    unrestricted(next.value);
                    require(self.value_note == value_note);
                    require(self.cov_id_note == cov_id_note);
                    require(foo_self.value >= 0);
                    become next <- Foo(self.state);
                }
            }

            actor Peer owns PeerState {}

            app Test {
                actor Foo;
                actor Peer;
            }
        "#;
    let path = PathBuf::from("test.ag");
    let module = crate::compiler::syntax::parser::parse_module(path.clone(), source.to_string()).expect("source parses");
    let program = Program { root: path, modules: vec![module] };
    let model = Model::from_program(&program).expect("model validates");
    let sil = emit_actor(model.actor("Foo").expect("actor exists"), &model).expect("actor emits");

    assert!(sil.contains("require(value_note == value_note);"), "{sil}");
    assert!(sil.contains("require(cov_id_note == cov_id_note);"), "{sil}");
    assert!(sil.contains("require(tx.inputs[gen__foo_self_input_idx].value >= 0);"), "{sil}");
}

#[test]
fn self_transition_uses_same_template_shortcut() {
    let module = crate::compiler::syntax::parser::parse_module(
        PathBuf::from("test.ag"),
        r#"
            state FooState {
                int count;
            }

            actor Foo owns FooState {
                entry bump(int amount) emits next: Foo {
                    unrestricted(next.value);
                    FooState next_state = {
                        count: count + amount,
                    };
                    become next <- Foo(next_state);
                }
            }

            app Test {
                actor Foo;
            }
            "#
        .to_string(),
    )
    .expect("source parses");
    let program = Program { root: PathBuf::from("test.ag"), modules: vec![module] };
    let model = Model::from_program(&program).expect("model validates");
    let actor = model.actor("Foo").expect("actor exists");
    let sil = emit_actor(actor, &model).expect("actor emits");
    let actor_sil = actor_sil_for_model(&model);
    let artifact = emit_artifact(&program, &model, &actor_sil).expect("artifact emits");

    assert!(sil.contains("validateOutputState(gen__next_output_idx, next_state);"), "{sil}");
    assert!(!sil.contains("validateOutputStateWithTemplate"), "{sil}");
    assert!(!sil.contains("byte[] gen__foo_prefix"), "{sil}");

    let foo = artifact.argent.actors.iter().find(|actor| actor.name == "Foo").expect("Foo actor is present");
    let bump = foo.entries.iter().find(|entry| entry.name == "bump").expect("bump entry is present");
    assert!(bump.hidden_params.is_empty());
    assert!(bump.witnesses.is_empty());
    assert!(bump.route_plan.witness_recipe_ids.is_empty());

    let sil_foo = artifact.sil_abi.contract("Foo").expect("Foo Sil ABI exists");
    let sil_bump = sil_foo.entry("bump").expect("bump Sil ABI exists");
    assert_eq!(sil_bump.params.iter().map(|param| param.name.as_str()).collect::<Vec<_>>(), ["amount"]);
}

#[test]
fn route_validation_kind_characterizes_current_representation_decisions() {
    let program = test_program();
    let actor = &program.modules[0].actors[0];

    let exact = RouteCall { output: "next".to_string(), actor: actor.name.clone(), state: " self.state ".to_string() };
    let spaced_exact = RouteCall { output: "next".to_string(), actor: actor.name.clone(), state: "self . state".to_string() };
    let changed = RouteCall { output: "next".to_string(), actor: actor.name.clone(), state: "next_state".to_string() };
    let foreign = RouteCall { output: "next".to_string(), actor: "Game".to_string(), state: "next_game".to_string() };

    assert_eq!(route_validation_kind(actor, &exact), RouteValidationKind::ExactScriptPublicKey);
    assert_eq!(route_validation_kind(actor, &spaced_exact), RouteValidationKind::SameTemplate);
    assert_eq!(route_validation_kind(actor, &changed), RouteValidationKind::SameTemplate);
    assert_eq!(route_validation_kind(actor, &foreign), RouteValidationKind::ForeignTemplate);
}

#[test]
fn state_returning_function_initializes_an_authored_local_once() {
    let (actor_sil, _) = inline_actor_sil_and_artifact(
        "state-function-local",
        r#"
            state CounterState {
                int left;
                int right;
            }

            actor Counter owns CounterState {
                fn successor() -> CounterState {
                    return CounterState {
                        left: left + 1,
                        right: right + 1,
                    };
                }

                entry bump() emits next: Counter {
                    CounterState candidate = successor();
                    unrestricted(next.value);
                    become next <- Counter(candidate);
                }
            }

            app Test {
                actor Counter;
            }
        "#,
    );

    let sil = actor_sil.get("Counter").expect("Counter emits");
    assert!(sil.contains("CounterState candidate = successor();"), "{sil}");
    assert!(!sil.contains("successor().left"), "{sil}");
    assert!(!sil.contains("successor().right"), "{sil}");
    assert!(sil.contains("left: candidate.left"), "{sil}");
    assert!(sil.contains("right: candidate.right"), "{sil}");
}

#[test]
fn state_valued_functions_are_characterized_in_aligned_and_augmented_contexts() {
    let fixture = "tests/fixtures/state_layout/function_contexts/app.ag";
    let (aligned_sil, artifact) = emit_selected_fixture(fixture, "Test", "Aligned");
    let (routed_sil, _) = emit_selected_fixture(fixture, "Test", "Routed");

    assert_eq!(aligned_sil, include_str!("../../../../tests/fixtures/state_layout/function_contexts/Aligned.sil"));
    assert_eq!(routed_sil, include_str!("../../../../tests/fixtures/state_layout/function_contexts/Routed.sil"));

    // Migration debt: the aligned contract still uses authored state types
    // instead of one coherent direct-State representation.
    for sil in [&aligned_sil, &routed_sil] {
        assert!(sil.contains("function global_identity(SharedState gen__glob_value) : SharedState"), "{sil}");
        assert!(sil.contains("function global_fixed(SharedState[2] gen__glob_values) : SharedState[2]"), "{sil}");
        assert!(sil.contains("function global_dynamic(SharedState[] gen__glob_values) : SharedState[]"), "{sil}");
        assert!(sil.contains("function actor_identity(SharedState value) : SharedState"), "{sil}");
        assert!(sil.contains("function actor_fixed(SharedState[2] values) : SharedState[2]"), "{sil}");
        assert!(sil.contains("function actor_dynamic(SharedState[] values) : SharedState[]"), "{sil}");
    }

    let advance = routed_sil
        .split_once("entry advance")
        .map(|(_, tail)| tail)
        .and_then(|tail| tail.split_once("// :: leader entry (1:N)\n    entry export").map(|(body, _)| body))
        .expect("advance entry is delimited in the generated SIL");
    assert_eq!(advance.matches("actor_identity(global_identity(value))").count(), 1, "{advance}");
    assert_eq!(advance.matches("actor_fixed(global_fixed(fixed))").count(), 1, "{advance}");
    assert_eq!(advance.matches("actor_dynamic(global_dynamic(dynamic))").count(), 1, "{advance}");
    assert_eq!(advance.matches("actor_identity(scalar)").count(), 1, "{advance}");
    assert!(!advance.contains("actor_identity(scalar).left"), "{advance}");
    assert!(!advance.contains("actor_identity(scalar).right"), "{advance}");
    assert!(advance.contains("SharedState gen__source_next_shared_state = actor_identity(scalar);"), "{advance}");
    assert!(advance.contains("validateOutputState(gen__next_output_idx, gen__state_next_state);"), "{advance}");
    assert!(routed_sil.contains("validateOutputStateWithTemplate("), "{routed_sil}");

    let shared = artifact.argent.states.iter().find(|state| state.name == "SharedState").expect("SharedState is recorded");
    assert_eq!(shared.fields.iter().map(|field| field.name.as_str()).collect::<Vec<_>>(), ["left", "right"]);

    let aligned = artifact.sil_abi.contract("Aligned").expect("Aligned contract exists");
    assert_eq!(aligned.runtime_state.fields.iter().map(|field| field.name.as_str()).collect::<Vec<_>>(), ["left", "right"]);
    assert!(runtime_state_plan(&artifact, "Aligned").is_none());

    let routed = artifact.sil_abi.contract("Routed").expect("Routed contract exists");
    assert_eq!(
        routed.runtime_state.fields.iter().map(|field| field.name.as_str()).collect::<Vec<_>>(),
        ["gen__foreign_template", "left", "right"]
    );
    assert_eq!(
        runtime_state_plan(&artifact, "Routed")
            .expect("Routed physical plan exists")
            .field_roles
            .iter()
            .map(|field| (field.name.as_str(), field.role.clone()))
            .collect::<Vec<_>>(),
        [("gen__foreign_template", RuntimeFieldRoleArtifact::Template { contract: "Foreign".to_string() })]
    );

    let templates = &artifact.argent.template_plan.templates;
    let aligned_template = templates.iter().find(|template| template.actor == "Aligned").expect("Aligned template exists");
    let routed_template = templates.iter().find(|template| template.actor == "Routed").expect("Routed template exists");
    assert_eq!(aligned_template.sil_template_hash, "9bff615cfd8b17ceab4136e3942fcdb7e94532235c2450c41f99c749ab27fbae");
    assert_eq!(routed_template.sil_template_hash, "74c9dee54f99c1d4fae770244ad71f0f119baca44a6422f751fdb2a843967647");
    assert!(aligned_template.actor_type_handle.context_fields.is_empty());
    assert_eq!(routed_template.actor_type_handle.context_fields, ["gen__foreign_template"]);
}

#[test]
fn current_state_array_entry_param_uses_authored_state_type() {
    let (actor_sil, artifact) = inline_actor_sil_and_artifact(
        "current-state-array-entry-param",
        r#"
            state NoteState {
                int nonce;
            }

            actor Note owns NoteState {
                entry inspect(NoteState[2] notes) emits none {
                    require(notes[0].nonce >= 0);
                }
            }

            app Test {
                actor Note;
            }
            "#,
    );

    let sil = actor_sil.get("Note").expect("Note emits");
    assert!(sil.contains("entry inspect(NoteState[2] notes)"), "{sil}");

    let inspect = artifact.sil_abi.contract("Note").expect("Note Sil ABI exists").entry("inspect").expect("inspect entry exists");
    assert_eq!(
        inspect.params[0].ty,
        TypeArtifact::FixedArray { item: Box::new(TypeArtifact::Struct { name: "NoteState".to_string() }), len: 2 }
    );
}

#[test]
fn routed_current_state_array_entry_param_uses_source_state_type() {
    let (actor_sil, artifact) = inline_actor_sil_and_artifact(
        "current-state-array-entry-param",
        r#"
            state NoteState {
                int nonce;
            }

            state ArchiveState {
                int nonce;
            }

            actor Note owns NoteState {
                entry choose(NoteState[2] notes) emits next: Note {
                    unrestricted(next.value);
                    require(notes[0].nonce < notes[1].nonce);
                    become next <- Note(notes[1]);
                }

                entry archive() emits saved: Archive {
                    unrestricted(saved.value);
                    become saved <- Archive(ArchiveState { nonce: nonce });
                }
            }

            actor Archive owns ArchiveState {
                entry hold() emits none {
                    require(nonce >= 0);
                }
            }

            app Test {
                actor Note;
                actor Archive;
            }
            "#,
    );

    let sil = actor_sil.get("Note").expect("Note emits");
    assert!(sil.contains("struct NoteState"), "{sil}");
    assert!(sil.contains("entry choose(NoteState[2] notes)"), "{sil}");
    assert!(sil.contains("gen__archive_template: gen__archive_template"), "{sil}");
    assert!(sil.contains("nonce: notes[1].nonce"), "{sil}");

    let choose = artifact.sil_abi.contract("Note").expect("Note Sil ABI exists").entry("choose").expect("choose entry exists");
    assert_eq!(
        choose.params[0].ty,
        TypeArtifact::FixedArray { item: Box::new(TypeArtifact::Struct { name: "NoteState".to_string() }), len: 2 }
    );
}

#[test]
fn expanded_entry_params_keep_the_authored_nested_layout() {
    let (actor_sil, artifact) = inline_actor_sil_and_artifact(
        "expanded-entry-param-layout",
        r#"
            state Capsule {
                int nonce;
                virtual detail;
            }

            state Details {
                int count;
            }

            state Expanded expands Capsule {
                detail: Details;
            }

            actor Vault owns Expanded {
                entry inspect(Expanded value, Expanded[] values) emits none {
                    require(value.detail.count >= 0);
                    require(values.length >= 0);
                }
            }

            state ReaderState {
                int nonce;
            }

            actor Reader owns ReaderState {
                entry inspect() consumes {
                    vault: Vault,
                } emits next: Reader {
                    require(vault.nonce >= 0);

                    Expanded candidate = {
                        nonce: vault.nonce,
                        detail: Details { count: 0 },
                    };
                    require(candidate.detail.count == 0);

                    unrestricted(next.value);
                    become next <- Reader(self.state);
                }
            }

            app ExpandedArgs {
                actor Vault;
                actor Reader;
            }
            "#,
    );
    let sil = &actor_sil["Vault"];

    assert!(sil.contains("struct Expanded {"), "{sil}");
    assert!(sil.contains("Details detail;"), "{sil}");
    assert!(sil.contains("entry inspect(Expanded value, Expanded[] values"), "{sil}");
    let inspect = artifact.sil_abi.contract("Vault").expect("Vault contract exists").entry("inspect").expect("inspect entry exists");
    assert_eq!(inspect.params[0].ty, TypeArtifact::Struct { name: "Expanded".to_string() });
    assert_eq!(
        inspect.params[1].ty,
        TypeArtifact::DynamicArray { item: Box::new(TypeArtifact::Struct { name: "Expanded".to_string() }) }
    );
    let expanded = artifact.sil_abi.states.iter().find(|state| state.name == "Expanded").expect("Expanded ABI state exists");
    assert_eq!(
        expanded.fields[1].ty,
        TypeArtifact::Struct { name: "Details".to_string() },
        "expanded entry arguments expose their authored nested field"
    );

    let reader_sil = &actor_sil["Reader"];
    assert!(reader_sil.contains("struct Gen__PhysicalExpanded {"), "{reader_sil}");
    assert!(reader_sil.contains("byte[32] detail;"), "{reader_sil}");
    assert!(reader_sil.contains("Gen__PhysicalExpanded vault = readInputStateWithTemplate("), "{reader_sil}");
}

#[test]
fn expanded_input_fields_require_validated_preimages() {
    let path = PathBuf::from("expanded-input-projection.ag");
    let module = crate::compiler::syntax::parser::parse_module(
        path.clone(),
        r#"
            state Capsule { int nonce; virtual detail; }
            state Details { int count; }
            state Expanded expands Capsule { detail: Details; }

            actor Vault owns Expanded {
                entry hold() emits none { require(nonce >= 0); }
            }

            state ReaderState { int nonce; }
            actor Reader owns ReaderState {
                entry inspect() consumes { vault: Vault, } emits next: Reader {
                    require(vault.detail.count >= 0);
                    unrestricted(next.value);
                    become next <- Reader(self.state);
                }
            }

            app Test { actor Vault; actor Reader; }
        "#
        .to_string(),
    )
    .expect("source parses");
    let program = Program { root: path, modules: vec![module] };
    let model = Model::from_program(&program).expect("expanded input plans");

    let err = emit_actor(model.actor("Reader").expect("Reader exists"), &model)
        .expect_err("a stored expansion digest cannot expose its authored payload");
    assert!(err.to_string().contains("expanded input field `detail`"), "unexpected error: {err}");
    assert!(err.to_string().contains("validated preimage"), "unexpected error: {err}");
}

#[test]
fn entry_state_params_keep_authored_types_for_actor_function_calls() {
    let (actor_sil, artifact) = inline_actor_sil_and_artifact(
        "current-state-dynamic-array-entry-param",
        r#"
            state NoteState {
                int nonce;
            }

            actor Note owns NoteState {
                fn read_nonce(NoteState note) -> int {
                    return note.nonce;
                }

                entry inspect(NoteState note) emits none {
                    require(read_nonce(note) >= 0);
                }

                entry inspect_many(NoteState[] notes) emits none {
                    require(notes.length > 0);
                    require(notes[0].nonce >= 0);
                }
            }

            app Test {
                actor Note;
            }
            "#,
    );

    let sil = actor_sil.get("Note").expect("Note emits");
    assert!(sil.contains("struct NoteState {"), "{sil}");
    assert!(sil.contains("function read_nonce(NoteState note) : int"), "{sil}");
    assert!(sil.contains("entry inspect(NoteState note)"), "{sil}");
    assert!(sil.contains("entry inspect_many(NoteState[] notes)"), "{sil}");

    let note = artifact.sil_abi.contract("Note").expect("Note Sil ABI exists");
    assert_eq!(
        note.entry("inspect").expect("inspect entry exists").params[0].ty,
        TypeArtifact::Struct { name: "NoteState".to_string() }
    );
    assert_eq!(
        note.entry("inspect_many").expect("inspect_many entry exists").params[0].ty,
        TypeArtifact::DynamicArray { item: Box::new(TypeArtifact::Struct { name: "NoteState".to_string() }) }
    );
    assert_eq!(artifact.argent.actors.iter().find(|actor| actor.name == "Note").expect("Note actor exists").state, "NoteState");
    assert!(artifact.argent.states.iter().any(|state| state.name == "NoteState"));
}

#[test]
fn terminal_state_does_not_carry_its_own_template() {
    let path = PathBuf::from("terminal-route.ag");
    let module = crate::compiler::syntax::parser::parse_module(
        path.clone(),
        r#"
            state SourceState {
                int count;
            }

            state TerminalState {
                int count;
            }

            actor Source owns SourceState {
                entry finish() emits next: Terminal {
                    unrestricted(next.value);
                    TerminalState next = {
                        count: count + 1,
                    };
                    become next <- Terminal(next);
                }
            }

            actor Terminal owns TerminalState {
                entry step() emits next: Terminal {
                    unrestricted(next.value);
                    TerminalState next = {
                        count: count + 1,
                    };
                    become next <- Terminal(next);
                }
            }

            app Test {
                actor Source;
                actor Terminal;
            }
            "#
        .to_string(),
    )
    .expect("source parses");
    let program = Program { root: path, modules: vec![module] };
    let model = Model::from_program(&program).expect("model validates");
    let terminal = model.actor("Terminal").expect("Terminal actor exists");
    let terminal_sil = emit_actor(terminal, &model).expect("Terminal emits");
    let artifact = emit_artifact(&program, &model, &actor_sil_for_model(&model)).expect("artifact emits");

    assert!(!terminal_sil.contains("byte[32] gen__init_terminal_template"), "{terminal_sil}");
    assert!(!terminal_sil.contains("byte[32] gen__terminal_template ="), "{terminal_sil}");
    assert!(terminal_sil.contains("validateOutputState(gen__next_output_idx, next);"), "{terminal_sil}");
    assert!(runtime_state_plan(&artifact, "Terminal").is_none());
    assert_eq!(
        runtime_state_plan(&artifact, "Source")
            .expect("Source carries the target template")
            .field_roles
            .iter()
            .map(|field| (field.name.as_str(), field.role.clone()))
            .collect::<Vec<_>>(),
        vec![("gen__terminal_template", RuntimeFieldRoleArtifact::Template { contract: "Terminal".to_string() })]
    );
    let source_template =
        artifact.argent.template_plan.templates.iter().find(|template| template.actor == "Source").expect("Source template exists");
    let source_handle = &source_template.actor_type_handle;
    assert_eq!(source_handle.state, "SourceState");
    assert_eq!(source_handle.context_fields, ["gen__terminal_template"]);
    assert_ne!(source_handle.template.hash_hex, source_template.sil_template_hash);
}

#[test]
fn emits_portable_artifact_schema() {
    let module = crate::compiler::syntax::parser::parse_module(
        PathBuf::from("test.ag"),
        r#"
            state FooState {
                byte[32] owner;
                int count;
            }

            actor Foo owns FooState {
                entry step(int amount) emits next: Foo {
                    require(next.value == self.value);
                    become next <- Foo(self.state);
                }
            }

            app Test {
                actor Foo;
            }
            "#
        .to_string(),
    )
    .expect("source parses");
    let program = Program { root: PathBuf::from("test.ag"), modules: vec![module] };
    let model = Model::from_program(&program).expect("model validates");
    let actor_sil = actor_sil_for_model(&model);

    let artifact = emit_artifact(&program, &model, &actor_sil).expect("artifact emits");
    artifact.check_schema_version().expect("schema version is current");
    let json = serde_json::to_string(&artifact).expect("artifact serializes");
    let artifact: crate::artifact::Artifact = serde_json::from_str(&json).expect("artifact deserializes");

    assert_eq!(artifact.schema_version, ARTIFACT_SCHEMA_VERSION);
    assert_eq!(artifact.generator.name, "argentc");
    assert_eq!(artifact.app, "Test");
    assert_eq!(artifact.root, "test.ag");
    assert_eq!(artifact.argent.templates[0].symbol, "gen__foo_template");
    assert_eq!(artifact.argent.templates[0].id, "template/foo");
    assert_eq!(artifact.argent.template_plan.templates[0].id, "template/foo");
    assert_eq!(
        artifact.argent.template_plan.templates[0].sil_template_hash,
        artifact.sil_abi.contract("Foo").unwrap().compiled.template_hash_hex
    );
    let template = &artifact.argent.template_plan.templates[0];
    assert_eq!(template.actor_type_handle.state, "FooState");
    assert!(template.actor_type_handle.context_fields.is_empty());
    assert_eq!(
        template.actor_type_handle.template,
        extract_sil_template(&artifact.sil_abi.contract("Foo").unwrap().compiled).expect("Sil template extracts"),
        "a plain actor still exports its Sil template as its source-state handle"
    );
    artifact.verify_template_plan().expect("template plan receipt verifies");
    assert_eq!(
        artifact
            .argent
            .states
            .iter()
            .map(|state| {
                (state.name.as_str(), state.fields.iter().map(|field| (field.name.as_str(), &field.ty)).collect::<Vec<_>>())
            })
            .collect::<Vec<_>>(),
        artifact
            .sil_abi
            .states
            .iter()
            .map(|state| {
                (state.name.as_str(), state.fields.iter().map(|field| (field.name.as_str(), &field.ty)).collect::<Vec<_>>())
            })
            .collect::<Vec<_>>(),
        "Argent state fields retain the lowered Sil ABI layout"
    );

    let state = artifact.argent.states.iter().find(|state| state.name == "FooState").expect("source state is present");
    assert_eq!(
        state.fields.iter().map(|field| field.name.as_str()).collect::<Vec<_>>(),
        ["owner", "count"],
        "source state field order must stay stable"
    );
    assert_eq!(state.fields[0].ty, TypeArtifact::FixedBytes { len: 32 });
    assert_eq!(state.fields[1].ty, TypeArtifact::Int);

    let actor = artifact.argent.actors.iter().find(|actor| actor.name == "Foo").expect("actor is present");
    assert_eq!(actor.abi.actor, "Foo");
    let sil_contract = artifact.sil_abi.contract(&actor.abi.actor).expect("outer actor should point at Sil ABI contract");
    assert_eq!(sil_contract.source_path, "sil/Foo.sil");
    assert_compiled_projection(sil_contract.name.as_str(), &sil_contract.compiled);
    assert_eq!(
        sil_contract.runtime_state.fields.iter().map(|field| field.name.as_str()).collect::<Vec<_>>(),
        ["owner", "count"],
        "runtime state field order must match generated Silverscript state order"
    );
    assert!(runtime_state_plan(&artifact, "Foo").is_none(), "pure source runtime state should not need an Argent field-role overlay");

    let entry = actor.entries.iter().find(|entry| entry.name == "step").expect("entry is present");
    assert_eq!(entry.kind, EntryKindArtifact::Leader);
    assert_eq!(entry.abi.actor, "Foo");
    assert_eq!(entry.abi.entry, "step");
    assert!(entry.hidden_params.is_empty(), "exact same-state continuation should not expose template witnesses");
    assert!(entry.witnesses.is_empty(), "exact same-state continuation should not expose route witnesses");
    assert!(matches!(&entry.emits, EmitArtifact::Outputs { outputs } if outputs.len() == 1 && outputs[0].name == "next"));
    assert_eq!(entry.routes[0].output, "next");
    assert_eq!(entry.routes[0].actor, "Foo");
    assert_eq!(entry.routes[0].state_expr, "self.state");
    assert_eq!(entry.route_plan.active_input.as_ref().map(|input| (input.actor.as_str(), input.cov_index)), Some(("Foo", Some(0))));
    assert_eq!(entry.route_plan.outputs[0].auth_index, 0);
    assert_eq!(entry.route_plan.outputs[0].name, "next");

    let sil_entry = sil_contract.entry(&entry.abi.entry).expect("outer entry should point at Sil ABI entry");
    assert_eq!(sil_entry.params.len(), 1);
    assert_eq!(sil_entry.params[0].name, "amount");
    assert_eq!(sil_entry.params[0].ty, TypeArtifact::Int);
    assert_eq!(
        entry
            .witnesses
            .iter()
            .map(|witness| (witness.param.clone(), subject_label(&witness.subject).to_string(), witness.purpose))
            .collect::<Vec<_>>(),
        entry
            .hidden_params
            .iter()
            .map(|param| (param.name.clone(), subject_label(&param.subject).to_string(), param.purpose))
            .collect::<Vec<_>>(),
        "outer witness recipes must correspond to outer hidden ABI params"
    );
}

#[test]
fn argent_states_preserve_lossy_source_types() {
    let artifact = inline_artifact(
        "source-state-types",
        r#"
            state PayloadState {
                int value;
            }

            state HolderState {
                cov_id controller;
                actor_type<PayloadState> payload_type;
                byte[32] raw;
            }

            actor Holder owns HolderState {
                entry hold() emits next: Holder {
                    unrestricted(next.value);
                    become next <- Holder(self.state);
                }
            }

            app Test {
                actor Holder;
            }
            "#,
    );

    let state = artifact.argent.states.iter().find(|state| state.name == "HolderState").expect("HolderState exists");
    assert_eq!(state.fields[0].ty, TypeArtifact::FixedBytes { len: 32 });
    assert_eq!(
        state.fields[0].source_type,
        Some(SourceTypeArtifact { name: word::COVENANT_ID.to_string(), array: None, actor_state: None })
    );
    assert_eq!(state.fields[1].ty, TypeArtifact::FixedBytes { len: 32 });
    assert_eq!(
        state.fields[1].source_type,
        Some(SourceTypeArtifact { name: word::ACTOR_TYPE.to_string(), array: None, actor_state: Some("PayloadState".to_string()) })
    );
    assert_eq!(state.fields[2].ty, TypeArtifact::FixedBytes { len: 32 });
    assert_eq!(state.fields[2].source_type, None);
}

#[test]
fn state_expansion_uses_base_storage_layout() {
    let (sil, artifact) = emit_fixture("state_expansion", "Forager");

    assert_eq!(sil, include_str!("../../../../tests/fixtures/emit/state_expansion/Forager.sil"));
    assert!(sil.contains("ForagerStrategy strategy;"), "{sil}");
    assert!(sil.contains("byte[32] strategy;"), "{sil}");
    assert!(sil.contains("validateOutputState(gen__next_output_idx, next_state);"), "{sil}");
    assert!(!sil.contains("validateOutputStateWithTemplate"), "{sil}");

    let expansion = artifact.argent.state_expansions.first().expect("state expansion is recorded");
    assert_eq!(expansion.state, "ForagerState");
    assert_eq!(expansion.base, "AgentCapsule");
    assert_eq!(expansion.digests.len(), 1);
    assert_eq!(expansion.digests[0].field, "strategy");
    assert_eq!(expansion.digests[0].state, "ForagerStrategy");

    let forager_state = artifact.argent.states.iter().find(|state| state.name == "ForagerState").expect("ForagerState exists");
    assert_eq!(forager_state.fields.iter().map(|field| field.name.as_str()).collect::<Vec<_>>(), ["strategy", "energy"]);
    assert!(forager_state.fields[0].virtual_slot);
    assert!(!forager_state.fields[1].virtual_slot);

    let contract = artifact.sil_abi.contract("Forager").expect("Forager Sil ABI exists");
    assert_eq!(contract.runtime_state.source, "ForagerState");
    assert_eq!(contract.runtime_state.fields.iter().map(|field| field.name.as_str()).collect::<Vec<_>>(), ["strategy", "energy"]);
    assert_eq!(contract.compiled.template_hash_hex, "621070c8650ccdf05b31451f1811bc9537ca5a6f09016bd173bc05224af5ecd3");
    assert!(runtime_state_plan(&artifact, "Forager").is_none());
    let template = artifact
        .argent
        .template_plan
        .templates
        .iter()
        .find(|template| template.actor == "Forager")
        .expect("Forager template receipt exists");
    assert!(template.actor_type_handle.context_fields.is_empty());
    assert_eq!(template.sil_template_hash, contract.compiled.template_hash_hex);
    let hold = contract.entry("hold").expect("hold ABI exists");
    assert_eq!(hold.params.iter().map(|param| param.name.as_str()).collect::<Vec<_>>(), ["gen__strategy_forager_strategy_preimage"]);
    assert_eq!(hold.params[0].ty, TypeArtifact::FixedBytes { len: 8 });

    let actor = artifact.argent.actors.iter().find(|actor| actor.name == "Forager").expect("Forager actor is present");
    let hold = actor.entries.iter().find(|entry| entry.name == "hold").expect("hold entry is present");
    assert_eq!(hold.hidden_params.len(), 1);
    assert_eq!(hold.hidden_params[0].name, "gen__strategy_forager_strategy_preimage");
    assert_eq!(hold.hidden_params[0].ty, TypeArtifact::FixedBytes { len: 8 });
    assert_eq!(hold.hidden_params[0].purpose, HiddenParamPurposeArtifact::StateExpansionPreimage);
}

#[test]
fn expanded_actor_records_sil_and_capsule_template_cuts() {
    let (sil, artifact) = emit_fixture("capsule_route_context", "ReserveAsset");
    let (wallet_sil, _) = emit_fixture("capsule_route_context", "WalletAsset");

    assert_eq!(sil, include_str!("../../../../tests/fixtures/emit/capsule_route_context/ReserveAsset.sil"));
    assert_eq!(wallet_sil, include_str!("../../../../tests/fixtures/emit/capsule_route_context/WalletAsset.sil"));
    assert!(sil.contains("byte[32] gen__wallet_asset_template"), "{sil}");
    assert!(sil.contains("validateOutputState(gen__next_output_idx, next_asset);"), "{sil}");
    assert!(sil.contains("validateOutputStateWithTemplate("), "{sil}");

    // Migration debt: exact continuation is still recognized from the
    // textual `CurrentActor(self.state)` route shape.
    assert!(wallet_sil.contains("tx.outputs[gen__next_output_idx].scriptPubKey"), "{wallet_sil}");
    assert!(wallet_sil.contains("== tx.inputs[this.activeInputIndex].scriptPubKey"), "{wallet_sil}");
    assert!(!wallet_sil.contains("validateOutputState"), "{wallet_sil}");

    let contract = artifact.sil_abi.contract("ReserveAsset").expect("ReserveAsset Sil ABI exists");
    assert_eq!(
        contract.runtime_state.fields.iter().map(|field| field.name.as_str()).collect::<Vec<_>>(),
        ["gen__reserve_asset_template", "gen__wallet_asset_template", "owner_kind", "owner_id", "policy", "balance"]
    );
    assert_eq!(contract.compiled.template_hash_hex, "461bc42e59fb2a8c5079458bd5d09ce4b4c3e654f2540fdfa500851627ebf294");
    let wallet_contract = artifact.sil_abi.contract("WalletAsset").expect("WalletAsset Sil ABI exists");
    assert_eq!(
        wallet_contract.runtime_state.fields.iter().map(|field| field.name.as_str()).collect::<Vec<_>>(),
        ["gen__reserve_asset_template", "gen__wallet_asset_template", "owner_kind", "owner_id", "policy", "balance"]
    );
    assert_eq!(wallet_contract.compiled.template_hash_hex, "7fcc79baaa34f0dce572b2d915ddd4697b487baab7f53d1e930d6aac0d82fedc");

    let source_state =
        artifact.argent.states.iter().find(|state| state.name == "ReserveAssetState").expect("ReserveAssetState exists");
    assert_eq!(
        source_state.fields.iter().map(|field| field.name.as_str()).collect::<Vec<_>>(),
        ["owner_kind", "owner_id", "policy", "balance"]
    );
    assert!(source_state.fields[2].virtual_slot);

    let wallet_actor = artifact.argent.actors.iter().find(|actor| actor.name == "WalletAsset").expect("WalletAsset actor exists");
    let hold = wallet_actor.entries.iter().find(|entry| entry.name == "hold").expect("WalletAsset hold entry exists");
    assert_eq!(hold.routes[0].state_expr, "self.state");

    let runtime_plan = runtime_state_plan(&artifact, "ReserveAsset").expect("route context is recorded");
    assert_eq!(
        runtime_plan.field_roles.iter().map(|field| (field.name.as_str(), field.role.clone())).collect::<Vec<_>>(),
        [
            ("gen__reserve_asset_template", RuntimeFieldRoleArtifact::Template { contract: "ReserveAsset".to_string() },),
            ("gen__wallet_asset_template", RuntimeFieldRoleArtifact::Template { contract: "WalletAsset".to_string() },),
        ]
    );
    assert_eq!(
        runtime_state_plan(&artifact, "WalletAsset")
            .expect("WalletAsset route context is recorded")
            .field_roles
            .iter()
            .map(|field| (field.name.as_str(), field.role.clone()))
            .collect::<Vec<_>>(),
        [
            ("gen__reserve_asset_template", RuntimeFieldRoleArtifact::Template { contract: "ReserveAsset".to_string() },),
            ("gen__wallet_asset_template", RuntimeFieldRoleArtifact::Template { contract: "WalletAsset".to_string() },),
        ]
    );
    let receipt = artifact
        .argent
        .template_plan
        .templates
        .iter()
        .find(|template| template.actor == "ReserveAsset")
        .expect("ReserveAsset template receipt exists");
    assert_eq!(receipt.sil_template_hash, contract.compiled.template_hash_hex);
    let handle = &receipt.actor_type_handle;
    assert_eq!(handle.state, "AssetCapsule");
    assert_eq!(handle.context_fields, runtime_plan.field_roles.iter().map(|field| field.name.clone()).collect::<Vec<_>>());
    assert_ne!(handle.template.hash_hex, receipt.sil_template_hash);

    let sil_template = extract_sil_template(&contract.compiled).expect("Sil template extracts");
    let sil_prefix = crate::codec::decode_hex(&sil_template.prefix_hex).expect("Sil prefix decodes");
    let capsule_prefix = crate::codec::decode_hex(&handle.template.prefix_hex).expect("capsule prefix decodes");
    assert!(capsule_prefix.starts_with(&sil_prefix));
    assert!(capsule_prefix.len() > sil_prefix.len());
    assert_eq!(handle.template.suffix_hex, sil_template.suffix_hex);
    artifact.verify_template_plan().expect("capsule template receipt verifies");

    let mut corrupted = artifact.clone();
    let receipt = corrupted
        .argent
        .template_plan
        .templates
        .iter_mut()
        .find(|template| template.actor == "ReserveAsset")
        .expect("ReserveAsset template receipt exists");
    let handle = &mut receipt.actor_type_handle;
    let mut prefix = crate::codec::decode_hex(&handle.template.prefix_hex).expect("capsule prefix decodes");
    *prefix.last_mut().expect("capsule prefix contains context") ^= 1;
    handle.template.prefix_hex = encode_hex(&prefix);
    let suffix = crate::codec::decode_hex(&handle.template.suffix_hex).expect("capsule suffix decodes");
    handle.template.hash_hex = encode_hex(&silverscript_lang::template::template_hash(&prefix, &suffix));
    let err = corrupted.verify_template_plan().expect_err("corrupted capsule context is rejected");
    assert!(matches!(err, TemplatePlanError::ActorTypeHandleMismatch { .. }), "unexpected error: {err}");

    let mut corrupted = artifact.clone();
    let handle = corrupted
        .argent
        .template_plan
        .templates
        .iter_mut()
        .find(|template| template.actor == "ReserveAsset")
        .map(|template| &mut template.actor_type_handle)
        .expect("ReserveAsset capsule handle exists");
    let prefix = crate::codec::decode_hex(&handle.template.prefix_hex).expect("capsule prefix decodes");
    let context = &prefix[sil_prefix.len()..];
    let first_push_end = 1 + context[0] as usize;
    assert!(first_push_end <= context.len(), "test context starts with one direct data push");
    let mut noncanonical_prefix = prefix[..sil_prefix.len()].to_vec();
    noncanonical_prefix.extend_from_slice(&[OpPushData1, context[0]]);
    noncanonical_prefix.extend_from_slice(&context[1..first_push_end]);
    noncanonical_prefix.extend_from_slice(&context[first_push_end..]);
    let suffix = crate::codec::decode_hex(&handle.template.suffix_hex).expect("capsule suffix decodes");
    handle.template.prefix_hex = encode_hex(&noncanonical_prefix);
    handle.template.hash_hex = encode_hex(&silverscript_lang::template::template_hash(&noncanonical_prefix, &suffix));
    let err = corrupted.verify_template_plan().expect_err("non-canonical capsule context is rejected");
    assert!(matches!(err, TemplatePlanError::ActorTypeHandleMismatch { .. }), "unexpected error: {err}");

    let mut corrupted = artifact.clone();
    let capsule =
        corrupted.sil_abi.states.iter_mut().find(|state| state.name == "AssetCapsule").expect("AssetCapsule Sil layout exists");
    capsule.fields.last_mut().expect("AssetCapsule has fields").ty = TypeArtifact::Bool;
    let err = corrupted.verify_template_plan().expect_err("capsule state layout mismatch is rejected");
    assert!(matches!(err, TemplatePlanError::ActorTypeHandleMismatch { .. }), "unexpected error: {err}");

    let mut corrupted = artifact.clone();
    let handle = corrupted
        .argent
        .template_plan
        .templates
        .iter_mut()
        .find(|template| template.actor == "ReserveAsset")
        .map(|template| &mut template.actor_type_handle)
        .expect("ReserveAsset capsule handle exists");
    handle.template.hash_hex = "00".repeat(32);
    let err = corrupted.verify_template_plan().expect_err("corrupted capsule hash is rejected");
    assert!(matches!(err, TemplatePlanError::ActorTypeHandleMismatch { .. }), "unexpected error: {err}");
}

#[test]
fn state_expansion_requires_virtual_byte32_backing_field() {
    let err = parse_and_validate(
        r#"
            state AgentCapsule {
                byte[32] strategy;
            }

            state ForagerStrategy {
                int hunger;
            }

            state ForagerState expands AgentCapsule {
                strategy: ForagerStrategy;
            }

            actor Forager owns ForagerState {}

            app Test {
                actor Forager;
            }
            "#,
    )
    .expect_err("non-digest backing field must be rejected");

    assert!(err.to_string().contains("expanded slots must be virtual"), "unexpected error: {err}");
}

#[test]
fn state_expansion_slots_require_typed_payload_constructors() {
    let module = crate::compiler::syntax::parser::parse_module(
        PathBuf::from("test.ag"),
        r#"
            state AgentCapsule {
                virtual strategy;
            }

            state ForagerStrategy {
                int hunger;
            }

            state ForagerState expands AgentCapsule {
                strategy: ForagerStrategy;
            }

            actor Forager owns ForagerState {
                entry step() emits next: Forager {
                    unrestricted(next.value);
                    ForagerState next_state = {
                        strategy: {
                            hunger: strategy.hunger + 1,
                        },
                    };

                    become next <- Forager(next_state);
                }
            }

            app Test {
                actor Forager;
            }
            "#
        .to_string(),
    )
    .expect("source parses");
    let program = Program { root: PathBuf::from("test.ag"), modules: vec![module] };
    let model = Model::from_program(&program).expect("model validates");
    let actor = model.actor("Forager").expect("Forager actor exists");
    let err = emit_actor(actor, &model).expect_err("untyped virtual slot payload must be rejected");

    assert!(err.to_string().contains("must use `ForagerStrategy { ... }`"), "unexpected error: {err}");
}

#[test]
fn builds_examples_with_compiled_artifacts() {
    assert_example_build_artifact(
        "examples/tickets.ag",
        "tickets",
        &[
            ("Issuer", "04e42d0f9f69e8c344142685c9e1512ed03e0e3f317c5f2649b0da3f61b06a13"),
            ("Ticket", "0480dbcb791c1d53b7f4668bf684c14be77fd7cf4ee02e49c9f9f2528e2fbd3e"),
        ],
    );
    assert_example_build_artifact("examples/stones/app.ag", "stones", &[]);
    assert_example_build_artifact("examples/icc/kcc20_asset.ag", "icc-kcc20-asset", &[]);
    assert_example_build_artifact("examples/icc/minter.ag", "icc-minter", &[]);
}

#[test]
fn observes_blocks_are_recorded_in_artifact() {
    let artifact = inline_artifact(
        "icc-observes",
        r#"
            state KCC20State {
                int amount;
            }

            state MinterProxyState {
                byte[32] controller_id;
            }

            state MinterState {
                cov_id kcc20_covid;
                int amount;
            }

            actor KCC20 owns KCC20State {
                entry hold() emits none {
                    require(amount >= 0);
                }
            }

            actor MinterProxy owns MinterProxyState {
                entry hold() emits none {
                    require(controller_id == controller_id);
                }
            }

            actor Minter owns MinterState {
                entry mint(int minted_amount)
                observes asset by self.kcc20_covid {
                    inputs {
                        proxy: MinterProxy,
                    }

                    outputs {
                        proxy: MinterProxy,
                        recipient: KCC20,
                    }
                }
                emits {
                    controller: Minter,
                } {
                    unrestricted(controller.value);
                    MinterState next_minter = {
                        kcc20_covid: kcc20_covid,
                        amount: amount - minted_amount,
                    };

                    become controller <- Minter(next_minter);
                }
            }

            app Test {
                actor KCC20;
                actor Minter;
                actor MinterProxy;
            }
            "#,
    );

    let minter = artifact.argent.actors.iter().find(|actor| actor.name == "Minter").expect("Minter actor exists");
    let mint = minter.entries.iter().find(|entry| entry.name == "mint").expect("mint entry exists");

    assert_eq!(mint.observes.len(), 1);
    let observe = &mint.observes[0];
    assert_eq!(observe.name, "asset");
    assert_eq!(observe.covenant_expr, "self.kcc20_covid");
    assert_eq!(observe.covenant_id_source, CovenantIdSourceArtifact::StateField { field: "kcc20_covid".to_string() });
    assert_eq!(observe.inputs[0].name, "proxy");
    assert!(matches!(
        &observe.inputs[0].target,
        ObservedTargetArtifact::StaticActor { app, actor } if app == "Test" && actor == "MinterProxy"
    ));
    assert_eq!(observe.outputs[0].name, "proxy");
    assert!(matches!(
        &observe.outputs[0].target,
        ObservedTargetArtifact::StaticActor { app, actor } if app == "Test" && actor == "MinterProxy"
    ));
    assert_eq!(observe.outputs[1].name, "recipient");
    assert!(matches!(
        &observe.outputs[1].target,
        ObservedTargetArtifact::StaticActor { app, actor } if app == "Test" && actor == "KCC20"
    ));
    assert_eq!(
        mint.hidden_params.iter().map(|param| (param.name.as_str(), &param.subject, param.purpose)).collect::<Vec<_>>(),
        vec![
            (
                "gen__kcc20_prefix",
                &HiddenParamSubjectArtifact::Actor { actor: "KCC20".to_string() },
                HiddenParamPurposeArtifact::TemplatePrefixBytes,
            ),
            (
                "gen__kcc20_suffix",
                &HiddenParamSubjectArtifact::Actor { actor: "KCC20".to_string() },
                HiddenParamPurposeArtifact::TemplateSuffixBytes,
            ),
            (
                "gen__minter_proxy_prefix_len",
                &HiddenParamSubjectArtifact::Actor { actor: "MinterProxy".to_string() },
                HiddenParamPurposeArtifact::TemplatePrefixLen,
            ),
            (
                "gen__minter_proxy_suffix_len",
                &HiddenParamSubjectArtifact::Actor { actor: "MinterProxy".to_string() },
                HiddenParamPurposeArtifact::TemplateSuffixLen,
            ),
        ]
    );
    assert_eq!(
        mint.route_plan.witness_recipe_ids.iter().map(String::as_str).collect::<Vec<_>>(),
        vec![
            "witness/kcc20/template_prefix_bytes",
            "witness/kcc20/template_suffix_bytes",
            "witness/minter_proxy/template_prefix_len",
            "witness/minter_proxy/template_suffix_len",
        ]
    );
    assert_eq!(
        runtime_state_plan(&artifact, "Minter")
            .expect("Minter runtime role overlay exists")
            .field_roles
            .iter()
            .map(|role| (role.name.as_str(), role.role.clone()))
            .collect::<Vec<_>>(),
        vec![
            ("gen__kcc20_template", RuntimeFieldRoleArtifact::Template { contract: "KCC20".to_string() },),
            ("gen__minter_proxy_template", RuntimeFieldRoleArtifact::Template { contract: "MinterProxy".to_string() },),
        ]
    );
}

#[test]
fn observe_entry_argument_source_is_recorded_by_index() {
    let artifact = inline_artifact(
        "observe-entry-argument",
        r#"
            state ForeignState {
                int count;
            }
            state LocalState {}

            actor Foreign owns ForeignState {
                entry hold() emits none {
                    require(1 == 1);
                }
            }

            actor Local owns LocalState {
                entry step(int unused, cov_id target_id)
                observes asset by target_id {
                    inputs {
                        foreign: Foreign,
                    }
                }
                emits none {
                    require(unused >= 0);
                }
            }

            app Test {
                actor Foreign;
                actor Local;
            }
            "#,
    );

    let local = artifact.argent.actors.iter().find(|actor| actor.name == "Local").expect("Local actor exists");
    let step = local.entries.iter().find(|entry| entry.name == "step").expect("step entry exists");
    assert_eq!(step.observes[0].covenant_id_source, CovenantIdSourceArtifact::EntryArgument { index: 1 });
}

#[test]
fn observe_covenant_id_source_rejects_computed_expressions() {
    let err = parse_and_validate(
        r#"
            state LocalState {}

            actor Local owns LocalState {
                entry step(cov_id first, cov_id second)
                observes asset by first + second {}
                emits none {
                    require(1 == 1);
                }
            }

            app Test {
                actor Local;
            }
            "#,
    )
    .expect_err("computed observe covenant ids must be rejected");

    assert!(
        err.to_string().contains("covenant id source must be a `self.<field>` state field or entry argument of type `cov_id`"),
        "unexpected error: {err}"
    );
}

#[test]
fn observe_covenant_id_source_requires_cov_id_type() {
    let err = parse_and_validate(
        r#"
            state LocalState {
                byte[32] target_id;
            }

            actor Local owns LocalState {
                entry step()
                observes asset by self.target_id {}
                emits none {
                    require(1 == 1);
                }
            }

            app Test {
                actor Local;
            }
            "#,
    )
    .expect_err("byte arrays must not stand in for covenant ids");

    assert!(err.to_string().contains("has type `byte[32]`; expected `cov_id`"), "unexpected error: {err}");
}

#[test]
fn observe_covenant_id_state_fields_require_self() {
    let err = parse_and_validate(
        r#"
            state LocalState {
                cov_id target_id;
            }

            actor Local owns LocalState {
                entry step()
                observes asset by target_id {}
                emits none {
                    require(1 == 1);
                }
            }

            app Test {
                actor Local;
            }
            "#,
    )
    .expect_err("bare observe covenant state fields must be rejected");

    assert!(err.to_string().contains("state field `target_id` must be referenced as `self.target_id`"), "unexpected error: {err}");
}

#[test]
fn observed_actor_type_state_fields_require_self() {
    let err = parse_and_validate(
        r#"
            state ForeignState {}
            state LocalState {
                cov_id target_id;
                actor_type<ForeignState> foreign_type;
            }

            actor Local owns LocalState {
                entry step()
                observes asset by self.target_id {
                    inputs {
                        foreign: foreign_type,
                    }
                }
                emits none {
                    require(1 == 1);
                }
            }

            app Test {
                actor Local;
            }
            "#,
    )
    .expect_err("bare observed actor-type state fields must be rejected");

    assert!(
        err.to_string().contains("state field `foreign_type` must be referenced as `self.foreign_type`"),
        "unexpected error: {err}"
    );
}

#[test]
fn spawned_actor_type_state_fields_require_self() {
    let err = parse_and_validate(
        r#"
            state PairState {}
            state LauncherState {
                actor_type<PairState> pair_type;
            }

            actor Launcher owns LauncherState {
                entry launch()
                spawns pair by pair_id {
                    outputs {
                        next_pair: pair_type,
                    }
                }
                emits none {
                    unrestricted(pair.outputs.next_pair.value);
                    require(1 == 1);
                }
            }

            app Test {
                actor Launcher;
            }
            "#,
    )
    .expect_err("bare spawned actor-type state fields must be rejected");

    assert!(err.to_string().contains("state field `pair_type` must be referenced as `self.pair_type`"), "unexpected error: {err}");
}

#[test]
fn observed_actor_type_sources_have_distinct_witness_names() {
    let artifact = inline_artifact(
        "observed-actor-type-sources",
        r#"
            state RemoteState {
                int value;
            }
            state LocalState {
                cov_id remote_id;
                actor_type<RemoteState> target;
            }

            actor Local owns LocalState {
                entry inspect(actor_type<RemoteState> self_target)
                observes remote by self.remote_id {
                    inputs {
                        stored: self.target,
                        argument: self_target,
                    }
                }
                emits none {
                    require(1 == 1);
                }
            }

            app Test {
                actor Local;
            }
            "#,
    );

    let inspect = artifact.argent.actors[0].entries.iter().find(|entry| entry.name == "inspect").expect("inspect entry exists");
    assert_eq!(
        inspect.hidden_params.iter().map(|param| param.name.as_str()).collect::<Vec<_>>(),
        vec![
            "gen__actor_type_self_target_prefix_len",
            "gen__actor_type_self_target_suffix_len",
            "gen__actor_type_arg_self_target_prefix_len",
            "gen__actor_type_arg_self_target_suffix_len",
        ]
    );
}

#[test]
fn observed_slots_lower_to_foreign_state_checks() {
    let out_dir = std::env::temp_dir().join(format!("argent-icc-observed-input-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&out_dir);

    crate::build_file("examples/icc/minter.ag", &out_dir).expect("ICC example builds");

    let minter_sil = fs::read_to_string(out_dir.join("sil/Minter.sil")).expect("Minter.sil exists");
    assert!(minter_sil.contains("byte[32] constant gen__kcc20_asset__kcc20_template_const = byte[32](0x"), "{minter_sil}");
    assert!(minter_sil.contains("entry mint(\n"), "{minter_sil}");
    assert!(minter_sil.contains("sig owner_sig,"), "{minter_sil}");
    assert!(minter_sil.contains("byte[32] recipient_owner,"), "{minter_sil}");
    assert!(minter_sil.contains("int gen__kcc20_asset__minter_proxy_prefix_len,"), "{minter_sil}");
    assert!(minter_sil.contains("byte[] gen__kcc20_asset__kcc20_suffix"), "{minter_sil}");
    assert!(
        minter_sil.contains("byte[32] gen__kcc20_asset__minter_proxy_template = gen__kcc20_asset__minter_proxy_template_const;"),
        "{minter_sil}"
    );
    assert!(!minter_sil.contains("gen__init_kcc20_asset__"), "{minter_sil}");
    assert!(minter_sil.contains("struct MinterProxyState"), "{minter_sil}");
    assert!(minter_sil.contains("struct KCC20State"), "{minter_sil}");
    assert!(minter_sil.contains("byte[32] gen__asset_cov_id = kcc20_covid; // observe asset"), "{minter_sil}");
    assert!(minter_sil.contains("require(OpCovInputCount(gen__asset_cov_id) == 1);"), "{minter_sil}");
    assert!(minter_sil.contains("require(OpCovOutputCount(gen__asset_cov_id) == 2);"), "{minter_sil}");
    assert!(!minter_sil.contains("gen__kcc20_asset__minter_proxy_prefix.length"), "{minter_sil}");
    assert!(!minter_sil.contains("gen__kcc20_asset__minter_proxy_suffix.length"), "{minter_sil}");
    assert!(minter_sil.contains("MinterProxyState gen__asset_proxy_state = readInputStateWithTemplate("), "{minter_sil}");
    assert!(minter_sil.contains("gen__asset_proxy_input_idx,"), "{minter_sil}");
    assert!(minter_sil.contains("gen__kcc20_asset__minter_proxy_template"), "{minter_sil}");
    assert!(minter_sil.contains("// :: observed output asset.proxy: KCC20Asset::MinterProxy"), "{minter_sil}");
    assert!(minter_sil.contains("int gen__asset_proxy_output_idx = OpCovOutputIdx(gen__asset_cov_id, 0);"), "{minter_sil}");
    assert!(minter_sil.contains("// :: observed output asset.recipient: KCC20Asset::KCC20"), "{minter_sil}");
    assert!(minter_sil.contains("int gen__asset_recipient_output_idx = OpCovOutputIdx(gen__asset_cov_id, 1);"), "{minter_sil}");
    assert!(minter_sil.contains("validateOutputStateWithInputTemplate(\n            gen__asset_proxy_output_idx,"), "{minter_sil}");
    assert!(minter_sil.contains("gen__asset_proxy_input_idx,"), "{minter_sil}");
    assert!(minter_sil.contains("validateOutputStateWithTemplate(\n            gen__asset_recipient_output_idx,"), "{minter_sil}");
    assert!(minter_sil.contains("gen__kcc20_asset__kcc20_template"), "{minter_sil}");
    assert!(minter_sil.contains("MinterProxyState prev_proxy = gen__asset_proxy_state;"), "{minter_sil}");

    let artifact_json = fs::read_to_string(out_dir.join("artifact.json")).expect("artifact json exists");
    let artifact: Artifact = serde_json::from_str(&artifact_json).expect("artifact deserializes");
    artifact.verify_template_plan().expect("observed witness receipts verify");

    let _ = fs::remove_dir_all(out_dir);
}

#[test]
fn icc_asset_lowers_cov_id_co_spend_and_else_if() {
    let out_dir = std::env::temp_dir().join(format!("argent-icc-asset-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&out_dir);

    let program = crate::compiler::loader::load_program(Path::new("examples/icc/kcc20_asset.ag")).expect("ICC asset app loads");
    emit_build(&program, &out_dir).expect("ICC asset app builds");

    let kcc20_sil = fs::read_to_string(out_dir.join("sil/KCC20.sil")).expect("KCC20.sil exists");
    assert!(kcc20_sil.contains("} else if (identifier_type == IDENTIFIER_COVENANT_ID) {"), "{kcc20_sil}");
    assert!(kcc20_sil.contains("require(checkSig(owner_sig, pubkey(owner_identifier)));"), "{kcc20_sil}");
    assert!(kcc20_sil.contains("// :: co-spent with owner_identifier"), "{kcc20_sil}");
    assert!(kcc20_sil.contains("require(OpCovInputCount(owner_identifier) > 0);"), "{kcc20_sil}");
    assert!(kcc20_sil.contains("State next_state = State {"), "{kcc20_sil}");

    let proxy_sil = fs::read_to_string(out_dir.join("sil/MinterProxy.sil")).expect("MinterProxy.sil exists");
    assert!(proxy_sil.contains("byte[32] controller_id = init_controller_id;"), "{proxy_sil}");
    assert!(proxy_sil.contains("entry mint(\n        MinterProxyState next_proxy,"), "{proxy_sil}");
    assert!(proxy_sil.contains("gen__kcc20_template: gen__kcc20_template"), "{proxy_sil}");
    assert!(proxy_sil.contains("controller_id: next_proxy.controller_id"), "{proxy_sil}");
    assert!(proxy_sil.contains("// :: co-spent with controller_id"), "{proxy_sil}");
    assert!(proxy_sil.contains("require(OpCovInputCount(controller_id) > 0);"), "{proxy_sil}");

    let artifact_json = fs::read_to_string(out_dir.join("artifact.json")).expect("artifact json exists");
    let artifact: Artifact = serde_json::from_str(&artifact_json).expect("artifact deserializes");
    let proxy_entry =
        artifact.sil_abi.contract("MinterProxy").expect("MinterProxy ABI exists").entry("mint").expect("mint ABI exists");
    assert_eq!(proxy_entry.params[0].name, "next_proxy");
    assert_eq!(proxy_entry.params[0].ty, TypeArtifact::Struct { name: "MinterProxyState".to_string() });

    let _ = fs::remove_dir_all(out_dir);
}

#[test]
fn lowers_co_spend_and_output_value_in_the_same_expression() {
    let source = r#"
            state CounterState {
                cov_id guard;
            }

            actor Counter owns CounterState {
                entry bump() emits next: Counter {
                    require(guard.co_spent() && next.value >= 0);
                    become next <- Counter(self.state);
                }
            }

            app Test {
                actor Counter;
            }
        "#;
    let path = PathBuf::from("test.ag");
    let module = crate::compiler::syntax::parser::parse_module(path.clone(), source.to_string()).expect("source parses");
    let program = Program { root: path, modules: vec![module] };
    let model = Model::from_program(&program).expect("model validates");
    let sil = emit_actor(model.actor("Counter").expect("actor exists"), &model).expect("actor emits");

    assert!(sil.contains("require(OpCovInputCount(guard) > 0 && tx.outputs[gen__next_output_idx].value >= 0);"), "{sil}");
}

#[test]
fn rejects_co_spent_on_non_cov_id_value() {
    let out_dir = std::env::temp_dir().join(format!("argent-co-spent-type-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&out_dir);
    let module = crate::compiler::syntax::parser::parse_module(
        PathBuf::from("test.ag"),
        r#"
            state FooState {
                byte[32] id;
            }

            actor Foo owns FooState {
                entry hold() emits none {
                    require(id.co_spent());
                }
            }

            app Test {
                actor Foo;
            }
            "#
        .to_string(),
    )
    .expect("source parses");
    let program = Program { root: PathBuf::from("test.ag"), modules: vec![module] };

    let err = emit_build(&program, &out_dir).expect_err("non-covenant-id co-spend must be rejected");
    assert!(err.to_string().contains(&format!("only available on `{}` values", word::COVENANT_ID)), "unexpected error: {err}");

    let _ = fs::remove_dir_all(out_dir);
}

#[test]
fn rejects_duplicate_observe_names() {
    let err = parse_and_validate(
        r#"
            state ForeignState {}
            state LocalState {
                cov_id target_id;
            }

            actor Foreign owns ForeignState {
                entry hold() emits none {
                    require(1 == 1);
                }
            }

            actor Local owns LocalState {
                entry step()
                observes asset by self.target_id {
                    inputs {
                        foreign: Foreign,
                    }
                }
                observes asset by self.target_id {
                    outputs {
                        foreign: Foreign,
                    }
                }
                emits none {
                    require(1 == 1);
                }
            }

            app Test {
                actor Foreign;
                actor Local;
            }
            "#,
    )
    .expect_err("duplicate observe names must be rejected");

    assert!(err.to_string().contains("declares observe `asset` more than once"), "unexpected error: {err}");
}

#[test]
fn rejects_duplicate_observed_handles() {
    let err = parse_and_validate(
        r#"
            state ForeignState {}
            state LocalState {
                cov_id target_id;
            }

            actor Foreign owns ForeignState {
                entry hold() emits none {
                    require(1 == 1);
                }
            }

            actor Local owns LocalState {
                entry step()
                observes asset by self.target_id {
                    inputs {
                        foreign: Foreign,
                        foreign: Foreign,
                    }
                }
                emits none {
                    require(1 == 1);
                }
            }

            app Test {
                actor Foreign;
                actor Local;
            }
            "#,
    )
    .expect_err("duplicate observed handles must be rejected");

    assert!(err.to_string().contains("observe `asset` declares input `foreign` more than once"), "unexpected error: {err}");
}

#[test]
fn in_app_observed_templates_use_shared_actor_witnesses() {
    let (sil, artifact) = emit_fixture("observed_template_witnesses", "Local");

    assert_eq!(sil, include_str!("../../../../tests/fixtures/emit/observed_template_witnesses/Local.sil"));
    assert!(sil.contains("ForeignState gen__asset_src_state = readInputStateWithTemplate("), "{sil}");
    assert!(sil.contains("validateOutputStateWithInputTemplate("), "{sil}");

    let foreign_state = artifact.argent.states.iter().find(|state| state.name == "ForeignState").expect("ForeignState exists");
    assert_eq!(foreign_state.fields.iter().map(|field| field.name.as_str()).collect::<Vec<_>>(), ["count"]);
    let local_state = artifact.argent.states.iter().find(|state| state.name == "LocalState").expect("LocalState exists");
    assert_eq!(local_state.fields.iter().map(|field| field.name.as_str()).collect::<Vec<_>>(), ["target_id"]);

    let foreign_contract = artifact.sil_abi.contract("Foreign").expect("Foreign contract exists");
    assert_eq!(foreign_contract.runtime_state.fields.iter().map(|field| field.name.as_str()).collect::<Vec<_>>(), ["count"]);
    assert_eq!(foreign_contract.compiled.template_hash_hex, "464dd0aa5c6a60a35f5a1f3e54be4822991b4578cfe58e5a266cb8650e524c94");
    let local_contract = artifact.sil_abi.contract("Local").expect("Local contract exists");
    assert_eq!(
        local_contract.runtime_state.fields.iter().map(|field| field.name.as_str()).collect::<Vec<_>>(),
        ["gen__foreign_template", "target_id"]
    );
    assert_eq!(local_contract.compiled.template_hash_hex, "ed3b4d44f4911b5dc91a4ad26acb9bb1d61526f3082e976b1de506969dccf81d");

    assert_eq!(
        runtime_state_plan(&artifact, "Local")
            .expect("Local runtime state role overlay exists")
            .field_roles
            .iter()
            .map(|role| (role.name.as_str(), role.role.clone()))
            .collect::<Vec<_>>(),
        vec![("gen__foreign_template", RuntimeFieldRoleArtifact::Template { contract: "Foreign".to_string() },)]
    );

    let local_actor = artifact.argent.actors.iter().find(|actor| actor.name == "Local").expect("Local artifact actor exists");
    let step = local_actor.entries.iter().find(|entry| entry.name == "step").expect("step entry exists");
    assert_eq!(step.observes[0].covenant_id_source, CovenantIdSourceArtifact::StateField { field: "target_id".to_string() });
    assert_eq!(
        step.hidden_params.iter().map(|param| param.name.as_str()).collect::<Vec<_>>(),
        vec!["gen__foreign_prefix_len", "gen__foreign_suffix_len"]
    );
    assert_eq!(
        step.route_plan.witness_recipe_ids.iter().map(String::as_str).collect::<Vec<_>>(),
        vec!["witness/foreign/template_prefix_len", "witness/foreign/template_suffix_len",]
    );
}

#[test]
fn in_app_observe_preserves_observed_route_context() {
    let (local_sil, artifact) = emit_fixture("in_app_observe_routes", "Local");
    let (foreign_sil, _) = emit_fixture("in_app_observe_routes", "Foreign");
    let (target_sil, _) = emit_fixture("in_app_observe_routes", "Target");

    assert_eq!(local_sil, include_str!("../../../../tests/fixtures/emit/in_app_observe_routes/Local.sil"));
    assert_eq!(foreign_sil, include_str!("../../../../tests/fixtures/emit/in_app_observe_routes/Foreign.sil"));
    assert_eq!(target_sil, include_str!("../../../../tests/fixtures/emit/in_app_observe_routes/Target.sil"));

    assert_eq!(
        runtime_state_plan(&artifact, "Foreign")
            .expect("Foreign runtime state role overlay exists")
            .field_roles
            .iter()
            .map(|role| (role.name.as_str(), role.role.clone()))
            .collect::<Vec<_>>(),
        vec![("gen__target_template", RuntimeFieldRoleArtifact::Template { contract: "Target".to_string() },)]
    );
    assert_eq!(
        runtime_state_plan(&artifact, "Local")
            .expect("Local runtime state role overlay exists")
            .field_roles
            .iter()
            .map(|role| (role.name.as_str(), role.role.clone()))
            .collect::<Vec<_>>(),
        vec![
            ("gen__foreign_template", RuntimeFieldRoleArtifact::Template { contract: "Foreign".to_string() },),
            ("gen__target_template", RuntimeFieldRoleArtifact::Template { contract: "Target".to_string() },),
        ]
    );

    let foreign_template =
        artifact.argent.template_plan.templates.iter().find(|template| template.actor == "Foreign").expect("Foreign template exists");
    assert_eq!(foreign_template.actor_type_handle.context_fields, vec!["gen__target_template"]);

    let local_actor = artifact.argent.actors.iter().find(|actor| actor.name == "Local").expect("Local actor exists");
    let step = local_actor.entries.iter().find(|entry| entry.name == "step").expect("step entry exists");
    assert_eq!(
        step.hidden_params.iter().map(|param| param.name.as_str()).collect::<Vec<_>>(),
        vec!["gen__foreign_prefix_len", "gen__foreign_suffix_len"]
    );
}

#[test]
fn consumed_route_reuses_input_template() {
    let (sil, artifact) = emit_fixture("input_template_route_reuse", "Controller");

    assert_eq!(sil, include_str!("../../../../tests/fixtures/emit/input_template_route_reuse/Controller.sil"));
    assert!(sil.contains("PeerState peer = readInputStateWithTemplate("), "{sil}");
    assert!(sil.contains("validateOutputStateWithInputTemplate("), "{sil}");

    let controller_state =
        artifact.argent.states.iter().find(|state| state.name == "ControllerState").expect("ControllerState exists");
    assert_eq!(controller_state.fields.iter().map(|field| field.name.as_str()).collect::<Vec<_>>(), ["tick"]);
    let peer_state = artifact.argent.states.iter().find(|state| state.name == "PeerState").expect("PeerState exists");
    assert_eq!(peer_state.fields.iter().map(|field| field.name.as_str()).collect::<Vec<_>>(), ["count"]);

    let controller_contract = artifact.sil_abi.contract("Controller").expect("Controller contract exists");
    assert_eq!(
        controller_contract.runtime_state.fields.iter().map(|field| field.name.as_str()).collect::<Vec<_>>(),
        ["gen__peer_template", "tick"]
    );
    assert_eq!(controller_contract.compiled.template_hash_hex, "3091c3cb1b9eac52e3f7be3661e80720b5d2be1c0b9569e76f440a493ce53413");
    let peer_contract = artifact.sil_abi.contract("Peer").expect("Peer contract exists");
    assert_eq!(peer_contract.runtime_state.fields.iter().map(|field| field.name.as_str()).collect::<Vec<_>>(), ["count"]);
    assert_eq!(peer_contract.compiled.template_hash_hex, "7cbdc27a4fffbaad655bcd7565a7d85682469203db07d26b953f665640f4271f");

    assert_eq!(
        runtime_state_plan(&artifact, "Controller")
            .expect("Controller runtime state role overlay exists")
            .field_roles
            .iter()
            .map(|field| (field.name.as_str(), field.role.clone()))
            .collect::<Vec<_>>(),
        [("gen__peer_template", RuntimeFieldRoleArtifact::Template { contract: "Peer".to_string() })]
    );

    let controller = artifact.argent.actors.iter().find(|actor| actor.name == "Controller").expect("Controller actor exists");
    let step = controller.entries.iter().find(|entry| entry.name == "step").expect("step entry exists");
    assert_eq!(
        step.hidden_params.iter().map(|param| (param.name.as_str(), param.purpose)).collect::<Vec<_>>(),
        vec![
            ("gen__peer_prefix_len", HiddenParamPurposeArtifact::TemplatePrefixLen),
            ("gen__peer_suffix_len", HiddenParamPurposeArtifact::TemplateSuffixLen),
        ]
    );
    assert_eq!(
        step.route_plan.witness_recipe_ids.iter().map(String::as_str).collect::<Vec<_>>(),
        vec!["witness/peer/template_prefix_len", "witness/peer/template_suffix_len"]
    );
}

#[test]
fn single_actor_self_consume_is_pinned() {
    let (sil, artifact) = emit_fixture("single_actor_self_consume", "Counter");

    assert_eq!(sil, include_str!("../../../../tests/fixtures/emit/single_actor_self_consume/Counter.sil"));
    assert!(sil.contains("State other = readInputState(gen__other_input_idx);"), "{sil}");
    assert!(!sil.contains("readInputStateWithTemplate"), "{sil}");
    assert!(sil.contains("validateOutputState(gen__next_output_idx, next);"), "{sil}");

    let source_state = artifact.argent.states.iter().find(|state| state.name == "CounterState").expect("CounterState exists");
    assert_eq!(source_state.fields.iter().map(|field| field.name.as_str()).collect::<Vec<_>>(), ["count"]);
    let contract = artifact.sil_abi.contract("Counter").expect("Counter contract exists");
    assert_eq!(contract.runtime_state.fields.iter().map(|field| field.name.as_str()).collect::<Vec<_>>(), ["count"]);
    assert_eq!(contract.compiled.template_hash_hex, "a7b02624738e66c0f5c0923250c6cc28bdfb3606782a1689954095633a6f938a");
    let template = artifact
        .argent
        .template_plan
        .templates
        .iter()
        .find(|template| template.actor == "Counter")
        .expect("Counter template receipt exists");
    assert!(template.actor_type_handle.context_fields.is_empty());
    assert_eq!(template.sil_template_hash, contract.compiled.template_hash_hex);

    let counter = artifact.argent.actors.iter().find(|actor| actor.name == "Counter").expect("Counter actor exists");
    let merge = counter.entries.iter().find(|entry| entry.name == "merge").expect("merge entry exists");
    assert!(merge.hidden_params.is_empty());
    assert!(merge.route_plan.witness_recipe_ids.is_empty());
    assert!(runtime_state_plan(&artifact, "Counter").is_none());
}

#[test]
fn selected_app_actor_count_controls_self_consume_template_authentication() {
    let path = PathBuf::from("multi_actor_self_consume.ag");
    let module = crate::compiler::syntax::parser::parse_module(
        path.clone(),
        r#"
            state CounterState {
                int count;
            }

            state GuardState {}

            actor Counter owns CounterState {
                entry merge()
                consumes {
                    other: Counter,
                }
                emits next: Counter {
                    unrestricted(next.value);
                    CounterState next = {
                        count: count + other.count,
                    };

                    become next <- Counter(next);
                }
            }

            actor Guard owns GuardState {
                entry hold() emits none {
                    require(1 == 1);
                }
            }

            app Single {
                actor Counter;
            }

            app Multi {
                actor Counter;
                actor Guard;
            }
            "#
        .to_string(),
    )
    .expect("source parses");
    let program = Program { root: path, modules: vec![module] };
    let multi_model = Model::from_program_app(&program, "Multi").expect("multi-actor model validates");
    let counter = multi_model.actor("Counter").expect("Counter actor exists");
    let sil = emit_actor(counter, &multi_model).expect("Counter emits for the multi-actor app");
    let actor_sil = actor_sil_for_model(&multi_model);
    let artifact = emit_artifact(&program, &multi_model, &actor_sil).expect("multi-actor artifact emits");

    assert!(sil.contains("State other = readInputStateWithTemplate("), "{sil}");
    assert!(!sil.contains("// :: direct input state"), "{sil}");
    let counter = artifact.argent.actors.iter().find(|actor| actor.name == "Counter").expect("Counter actor exists");
    let merge = counter.entries.iter().find(|entry| entry.name == "merge").expect("merge entry exists");
    assert_eq!(
        merge.hidden_params.iter().map(|param| param.name.as_str()).collect::<Vec<_>>(),
        vec!["gen__counter_prefix_len", "gen__counter_suffix_len"]
    );
    assert_eq!(
        merge.route_plan.witness_recipe_ids.iter().map(String::as_str).collect::<Vec<_>>(),
        vec!["witness/counter/template_prefix_len", "witness/counter/template_suffix_len"]
    );
    assert!(runtime_state_plan(&artifact, "Counter").is_some());

    let single_model = Model::from_program_app(&program, "Single").expect("single-actor model validates");
    let counter = single_model.actor("Counter").expect("Counter actor exists");
    let sil = emit_actor(counter, &single_model).expect("Counter emits for the single-actor app");
    let actor_sil = actor_sil_for_model(&single_model);
    let artifact = emit_artifact(&program, &single_model, &actor_sil).expect("single-actor artifact emits");

    assert!(sil.contains("State other = readInputState(gen__other_input_idx);"), "{sil}");
    assert!(!sil.contains("readInputStateWithTemplate"), "{sil}");
    assert!(runtime_state_plan(&artifact, "Counter").is_none());
}

#[test]
fn unselected_actors_do_not_shape_selected_app_state() {
    let path = PathBuf::from("app_state_isolation.ag");
    let module = crate::compiler::syntax::parser::parse_module(
        path.clone(),
        r#"
            state SharedState {
                int count;
            }

            state TargetState {}

            actor Current owns SharedState {
                entry step() emits next: Current {
                    unrestricted(next.value);
                    become next <- Current(self.state);
                }
            }

            actor Outside owns SharedState {
                entry step() emits next: Target {
                    unrestricted(next.value);
                    TargetState next = {};
                    become next <- Target(next);
                }
            }

            actor Target owns TargetState {
                entry hold() emits none {
                    require(1 == 1);
                }
            }

            app CurrentApp {
                actor Current;
            }

            app OtherApp {
                actor Outside;
                actor Target;
            }
            "#
        .to_string(),
    )
    .expect("source parses");
    let program = Program { root: path, modules: vec![module] };
    let model = Model::from_program_app(&program, "CurrentApp").expect("selected app model validates");
    let actor_sil = actor_sil_for_model(&model);
    let artifact = emit_artifact(&program, &model, &actor_sil).expect("selected app artifact emits");
    let template =
        artifact.argent.template_plan.templates.iter().find(|template| template.actor == "Current").expect("Current template exists");

    assert!(runtime_state_plan(&artifact, "Current").is_none());
    assert!(template.actor_type_handle.context_fields.is_empty());
    assert_eq!(template.actor_type_handle.template.hash_hex, template.sil_template_hash);
}

#[test]
fn open_observed_actor_binding_lowers_to_runtime_template_handle() {
    let (sil, artifact) = emit_fixture("open_observed_actor_binding", "Cell");

    assert_eq!(sil, include_str!("../../../../tests/fixtures/emit/open_observed_actor_binding/Cell.sil"));

    assert!(runtime_state_plan(&artifact, "Cell").is_none(), "{:#?}", artifact.argent.template_plan.runtime_states);

    let cell_actor = artifact.argent.actors.iter().find(|actor| actor.name == "Cell").expect("Cell artifact actor exists");
    let advance = cell_actor.entries.iter().find(|entry| entry.name == "advance").expect("advance entry exists");
    let observe = advance.observes.first().expect("advance observes remote");
    assert_eq!(observe.inputs[0].target, ObservedTargetArtifact::DynamicActor { state: "AgentCapsule".to_string() });
    assert_eq!(observe.outputs[0].target, ObservedTargetArtifact::DynamicActor { state: "AgentCapsule".to_string() });
    assert_eq!(
        advance.hidden_params.iter().map(|param| (param.name.as_str(), param.purpose)).collect::<Vec<_>>(),
        vec![
            ("gen__remote_observed_agent_prefix_len", HiddenParamPurposeArtifact::TemplatePrefixLen),
            ("gen__remote_observed_agent_suffix_len", HiddenParamPurposeArtifact::TemplateSuffixLen),
            ("gen__remote_observed_agent_template", HiddenParamPurposeArtifact::TemplateHash),
        ]
    );
}

#[test]
fn open_observed_state_handle_lowers_to_source_actor_type() {
    let (sil, artifact) = emit_fixture("open_observed_state_handle", "Cell");

    assert_eq!(sil, include_str!("../../../../tests/fixtures/emit/open_observed_state_handle/Cell.sil"));

    assert!(runtime_state_plan(&artifact, "Cell").is_none(), "{:#?}", artifact.argent.template_plan.runtime_states);

    let cell_actor = artifact.argent.actors.iter().find(|actor| actor.name == "Cell").expect("Cell artifact actor exists");
    let advance = cell_actor.entries.iter().find(|entry| entry.name == "advance").expect("advance entry exists");
    let observe = advance.observes.first().expect("advance observes remote");
    assert_eq!(observe.inputs[0].target, ObservedTargetArtifact::DynamicActor { state: "AgentCapsule".to_string() });
    assert_eq!(observe.outputs[0].target, ObservedTargetArtifact::DynamicActor { state: "AgentCapsule".to_string() });
    assert_eq!(
        advance.hidden_params.iter().map(|param| param.name.as_str()).collect::<Vec<_>>(),
        vec!["gen__actor_type_self_agent_type_prefix_len", "gen__actor_type_self_agent_type_suffix_len"]
    );
}

#[test]
fn rejects_input_only_open_observed_actor_binding() {
    let err = parse_and_validate(
        r#"
            state AgentCapsule {
                int energy;
            }

            state CellState {
                cov_id agent_covid;
            }

            actor Cell owns CellState {
                entry inspect()
                observes remote by self.agent_covid {
                    inputs {
                        agent: actor_type<AgentCapsule> as observed_agent,
                    }
                }
                emits none {
                    AgentCapsule prev_state = remote.inputs.agent.state;
                    require(prev_state.energy >= 0);
                }
            }

            app Test {
                actor Cell;
            }
            "#,
    )
    .expect_err("input-only open observed binding must be rejected");

    assert!(
        err.to_string().contains("open observed actor binding `observed_agent` must be used by an output"),
        "unexpected error: {err}"
    );
}

#[test]
fn rejects_missing_observed_output_become_coverage() {
    let err = emit_inline_error(
        r#"
            state ForeignState {
                int amount;
            }

            state LocalState {
                cov_id target_id;
            }

            actor Foreign owns ForeignState {
                entry hold() emits none {
                    require(amount >= 0);
                }
            }

            actor Local owns LocalState {
                entry step()
                observes asset by self.target_id {
                    outputs {
                        a: Foreign,
                        b: Foreign,
                    }
                }
                emits none {
                    ForeignState next = {
                        amount: 1,
                    };

                    require asset.outputs become {
                        a <- Foreign(next),
                    };
                }
            }

            app Test {
                actor Foreign;
                actor Local;
            }
            "#,
    );

    assert!(err.to_string().contains("observe `asset` does not validate output `b`"), "unexpected error: {err}");
}

#[test]
fn rejects_semicolons_in_observed_become_route_lists() {
    let err = parse_and_validate(
        r#"
            state ForeignState {
                int amount;
            }

            state LocalState {
                cov_id target_id;
            }

            actor Foreign owns ForeignState {
                entry hold() emits none {
                    require(amount >= 0);
                }
            }

            actor Local owns LocalState {
                entry step()
                observes asset by self.target_id {
                    outputs {
                        a: Foreign,
                        b: Foreign,
                    }
                }
                emits none {
                    ForeignState next = {
                        amount: 1,
                    };

                    require asset.outputs become {
                        a <- Foreign(next);
                        b <- Foreign(next);
                    };
                }
            }

            app Test {
                actor Foreign;
                actor Local;
            }
            "#,
    )
    .expect_err("semicolon-separated output routes must not parse");

    assert!(err.to_string().contains("expected `,` or `}`"), "unexpected error: {err}");
}

#[test]
fn rejects_observed_output_become_actor_mismatch() {
    let err = emit_inline_error(
        r#"
            state ForeignState {
                int amount;
            }

            state LocalState {
                cov_id target_id;
            }

            actor ForeignA owns ForeignState {
                entry hold() emits none {
                    require(amount >= 0);
                }
            }

            actor ForeignB owns ForeignState {
                entry hold() emits none {
                    require(amount >= 0);
                }
            }

            actor Local owns LocalState {
                entry step()
                observes asset by self.target_id {
                    outputs {
                        next: ForeignA,
                    }
                }
                emits none {
                    ForeignState next_state = {
                        amount: 1,
                    };

                    require asset.outputs become {
                        next <- ForeignB(next_state),
                    };
                }
            }

            app Test {
                actor ForeignA;
                actor ForeignB;
                actor Local;
            }
            "#,
    );

    assert!(
        err.to_string().contains("observe `asset` output `next` expects `ForeignA`, but route uses `ForeignB`"),
        "unexpected error: {err}"
    );
}

#[test]
fn stones_delegate_reads_use_length_only_template_witnesses() {
    let out_dir = std::env::temp_dir().join(format!("argent-stones-length-witness-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&out_dir);

    let program = crate::compiler::loader::load_program(Path::new("examples/stones/app.ag")).expect("stones example loads");
    emit_build(&program, &out_dir).expect("stones example builds");
    let player_sil = fs::read_to_string(out_dir.join("sil/Player.sil")).expect("Player.sil exists");
    let league_sil = fs::read_to_string(out_dir.join("sil/League.sil")).expect("League.sil exists");
    let artifact_json = fs::read_to_string(out_dir.join("artifact.json")).expect("artifact json exists");
    let artifact: Artifact = serde_json::from_str(&artifact_json).expect("artifact deserializes");

    assert!(player_sil.contains("entry accept_start(\n"), "{player_sil}");
    assert!(player_sil.contains("sig owner_sig,"), "{player_sil}");
    assert!(player_sil.contains("pubkey owner_pk,"), "{player_sil}");
    assert!(player_sil.contains("int gen__player_prefix_len,"), "{player_sil}");
    assert!(player_sil.contains("int gen__player_suffix_len"), "{player_sil}");
    assert!(!player_sil.contains("entry accept_start(sig owner_sig, pubkey owner_pk, byte[]"), "{player_sil}");
    assert!(player_sil.contains("entry start_game(\n"), "{player_sil}");
    assert!(player_sil.contains("int gen__player_prefix_len,"), "{player_sil}");
    assert!(player_sil.contains("byte[] gen__stones_game_prefix,"), "{player_sil}");
    assert!(player_sil.contains("byte[] gen__stones_game_suffix"), "{player_sil}");
    assert!(!player_sil.contains("byte[] gen__player_prefix"), "{player_sil}");
    assert!(player_sil.contains("byte[32] gen__init_player_template"), "{player_sil}");
    assert!(player_sil.contains("byte[32] gen__init_stones_game_template"), "{player_sil}");
    assert!(player_sil.contains("byte[32] gen__init_stones_settle_template"), "{player_sil}");
    assert!(player_sil.contains("byte[32] gen__player_template = gen__init_player_template;"), "{player_sil}");
    assert!(player_sil.contains("byte[32] gen__stones_game_template = gen__init_stones_game_template;"), "{player_sil}");
    assert!(!player_sil.contains("gen__template_root"), "{player_sil}");
    assert!(!player_sil.contains("gen__player_template_proof"), "{player_sil}");
    assert!(
        !player_sil.contains("gen__template_table") && !player_sil.contains("gen__init_template_table"),
        "ordinary direct-template state should not store a packed table: {player_sil}"
    );
    assert!(
        !player_sil.contains("byte[32][]") && !player_sil.contains("byte[][]"),
        "template roots/proofs should use fixed bytes, not nested arrays: {player_sil}"
    );
    assert!(
        !player_sil.contains("gen__league_template"),
        "Player route-family template root should not carry unrelated League template: {player_sil}"
    );
    assert!(player_sil.contains("validateOutputState(gen__self_out_output_idx, next_self);"), "{player_sil}");
    assert!(player_sil.contains("validateOutputState(gen__opponent_out_output_idx, next_opponent);"), "{player_sil}");
    assert!(player_sil.contains("validateOutputStateWithTemplate(\n            gen__game_output_idx,"), "{player_sil}");
    assert!(league_sil.contains("entry register_player(\n"), "{league_sil}");
    assert!(league_sil.contains("byte[] gen__player_prefix,"), "{league_sil}");
    assert!(league_sil.contains("byte[] gen__player_suffix"), "{league_sil}");
    assert!(!league_sil.contains("gen__league_prefix"), "{league_sil}");
    assert!(league_sil.contains("tx.outputs[gen__league_output_idx].scriptPubKey"), "{league_sil}");
    assert!(league_sil.contains("== tx.inputs[this.activeInputIndex].scriptPubKey"), "{league_sil}");
    assert!(league_sil.contains("validateOutputStateWithTemplate(\n            gen__player_output_idx,"), "{league_sil}");

    let player_actor = artifact.argent.actors.iter().find(|actor| actor.name == "Player").expect("Player actor exists");
    let accept_start = player_actor.entries.iter().find(|entry| entry.name == "accept_start").expect("accept_start ABI exists");
    assert_eq!(accept_start.hidden_params.len(), 2);
    assert_eq!(accept_start.hidden_params[0].name, "gen__player_prefix_len");
    assert_eq!(accept_start.hidden_params[0].ty, TypeArtifact::Int);
    assert_eq!(subject_label(&accept_start.hidden_params[0].subject), "Player");
    assert_eq!(accept_start.hidden_params[0].purpose, HiddenParamPurposeArtifact::TemplatePrefixLen);
    assert_eq!(accept_start.hidden_params[1].name, "gen__player_suffix_len");
    assert_eq!(accept_start.hidden_params[1].ty, TypeArtifact::Int);
    assert_eq!(subject_label(&accept_start.hidden_params[1].subject), "Player");
    assert_eq!(accept_start.hidden_params[1].purpose, HiddenParamPurposeArtifact::TemplateSuffixLen);

    let start_game = player_actor.entries.iter().find(|entry| entry.name == "start_game").expect("start_game ABI exists");
    assert_eq!(
        start_game
            .hidden_params
            .iter()
            .map(|param| (param.name.as_str(), param.ty.clone(), subject_label(&param.subject), param.purpose))
            .collect::<Vec<_>>(),
        vec![
            ("gen__player_prefix_len", TypeArtifact::Int, "Player", HiddenParamPurposeArtifact::TemplatePrefixLen),
            ("gen__player_suffix_len", TypeArtifact::Int, "Player", HiddenParamPurposeArtifact::TemplateSuffixLen),
            ("gen__stones_game_prefix", TypeArtifact::Bytes, "StonesGame", HiddenParamPurposeArtifact::TemplatePrefixBytes),
            ("gen__stones_game_suffix", TypeArtifact::Bytes, "StonesGame", HiddenParamPurposeArtifact::TemplateSuffixBytes),
        ]
    );

    let player_contract = artifact.sil_abi.contract("Player").expect("Player Sil ABI contract exists");
    let player_runtime_plan = runtime_state_plan(&artifact, "Player").expect("Player runtime role overlay exists");
    assert_eq!(player_contract.runtime_state.fields[0].name, "gen__player_template");
    assert_eq!(player_contract.runtime_state.fields[0].ty, TypeArtifact::FixedBytes { len: 32 });
    assert_eq!(player_runtime_plan.field_roles[0].role, RuntimeFieldRoleArtifact::Template { contract: "Player".to_string() });
    assert_eq!(player_contract.runtime_state.fields[1].name, "gen__stones_game_template");
    assert_eq!(player_runtime_plan.field_roles[1].role, RuntimeFieldRoleArtifact::Template { contract: "StonesGame".to_string() });
    assert_eq!(player_contract.runtime_state.fields[2].name, "gen__stones_settle_template");
    assert_eq!(player_runtime_plan.field_roles[2].role, RuntimeFieldRoleArtifact::Template { contract: "StonesSettle".to_string() });
    assert!(artifact.argent.template_plan.route_tables.is_empty());
    assert!(artifact.argent.template_plan.route_proofs.is_empty());
    let sil_accept_start = player_contract.entry("accept_start").expect("accept_start Sil ABI entry exists");
    assert_eq!(
        sil_accept_start.params.iter().map(|param| (param.name.as_str(), param.ty.clone())).collect::<Vec<_>>(),
        vec![
            ("owner_sig", TypeArtifact::Sig),
            ("owner_pk", TypeArtifact::Pubkey),
            ("gen__player_prefix_len", TypeArtifact::Int),
            ("gen__player_suffix_len", TypeArtifact::Int),
        ]
    );

    let league_actor = artifact.argent.actors.iter().find(|actor| actor.name == "League").expect("League actor exists");
    let register_player = league_actor.entries.iter().find(|entry| entry.name == "register_player").expect("register_player exists");
    assert_eq!(
        register_player
            .hidden_params
            .iter()
            .map(|param| (param.name.as_str(), param.ty.clone(), subject_label(&param.subject), param.purpose))
            .collect::<Vec<_>>(),
        vec![
            ("gen__player_prefix", TypeArtifact::Bytes, "Player", HiddenParamPurposeArtifact::TemplatePrefixBytes),
            ("gen__player_suffix", TypeArtifact::Bytes, "Player", HiddenParamPurposeArtifact::TemplateSuffixBytes),
        ]
    );
    let _ = fs::remove_dir_all(out_dir);
}

#[test]
fn compiler_lowers_injected_deep_forest_cuts() {
    use crate::routing::{CommitmentForest, CommitmentPlan, Cut, FamilyPlan, NodePath, RoutePlan};

    fn leaf(actor: &str) -> CommitmentNode {
        CommitmentNode::Leaf { actor: actor.to_string() }
    }

    fn cut(paths: &[&[usize]]) -> Cut {
        paths.iter().map(|path| NodePath::new(path.to_vec())).collect()
    }

    let source = r#"
            state SourceState {
                int amount;
            }

            state SharedState {
                int amount;
            }

            state TailState {
                int amount;
            }

            actor Source owns SourceState {
                entry start() emits next: HubA {
                    unrestricted(next.value);
                    SharedState next = { amount: amount + 1 };
                    become next <- HubA(next);
                }
            }

            actor HubA owns SharedState {
                entry advance() emits next: A1 {
                    unrestricted(next.value);
                    SharedState next = { amount: amount + 1 };
                    become next <- A1(next);
                }
            }

            actor A1 owns SharedState {
                entry advance() emits next: A2 {
                    unrestricted(next.value);
                    SharedState next = { amount: amount + 1 };
                    become next <- A2(next);
                }
            }

            actor A2 owns SharedState {
                entry cross() emits next: HubB {
                    unrestricted(next.value);
                    SharedState next = { amount: amount + 1 };
                    become next <- HubB(next);
                }
            }

            actor HubB owns SharedState {
                entry advance() emits next: B1 {
                    unrestricted(next.value);
                    SharedState next = { amount: amount + 1 };
                    become next <- B1(next);
                }

                entry rewind() emits next: A1 {
                    unrestricted(next.value);
                    SharedState next = { amount: amount + 1 };
                    become next <- A1(next);
                }
            }

            actor B1 owns SharedState {
                entry advance() emits next: B2 {
                    unrestricted(next.value);
                    SharedState next = { amount: amount + 1 };
                    become next <- B2(next);
                }
            }

            actor B2 owns SharedState {
                entry finish() emits next: Tail {
                    unrestricted(next.value);
                    TailState next = { amount: amount + 1 };
                    become next <- Tail(next);
                }
            }

            actor Tail owns TailState {
                entry finish() emits none {
                    require(amount >= 0);
                }
            }

            app DeepForest {
                actor Source;
                actor HubA;
                actor A1;
                actor A2;
                actor HubB;
                actor B1;
                actor B2;
                actor Tail;
            }
        "#;
    let path = PathBuf::from("deep-forest.ag");
    let module = crate::compiler::syntax::parser::parse_module(path.clone(), source.to_string()).expect("deep-forest app parses");
    let program = Program { root: path, modules: vec![module] };

    // The useful family branches live four levels below the forest root.
    // Wrapper branches are deliberately absent from every partial cut;
    // cuts mix packed families, opened family leaves, and a retained leaf.
    let forest = CommitmentForest {
        roots: vec![CommitmentNode::Branch {
            children: vec![
                leaf("Source"),
                CommitmentNode::Branch {
                    children: vec![CommitmentNode::Branch {
                        children: vec![
                            CommitmentNode::Branch { children: vec![leaf("HubA"), leaf("A1"), leaf("A2")] },
                            leaf("Tail"),
                            CommitmentNode::Branch { children: vec![leaf("HubB"), leaf("B1"), leaf("B2")] },
                        ],
                    }],
                },
            ],
        }],
    };
    let packed = cut(&[&[0, 1, 0, 0], &[0, 1, 0, 1], &[0, 1, 0, 2]]);
    let family_a_open = cut(&[&[0, 1, 0, 0, 0], &[0, 1, 0, 0, 1], &[0, 1, 0, 0, 2], &[0, 1, 0, 1], &[0, 1, 0, 2]]);
    let family_b_open = cut(&[&[0, 1, 0, 0], &[0, 1, 0, 1], &[0, 1, 0, 2, 0], &[0, 1, 0, 2, 1], &[0, 1, 0, 2, 2]]);
    let commitments = CommitmentPlan {
        forest,
        cuts: BTreeMap::from([
            ("Source".to_string(), packed),
            ("HubA".to_string(), family_a_open.clone()),
            ("A1".to_string(), family_a_open.clone()),
            ("A2".to_string(), family_a_open),
            ("HubB".to_string(), family_b_open.clone()),
            ("B1".to_string(), family_b_open.clone()),
            ("B2".to_string(), family_b_open),
            ("Tail".to_string(), Cut::new()),
        ]),
    };
    let crafted_plan = RoutePlan {
        families: vec![
            FamilyPlan {
                domain: "SharedState".to_string(),
                rep: "HubA".to_string(),
                members: ["HubA", "A1", "A2"].into_iter().map(str::to_string).collect(),
                gates: vec!["HubA".to_string()],
                table: ["HubA", "A1", "A2"].into_iter().map(str::to_string).collect(),
            },
            FamilyPlan {
                domain: "SharedState".to_string(),
                rep: "HubB".to_string(),
                members: ["HubB", "B1", "B2"].into_iter().map(str::to_string).collect(),
                gates: vec!["HubB".to_string()],
                table: ["HubB", "B1", "B2"].into_iter().map(str::to_string).collect(),
            },
        ],
        commitments,
    };
    assert!(crafted_plan.commitments.cuts.values().all(|cut| crafted_plan.commitments.forest.is_valid_cut(cut)));
    let cross = crafted_plan.commitments.cut_transition("A2", "HubB").expect("cross-family cut is derivable");
    assert_eq!(cross.branches_to_open.len(), 1);
    assert_eq!(cross.branches_to_pack.len(), 1);

    let injected_planner = move |_graph: &RouteGraph, domains: &BTreeMap<String, Vec<String>>, selectors: &[SelectorRequirement]| {
        assert_eq!(domains["SharedState"], ["HubA", "A1", "A2", "HubB", "B1", "B2"]);
        assert!(selectors.is_empty());
        Ok(crafted_plan.clone())
    };
    let model = Model::from_program_with_route_planner(&program, &injected_planner).expect("injected route plan validates");

    let family_a_id = "route_family/SharedState/hub_a".to_string();
    let family_b_id = "route_family/SharedState/hub_b".to_string();
    assert_eq!(
        model.route_transitions[&("A2".to_string(), "HubB".to_string())],
        CompilerRouteTransition { families_to_open: vec![family_b_id], families_to_pack: vec![family_a_id] }
    );

    let actor_sil = actor_sil_for_model(&model);
    let a2_sil = &actor_sil["A2"];
    assert!(a2_sil.contains("byte[96] gen__hub_b_routes"), "{a2_sil}");
    assert!(a2_sil.contains("gen__hub_a_routes_digest: blake3(byte[](gen__hub_a_routes)),"), "{a2_sil}");
    let hub_b_sil = &actor_sil["HubB"];
    assert!(hub_b_sil.contains("byte[96] gen__hub_a_routes"), "{hub_b_sil}");
    assert!(hub_b_sil.contains("gen__hub_b_routes_digest: blake3(byte[](gen__hub_b_routes)),"), "{hub_b_sil}");

    let artifact = emit_artifact(&program, &model, &actor_sil).expect("deep-forest artifact emits");
    artifact.verify_template_plan().expect("deep-forest template plan verifies");
    assert_eq!(artifact.argent.template_plan.route_families.len(), 2);
}

#[test]
fn shared_state_actors_retain_distinct_transitive_cuts() {
    let path = PathBuf::from("actor-route-cuts.ag");
    let module = crate::compiler::syntax::parser::parse_module(
        path.clone(),
        r#"
            state SharedState {
                int amount;
            }

            state MiddleState {
                int amount;
            }

            state TailState {
                int amount;
            }

            actor A owns SharedState {
                entry leave() emits next: Middle {
                    unrestricted(next.value);
                    MiddleState next_middle = {
                        amount: amount,
                    };
                    become next <- Middle(next_middle);
                }
            }

            actor B owns SharedState {}

            actor Middle owns MiddleState {
                entry leave() emits next: Tail {
                    unrestricted(next.value);
                    TailState next_tail = {
                        amount: amount,
                    };
                    become next <- Tail(next_tail);
                }
            }

            actor Tail owns TailState {}

            app ActorRouteCuts {
                actor A;
                actor B;
                actor Middle;
                actor Tail;
            }
            "#
        .to_string(),
    )
    .expect("source parses");
    let program = Program { root: path, modules: vec![module] };

    let model = Model::from_program(&program).expect("model retains the planned actor cuts");

    assert_eq!(
        model.route_leaves_by_actor["A"],
        [RouteRootLeaf::Actor("Middle".to_string()), RouteRootLeaf::Actor("Tail".to_string())]
    );
    assert!(model.route_leaves_by_actor["B"].is_empty());

    match route_field_kind_for_actor("A", &model) {
        RouteFieldKind::Direct { actor_templates, family_commitments } => {
            assert_eq!(actor_templates, ["Middle", "Tail"]);
            assert!(family_commitments.is_empty());
        }
        RouteFieldKind::None | RouteFieldKind::FamilyTables { .. } => panic!("A has two direct actor templates"),
    }
    assert!(matches!(route_field_kind_for_actor("B", &model), RouteFieldKind::None));

    let actor_a = model.actor("A").expect("A exists");
    let actor_b = model.actor("B").expect("B exists");
    let a_sil = emit_actor(actor_a, &model).expect("A Sil emits");
    let b_sil = emit_actor(actor_b, &model).expect("B Sil emits");
    let middle_sil = emit_actor(model.actor("Middle").expect("Middle exists"), &model).expect("Middle Sil emits");
    let shared_layout = middle_sil
        .split("struct SharedState {")
        .nth(1)
        .and_then(|rest| rest.split("    }").next())
        .expect("plain SharedState layout exists");
    assert!(!shared_layout.contains("gen__"), "{middle_sil}");
    assert!(a_sil.contains("byte[32] gen__init_middle_template"), "{a_sil}");
    assert!(a_sil.contains("byte[32] gen__middle_template = gen__init_middle_template;"), "{a_sil}");
    assert!(a_sil.contains("byte[32] gen__tail_template = gen__init_tail_template;"), "{a_sil}");
    assert!(!b_sil.contains("gen__middle_template"), "{b_sil}");
    assert!(!b_sil.contains("gen__tail_template"), "{b_sil}");
    assert_eq!(
        runtime_state_fields_for_actor(actor_a, &model)
            .expect("A runtime fields lower")
            .into_iter()
            .map(|field| field.name)
            .collect::<Vec<_>>(),
        ["gen__middle_template", "gen__tail_template", "amount"]
    );
    assert_eq!(
        runtime_state_fields_for_actor(actor_b, &model)
            .expect("B runtime fields lower")
            .into_iter()
            .map(|field| field.name)
            .collect::<Vec<_>>(),
        ["amount"]
    );
}

#[test]
fn foreign_routes_materialize_the_target_actors_cut() {
    let source = r#"
            state SourceState {
                int amount;
            }

            state SharedState {
                int amount;
            }

            state TailAState {
                int amount;
            }

            state TailBState {
                int amount;
            }

            actor Source owns SourceState {
                entry send() emits next: A {
                    unrestricted(next.value);
                    SharedState next = {
                        amount: amount,
                    };
                    become next <- A(next);
                }
            }

            actor A owns SharedState {
                entry leave() emits next: TailA {
                    unrestricted(next.value);
                    TailAState next = {
                        amount: amount,
                    };
                    become next <- TailA(next);
                }
            }

            actor B owns SharedState {
                entry leave() emits next: TailB {
                    unrestricted(next.value);
                    TailBState next = {
                        amount: amount,
                    };
                    become next <- TailB(next);
                }
            }

            actor TailA owns TailAState {
                entry hold() emits none {
                    require(amount >= 0);
                }
            }

            actor TailB owns TailBState {
                entry hold() emits none {
                    require(amount >= 0);
                }
            }

            app ForeignActorCuts {
                actor Source;
                actor A;
                actor B;
                actor TailA;
                actor TailB;
            }
        "#;

    let path = PathBuf::from("foreign-actor-cuts.ag");
    let module = crate::compiler::syntax::parser::parse_module(path.clone(), source.to_string()).expect("source parses");
    let program = Program { root: path, modules: vec![module] };
    let model = Model::from_program(&program).expect("model validates");
    let source_sil = emit_actor(model.actor("Source").expect("Source exists"), &model).expect("Source Sil emits");

    let actor_layout = source_sil
        .split("struct Gen__AState {")
        .nth(1)
        .and_then(|rest| rest.split("    }").next())
        .expect("A receives an actor-qualified foreign state layout");
    assert!(actor_layout.contains("byte[32] gen__tail_a_template;"), "{source_sil}");
    assert!(!actor_layout.contains("gen__tail_b_template"), "{source_sil}");
    assert!(source_sil.contains("SharedState next = SharedState {"), "{source_sil}");
    assert!(source_sil.contains("Gen__AState gen__state_next_gen__a_state = Gen__AState {"), "{source_sil}");
    assert!(source_sil.contains("amount: next.amount,"), "{source_sil}");
    assert!(source_sil.contains("gen__tail_a_template: gen__tail_a_template,"), "{source_sil}");
    assert!(!source_sil.contains("gen__tail_b_template:"), "{source_sil}");

    inline_artifact("foreign-actor-cuts", source);
}

#[test]
fn actor_route_field_kind_distinguishes_local_tables_from_foreign_commitments() {
    let path = PathBuf::from("actor-route-field-kinds.ag");
    let module = crate::compiler::syntax::parser::parse_module(path.clone(), toy_chess_source()).expect("toy chess source parses");
    let program = Program { root: path, modules: vec![module] };

    let model = Model::from_program(&program).expect("toy chess model validates");

    match route_field_kind_for_actor("Mux", &model) {
        RouteFieldKind::FamilyTables { actor_templates, family_commitments, families } => {
            assert!(actor_templates.is_empty());
            assert!(family_commitments.is_empty());
            assert_eq!(families.iter().map(|family| family.id.as_str()).collect::<Vec<_>>(), ["route_family/BoardState/mux"]);
        }
        RouteFieldKind::None | RouteFieldKind::Direct { .. } => panic!("Mux owns its local route table"),
    }

    match route_field_kind_for_actor("Player", &model) {
        RouteFieldKind::Direct { actor_templates, family_commitments } => {
            assert_eq!(actor_templates, ["Mux"]);
            assert_eq!(
                family_commitments.iter().map(|family| family.id.as_str()).collect::<Vec<_>>(),
                ["route_family/BoardState/mux"]
            );
        }
        RouteFieldKind::None | RouteFieldKind::FamilyTables { .. } => {
            panic!("Player carries a foreign family commitment")
        }
    }

    assert_eq!(
        model.route_transitions[&("Player".to_string(), "Mux".to_string())],
        CompilerRouteTransition { families_to_open: vec!["route_family/BoardState/mux".to_string()], families_to_pack: Vec::new() }
    );
}

#[test]
fn family_table_witnesses_follow_cut_transitions() {
    let path = PathBuf::from("transition-family-witnesses.ag");
    let module = crate::compiler::syntax::parser::parse_module(path.clone(), toy_chess_source()).expect("toy chess source parses");
    let program = Program { root: path, modules: vec![module] };
    let model = Model::from_program(&program).expect("toy chess model validates");

    let player = model.actor("Player").expect("Player exists");
    let enter_mux = player.entries.iter().find(|entry| entry.name == "enter_mux").expect("enter_mux exists");
    assert_eq!(
        entry_witness_specs(player, enter_mux, &model).expect("enter_mux witnesses lower").families,
        [RouteFamilyWitnessSpec { family_id: "route_family/BoardState/mux".to_string(), byte_len: 64 }]
    );

    let mux = model.actor("Mux").expect("Mux exists");
    let choose_pawn = mux.entries.iter().find(|entry| entry.name == "choose_pawn").expect("choose_pawn exists");
    assert!(entry_witness_specs(mux, choose_pawn, &model).expect("choose_pawn witnesses lower").families.is_empty());

    let read_only_mux = template_witness_specs_for_actor(player, &model, BTreeSet::from(["Mux".to_string()]), BTreeSet::new());
    assert!(read_only_mux.families.is_empty());
}

#[test]
fn in_app_observed_output_opens_the_target_family_cut() {
    let artifact = inline_artifact(
        "in-app-observed-family",
        r#"
            state ObserverState {
                cov_id game_id;
                int steps;
            }

            state BoardState {
                int turn;
            }

            actor Observer owns ObserverState {
                entry advance()
                observes game by self.game_id {
                    outputs {
                        mux: Mux,
                    }
                }
                emits next: Observer {
                    unrestricted(next.value);
                    BoardState board = { turn: 0 };
                    require game.outputs become {
                        mux <- Mux(board),
                    };
                    ObserverState next = {
                        game_id: game_id,
                        steps: steps + 1,
                    };
                    become next <- Observer(next);
                }
            }

            actor Mux owns BoardState {
                entry move() emits next: Pawn {
                    unrestricted(next.value);
                    BoardState next = { turn: turn + 1 };
                    become next <- Pawn(next);
                }
            }

            actor Pawn owns BoardState {
                entry finish() emits next: Mux {
                    unrestricted(next.value);
                    BoardState next = { turn: turn + 1 };
                    become next <- Mux(next);
                }
            }

            actor Knight owns BoardState {
                entry finish() emits next: Mux {
                    unrestricted(next.value);
                    BoardState next = { turn: turn + 1 };
                    become next <- Mux(next);
                }
            }

            app Test {
                actor Observer;
                actor Mux;
                actor Pawn;
                actor Knight;
            }
            "#,
    );

    let observer = artifact.argent.actors.iter().find(|actor| actor.name == "Observer").expect("Observer artifact exists");
    let advance = observer.entries.iter().find(|entry| entry.name == "advance").expect("advance entry exists");
    assert!(advance.hidden_params.iter().any(|param| param.purpose == HiddenParamPurposeArtifact::RouteFamilyTable));
    assert!(advance.hidden_params.iter().any(|param| {
        param.name == "gen__mux_prefix" && param.subject == HiddenParamSubjectArtifact::Actor { actor: "Mux".to_string() }
    }));
    assert!(advance.hidden_params.iter().any(|param| {
        param.name == "gen__mux_suffix" && param.subject == HiddenParamSubjectArtifact::Actor { actor: "Mux".to_string() }
    }));
    assert!(!advance.hidden_params.iter().any(|param| matches!(param.subject, HiddenParamSubjectArtifact::ObservedActor { .. })));
    artifact.verify_template_plan().expect("in-app observed output uses the planned target cut");
}

#[test]
fn in_app_observed_input_uses_a_direct_template_dependency() {
    let artifact = inline_artifact(
        "in-app-observed-input",
        r#"
            state ForeignState {
                int amount;
            }

            state LocalState {
                cov_id foreign_id;
            }

            state TargetState {
                int amount;
            }

            actor Foreign owns ForeignState {
                entry route() emits next: Target {
                    unrestricted(next.value);
                    TargetState next = { amount: amount };
                    become next <- Target(next);
                }
            }

            actor Local owns LocalState {
                entry inspect()
                observes remote by self.foreign_id {
                    inputs {
                        src: Foreign,
                    }
                }
                emits none {
                    require(remote.inputs.src.state.amount >= 0);
                }
            }

            actor Target owns TargetState {
                entry hold() emits none {
                    require(amount >= 0);
                }
            }

            app Test {
                actor Foreign;
                actor Local;
                actor Target;
            }
            "#,
    );

    assert_eq!(
        runtime_state_plan(&artifact, "Local")
            .expect("Local directly carries the observed input template")
            .field_roles
            .iter()
            .map(|field| (field.name.as_str(), field.role.clone()))
            .collect::<Vec<_>>(),
        vec![("gen__foreign_template", RuntimeFieldRoleArtifact::Template { contract: "Foreign".to_string() })]
    );
    let local = artifact.argent.actors.iter().find(|actor| actor.name == "Local").expect("Local artifact exists");
    let inspect = local.entries.iter().find(|entry| entry.name == "inspect").expect("inspect entry exists");
    assert_eq!(
        inspect.hidden_params.iter().map(|param| (param.name.as_str(), &param.subject, param.purpose)).collect::<Vec<_>>(),
        vec![
            (
                "gen__foreign_prefix_len",
                &HiddenParamSubjectArtifact::Actor { actor: "Foreign".to_string() },
                HiddenParamPurposeArtifact::TemplatePrefixLen,
            ),
            (
                "gen__foreign_suffix_len",
                &HiddenParamSubjectArtifact::Actor { actor: "Foreign".to_string() },
                HiddenParamPurposeArtifact::TemplateSuffixLen,
            ),
        ]
    );
}

#[test]
fn in_app_observed_output_reuses_a_current_input_template() {
    let artifact = inline_artifact(
        "in-app-observed-output-reuse",
        r#"
            state ForeignState {
                int amount;
            }

            state LocalState {
                cov_id remote_id;
            }

            actor Foreign owns ForeignState {
                entry hold() emits none {
                    require(amount >= 0);
                }
            }

            actor Local owns LocalState {
                entry step()
                consumes {
                    source: Foreign,
                }
                observes remote by self.remote_id {
                    outputs {
                        next: Foreign,
                    }
                }
                emits none {
                    ForeignState next = source;
                    require remote.outputs become {
                        next <- Foreign(next),
                    };
                }
            }

            app Test {
                actor Foreign;
                actor Local;
            }
            "#,
    );

    let local = artifact.argent.actors.iter().find(|actor| actor.name == "Local").expect("Local artifact exists");
    let step = local.entries.iter().find(|entry| entry.name == "step").expect("step entry exists");
    assert_eq!(
        step.hidden_params.iter().map(|param| (param.name.as_str(), &param.subject, param.purpose)).collect::<Vec<_>>(),
        vec![
            (
                "gen__foreign_prefix_len",
                &HiddenParamSubjectArtifact::Actor { actor: "Foreign".to_string() },
                HiddenParamPurposeArtifact::TemplatePrefixLen,
            ),
            (
                "gen__foreign_suffix_len",
                &HiddenParamSubjectArtifact::Actor { actor: "Foreign".to_string() },
                HiddenParamPurposeArtifact::TemplateSuffixLen,
            ),
        ]
    );
}

#[test]
fn in_app_current_output_reuses_an_observed_input_template() {
    let artifact = inline_artifact(
        "in-app-current-output-reuse",
        r#"
            state ForeignState {
                int amount;
            }

            state LocalState {
                cov_id remote_id;
            }

            actor Foreign owns ForeignState {
                entry hold() emits none {
                    require(amount >= 0);
                }
            }

            actor Local owns LocalState {
                entry step()
                observes remote by self.remote_id {
                    inputs {
                        source: Foreign,
                    }
                }
                emits next: Foreign {
                    unrestricted(next.value);
                    ForeignState next = remote.inputs.source.state;
                    become next <- Foreign(next);
                }
            }

            app Test {
                actor Foreign;
                actor Local;
            }
            "#,
    );

    let local = artifact.argent.actors.iter().find(|actor| actor.name == "Local").expect("Local artifact exists");
    let step = local.entries.iter().find(|entry| entry.name == "step").expect("step entry exists");
    assert_eq!(
        step.hidden_params.iter().map(|param| (param.name.as_str(), &param.subject, param.purpose)).collect::<Vec<_>>(),
        vec![
            (
                "gen__foreign_prefix_len",
                &HiddenParamSubjectArtifact::Actor { actor: "Foreign".to_string() },
                HiddenParamPurposeArtifact::TemplatePrefixLen,
            ),
            (
                "gen__foreign_suffix_len",
                &HiddenParamSubjectArtifact::Actor { actor: "Foreign".to_string() },
                HiddenParamPurposeArtifact::TemplateSuffixLen,
            ),
        ]
    );
}

#[test]
fn selected_gates_open_from_the_family_table_and_direct_consumes_stay_concrete() {
    let source = r#"
            state SourceState {
                int nonce;
            }

            state BoardState {
                int ply;
            }

            state ConsumerState {
                int nonce;
            }

            state ArchiveState {
                int nonce;
            }

            actor enum MoveActor {
                Pawn;
                Knight;
            }

            actor Source owns SourceState {
                entry enter_pawn() emits next: Pawn {
                    unrestricted(next.value);
                    BoardState next = {
                        ply: nonce,
                    };
                    become next <- Pawn(next);
                }
            }

            actor Mux owns BoardState {
                entry choose(MoveActor target) emits next: MoveActor {
                    unrestricted(next.value);
                    BoardState next = {
                        ply: ply + 1,
                    };
                    become next <- target(next);
                }
            }

            actor Pawn owns BoardState {
                entry inspect() emits none {
                    require(ply >= 0);
                }
            }

            actor Knight owns BoardState {
                entry inspect() emits none {
                    require(ply >= 0);
                }
            }

            actor Consumer owns ConsumerState {
                entry verify() consumes {
                    pawn: Pawn,
                } emits next: Archive {
                    unrestricted(next.value);
                    require(pawn.ply >= 0);

                    ArchiveState next = {
                        nonce: nonce + 1,
                    };
                    become next <- Archive(next);
                }
            }

            actor Archive owns ArchiveState {
                entry reopen() emits next: Pawn {
                    unrestricted(next.value);
                    BoardState next = {
                        ply: nonce,
                    };
                    become next <- Pawn(next);
                }
            }

            app SelectedGate {
                actor Source;
                actor Mux;
                actor Pawn;
                actor Knight;
                actor Consumer;
                actor Archive;
            }
        "#;
    let path = PathBuf::from("selected-family-gate.ag");
    let module = crate::compiler::syntax::parser::parse_module(path.clone(), source.to_string()).expect("source parses");
    let program = Program { root: path, modules: vec![module] };
    let model = Model::from_program(&program).expect("model validates");

    let family = model.route_family_for_actor("Pawn").expect("Pawn belongs to the selected family");
    assert!(family.direct_template_actors().is_empty());
    assert_eq!(family.table_actors(), ["Pawn", "Knight", "Mux"]);
    assert_eq!(family.rep(), "Pawn");

    let source_actor = model.actor("Source").expect("Source exists");
    let enter_pawn = source_actor.entries.first().expect("enter_pawn exists");
    let specs = entry_witness_specs(source_actor, enter_pawn, &model).expect("entry witnesses lower");
    let pawn = specs.templates.iter().find(|spec| spec.actor == "Pawn").expect("Pawn template witness exists");
    assert_eq!(pawn.source, TemplateWitnessSource::FamilyTable { family_id: family.id.clone(), offset: 0 });

    let actor_sil = actor_sil_for_model(&model);
    let source_sil = &actor_sil["Source"];
    assert!(source_sil.contains("byte[32] gen__pawn_template = byte[32](gen__pawn_routes.slice(0, 32));"), "{source_sil}");
    let consumer_sil = &actor_sil["Consumer"];
    assert!(consumer_sil.contains("byte[32] gen__pawn_template = gen__init_pawn_template;"), "{consumer_sil}");
    assert!(consumer_sil.contains("byte[32] gen__knight_template = gen__init_knight_template;"), "{consumer_sil}");
    assert!(consumer_sil.contains("byte[32] gen__mux_template = gen__init_mux_template;"), "{consumer_sil}");
    assert!(
        consumer_sil
            .contains("gen__pawn_routes_digest: blake3(byte[](gen__pawn_template + gen__knight_template + gen__mux_template)),"),
        "{consumer_sil}"
    );

    let artifact = emit_artifact(&program, &model, &actor_sil).expect("generated Sil compiles");
    artifact.verify_template_plan().expect("representative actors may be stored inside their family table");
}

#[test]
fn family_commitments_pack_on_planned_cut_transitions() {
    let source = toy_chess_source().replace(
        "            actor Mux owns BoardState {\n",
        r#"            actor Mux owns BoardState {
                entry return_to_player() emits next: Player {
                    unrestricted(next.value);
                    PlayerState next_player = {
                        nonce: ply,
                    };
                    become next <- Player(next_player);
                }

"#,
    );
    let path = PathBuf::from("family-pack-transition.ag");
    let module = crate::compiler::syntax::parser::parse_module(path.clone(), source.clone()).expect("source parses");
    let program = Program { root: path, modules: vec![module] };
    let model = Model::from_program(&program).expect("model validates");

    let family_id = "route_family/BoardState/mux".to_string();
    assert_eq!(
        model.route_transition("Mux", "Player"),
        Some(&CompilerRouteTransition { families_to_open: Vec::new(), families_to_pack: vec![family_id] })
    );

    let mux_sil = emit_actor(model.actor("Mux").expect("Mux exists"), &model).expect("Mux Sil emits");
    assert!(mux_sil.contains("PlayerState next_player = PlayerState {"), "{mux_sil}");
    assert!(mux_sil.contains("gen__mux_routes_digest: blake3(byte[](gen__mux_routes)),"), "{mux_sil}");
    assert!(!mux_sil.contains("gen__mux_routes_digest: gen__mux_routes_digest,"), "{mux_sil}");

    inline_artifact("family-pack-transition", &source);
}

#[test]
fn route_neutral_state_locals_convert_at_actor_routes() {
    let straight_path = PathBuf::from("examples/route_state_bodies.ag");
    let straight_source = fs::read_to_string(&straight_path).expect("route state body example exists");
    let straight_module =
        crate::compiler::syntax::parser::parse_module(straight_path.clone(), straight_source.clone()).expect("example parses");
    let straight_program = Program { root: straight_path, modules: vec![straight_module] };
    let straight_model = Model::from_program(&straight_program).expect("example model validates");
    let straight_sil = actor_sil_for_model(&straight_model);

    assert!(straight_sil["Lobby"].contains("struct BoardState {"), "{}", straight_sil["Lobby"]);
    assert!(straight_sil["Lobby"].contains("BoardState next_board = BoardState {"), "{}", straight_sil["Lobby"]);
    assert!(
        straight_sil["Lobby"].contains("Gen__MuxState gen__state_next_gen__mux_state = Gen__MuxState {"),
        "{}",
        straight_sil["Lobby"]
    );
    assert!(straight_sil["Mux"].contains("struct ArchiveState {"), "{}", straight_sil["Mux"]);
    assert!(straight_sil["Mux"].contains("ArchiveState next_archive = ArchiveState {"), "{}", straight_sil["Mux"]);
    assert!(
        straight_sil["Mux"].contains("Gen__ArchiveState gen__state_next_gen__archive_state = Gen__ArchiveState {"),
        "{}",
        straight_sil["Mux"]
    );
    assert!(straight_sil["Archive"].contains("BoardState next_board = BoardState {"), "{}", straight_sil["Archive"]);
    assert!(
        straight_sil["Archive"].contains("Gen__PawnState gen__state_next_gen__pawn_state = Gen__PawnState {"),
        "{}",
        straight_sil["Archive"]
    );

    let choice_path = PathBuf::from("examples/route_state_body_choice.ag");
    let choice_source = fs::read_to_string(&choice_path).expect("route state body choice example exists");
    let choice_module = crate::compiler::syntax::parser::parse_module(choice_path.clone(), choice_source).expect("example parses");
    let choice_program = Program { root: choice_path, modules: vec![choice_module] };
    let choice_model = Model::from_program(&choice_program).expect("example model validates");
    let choice_sil = emit_actor(choice_model.actor("Lobby").expect("Lobby exists"), &choice_model).expect("Lobby Sil emits");

    assert!(choice_sil.contains("struct BoardState {"), "{choice_sil}");
    assert!(choice_sil.contains("BoardState next_board = BoardState {"), "{choice_sil}");
    assert!(choice_sil.contains("Gen__MuxState gen__state_next_gen__mux_state = Gen__MuxState {"), "{choice_sil}");
    assert!(
        choice_sil.contains("validateOutputStateWithTemplate(\n                gen__next_output_idx,\n                next_board,"),
        "{choice_sil}"
    );
    assert!(choice_sil.contains("ply: next_board.ply,"), "{choice_sil}");
}

#[test]
fn direct_route_families_are_inferred_without_hints() {
    let artifact = inline_artifact("toy-chess-family", &toy_chess_source());
    let families = artifact.argent.template_plan.route_families.iter().map(|family| {
        (
            family.id.as_str(),
            family.state.as_str(),
            family.representative_actor.as_str(),
            family.entry_actors.iter().map(String::as_str).collect::<Vec<_>>(),
            family.table_id.as_str(),
            family.actors.iter().map(String::as_str).collect::<Vec<_>>(),
        )
    });

    assert_eq!(
        families.collect::<Vec<_>>(),
        vec![(
            "route_family/BoardState/mux",
            "BoardState",
            "Mux",
            vec!["Mux"],
            "route_table/BoardState/gen__mux_routes",
            vec!["Mux", "Pawn", "Knight"]
        )]
    );

    let board_table = artifact
        .argent
        .template_plan
        .route_tables
        .iter()
        .find(|table| table.id == route_template_table_receipt_id("BoardState", "gen__mux_routes"))
        .expect("BoardState route table exists");
    assert_eq!(board_table.byte_len, 64);
    assert_eq!(
        board_table.entries.iter().map(|entry| entry.leaf.clone()).collect::<Vec<_>>(),
        vec![
            RouteTemplateLeafArtifact::Template { actor: "Pawn".to_string(), template_id: "template/pawn".to_string() },
            RouteTemplateLeafArtifact::Template { actor: "Knight".to_string(), template_id: "template/knight".to_string() },
        ]
    );

    assert_eq!(
        artifact
            .argent
            .actor_enums
            .iter()
            .map(|actor_enum| {
                (
                    actor_enum.name.as_str(),
                    actor_enum.state.as_str(),
                    actor_enum.variants.iter().map(String::as_str).collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>(),
        vec![("MoveActor", "BoardState", vec!["Pawn", "Knight"])]
    );

    assert_eq!(
        runtime_state_plan(&artifact, "Player")
            .expect("Player runtime role overlay exists")
            .field_roles
            .iter()
            .map(|field| (field.name.as_str(), field.role.clone()))
            .collect::<Vec<_>>(),
        vec![
            ("gen__mux_template", RuntimeFieldRoleArtifact::Template { contract: "Mux".to_string() }),
            ("gen__mux_routes_digest", RuntimeFieldRoleArtifact::TemplateDigest { id: "route_family/BoardState/mux".to_string() }),
        ]
    );

    assert_eq!(
        runtime_state_plan(&artifact, "Mux").expect("Mux runtime role overlay exists").field_roles[..2]
            .iter()
            .map(|field| (field.name.as_str(), field.role.clone()))
            .collect::<Vec<_>>(),
        vec![
            ("gen__mux_template", RuntimeFieldRoleArtifact::Template { contract: "Mux".to_string() }),
            ("gen__mux_routes", RuntimeFieldRoleArtifact::TemplateTable { contracts: vec!["Pawn".to_string(), "Knight".to_string()] }),
        ]
    );

    let player_actor = artifact.argent.actors.iter().find(|actor| actor.name == "Player").expect("Player actor exists");
    let enter_mux = player_actor.entries.iter().find(|entry| entry.name == "enter_mux").expect("enter_mux entry exists");
    assert_eq!(
        enter_mux
            .hidden_params
            .iter()
            .map(|param| (param.name.as_str(), subject_label(&param.subject), param.purpose, param.route_proof_id.as_deref()))
            .collect::<Vec<_>>(),
        vec![
            ("gen__mux_prefix", "Mux", HiddenParamPurposeArtifact::TemplatePrefixBytes, None),
            ("gen__mux_suffix", "Mux", HiddenParamPurposeArtifact::TemplateSuffixBytes, None),
            ("gen__mux_routes", "route_family/BoardState/mux", HiddenParamPurposeArtifact::RouteFamilyTable, None),
        ]
    );

    let mux_actor = artifact.argent.actors.iter().find(|actor| actor.name == "Mux").expect("Mux actor exists");
    let choose = mux_actor.entries.iter().find(|entry| entry.name == "choose").expect("choose entry exists");
    assert_eq!(
        choose
            .template_selectors
            .iter()
            .map(|selector| {
                (
                    selector.name.as_str(),
                    selector.actor_enum.as_str(),
                    selector.state.as_str(),
                    selector.variants.iter().map(String::as_str).collect::<Vec<_>>(),
                    selector.fixed_actor.as_deref(),
                )
            })
            .collect::<Vec<_>>(),
        vec![("target", "MoveActor", "BoardState", vec!["Pawn", "Knight"], None)]
    );
    assert_eq!(
        choose
            .hidden_params
            .iter()
            .map(|param| (param.name.as_str(), subject_label(&param.subject), param.purpose))
            .collect::<Vec<_>>(),
        vec![
            ("gen__target_prefix", "target", HiddenParamPurposeArtifact::TemplatePrefixBytes),
            ("gen__target_suffix", "target", HiddenParamPurposeArtifact::TemplateSuffixBytes),
        ]
    );
    let choose_knight_const =
        mux_actor.entries.iter().find(|entry| entry.name == "choose_knight_const").expect("choose_knight_const entry exists");
    assert_eq!(
        choose_knight_const
            .template_selectors
            .iter()
            .map(|selector| {
                (
                    selector.name.as_str(),
                    selector.actor_enum.as_str(),
                    selector.state.as_str(),
                    selector.variants.iter().map(String::as_str).collect::<Vec<_>>(),
                    selector.fixed_actor.as_deref(),
                )
            })
            .collect::<Vec<_>>(),
        vec![("target", "MoveActor", "BoardState", vec!["Pawn", "Knight"], Some("Knight"))]
    );
    assert_eq!(choose_knight_const.routes.iter().map(|route| route.actor.as_str()).collect::<Vec<_>>(), vec!["Knight"]);
    artifact.verify_template_plan().expect("template plan receipt verifies inferred route family");
}

#[test]
fn toy_chess_sil_uses_one_level_route_family_shape() {
    let path = PathBuf::from("toy-chess-shape.ag");
    let module = crate::compiler::syntax::parser::parse_module(path.clone(), toy_chess_source()).expect("toy chess source parses");
    let program = Program { root: path, modules: vec![module] };
    let model = Model::from_program(&program).expect("toy chess model validates");
    let actor_sil = actor_sil_for_model(&model);

    let league_sil = actor_sil.get("League").expect("League Sil is emitted");
    assert!(league_sil.contains("byte[32] gen__init_mux_template"), "{league_sil}");
    assert!(league_sil.contains("byte[32] gen__init_mux_routes_digest"), "{league_sil}");
    assert!(league_sil.contains("byte[32] gen__mux_routes_digest = gen__init_mux_routes_digest;"), "{league_sil}");
    assert!(!league_sil.contains("gen__pawn_template"), "{league_sil}");
    assert!(!league_sil.contains("gen__knight_template"), "{league_sil}");
    assert!(!league_sil.contains("byte[64] gen__init_mux_routes"), "{league_sil}");
    assert!(!league_sil.contains("byte[64] gen__mux_routes = gen__init_mux_routes;"), "{league_sil}");

    let player_sil = actor_sil.get("Player").expect("Player Sil is emitted");
    assert!(player_sil.contains("byte[32] gen__init_mux_template"), "{player_sil}");
    assert!(player_sil.contains("byte[32] gen__init_mux_routes_digest"), "{player_sil}");
    assert!(player_sil.contains("entry enter_mux("), "{player_sil}");
    assert!(player_sil.contains("byte[] gen__mux_prefix,"), "{player_sil}");
    assert!(player_sil.contains("byte[] gen__mux_suffix,"), "{player_sil}");
    assert!(player_sil.contains("byte[64] gen__mux_routes"), "{player_sil}");
    assert!(player_sil.contains("require(blake3(byte[](gen__mux_routes)) == gen__mux_routes_digest);"), "{player_sil}");
    assert!(player_sil.contains("BoardState next_board = BoardState {"), "{player_sil}");
    assert!(player_sil.contains("Gen__MuxState gen__state_next_gen__mux_state = Gen__MuxState {"), "{player_sil}");
    assert!(!player_sil.contains("gen__pawn_template"), "{player_sil}");
    assert!(!player_sil.contains("gen__knight_template"), "{player_sil}");

    let mux_sil = actor_sil.get("Mux").expect("Mux Sil is emitted");
    assert!(mux_sil.contains("byte[64] gen__init_mux_routes"), "{mux_sil}");
    assert!(mux_sil.contains("byte[64] gen__mux_routes = gen__init_mux_routes;"), "{mux_sil}");
    assert!(mux_sil.contains("entry choose(int target, byte[] gen__target_prefix, byte[] gen__target_suffix)"), "{mux_sil}");
    assert!(mux_sil.contains("if (target == 1 /*KNIGHT*/)"), "{mux_sil}");
    assert!(mux_sil.contains("int gen__target_selector = target;"), "{mux_sil}");
    assert!(mux_sil.contains("require(gen__target_selector >= 0);"), "{mux_sil}");
    assert!(mux_sil.contains("require(gen__target_selector < 2);"), "{mux_sil}");
    assert!(mux_sil.contains("byte[32] gen__target_template = byte[32]("), "{mux_sil}");
    assert!(mux_sil.contains("gen__mux_routes.slice(gen__target_selector * 32, gen__target_selector * 32 + 32)"), "{mux_sil}");
    assert!(mux_sil.contains("validateOutputStateWithTemplate(\n            gen__next_output_idx,"), "{mux_sil}");
    assert!(mux_sil.contains("gen__target_prefix,"), "{mux_sil}");
    assert!(mux_sil.contains("gen__target_template"), "{mux_sil}");
    assert!(mux_sil.contains("entry choose_knight_const(byte[] gen__target_prefix, byte[] gen__target_suffix)"), "{mux_sil}");
    assert!(mux_sil.contains("int gen__target_selector = 1 /*KNIGHT*/;"), "{mux_sil}");
    assert!(mux_sil.contains("byte[32] gen__pawn_template = byte[32](gen__mux_routes.slice(0, 32));"), "{mux_sil}");
    assert!(mux_sil.contains("byte[32] gen__knight_template = byte[32](gen__mux_routes.slice(32, 64));"), "{mux_sil}");
    assert!(mux_sil.contains("gen__pawn_prefix,"), "{mux_sil}");
    assert!(mux_sil.contains("gen__pawn_template"), "{mux_sil}");
    assert!(mux_sil.contains("gen__knight_prefix,"), "{mux_sil}");
    assert!(mux_sil.contains("gen__knight_template"), "{mux_sil}");

    let pawn_sil = actor_sil.get("Pawn").expect("Pawn Sil is emitted");
    assert!(pawn_sil.contains("byte[64] gen__init_mux_routes"), "{pawn_sil}");
    assert!(pawn_sil.contains("byte[64] gen__mux_routes = gen__init_mux_routes;"), "{pawn_sil}");
    assert!(!pawn_sil.contains("gen__pawn_template"), "{pawn_sil}");
    assert!(!pawn_sil.contains("gen__knight_template"), "{pawn_sil}");
}

#[test]
fn route_family_state_keeps_downstream_templates() {
    let path = PathBuf::from("route-family-with-downstream-actor.ag");
    let module = crate::compiler::syntax::parser::parse_module(
        path.clone(),
        r#"
            state BoardState {
                int ply;
            }

            state ReceiptState {
                int final_ply;
            }

            actor enum MoveActor {
                Pawn;
                Knight;
            }

            actor Mux owns BoardState {
                entry choose(MoveActor target) emits next: MoveActor {
                    unrestricted(next.value);
                    BoardState next_board = {
                        ply: ply + 1,
                    };
                    become next <- target(next_board);
                }

                entry finish() emits next: Receipt {
                    unrestricted(next.value);
                    ReceiptState receipt = {
                        final_ply: ply,
                    };
                    become next <- Receipt(receipt);
                }
            }

            actor Pawn owns BoardState {
                entry back_to_mux() emits next: Mux {
                    unrestricted(next.value);
                    BoardState next_board = {
                        ply: ply + 1,
                    };
                    become next <- Mux(next_board);
                }
            }

            actor Knight owns BoardState {
                entry back_to_mux() emits next: Mux {
                    unrestricted(next.value);
                    BoardState next_board = {
                        ply: ply + 1,
                    };
                    become next <- Mux(next_board);
                }
            }

            actor Receipt owns ReceiptState {
                entry resume() emits next: Mux {
                    unrestricted(next.value);
                    BoardState next_board = {
                        ply: final_ply + 1,
                    };
                    become next <- Mux(next_board);
                }
            }

            app RoutedLifecycle {
                actor Mux;
                actor Pawn;
                actor Knight;
                actor Receipt;
            }
            "#
        .to_string(),
    )
    .expect("source parses");
    let program = Program { root: path, modules: vec![module] };
    let model = Model::from_program(&program).expect("model validates");
    let actor_sil = actor_sil_for_model(&model);

    let mux_sil = actor_sil.get("Mux").expect("Mux Sil is emitted");
    assert!(mux_sil.contains("byte[32] gen__receipt_template = gen__init_receipt_template;"), "{mux_sil}");
    assert!(mux_sil.contains("byte[64] gen__mux_routes = gen__init_mux_routes;"), "{mux_sil}");
    assert!(mux_sil.contains("gen__mux_routes_digest: blake3(byte[](gen__mux_routes)),"), "{mux_sil}");

    emit_artifact(&program, &model, &actor_sil).expect("generated Sil compiles");
}

#[test]
fn actor_enum_order_drives_route_table_order() {
    let source = toy_chess_source().replace(
        "actor enum MoveActor {\n                Pawn;\n                Knight;\n            }",
        "actor enum MoveActor {\n                Knight;\n                Pawn;\n            }",
    );
    let path = PathBuf::from("toy-chess-selector-order.ag");
    let module = crate::compiler::syntax::parser::parse_module(path.clone(), source).expect("toy chess source parses");
    let program = Program { root: path, modules: vec![module] };
    let model = Model::from_program(&program).expect("reordered selector enum defines route table order");
    let actor_sil = actor_sil_for_model(&model);
    let artifact = emit_artifact(&program, &model, &actor_sil).expect("artifact emits");

    let board_table = artifact
        .argent
        .template_plan
        .route_tables
        .iter()
        .find(|table| table.id == route_template_table_receipt_id("BoardState", "gen__mux_routes"))
        .expect("BoardState route table exists");
    assert_eq!(
        board_table.entries.iter().map(|entry| entry.leaf.clone()).collect::<Vec<_>>(),
        vec![
            RouteTemplateLeafArtifact::Template { actor: "Knight".to_string(), template_id: "template/knight".to_string() },
            RouteTemplateLeafArtifact::Template { actor: "Pawn".to_string(), template_id: "template/pawn".to_string() },
        ]
    );
    assert_eq!(
        runtime_state_plan(&artifact, "Mux").expect("Mux runtime role overlay exists").field_roles[1].role,
        RuntimeFieldRoleArtifact::TemplateTable { contracts: vec!["Knight".to_string(), "Pawn".to_string()] }
    );

    let mux_sil = actor_sil.get("Mux").expect("Mux Sil is emitted");
    assert!(mux_sil.contains("if (target == 0 /*KNIGHT*/)"), "{mux_sil}");
    assert!(mux_sil.contains("int gen__target_selector = 0 /*KNIGHT*/;"), "{mux_sil}");
    assert!(mux_sil.contains("byte[32] gen__knight_template = byte[32](gen__mux_routes.slice(0, 32));"), "{mux_sil}");
    assert!(mux_sil.contains("byte[32] gen__pawn_template = byte[32](gen__mux_routes.slice(32, 64));"), "{mux_sil}");
}

#[test]
fn gate_less_family_appends_rep_after_selector_variants() {
    let path = PathBuf::from("fixed-selector-table.ag");
    let module = crate::compiler::syntax::parser::parse_module(
        path.clone(),
        r#"
            state BoardState {
                int ply;
            }

            actor enum MoveActor {
                Pawn;
                Knight;
            }

            actor Mux owns BoardState {
                entry choose_knight_const() emits next: MoveActor {
                    unrestricted(next.value);
                    BoardState next_board = {
                        ply: ply + 1,
                    };

                    actor_type<BoardState> target = MoveActor::Knight;
                    become next <- target(next_board);
                }
            }

            actor Pawn owns BoardState {
                entry idle() emits none {
                    require(ply >= 0);
                }
            }

            actor Knight owns BoardState {
                entry idle() emits none {
                    require(ply >= 0);
                }
            }

            app FixedSelectorTable {
                actor Mux;
                actor Pawn;
                actor Knight;
            }
            "#
        .to_string(),
    )
    .expect("source parses");
    let program = Program { root: path, modules: vec![module] };
    let model = Model::from_program(&program).expect("fixed selector still infers the full enum table");
    let actor_sil = actor_sil_for_model(&model);
    let artifact = emit_artifact(&program, &model, &actor_sil).expect("artifact emits");

    let family = artifact.argent.template_plan.route_families.first().expect("route family is inferred");
    assert!(family.entry_actors.is_empty());
    assert_eq!(family.representative_actor, "Mux");

    let board_table = artifact
        .argent
        .template_plan
        .route_tables
        .iter()
        .find(|table| table.id == route_template_table_receipt_id("BoardState", "gen__mux_routes"))
        .expect("BoardState route table exists");
    assert_eq!(
        board_table.entries.iter().map(|entry| entry.leaf.clone()).collect::<Vec<_>>(),
        vec![
            RouteTemplateLeafArtifact::Template { actor: "Pawn".to_string(), template_id: "template/pawn".to_string() },
            RouteTemplateLeafArtifact::Template { actor: "Knight".to_string(), template_id: "template/knight".to_string() },
            RouteTemplateLeafArtifact::Template { actor: "Mux".to_string(), template_id: "template/mux".to_string() },
        ]
    );

    let mux_actor = artifact.argent.actors.iter().find(|actor| actor.name == "Mux").expect("Mux actor exists");
    let choose_knight_const = mux_actor.entries.iter().find(|entry| entry.name == "choose_knight_const").expect("entry exists");
    assert_eq!(
        choose_knight_const
            .template_selectors
            .iter()
            .map(|selector| (selector.name.as_str(), selector.fixed_actor.as_deref()))
            .collect::<Vec<_>>(),
        vec![("target", Some("Knight"))]
    );
    assert_eq!(choose_knight_const.routes.iter().map(|route| route.actor.as_str()).collect::<Vec<_>>(), vec!["Knight"]);

    let mux_sil = actor_sil.get("Mux").expect("Mux Sil is emitted");
    assert!(mux_sil.contains("int gen__target_selector = 1 /*KNIGHT*/;"), "{mux_sil}");
    assert!(mux_sil.contains("require(gen__target_selector < 2);"), "{mux_sil}");
    assert!(mux_sil.contains("byte[32] gen__target_template = byte[32]("), "{mux_sil}");
    assert!(mux_sil.contains("gen__mux_routes.slice(gen__target_selector * 32, gen__target_selector * 32 + 32)"), "{mux_sil}");
    artifact.verify_template_plan().expect("template plan receipt verifies");
}

#[test]
fn actor_enum_local_drives_selector_domain_and_route_expansion() {
    let path = PathBuf::from("local-actor-enum-selector.ag");
    let module = crate::compiler::syntax::parser::parse_module(
        path.clone(),
        r#"
            state BoardState {
                int ply;
            }

            actor enum MoveActor {
                Pawn;
                Knight;
            }

            actor Mux owns BoardState {
                entry choose(int index) emits next: MoveActor {
                    unrestricted(next.value);
                    MoveActor target = MoveActor[index];
                    BoardState next_state = {
                        ply: ply + 1,
                    };
                    become next <- target(next_state);
                }
            }

            actor Pawn owns BoardState {
                entry idle() emits none {
                    require(ply >= 0);
                }
            }

            actor Knight owns BoardState {
                entry idle() emits none {
                    require(ply >= 0);
                }
            }

            app LocalSelector {
                actor Mux;
                actor Pawn;
                actor Knight;
            }
            "#
        .to_string(),
    )
    .expect("source parses");
    let program = Program { root: path, modules: vec![module] };
    let model = Model::from_program(&program).expect("local actor enum defines a selector domain");
    let actor_sil = actor_sil_for_model(&model);
    let artifact = emit_artifact(&program, &model, &actor_sil).expect("generated Sil compiles");

    let mux = artifact.argent.actors.iter().find(|actor| actor.name == "Mux").expect("Mux actor exists");
    let choose = mux.entries.iter().find(|entry| entry.name == "choose").expect("choose entry exists");
    assert_eq!(choose.template_selectors.iter().map(|selector| selector.name.as_str()).collect::<Vec<_>>(), vec!["target"]);
    assert_eq!(choose.routes.iter().map(|route| route.actor.as_str()).collect::<Vec<_>>(), vec!["Pawn", "Knight"]);

    let mux_sil = actor_sil.get("Mux").expect("Mux Sil is emitted");
    assert!(mux_sil.contains("int target = index;"), "{mux_sil}");
    assert!(mux_sil.contains("int gen__target_selector = target;"), "{mux_sil}");
    artifact.verify_template_plan().expect("local selector route plan verifies");
}

#[test]
fn actor_enums_over_same_route_table_must_use_one_order() {
    let source = r#"
            state BoardState {
                int selector;
                int ply;
            }

            actor enum FirstMove {
                Pawn;
                Knight;
            }

            actor enum SecondMove {
                Knight;
                Pawn;
            }

            actor Mux owns BoardState {
                entry choose_first(FirstMove target) emits next: FirstMove {
                    unrestricted(next.value);
                    BoardState next_board = {
                        selector: selector,
                        ply: ply + 1,
                    };
                    become next <- target(next_board);
                }

                entry choose_second(SecondMove target) emits next: SecondMove {
                    unrestricted(next.value);
                    BoardState next_board = {
                        selector: selector,
                        ply: ply + 1,
                    };
                    become next <- target(next_board);
                }
            }

            actor Pawn owns BoardState {}
            actor Knight owns BoardState {}

            app ConflictingSelectorOrder {
                actor Mux;
                actor Pawn;
                actor Knight;
            }
        "#;
    let path = PathBuf::from("conflicting-selector-order.ag");
    let module = crate::compiler::syntax::parser::parse_module(path.clone(), source.to_string()).expect("source parses");
    let program = Program { root: path, modules: vec![module] };

    let err = Model::from_program(&program).expect_err("conflicting actor enum orders must be rejected");
    assert!(err.to_string().contains("conflicts with requirement"), "unexpected error: {err}");
}

#[test]
fn actor_enum_variants_form_a_prefix_of_the_inferred_route_family() {
    let source = r#"
            state BoardState {
                int selector;
                int ply;
            }

            actor enum MoveActor {
                Pawn;
                Knight;
            }

            actor Mux owns BoardState {
                entry choose(MoveActor target) emits next: MoveActor {
                    unrestricted(next.value);
                    BoardState next_board = {
                        selector: selector,
                        ply: ply + 1,
                    };
                    become next <- target(next_board);
                }

                entry visit_bishop() emits next: Bishop {
                    unrestricted(next.value);
                    BoardState next_board = {
                        selector: selector,
                        ply: ply + 1,
                    };
                    become next <- Bishop(next_board);
                }
            }

            actor Pawn owns BoardState {}
            actor Knight owns BoardState {}
            actor Bishop owns BoardState {}

            app SelectorPrefix {
                actor Mux;
                actor Pawn;
                actor Knight;
                actor Bishop;
            }
        "#;
    let path = PathBuf::from("selector-prefix.ag");
    let module = crate::compiler::syntax::parser::parse_module(path.clone(), source.to_string()).expect("source parses");
    let program = Program { root: path, modules: vec![module] };

    let model = Model::from_program(&program).expect("selector variants may prefix other family actors");

    assert_eq!(model.route_families.len(), 1);
    assert_eq!(model.route_families[0].table_actors, ["Pawn", "Knight", "Mux", "Bishop"]);
}

#[test]
fn selector_can_include_its_source_actor() {
    let artifact = inline_artifact(
        "self-selector-variant",
        r#"
            state SharedState {
                int amount;
            }

            actor enum NextActor {
                Worker;
                Challenge;
            }

            actor Worker owns SharedState {
                entry inspect() emits none {
                    require(amount >= 0);
                }
            }

            actor Challenge owns SharedState {
                entry choose(NextActor target) emits next: NextActor {
                    unrestricted(next.value);
                    SharedState next = {
                        amount: amount + 1,
                    };
                    become next <- target(next);
                }
            }

            app SelfSelectorVariant {
                actor Worker;
                actor Challenge;
            }
            "#,
    );

    artifact.verify_template_plan().expect("self selector variant has a valid identity cut transition");
}

#[test]
fn rejects_actor_enum_variants_with_different_owned_states() {
    let artifact_source = r#"
            state AState {
                int n;
            }

            state BState {
                int n;
            }

            actor A owns AState {}
            actor B owns BState {}

            actor enum MixedActor {
                A;
                B;
            }

            app BadEnum {
                actor A;
                actor B;
            }
            "#;
    let path = PathBuf::from("bad-actor-enum.ag");
    let module = crate::compiler::syntax::parser::parse_module(path.clone(), artifact_source.to_string()).expect("source parses");
    let program = Program { root: path, modules: vec![module] };

    let err = Model::from_program(&program).expect_err("mixed actor enum state must be rejected");
    assert!(err.to_string().contains("variant `B` owns state `BState`, expected `AState`"), "unexpected error: {err}");
}

#[test]
fn two_actor_routes_use_direct_template_fields() {
    let artifact = inline_artifact(
        "genesis-route-family",
        r#"
            state BoardState {
                int n;
            }

            actor A owns BoardState {
                entry to_b() emits next: B {
                    unrestricted(next.value);
                    BoardState next = {
                        n: n + 1,
                    };

                    become next <- B(next);
                }
            }

            actor B owns BoardState {
                entry to_a() emits next: A {
                    unrestricted(next.value);
                    BoardState next = {
                        n: n + 1,
                    };

                    become next <- A(next);
                }
            }

            app GenesisFamily {
                actor A;
                actor B;
            }
            "#,
    );

    assert!(artifact.argent.template_plan.route_families.is_empty());
    assert!(artifact.argent.template_plan.route_tables.is_empty());

    assert_eq!(
        runtime_state_plan(&artifact, "A")
            .expect("A runtime role overlay exists")
            .field_roles
            .iter()
            .map(|field| (field.name.as_str(), field.role.clone()))
            .collect::<Vec<_>>(),
        vec![
            ("gen__a_template", RuntimeFieldRoleArtifact::Template { contract: "A".to_string() }),
            ("gen__b_template", RuntimeFieldRoleArtifact::Template { contract: "B".to_string() }),
        ]
    );
    artifact.verify_template_plan().expect("direct two-actor route plan verifies");
}

#[test]
fn route_family_state_can_have_multiple_disconnected_families() {
    let artifact = inline_artifact(
        "multi-family-route-state",
        r#"
            state BoardState {
                int n;
            }

            actor A owns BoardState {
                entry to_b() emits next: B {
                    unrestricted(next.value);
                    BoardState next = {
                        n: n + 1,
                    };

                    become next <- B(next);
                }
            }

            actor B owns BoardState {
                entry to_c() emits next: C {
                    unrestricted(next.value);
                    BoardState next = {
                        n: n + 1,
                    };

                    become next <- C(next);
                }
            }

            actor C owns BoardState {
                entry to_a() emits next: A {
                    unrestricted(next.value);
                    BoardState next = {
                        n: n + 1,
                    };

                    become next <- A(next);
                }
            }

            actor D owns BoardState {
                entry to_e() emits next: E {
                    unrestricted(next.value);
                    BoardState next = {
                        n: n + 1,
                    };

                    become next <- E(next);
                }
            }

            actor E owns BoardState {
                entry to_f() emits next: F {
                    unrestricted(next.value);
                    BoardState next = {
                        n: n + 1,
                    };

                    become next <- F(next);
                }
            }

            actor F owns BoardState {
                entry to_d() emits next: D {
                    unrestricted(next.value);
                    BoardState next = {
                        n: n + 1,
                    };

                    become next <- D(next);
                }
            }

            app MultiFamilyState {
                actor A;
                actor B;
                actor C;
                actor D;
                actor E;
                actor F;
            }
            "#,
    );

    let families = artifact
        .argent
        .template_plan
        .route_families
        .iter()
        .map(|family| {
            (
                family.id.as_str(),
                family.representative_actor.as_str(),
                family.actors.iter().map(String::as_str).collect::<Vec<_>>(),
                family.table_id.as_str(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        families,
        vec![
            ("route_family/BoardState/a", "A", vec!["A", "B", "C"], "route_table/BoardState/gen__a_routes"),
            ("route_family/BoardState/d", "D", vec!["D", "E", "F"], "route_table/BoardState/gen__d_routes"),
        ]
    );

    assert_eq!(
        artifact
            .argent
            .template_plan
            .route_tables
            .iter()
            .map(|table| { (table.id.as_str(), table.entries.iter().map(|entry| entry.leaf.clone()).collect::<Vec<_>>(),) })
            .collect::<Vec<_>>(),
        vec![
            (
                "route_table/BoardState/gen__a_routes",
                vec![
                    RouteTemplateLeafArtifact::Template { actor: "A".to_string(), template_id: "template/a".to_string() },
                    RouteTemplateLeafArtifact::Template { actor: "B".to_string(), template_id: "template/b".to_string() },
                    RouteTemplateLeafArtifact::Template { actor: "C".to_string(), template_id: "template/c".to_string() },
                ],
            ),
            (
                "route_table/BoardState/gen__d_routes",
                vec![
                    RouteTemplateLeafArtifact::Template { actor: "D".to_string(), template_id: "template/d".to_string() },
                    RouteTemplateLeafArtifact::Template { actor: "E".to_string(), template_id: "template/e".to_string() },
                    RouteTemplateLeafArtifact::Template { actor: "F".to_string(), template_id: "template/f".to_string() },
                ],
            ),
        ]
    );

    assert_eq!(
        runtime_state_plan(&artifact, "A")
            .expect("A runtime role overlay exists")
            .field_roles
            .iter()
            .map(|field| (field.name.as_str(), field.role.clone()))
            .collect::<Vec<_>>(),
        vec![(
            "gen__a_routes",
            RuntimeFieldRoleArtifact::TemplateTable { contracts: vec!["A".to_string(), "B".to_string(), "C".to_string()] }
        ),]
    );
    assert_eq!(
        runtime_state_plan(&artifact, "D")
            .expect("D runtime role overlay exists")
            .field_roles
            .iter()
            .map(|field| (field.name.as_str(), field.role.clone()))
            .collect::<Vec<_>>(),
        vec![(
            "gen__d_routes",
            RuntimeFieldRoleArtifact::TemplateTable { contracts: vec!["D".to_string(), "E".to_string(), "F".to_string()] }
        ),]
    );
    artifact.verify_template_plan().expect("multi-family route state receipt verifies");
}

#[test]
fn route_family_with_one_table_actor_uses_direct_template_fields() {
    let artifact = inline_artifact(
        "single-entry-route-table",
        r#"
            state PlayerState {
                int n;
            }

            state BoardState {
                int n;
            }

            actor PlayerA owns PlayerState {
                entry enter_a() emits next: HubA {
                    unrestricted(next.value);
                    BoardState next = {
                        n: n,
                    };

                    become next <- HubA(next);
                }
            }

            actor PlayerB owns PlayerState {
                entry enter_b() emits next: HubB {
                    unrestricted(next.value);
                    BoardState next = {
                        n: n,
                    };

                    become next <- HubB(next);
                }
            }

            actor HubB owns BoardState {
                entry to_leaf() emits next: Leaf {
                    unrestricted(next.value);
                    BoardState next = {
                        n: n + 1,
                    };

                    become next <- Leaf(next);
                }
            }

            actor HubA owns BoardState {
                entry to_leaf() emits next: Leaf {
                    unrestricted(next.value);
                    BoardState next = {
                        n: n + 1,
                    };

                    become next <- Leaf(next);
                }
            }

            actor Leaf owns BoardState {
                entry idle() emits none {
                    require(n >= 0);
                }
            }

            app SingleEntryRouteTable {
                actor PlayerA;
                actor PlayerB;
                actor HubB;
                actor HubA;
                actor Leaf;
            }
            "#,
    );

    assert!(artifact.argent.template_plan.route_families.is_empty());
    assert!(artifact.argent.template_plan.route_tables.is_empty());
    assert_eq!(
        runtime_state_plan(&artifact, "HubB")
            .expect("HubB runtime role overlay exists")
            .field_roles
            .iter()
            .map(|field| (field.name.as_str(), field.role.clone()))
            .collect::<Vec<_>>(),
        vec![
            ("gen__hub_a_template", RuntimeFieldRoleArtifact::Template { contract: "HubA".to_string() }),
            ("gen__hub_b_template", RuntimeFieldRoleArtifact::Template { contract: "HubB".to_string() }),
            ("gen__leaf_template", RuntimeFieldRoleArtifact::Template { contract: "Leaf".to_string() }),
        ]
    );
    artifact.verify_template_plan().expect("direct route plan verifies");
}

#[test]
fn route_family_with_multiple_external_entries_uses_first_entry_as_representative() {
    let artifact = inline_artifact(
        "multi-entry-route-family",
        r#"
            state PlayerState {
                int n;
            }

            state BoardState {
                int n;
            }

            actor PlayerA owns PlayerState {
                entry enter_a() emits next: HubA {
                    unrestricted(next.value);
                    BoardState next = {
                        n: n,
                    };

                    become next <- HubA(next);
                }
            }

            actor PlayerB owns PlayerState {
                entry enter_b() emits next: HubB {
                    unrestricted(next.value);
                    BoardState next = {
                        n: n,
                    };

                    become next <- HubB(next);
                }
            }

            actor HubB owns BoardState {
                entry to_leaf_a() emits next: LeafA {
                    unrestricted(next.value);
                    BoardState next = {
                        n: n + 1,
                    };

                    become next <- LeafA(next);
                }
            }

            actor HubA owns BoardState {
                entry to_leaf_b() emits next: LeafB {
                    unrestricted(next.value);
                    BoardState next = {
                        n: n + 1,
                    };

                    become next <- LeafB(next);
                }
            }

            actor LeafA owns BoardState {
                entry to_a() emits next: HubA {
                    unrestricted(next.value);
                    BoardState next = {
                        n: n + 1,
                    };

                    become next <- HubA(next);
                }
            }

            actor LeafB owns BoardState {
                entry to_b() emits next: HubB {
                    unrestricted(next.value);
                    BoardState next = {
                        n: n + 1,
                    };

                    become next <- HubB(next);
                }
            }

            app MultiEntryFamily {
                actor PlayerA;
                actor PlayerB;
                actor HubB;
                actor HubA;
                actor LeafA;
                actor LeafB;
            }
            "#,
    );

    let family = artifact.argent.template_plan.route_families.first().expect("route family is inferred");
    assert_eq!(family.id, "route_family/BoardState/hub_b");
    assert_eq!(family.representative_actor, "HubB");
    assert_eq!(family.entry_actors, vec!["HubB", "HubA"]);
    assert_eq!(family.actors, vec!["HubB", "HubA", "LeafA", "LeafB"]);
    assert_eq!(family.table_id, "route_table/BoardState/gen__hub_b_routes");

    assert_eq!(
        runtime_state_plan(&artifact, "HubB").expect("HubB runtime role overlay exists").field_roles[..3]
            .iter()
            .map(|field| (field.name.as_str(), field.role.clone()))
            .collect::<Vec<_>>(),
        vec![
            ("gen__hub_b_template", RuntimeFieldRoleArtifact::Template { contract: "HubB".to_string() }),
            ("gen__hub_a_template", RuntimeFieldRoleArtifact::Template { contract: "HubA".to_string() }),
            (
                "gen__hub_b_routes",
                RuntimeFieldRoleArtifact::TemplateTable { contracts: vec!["LeafA".to_string(), "LeafB".to_string()] },
            ),
        ]
    );
    artifact.verify_template_plan().expect("multi-entry route family receipt verifies");
}

fn inline_artifact(name: &str, source: &str) -> Artifact {
    inline_actor_sil_and_artifact(name, source).1
}

fn inline_actor_sil_and_artifact(name: &str, source: &str) -> (BTreeMap<String, String>, Artifact) {
    let path = PathBuf::from(format!("{name}.ag"));
    let module = crate::compiler::syntax::parser::parse_module(path.clone(), source.to_string()).expect("source parses");
    let program = Program { root: path, modules: vec![module] };
    let model = Model::from_program(&program).expect("model validates");
    let actor_sil = actor_sil_for_model(&model);
    let artifact = emit_artifact(&program, &model, &actor_sil).expect("artifact emits");
    (actor_sil, artifact)
}

#[test]
fn standalone_entry_body_block_lowers_and_compiles() {
    let (actor_sil, _) = inline_actor_sil_and_artifact(
        "standalone-entry-body-block",
        r#"
            state CounterState {
                int count;
            }

            actor Counter owns CounterState {
                entry inspect(int delta) emits none {
                    {
                        int candidate = count + delta;
                        require((candidate >= count) || (delta < 0));
                    }
                    require(count >= 0);
                }
            }

            app StandaloneEntryBodyBlock {
                actor Counter;
            }
            "#,
    );

    let sil = actor_sil.get("Counter").expect("Counter emits");
    assert!(
        sil.contains(
            r#"        {
            int candidate = count + delta;
            require((candidate >= count) || (delta < 0));
        }
        require(count >= 0);"#
        ),
        "standalone block must remain a nested Sil block:\n{sil}"
    );
}

#[test]
fn complex_nested_entry_body_lowers_and_compiles() {
    inline_artifact(
        "complex-nested-entry-body",
        r#"
            state CounterState {
                int count;
                int limit;
            }

            actor Counter owns CounterState {
                entry advance(int delta, int scale, bool prefer_limit) emits next: Counter {
                    int base = count + ((delta * scale) + (limit - count));
                    {
                        int bounded = base + ((limit - base) * scale);
                        require((bounded >= count) || ((delta < 0) && !prefer_limit));
                        {
                            int mixed = (bounded + (delta * (scale + 1))) % (limit + 1);
                            require(((mixed >= 0) && (bounded <= limit)) || prefer_limit);
                        }
                    }

                    if ((delta > 0) && ((base < limit) || prefer_limit)) {
                        CounterState next_state = {
                            count: base + (scale * 2),
                            limit: limit,
                        };
                        require(
                            (next.value >= self.value)
                                && ((next_state.count > count) || (delta > scale))
                        );
                        become next <- Counter(next_state);
                    } else if ((delta == 0) || ((scale > limit) && !prefer_limit)) {
                        CounterState next_state = {
                            count: count,
                            limit: limit + (scale - delta),
                        };
                        require(
                            (next.value == self.value)
                                || ((next.value > self.value) && (next_state.limit >= limit))
                        );
                        become next <- Counter(next_state);
                    } else {
                        CounterState next_state = {
                            count: count + ((limit - scale) * (delta + 1)),
                            limit: limit,
                        };
                        require(
                            ((next.value >= 0) && (self.value >= 0))
                                || ((next_state.count <= limit) && !prefer_limit)
                        );
                        become next <- Counter(next_state);
                    }
                }
            }

            app ComplexNestedEntryBody {
                actor Counter;
            }
            "#,
    );
}

#[test]
fn for_loop_entry_body_lowers_and_compiles() {
    inline_artifact(
        "for-loop-entry-body",
        r#"
            state CounterState {
                int count;
            }

            actor Counter owns CounterState {
                entry inspect(int iterations) emits none {
                    for (i, 0, iterations, 16) {
                        require((i >= 0) && (i < iterations));
                    }
                }
            }

            app ForLoopEntryBody {
                actor Counter;
            }
            "#,
    );
}

#[test]
fn brace_leading_assignments_lower_and_compile_as_sil_statements() {
    let (actor_sil, _) = inline_actor_sil_and_artifact(
        "brace-leading-assignments",
        r#"
            state CounterState {
                int count;
            }

            fn identity(int value) -> int {
                return value;
            }

            actor Counter owns CounterState {
                entry inspect(byte[4] packed) emits none {
                    CounterState snapshot = {
                        count: count,
                    };
                    CounterState {count: int copied} = snapshot;
                    State {count: int current} = readInputState(this.activeInputIndex);
                    (byte[] left, byte[] right) = packed.split(2);
                    byte[] first, byte[] second = packed.split(2);
                    (int returned) = identity(count);
                    require(
                        (copied == count)
                            && (current == count)
                            && (left == first)
                            && (right == second)
                            && (returned == count)
                    );
                }
            }

            app BraceLeadingAssignments {
                actor Counter;
            }
            "#,
    );

    let sil = actor_sil.get("Counter").expect("Counter emits");
    assert!(sil.contains("State {count: int copied} = snapshot;"), "{sil}");
    assert!(sil.contains("State {count: int current} = readInputState(this.activeInputIndex);"), "{sil}");
    assert!(sil.contains("(byte[] left, byte[] right) = packed.split(2);"), "{sil}");
    assert!(sil.contains("byte[] first, byte[] second = packed.split(2);"), "{sil}");
    assert!(sil.contains("(int returned) = identity(count);"), "{sil}");
}

#[test]
fn typed_destructuring_keeps_an_expanded_entry_parameter_authored() {
    let (actor_sil, _) = inline_actor_sil_and_artifact(
        "expanded-parameter-destructuring",
        r#"
            state Detail {
                int count;
            }

            state Capsule {
                int amount;
                virtual detail;
            }

            state Expanded expands Capsule {
                detail: Detail;
            }

            actor Vault owns Expanded {
                entry inspect(Expanded value) emits none {
                    Expanded {
                        amount: int amount_value,
                        detail: Detail detail_value,
                    } = value;
                    require(amount_value >= 0);
                    require(detail_value.count >= 0);
                }
            }

            app ExpandedDestructuring {
                actor Vault;
            }
            "#,
    );

    let sil = actor_sil.get("Vault").expect("Vault emits");
    assert!(sil.contains("        Expanded {\n"), "{sil}");
    assert!(!sil.contains("        State {\n"), "{sil}");
}

#[test]
fn genesis_spawn_lowers_to_pinned_sil_and_artifact_metadata() {
    let (controller_sil, controller_artifact) =
        emit_selected_fixture("tests/fixtures/runtime/context_genesis_spawn/app.ag", "ControllerApp", "Controller");
    assert_eq!(controller_sil, include_str!("../../../../tests/fixtures/runtime/context_genesis_spawn/Controller.sil"));
    let launch =
        controller_artifact.argent.actors[0].entries.iter().find(|entry| entry.name == "launch").expect("launch entry exists");
    assert_eq!(launch.spawns.len(), 1);
    assert_eq!(launch.spawns[0].name, "new_pair");
    assert_eq!(launch.spawns[0].covenant, "pair_id");
    assert_eq!(
        launch.spawns[0].outputs.iter().map(|output| (output.name.as_str(), output.group_index)).collect::<Vec<_>>(),
        vec![("left", 0), ("right", 1)]
    );
    assert_eq!(
        launch.hidden_params.iter().map(|param| param.name.as_str()).collect::<Vec<_>>(),
        vec![
            "gen__new_pair_left_output_idx",
            "gen__new_pair_right_output_idx",
            "gen__actor_type_self_pair_type_prefix",
            "gen__actor_type_self_pair_type_suffix",
        ]
    );
    controller_artifact.verify_template_plan().expect("spawn metadata verifies");
    let mut malformed = controller_artifact.clone();
    malformed.argent.actors[0].entries[0].spawns[0].outputs[1].group_index = 2;
    assert!(
        matches!(malformed.verify_template_plan(), Err(TemplatePlanError::InvalidSpawnMetadata { .. })),
        "malformed spawn output order must be rejected"
    );
    let mut noncanonical_template_subject = controller_artifact.clone();
    let prefix = noncanonical_template_subject.argent.actors[0].entries[0]
        .hidden_params
        .iter_mut()
        .find(|param| param.purpose == HiddenParamPurposeArtifact::TemplatePrefixBytes)
        .expect("spawn prefix witness exists");
    let HiddenParamSubjectArtifact::SpawnActor { handle, .. } = &mut prefix.subject else {
        panic!("spawn prefix has a spawn actor subject");
    };
    *handle = "right".to_string();
    assert!(
        matches!(noncanonical_template_subject.verify_template_plan(), Err(TemplatePlanError::InvalidSpawnMetadata { .. })),
        "shared spawn template witnesses must use their first output as subject"
    );

    let (pair_sil, _) = emit_selected_fixture("tests/fixtures/runtime/context_genesis_spawn/app.ag", "PairApp", "Pair");
    assert_eq!(pair_sil, include_str!("../../../../tests/fixtures/runtime/context_genesis_spawn/Pair.sil"));
}

#[test]
fn multiple_genesis_spawns_lower_to_pinned_sil_and_artifact_metadata() {
    let source = "tests/fixtures/runtime/context_multiple_genesis_spawns/app.ag";
    let (controller_sil, controller_artifact) = emit_selected_fixture(source, "ControllerApp", "Controller");
    assert_eq!(controller_sil, include_str!("../../../../tests/fixtures/runtime/context_multiple_genesis_spawns/Controller.sil"));
    let launch =
        controller_artifact.argent.actors[0].entries.iter().find(|entry| entry.name == "launch").expect("launch entry exists");
    assert_eq!(
        launch
            .spawns
            .iter()
            .map(|spawn| {
                (
                    spawn.name.as_str(),
                    spawn.outputs.iter().map(|output| (output.name.as_str(), output.group_index)).collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>(),
        vec![
            ("first_pair", vec![("left", 0), ("right", 1)]),
            ("second_pair", vec![("pair", 0)]),
            ("third_pair", vec![("left", 0), ("right", 1)]),
        ]
    );
    assert_eq!(
        launch.hidden_params.iter().map(|param| param.name.as_str()).collect::<Vec<_>>(),
        vec![
            "gen__first_pair_left_output_idx",
            "gen__first_pair_right_output_idx",
            "gen__second_pair_pair_output_idx",
            "gen__third_pair_left_output_idx",
            "gen__third_pair_right_output_idx",
            "gen__actor_type_self_pair_type_prefix",
            "gen__actor_type_self_pair_type_suffix",
        ]
    );
    controller_artifact.verify_template_plan().expect("multiple-spawn metadata verifies");

    let (pair_sil, _) = emit_selected_fixture(source, "PairApp", "Pair");
    assert_eq!(pair_sil, include_str!("../../../../tests/fixtures/runtime/context_multiple_genesis_spawns/Pair.sil"));
}

#[test]
fn observed_and_spawned_source_actor_share_pinned_witnesses() {
    let source = "tests/fixtures/runtime/context_shared_actor_witness/app.ag";
    let (controller_sil, controller_artifact) = emit_selected_fixture(source, "SharedActorWitness", "Controller");
    assert_eq!(controller_sil, include_str!("../../../../tests/fixtures/runtime/context_shared_actor_witness/Controller.sil"));

    let advance =
        controller_artifact.argent.actors[0].entries.iter().find(|entry| entry.name == "advance").expect("advance entry exists");
    assert_eq!(
        advance.hidden_params.iter().map(|param| param.name.as_str()).collect::<Vec<_>>(),
        vec![
            "gen__anchor_prefix_len",
            "gen__anchor_suffix_len",
            "gen__newborn_pair_output_idx",
            "gen__actor_type_self_pair_type_prefix",
            "gen__actor_type_self_pair_type_suffix",
        ]
    );
    let source_witnesses =
        advance.hidden_params.iter().filter(|param| param.name.starts_with("gen__actor_type_self_pair_type_")).collect::<Vec<_>>();
    assert_eq!(source_witnesses.len(), 2);
    assert!(source_witnesses.iter().all(|param| {
        matches!(
            &param.subject,
            HiddenParamSubjectArtifact::ObservedActor {
                observe,
                side: ObservedActorSideArtifact::Output,
                handle,
                actor,
            } if observe == "existing" && handle == "pair" && actor == "self.pair_type"
        )
    }));
    assert_eq!(
        source_witnesses.iter().map(|param| param.recipe_id.as_str()).collect::<Vec<_>>(),
        vec![
            "witness/Controller/advance/actor_type/self_pair_type/template_prefix_bytes",
            "witness/Controller/advance/actor_type/self_pair_type/template_suffix_bytes",
        ]
    );
    controller_artifact.verify_template_plan().expect("shared observe/spawn witness metadata verifies");

    let mut malformed = controller_artifact.clone();
    let prefix = malformed.argent.actors[0].entries[0]
        .hidden_params
        .iter_mut()
        .find(|param| param.name == "gen__actor_type_self_pair_type_prefix")
        .expect("shared prefix witness exists");
    let HiddenParamSubjectArtifact::ObservedActor { handle, .. } = &mut prefix.subject else {
        panic!("shared prefix uses its first observed output");
    };
    *handle = "missing".to_string();
    assert!(
        matches!(malformed.verify_template_plan(), Err(TemplatePlanError::InvalidSpawnMetadata { .. })),
        "spawn metadata rejects an invalid observed witness provider"
    );

    let (anchor_sil, _) = emit_selected_fixture(source, "SharedActorWitness", "Anchor");
    assert_eq!(anchor_sil, include_str!("../../../../tests/fixtures/runtime/context_shared_actor_witness/Anchor.sil"));
    let (pair_sil, _) = emit_selected_fixture(source, "SharedActorWitness", "Pair");
    assert_eq!(pair_sil, include_str!("../../../../tests/fixtures/runtime/context_shared_actor_witness/Pair.sil"));
}

#[test]
fn spawn_actor_type_sources_have_distinct_witness_names() {
    let artifact = inline_artifact(
        "spawn-actor-type-sources",
        r#"
            state PairState {
                int value;
            }
            state LauncherState {
                actor_type<PairState> pair_type;
            }

            actor Launcher owns LauncherState {
                entry launch(actor_type<PairState> self_pair_type)
                spawns stored by stored_id {
                    outputs {
                        pair: self.pair_type,
                    }
                }
                spawns argument by argument_id {
                    outputs {
                        pair: self_pair_type,
                    }
                }
                emits next: Launcher {
                    unrestricted(stored.outputs.pair.value);
                    unrestricted(argument.outputs.pair.value);
                    unrestricted(next.value);
                    PairState stored_pair = { value: 1 };
                    PairState argument_pair = { value: 2 };
                    require stored.outputs become {
                        pair <- self.pair_type(stored_pair),
                    };
                    require argument.outputs become {
                        pair <- self_pair_type(argument_pair),
                    };
                    become next <- Launcher(self.state);
                }
            }

            app Test {
                actor Launcher;
            }
            "#,
    );

    let launch = artifact.argent.actors[0].entries.iter().find(|entry| entry.name == "launch").expect("launch entry exists");
    assert_eq!(
        launch.hidden_params.iter().map(|param| param.name.as_str()).collect::<Vec<_>>(),
        vec![
            "gen__stored_pair_output_idx",
            "gen__argument_pair_output_idx",
            "gen__actor_type_self_pair_type_prefix",
            "gen__actor_type_self_pair_type_suffix",
            "gen__actor_type_arg_self_pair_type_prefix",
            "gen__actor_type_arg_self_pair_type_suffix",
        ]
    );
}

#[test]
fn spawn_witness_recipe_ids_are_scoped_to_the_entry() {
    let artifact = inline_artifact(
        "entry-scoped-spawn-recipes",
        r#"
            state PairState {
                int value;
            }
            state LauncherState {
                actor_type<PairState> first_type;
                actor_type<PairState> second_type;
            }

            actor Launcher owns LauncherState {
                entry launch_first()
                spawns child by child_id {
                    outputs {
                        pair: self.first_type,
                    }
                }
                emits next: Launcher {
                    unrestricted(child.outputs.pair.value);
                    unrestricted(next.value);
                    PairState pair = { value: 1 };
                    require child.outputs become {
                        pair <- self.first_type(pair),
                    };
                    become next <- Launcher(self.state);
                }

                entry launch_second()
                spawns child by child_id {
                    outputs {
                        pair: self.second_type,
                    }
                }
                emits next: Launcher {
                    unrestricted(child.outputs.pair.value);
                    unrestricted(next.value);
                    PairState pair = { value: 2 };
                    require child.outputs become {
                        pair <- self.second_type(pair),
                    };
                    become next <- Launcher(self.state);
                }
            }

            app Test {
                actor Launcher;
            }
            "#,
    );

    let launcher = &artifact.argent.actors[0];
    let first = launcher.entries.iter().find(|entry| entry.name == "launch_first").expect("first entry exists");
    let second = launcher.entries.iter().find(|entry| entry.name == "launch_second").expect("second entry exists");
    let first_recipe = first
        .hidden_params
        .iter()
        .find(|param| param.purpose == HiddenParamPurposeArtifact::TemplatePrefixBytes)
        .expect("first spawn prefix exists");
    let second_recipe = second
        .hidden_params
        .iter()
        .find(|param| param.purpose == HiddenParamPurposeArtifact::TemplatePrefixBytes)
        .expect("second spawn prefix exists");
    assert_eq!(first_recipe.recipe_id, "witness/Launcher/launch_first/actor_type/self_first_type/template_prefix_bytes");
    assert_eq!(second_recipe.recipe_id, "witness/Launcher/launch_second/actor_type/self_second_type/template_prefix_bytes");
    assert_ne!(first_recipe.recipe_id, second_recipe.recipe_id);
}

#[test]
fn genesis_spawn_groups_must_follow_first_output_order() {
    let source = r#"
            state PairState {
                int value;
            }

            state LauncherState {
                actor_type<PairState> pair_type;
            }

            actor Launcher owns LauncherState {
                entry launch()
                spawns first by first_id {
                    outputs {
                        pair: self.pair_type,
                    }
                }
                spawns second by second_id {
                    outputs {
                        pair: self.pair_type,
                    }
                }
                emits next: Launcher {
                    unrestricted(first.outputs.pair.value);
                    unrestricted(second.outputs.pair.value);
                    unrestricted(next.value);
                    PairState pair = { value: 1 };
                    require first.outputs become {
                        pair <- self.pair_type(pair),
                    };
                    require second.outputs become {
                        pair <- self.pair_type(pair),
                    };
                    become next <- Launcher(self.state);
                }
            }

            app Test {
                actor Launcher;
            }
        "#;
    let path = PathBuf::from("test.ag");
    let module = crate::compiler::syntax::parser::parse_module(path.clone(), source.to_string()).expect("source parses");
    let program = Program { root: path, modules: vec![module] };
    let model = Model::from_program(&program).expect("model validates");
    let sil = emit_actor(model.actor("Launcher").expect("launcher exists"), &model).expect("Launcher emits");
    assert!(sil.contains("require(gen__first_pair_output_idx < gen__second_pair_output_idx);"), "{sil}");
    let actor_sil = actor_sil_for_model(&model);
    emit_artifact(&program, &model, &actor_sil).expect("generated Sil compiles");
}

#[test]
fn rejects_actor_type_parameter_shadowing_same_named_app_actor() {
    let err = parse_and_validate(
        r#"
            state LauncherState {
                int launches;
            }

            state ChildState {
                int amount;
            }

            actor Launcher owns LauncherState {
                entry launch(actor_type<ChildState> Child)
                spawns child_group by child_id {
                    outputs {
                        child: Child,
                    }
                }
                emits next: Launcher {
                    unrestricted(child_group.outputs.child.value);
                    unrestricted(next.value);
                    ChildState child_state = { amount: 1 };
                    LauncherState next = { launches: launches + 1 };
                    require child_group.outputs become {
                        child <- Child(child_state),
                    };
                    become next <- Launcher(next);
                }
            }

            actor Child owns ChildState {
                entry hold() emits none {
                    require(amount >= 0);
                }
            }

            app Test {
                actor Launcher;
                actor Child;
            }
            "#,
    )
    .expect_err("actor_type parameters must not shadow actor references");

    assert_eq!(
        err.message,
        "entry `Launcher::launch` actor_type parameter `Child` shadows an actor reference with the same name; rename the parameter"
    );
}

#[test]
fn fixed_actor_spawn_uses_compiler_owned_template_and_keeps_its_closure() {
    let source = r#"
            state LauncherState {
                int launches;
            }

            state ChildState {
                int amount;
            }

            actor Launcher owns LauncherState {
                entry launch(int amount)
                spawns child_group by child_id {
                    outputs {
                        child: Child,
                        sibling: Child,
                    }
                }
                emits next: Launcher {
                    unrestricted(child_group.outputs.child.value);
                    unrestricted(child_group.outputs.sibling.value);
                    unrestricted(next.value);
                    ChildState child_state = { amount: amount };
                    ChildState sibling_state = { amount: amount + 1 };
                    LauncherState next = { launches: launches + 1 };
                    require child_group.outputs become {
                        child <- Child(child_state),
                        sibling <- Child(sibling_state),
                    };
                    become next <- Launcher(next);
                }
            }

            // The spawned actor itself needs Launcher's template for its
            // normal successor, exercising the spawned template closure.
            actor Child owns ChildState {
                entry return_to_launcher() emits next: Launcher {
                    unrestricted(next.value);
                    LauncherState next = { launches: 0 };
                    become next <- Launcher(next);
                }
            }

            app Test {
                actor Launcher;
                actor Child;
            }
        "#;
    let artifact = inline_artifact("fixed_actor_spawn", source);
    let launcher = artifact.argent.actors.iter().find(|actor| actor.name == "Launcher").expect("Launcher artifact exists");
    assert_eq!(launcher.entries[0].spawns[0].outputs[0].actor, "Child");
    assert_eq!(
        launcher.entries[0]
            .hidden_params
            .iter()
            .filter(|param| {
                matches!(
                    param.purpose,
                    HiddenParamPurposeArtifact::TemplatePrefixBytes | HiddenParamPurposeArtifact::TemplateSuffixBytes
                )
            })
            .count(),
        2,
        "two fixed outputs for one actor share one prefix/suffix pair"
    );
    assert!(launcher.entries[0].hidden_params.iter().any(|param| {
        param.name == "gen__child_prefix" && param.subject == HiddenParamSubjectArtifact::Actor { actor: "Child".to_string() }
    }));
    assert!(!launcher.entries[0].hidden_params.iter().any(|param| {
        matches!(param.subject, HiddenParamSubjectArtifact::SpawnActor { .. })
            && matches!(
                param.purpose,
                HiddenParamPurposeArtifact::TemplatePrefixBytes | HiddenParamPurposeArtifact::TemplateSuffixBytes
            )
    }));
    artifact.verify_template_plan().expect("fixed spawn template closure verifies");
    let mut malformed = artifact.clone();
    malformed.argent.actors[0].entries[0].hidden_params.retain(|param| param.name != "gen__child_suffix");
    assert!(
        matches!(malformed.verify_template_plan(), Err(TemplatePlanError::InvalidSpawnMetadata { .. })),
        "static spawn metadata must retain one complete actor-scoped template pair"
    );

    let unselected = source.replace("                actor Child;\n", "");
    let err = parse_and_validate(&unselected).expect_err("fixed spawn actor must belong to the selected app");
    assert!(err.to_string().contains("selected-app or linked actor"), "unexpected error: {err}");
}

#[test]
fn fixed_actor_spawn_reuses_consumed_template_in_pinned_sil() {
    let source = "tests/fixtures/runtime/context_static_actor_spawn/app.ag";
    let (launcher_sil, launcher_artifact) = emit_selected_fixture(source, "StaticActorSpawn", "Launcher");
    let (child_sil, _) = emit_selected_fixture(source, "StaticActorSpawn", "Child");

    assert_eq!(launcher_sil, include_str!("../../../../tests/fixtures/runtime/context_static_actor_spawn/Launcher.sil"));
    assert_eq!(child_sil, include_str!("../../../../tests/fixtures/runtime/context_static_actor_spawn/Child.sil"));
    let launch = launcher_artifact.argent.actors[0].entries.iter().find(|entry| entry.name == "launch").expect("launch entry exists");
    assert_eq!(
        launch.hidden_params.iter().map(|param| (param.name.as_str(), &param.subject, param.purpose)).collect::<Vec<_>>(),
        vec![
            (
                "gen__child_prefix_len",
                &HiddenParamSubjectArtifact::Actor { actor: "Child".to_string() },
                HiddenParamPurposeArtifact::TemplatePrefixLen,
            ),
            (
                "gen__child_suffix_len",
                &HiddenParamSubjectArtifact::Actor { actor: "Child".to_string() },
                HiddenParamPurposeArtifact::TemplateSuffixLen,
            ),
            (
                "gen__child_group_child_output_idx",
                &HiddenParamSubjectArtifact::SpawnActor {
                    spawn: "child_group".to_string(),
                    handle: "child".to_string(),
                    actor: "Child".to_string(),
                },
                HiddenParamPurposeArtifact::SpawnOutputIndex,
            ),
        ]
    );
    launcher_artifact.verify_template_plan().expect("pinned fixed-spawn template plan verifies");
}

#[test]
fn fixed_actor_self_spawn_uses_the_active_template() {
    let artifact = inline_artifact(
        "fixed_actor_self_spawn",
        r#"
            state NodeState {
                int amount;
            }

            actor Node owns NodeState {
                entry fork(int next_amount)
                spawns child_group by child_id {
                    outputs {
                        child: Node,
                    }
                }
                emits next: Node {
                    unrestricted(child_group.outputs.child.value);
                    unrestricted(next.value);
                    NodeState child = { amount: next_amount };
                    NodeState next = { amount: next_amount + 1 };
                    require child_group.outputs become {
                        child <- Node(child),
                    };
                    become next <- Node(next);
                }
            }

            app Test {
                actor Node;
            }
            "#,
    );

    assert!(runtime_state_plan(&artifact, "Node").is_none(), "self-spawning Node needs no stored route context");
    let fork = artifact.argent.actors[0].entries.iter().find(|entry| entry.name == "fork").expect("fork entry exists");
    assert!(!fork.hidden_params.iter().any(|param| {
        matches!(
            param.purpose,
            HiddenParamPurposeArtifact::TemplatePrefixBytes
                | HiddenParamPurposeArtifact::TemplateSuffixBytes
                | HiddenParamPurposeArtifact::TemplatePrefixLen
                | HiddenParamPurposeArtifact::TemplateSuffixLen
        )
    }));
    artifact.verify_template_plan().expect("fixed self-spawn template plan verifies");
}

#[test]
fn fixed_actor_spawn_opens_the_target_family_cut() {
    let artifact = inline_artifact(
        "fixed_actor_spawn_family",
        r#"
            state LauncherState {
                int launches;
            }

            state BoardState {
                int turn;
            }

            actor Launcher owns LauncherState {
                entry launch()
                spawns game by game_id {
                    outputs {
                        mux: Mux,
                    }
                }
                emits next: Launcher {
                    unrestricted(game.outputs.mux.value);
                    unrestricted(next.value);
                    BoardState board = { turn: 0 };
                    LauncherState next = { launches: launches + 1 };
                    require game.outputs become {
                        mux <- Mux(board),
                    };
                    become next <- Launcher(next);
                }
            }

            actor Mux owns BoardState {
                entry move() emits next: Pawn {
                    unrestricted(next.value);
                    BoardState next = { turn: turn + 1 };
                    become next <- Pawn(next);
                }
            }

            actor Pawn owns BoardState {
                entry finish() emits next: Mux {
                    unrestricted(next.value);
                    BoardState next = { turn: turn + 1 };
                    become next <- Mux(next);
                }
            }

            actor Knight owns BoardState {
                entry finish() emits next: Mux {
                    unrestricted(next.value);
                    BoardState next = { turn: turn + 1 };
                    become next <- Mux(next);
                }
            }

            app Test {
                actor Launcher;
                actor Mux;
                actor Pawn;
                actor Knight;
            }
            "#,
    );

    let launcher = artifact.argent.actors.iter().find(|actor| actor.name == "Launcher").expect("Launcher artifact exists");
    let launch = launcher.entries.iter().find(|entry| entry.name == "launch").expect("launch entry exists");
    assert!(launch.hidden_params.iter().any(|param| param.purpose == HiddenParamPurposeArtifact::RouteFamilyTable));
    assert!(launch.hidden_params.iter().any(|param| {
        param.name == "gen__mux_prefix" && param.subject == HiddenParamSubjectArtifact::Actor { actor: "Mux".to_string() }
    }));
    assert!(launch.hidden_params.iter().any(|param| {
        param.name == "gen__mux_suffix" && param.subject == HiddenParamSubjectArtifact::Actor { actor: "Mux".to_string() }
    }));
    artifact.verify_template_plan().expect("fixed family spawn template plan verifies");
}

#[test]
fn rejects_spawn_name_shared_with_observe() {
    let err = parse_and_validate(
        r#"
            state PairState {}
            state LauncherState {
                cov_id observed_id;
                actor_type<PairState> pair_type;
            }

            actor Launcher owns LauncherState {
                entry launch()
                observes pair by self.observed_id {}
                spawns pair by pair_id {
                    outputs {
                        next_pair: self.pair_type,
                    }
                }
                emits next: Launcher {
                    unrestricted(pair.outputs.next_pair.value);
                    unrestricted(next.value);
                    require(1 == 1);
                    become next <- Launcher(self.state);
                }
            }

            app Test {
                actor Launcher;
            }
            "#,
    )
    .expect_err("observe and spawn names must not be ambiguous");

    assert!(err.to_string().contains("uses `pair` as both an observe and a spawn"), "unexpected error: {err}");
}

#[test]
fn rejects_spawn_covenant_binding_shared_with_source_value() {
    let err = parse_and_validate(
        r#"
            state PairState {}
            state LauncherState {
                cov_id pair_id;
                actor_type<PairState> pair_type;
            }

            actor Launcher owns LauncherState {
                entry launch()
                spawns pair by pair_id {
                    outputs {
                        next_pair: self.pair_type,
                    }
                }
                emits next: Launcher {
                    unrestricted(pair.outputs.next_pair.value);
                    unrestricted(next.value);
                    require(1 == 1);
                    become next <- Launcher(self.state);
                }
            }

            app Test {
                actor Launcher;
            }
            "#,
    )
    .expect_err("spawn covenant bindings must not shadow source values");

    assert!(err.to_string().contains("spawn covenant binding `pair_id` collides with a source value"), "unexpected error: {err}");
}

fn emit_fixture(case: &str, actor: &str) -> (String, Artifact) {
    let path = PathBuf::from("tests/fixtures/emit").join(case).join("app.ag");
    let source = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(&path)).expect("fixture source exists");
    let module = crate::compiler::syntax::parser::parse_module(path.clone(), source).expect("fixture source parses");
    let program = Program { root: path, modules: vec![module] };
    let model = Model::from_program(&program).expect("fixture model validates");
    let actor = model.actor(actor).expect("fixture actor exists");
    let sil = emit_actor(actor, &model).expect("fixture actor emits");
    let actor_sil = actor_sil_for_model(&model);
    let artifact = emit_artifact(&program, &model, &actor_sil).expect("fixture artifact emits");
    (sil, artifact)
}

fn emit_selected_fixture(path: &str, app: &str, actor: &str) -> (String, Artifact) {
    let path = PathBuf::from(path);
    let source = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(&path)).expect("fixture source exists");
    let module = crate::compiler::syntax::parser::parse_module(path.clone(), source).expect("fixture source parses");
    let program = Program { root: path, modules: vec![module] };
    let model = Model::from_program_app(&program, app).expect("selected fixture model validates");
    let actor = model.actor(actor).expect("selected fixture actor exists");
    let sil = emit_actor(actor, &model).expect("selected fixture actor emits");
    let actor_sil = actor_sil_for_model(&model);
    let artifact = emit_artifact(&program, &model, &actor_sil).expect("selected fixture artifact emits");
    (sil, artifact)
}

fn emit_inline_error(source: &str) -> ArgentError {
    let path = PathBuf::from("test.ag");
    let module = crate::compiler::syntax::parser::parse_module(path.clone(), source.to_string()).expect("source parses");
    let program = Program { root: path, modules: vec![module] };
    let model = Model::from_program(&program).expect("model validates");
    for actor in &model.actors {
        if let Err(err) = emit_actor(actor, &model) {
            return err;
        }
    }
    panic!("expected inline source to fail during emission")
}

fn parse_and_validate(source: &str) -> Result<()> {
    let path = PathBuf::from("test.ag");
    let module = crate::compiler::syntax::parser::parse_module(path.clone(), source.to_string())?;
    let program = Program { root: path, modules: vec![module] };
    Model::from_program(&program).map(|_| ())
}

fn toy_chess_source() -> String {
    r#"
            state LeagueState {
                int nonce;
            }

            state PlayerState {
                int nonce;
            }

            state BoardState {
                int selector;
                int ply;
            }

            actor enum MoveActor {
                Pawn;
                Knight;
            }

            actor League owns LeagueState {
                entry register() emits next: Player {
                    unrestricted(next.value);
                    PlayerState next_player = {
                        nonce: nonce,
                    };
                    become next <- Player(next_player);
                }
            }

            actor Player owns PlayerState {
                entry enter_mux() emits next: Mux {
                    unrestricted(next.value);
                    BoardState next_board = {
                        selector: nonce,
                        ply: 0,
                    };
                    become next <- Mux(next_board);
                }
            }

            actor Mux owns BoardState {
                entry choose(MoveActor target) emits next: MoveActor {
                    unrestricted(next.value);
                    if (target == MoveActor::Knight) {
                        require(selector >= 0);
                    }

                    BoardState next_board = {
                        selector: selector,
                        ply: ply + 1,
                    };

                    become next <- target(next_board);
                }

                entry choose_knight_const() emits next: MoveActor {
                    unrestricted(next.value);
                    BoardState next_board = {
                        selector: selector,
                        ply: ply + 1,
                    };

                    actor_type<BoardState> target = MoveActor::Knight;
                    become next <- target(next_board);
                }

                entry choose_pawn() emits next: Pawn {
                    unrestricted(next.value);
                    BoardState next_board = {
                        selector: selector,
                        ply: ply + 1,
                    };
                    become next <- Pawn(next_board);
                }

                entry choose_knight() emits next: Knight {
                    unrestricted(next.value);
                    BoardState next_board = {
                        selector: selector,
                        ply: ply + 1,
                    };
                    become next <- Knight(next_board);
                }
            }

            actor Pawn owns BoardState {
                entry back_to_mux() emits next: Mux {
                    unrestricted(next.value);
                    BoardState next_board = {
                        selector: selector,
                        ply: ply + 1,
                    };
                    become next <- Mux(next_board);
                }
            }

            actor Knight owns BoardState {
                entry back_to_mux() emits next: Mux {
                    unrestricted(next.value);
                    require(selector >= 0);

                    BoardState next_board = {
                        selector: selector,
                        ply: ply + 1,
                    };
                    become next <- Mux(next_board);
                }
            }

            app ToyChess {
                actor League;
                actor Player;
                actor Mux;
                actor Pawn;
                actor Knight;
            }
            "#
    .to_string()
}

#[test]
fn artifact_codec_matches_silverscript_sigscript_builder() {
    let module = crate::compiler::syntax::parser::parse_module(
        PathBuf::from("test.ag"),
        r#"
            state FooState {
                int count;
                byte[4] tag;
                bool flag;
            }

            actor Foo owns FooState {
                entry bump(int amount, byte[4] next_tag, bool next_flag, byte b) emits none {
                    require(amount >= 0);
                }

                entry done() emits none {
                    require(1 == 1);
                }
            }

            app Test {
                actor Foo;
            }
            "#
        .to_string(),
    )
    .expect("source parses");
    let program = Program { root: PathBuf::from("test.ag"), modules: vec![module] };
    let model = Model::from_program(&program).expect("model validates");
    let actor = model.actor("Foo").expect("actor exists");
    let actor_sil = actor_sil_for_model(&model);
    let artifact = emit_artifact(&program, &model, &actor_sil).expect("artifact emits");
    let sil_abi_json = serde_json::to_string(&artifact.sil_abi).expect("Sil ABI artifact serializes");
    let sil_abi: SilAbiArtifact = serde_json::from_str(&sil_abi_json).expect("Sil ABI artifact deserializes");
    sil_abi.check_schema_version().expect("Sil ABI schema version is current");
    let sil = actor_sil.get("Foo").expect("Foo Sil exists");
    let constructor_args = constructor_args_for_actor(actor, &model).expect("constructor args build");
    let compiled = compile_contract(sil, &constructor_args, CompileOptions::default()).expect("generated Sil compiles");

    let sil_contract = sil_abi.contract("Foo").expect("Foo Sil ABI exists");
    let bump = sil_contract.entries.iter().find(|entry| entry.name == "bump").expect("bump entry exists");
    let done = sil_contract.entries.iter().find(|entry| entry.name == "done").expect("done entry exists");
    assert_eq!(bump.dispatch_tag.into_bytes(), compiled.entry_by_name("bump").expect("bump ABI exists").dispatch_tag());
    assert_eq!(done.dispatch_tag.into_bytes(), compiled.entry_by_name("done").expect("done ABI exists").dispatch_tag());

    let portable_bump = crate::codec::encode_contract_entry_sig_script(
        &sil_abi,
        "Foo",
        "bump",
        &[
            crate::codec::ArtifactValue::Int(17),
            crate::codec::ArtifactValue::Bytes(vec![1, 2, 3, 4]),
            crate::codec::ArtifactValue::Bool(true),
            crate::codec::ArtifactValue::Byte(1),
        ],
    )
    .expect("portable bump sigscript builds");
    let sil_bump = compiled
        .build_sig_script("bump", vec![SilExpr::int(17), SilExpr::bytes(vec![1, 2, 3, 4]), SilExpr::bool(true), SilExpr::byte(1)])
        .expect("Sil bump sigscript builds");
    assert_eq!(portable_bump, sil_bump);
    assert_eq!(encode_hex(&portable_bump), "011104010203045151045bdffea8");

    let portable_done =
        crate::codec::encode_contract_entry_sig_script(&sil_abi, "Foo", "done", &[]).expect("portable done sigscript builds");
    let sil_done = compiled.build_sig_script("done", vec![]).expect("Sil done sigscript builds");
    assert_eq!(portable_done, sil_done);
}

#[test]
fn sil_signature_builtins_pass_through() {
    let module = crate::compiler::syntax::parser::parse_module(
        PathBuf::from("test.ag"),
        r#"
            state AuthState {
                int nonce;
            }

            actor Auth owns AuthState {
                entry verify(
                    sig tx_signature,
                    byte[33] ecdsa_key,
                    datasig message_signature,
                    byte[32] digest,
                    pubkey schnorr_key,
                ) emits none {
                    require(checkSig(tx_signature, schnorr_key));
                    require(checkSigEcdsa(tx_signature, ecdsa_key));
                    require(checkMsgSig(message_signature, digest, schnorr_key));
                    require(checkMsgSigEcdsa(message_signature, digest, ecdsa_key));
                }
            }

            app Test {
                actor Auth;
            }
            "#
        .to_string(),
    )
    .expect("source parses");
    let program = Program { root: PathBuf::from("test.ag"), modules: vec![module] };
    let model = Model::from_program(&program).expect("model validates");
    let actor_sil = actor_sil_for_model(&model);
    let sil = actor_sil.get("Auth").expect("Auth Sil exists");

    for call in ["checkSig(", "checkSigEcdsa(", "checkMsgSig(", "checkMsgSigEcdsa("] {
        assert!(sil.contains(call), "missing `{call}` in generated Sil:\n{sil}");
    }
    emit_artifact(&program, &model, &actor_sil).expect("generated Sil compiles");
}

#[test]
fn manifest_uses_relative_paths_when_possible() {
    let cwd = std::env::current_dir().expect("current dir");
    let mut program = test_program();
    program.root = cwd.join("examples/tickets.ag");
    program.modules[0].path = cwd.join("examples/tickets.ag");
    program.modules[0].actors[0].entries.clear();
    let model = Model::from_program(&program).expect("model validates");

    let manifest = emit_manifest(&program, &model);

    assert!(manifest.contains(r#""root": "examples/tickets.ag""#), "{manifest}");
    assert!(manifest.contains(r#""examples/tickets.ag""#), "{manifest}");
    assert!(!manifest.contains(&display_path(&cwd)), "{manifest}");
}

#[test]
fn generated_snake_suffixes_preserve_acronym_runs() {
    assert_eq!(to_snake("MinterProxy"), "minter_proxy");
    assert_eq!(to_snake("KCC20"), "kcc20");
    assert_eq!(to_snake("KCC20Minter"), "kcc20_minter");
}

fn assert_duplicate_top_level_error(err: &ArgentError, kind: &str, name: &str) {
    let message = err.to_string();
    assert!(message.contains(&format!("duplicate top-level {kind} `{name}`")), "unexpected error: {err}");
    assert!(message.contains("second.ag"), "expected duplicate path in error: {err}");
    assert!(message.contains("test.ag"), "expected first declaration path in error: {err}");
}

fn empty_module(path: &str) -> Module {
    Module {
        path: PathBuf::from(path),
        imports: Vec::new(),
        consts: Vec::new(),
        states: Vec::new(),
        functions: Vec::new(),
        actors: Vec::new(),
        actor_enums: Vec::new(),
        apps: Vec::new(),
    }
}

fn actor_sil_for_model(model: &Model<'_>) -> BTreeMap<String, String> {
    model.actors.iter().map(|actor| (actor.name.clone(), emit_actor(actor, model).expect("actor emits"))).collect()
}

fn assert_example_build_artifact(input: &str, name: &str, expected_hashes: &[(&str, &str)]) {
    let out_dir = std::env::temp_dir().join(format!("argent-{name}-artifact-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&out_dir);

    let artifact = crate::build_file(input, &out_dir).expect("example builds");
    artifact.check_schema_version().expect("artifact schema version is supported");
    artifact.verify_template_plan().expect("template plan receipt verifies");

    let expected_hashes = expected_hashes.iter().copied().collect::<BTreeMap<_, _>>();
    assert!(!artifact.argent.actors.is_empty(), "artifact should contain Argent actors");
    for actor in &artifact.argent.actors {
        let sil_contract = artifact
            .sil_abi
            .contract(&actor.abi.actor)
            .unwrap_or_else(|| panic!("actor `{}` should reference a Sil ABI contract", actor.name));
        assert_compiled_projection(sil_contract.name.as_str(), &sil_contract.compiled);
        assert_runtime_state_round_trip(sil_contract, &sil_contract.compiled);
        if let Some(expected_hash) = expected_hashes.get(sil_contract.name.as_str()) {
            assert_eq!(&sil_contract.compiled.template_hash_hex, expected_hash, "actor `{}` template hash changed", actor.name);
        }
    }

    let _ = fs::remove_dir_all(out_dir);
}

fn assert_runtime_state_round_trip(actor: &SilContractArtifact, compiled: &CompiledContractArtifact) {
    let script = crate::codec::decode_hex(&compiled.script_hex).expect("script hex decodes");
    let (_, state_script, _) = compiled.script_parts(&script).expect("state span fits the compiled script");
    let state_values = crate::codec::decode_runtime_state_script(&actor.runtime_state, state_script).expect("runtime state decodes");
    let reencoded = crate::codec::encode_runtime_state_script(&actor.runtime_state, &state_values).expect("runtime state re-encodes");
    assert_eq!(reencoded, state_script, "actor `{}` runtime state must re-encode byte-for-byte", actor.name);
}

fn assert_compiled_projection(actor: &str, compiled: &CompiledContractArtifact) {
    assert!(!compiled.script_hex.is_empty(), "actor `{actor}` should have script bytes");
    assert!(compiled.state_span.len > 0, "actor `{actor}` should have a non-empty state span");
    assert_eq!(compiled.template_hash_hex.len(), 64, "actor `{actor}` should have a 32-byte template hash");

    let script = crate::codec::decode_hex(&compiled.script_hex).expect("script hex decodes");
    let (prefix, _, suffix) = compiled.script_parts(&script).expect("state span fits the compiled script");
    let template_hash = silverscript_lang::template::template_hash(prefix, suffix);
    assert_eq!(encode_hex(&template_hash), compiled.template_hash_hex, "actor `{actor}` template hash must use the Sil template hash");
}

fn runtime_state_plan<'a>(artifact: &'a Artifact, contract: &str) -> Option<&'a RuntimeStatePlanArtifact> {
    artifact.argent.template_plan.runtime_states.iter().find(|state| state.contract == contract)
}

fn subject_label(subject: &HiddenParamSubjectArtifact) -> &str {
    match subject {
        HiddenParamSubjectArtifact::Actor { actor } => actor,
        HiddenParamSubjectArtifact::ObservedActor { actor, .. } => actor,
        HiddenParamSubjectArtifact::SpawnActor { actor, .. } => actor,
        HiddenParamSubjectArtifact::ObservedOutputField { field, .. } => field,
        HiddenParamSubjectArtifact::RouteFamily { family_id } => family_id,
        HiddenParamSubjectArtifact::TemplateSelector { selector } => selector,
        HiddenParamSubjectArtifact::StateExpansion { memory_state, .. } => memory_state,
    }
}

fn test_program() -> Program {
    Program {
        root: PathBuf::from("test.ag"),
        modules: vec![Module {
            path: PathBuf::from("test.ag"),
            imports: Vec::new(),
            consts: Vec::new(),
            states: vec![
                StateDecl { name: "PlayerState".to_string(), fields: Vec::new(), expansion: None },
                StateDecl { name: "GameState".to_string(), fields: Vec::new(), expansion: None },
            ],
            functions: Vec::new(),
            actors: vec![
                ActorDecl {
                    name: "Player".to_string(),
                    state: "PlayerState".to_string(),
                    functions: Vec::new(),
                    entries: vec![EntryDecl {
                        kind: EntryKind::Leader,
                        name: "step".to_string(),
                        params: Vec::new(),
                        consumes: Vec::new(),
                        observes: Vec::new(),
                        spawns: Vec::new(),
                        emits: EmitSpec::Outputs(vec![EmitOutput {
                            name: "next".to_string(),
                            actors: vec!["Player".to_string()],
                            auth_index: 0,
                        }]),
                        body: EntryBody::default(),
                        routes: Vec::new(),
                        terminal_route_sets: Vec::new(),
                    }],
                },
                ActorDecl { name: "Game".to_string(), state: "GameState".to_string(), functions: Vec::new(), entries: Vec::new() },
            ],
            actor_enums: Vec::new(),
            apps: vec![AppDecl { name: "Test".to_string(), actors: vec!["Player".to_string(), "Game".to_string()] }],
        }],
    }
}

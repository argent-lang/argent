use std::path::PathBuf;

use super::parse_module;
use super::{EmitSpec, Import, TypeRef};

#[test]
fn parses_source_backed_app_imports() {
    let module = parse_module(
        PathBuf::from("controller.ag"),
        r#"
            import actor AssetApp::Asset from "./asset.ag";
            import app RegistryApp from "./registry.ag";
            import actor Helper from "./helper.ag";
            "#
        .to_string(),
    )
    .expect("app-qualified imports parse");

    assert!(matches!(
        &module.imports[0],
        Import::AppActor { app, actor, path }
            if app == "AssetApp" && actor == "Asset" && path == "./asset.ag"
    ));
    assert!(matches!(
        &module.imports[1],
        Import::App { app, path }
            if app == "RegistryApp" && path == "./registry.ag"
    ));
    assert!(matches!(
        &module.imports[2],
        Import::Actor { actor, path }
            if actor == "Helper" && path == "./helper.ag"
    ));
}

#[test]
fn parses_type_first_function_entry_and_delegate_parameters() {
    let module = parse_module(
        PathBuf::from("params.ag"),
        r#"
            state State {
                int value;
            }

            fn helper(byte[32] owner, int amount,) -> int {
                return amount;
            }

            actor Actor owns State {
                entry update(int amount, actor_type<State> target,) emits none {}
                delegate verify(sig owner_sig,) consumes {
                    leader: Actor,
                } {}
            }
            "#
        .to_string(),
    )
    .expect("type-first parameters parse");

    assert_eq!(module.functions[0].params[0].name, "owner");
    assert_eq!(module.functions[0].params[0].ty, TypeRef::array("byte", 32));
    assert_eq!(module.functions[0].params[1].name, "amount");
    assert_eq!(module.functions[0].params[1].ty, TypeRef::new("int"));
    assert_eq!(module.functions[0].return_ty, Some(TypeRef::new("int")));

    let actor = &module.actors[0];
    assert_eq!(actor.entries[0].params[0].name, "amount");
    assert_eq!(actor.entries[0].params[0].ty, TypeRef::new("int"));
    assert_eq!(actor.entries[0].params[1].name, "target");
    assert_eq!(actor.entries[0].params[1].ty, TypeRef::actor_type("State"));
    assert_eq!(actor.entries[1].params[0].name, "owner_sig");
    assert_eq!(actor.entries[1].params[0].ty, TypeRef::new("sig"));
}

#[test]
fn parses_helper_functions_without_return_types() {
    let module = parse_module(
        PathBuf::from("helpers.ag"),
        r#"
            fn authorize(int value) {
                require(value > 0);
            }

            fn identity(int value) -> int {
                return value;
            }
            "#
        .to_string(),
    )
    .expect("void and value-returning helpers parse");

    assert_eq!(module.functions[0].return_ty, None);
    assert_eq!(module.functions[1].return_ty, Some(TypeRef::new("int")));
}

#[test]
fn rejects_name_first_parameters() {
    let err = parse_module(PathBuf::from("params.ag"), "fn helper(amount: int) -> int { return amount; }".to_string())
        .expect_err("name-first parameters must not parse");

    assert!(err.to_string().contains("expected identifier, found `:`"), "unexpected error: {err}");
}

#[test]
fn reports_path_line_and_column_for_parse_errors() {
    let err = parse_module(PathBuf::from("event.ag"), "state Event {\n    int remaining;\n}\n\n/\n".to_string())
        .expect_err("stray slash must not parse");

    assert_eq!(err.to_string(), "event.ag:5:1: expected top-level declaration, found `/`");
    assert_eq!(err.location.expect("source location").byte_offset, 36);
}

#[test]
fn reports_path_line_and_column_for_lexer_errors() {
    let err =
        parse_module(PathBuf::from("event.ag"), "state Event {}\n@\n".to_string()).expect_err("unsupported character must not lex");

    assert_eq!(err.to_string(), "event.ag:2:1: unexpected character `@`");
}

#[test]
fn parses_comma_separated_role_and_route_bindings() {
    let module = parse_module(
        PathBuf::from("bindings.ag"),
        r#"
            state State {
                int value;
            }

            actor Actor owns State {
                entry update()
                observes remote by remote_id {
                    inputs {
                        input: actor_type<State> as observed_actor,
                    }
                    outputs {
                        output: observed_actor
                    }
                }
                spawns child by child_id {
                    outputs {
                        first: Actor,
                        second: observed_actor
                    }
                }
                consumes {
                    peer: Actor,
                    other: Actor
                }
                emits {
                    first: Actor,
                    second: Actor
                } {
                    become {
                        first <- Actor(self.state),
                        second <- Actor(self.state)
                    };
                }
            }
            "#
        .to_string(),
    )
    .expect("comma-separated bindings parse");

    let entry = &module.actors[0].entries[0];
    assert_eq!(entry.observes[0].inputs.len(), 1);
    assert_eq!(entry.observes[0].outputs.len(), 1);
    assert_eq!(entry.spawns[0].outputs.len(), 2);
    assert_eq!(entry.consumes.len(), 2);
    assert!(matches!(&entry.emits, EmitSpec::Outputs(outputs) if outputs.len() == 2));
    assert_eq!(entry.routes.len(), 2);
}

#[test]
fn parses_named_single_output_shorthand() {
    let module = parse_module(
        PathBuf::from("single-output.ag"),
        r#"
            state State {}

            actor Actor owns State {
                entry update() emits result: Actor {
                    become result <- Actor(self.state);
                }
            }
            "#
        .to_string(),
    )
    .expect("named single-output shorthand parses");

    let entry = &module.actors[0].entries[0];
    let EmitSpec::Outputs(outputs) = &entry.emits else {
        panic!("single-output shorthand normalizes to named outputs");
    };
    assert_eq!(outputs.len(), 1);
    assert_eq!(outputs[0].name, "result");
    assert_eq!(outputs[0].actors, ["Actor"]);
    assert_eq!(outputs[0].auth_index, 0);
    assert_eq!(entry.routes[0].output, "result");
}

#[test]
fn allows_one_as_named_single_output_handle() {
    let module = parse_module(
        PathBuf::from("single-output.ag"),
        r#"
            state State {}

            actor Actor owns State {
                entry update() emits one: Actor {
                    become one <- Actor(self.state);
                }
            }
            "#
        .to_string(),
    )
    .expect("`one` is an ordinary named output handle");

    let EmitSpec::Outputs(outputs) = &module.actors[0].entries[0].emits else {
        panic!("single-output shorthand normalizes to named outputs");
    };
    assert_eq!(outputs[0].name, "one");
    assert_eq!(module.actors[0].entries[0].routes[0].output, "one");
}

#[test]
fn rejects_removed_emits_one_syntax() {
    let err = parse_module(
        PathBuf::from("single-output.ag"),
        r#"
            state State {}

            actor Actor owns State {
                entry update() emits one Actor {
                    become Actor(self.state);
                }
            }
            "#
        .to_string(),
    )
    .expect_err("removed emits-one syntax must not parse");

    assert!(err.to_string().contains("`emits one Type` has been removed"), "unexpected error: {err}");
}

#[test]
fn rejects_semicolons_in_role_binding_lists() {
    for source in [
        r#"
                state State {}
                actor Actor owns State {
                    entry update() consumes { peer: Actor; } emits none {}
                }
            "#,
        r#"
                state State {}
                actor Actor owns State {
                    entry update() emits { next: Actor; } {}
                }
            "#,
        r#"
                state State {}
                actor Actor owns State {
                    entry update()
                    spawns child by child_id { outputs { next: Actor; } }
                    emits none {}
                }
            "#,
        r#"
                state State {}
                actor Actor owns State {
                    entry update()
                    observes remote by remote_id { inputs { peer: Actor; } }
                    emits none {}
                }
            "#,
    ] {
        let err = parse_module(PathBuf::from("bindings.ag"), source.to_string())
            .expect_err("semicolon-separated role bindings must not parse");
        assert!(err.to_string().contains("expected `,` or `}`"), "unexpected error: {err}");
    }
}

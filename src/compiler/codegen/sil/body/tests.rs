use std::path::PathBuf;

use super::*;

#[test]
fn state_constructor_fields_require_names_and_expressions() {
    for body in [": 1", "count:", "1count: 1"] {
        assert!(parse_state_fields(body).is_err(), "malformed component `{body}` must be rejected");
    }
}

#[test]
fn active_field_projection_follows_lexical_binding_resolution() {
    let mut bindings = BodyBindings::new();
    bindings.declare("detail", BodyBinding::lowered_typed("byte[32]").with_active_field_projection("detail", true));
    assert!(bindings.active_field_projection("detail").is_some());

    bindings.enter_scope();
    bindings.declare("detail", BodyBinding::lowered_typed("Details"));
    assert!(bindings.active_field_projection("detail").is_none());
    bindings.exit_scope();

    assert!(bindings.active_field_projection("detail").is_some());
}

#[test]
fn registry_covers_every_entry_namespace_role() {
    let module = crate::compiler::syntax::parser::parse_module(
        PathBuf::from("entry-namespace.ag"),
        r#"
            state OwnerState { cov_id peer_id; }
            state PeerState { int value; }

            actor Owner owns OwnerState {
                entry inspect(int parameter)
                consumes { consumed: Peer, }
                observes observed by self.peer_id {
                    inputs { agent: actor_type<PeerState> as open_peer, }
                    outputs { agent: open_peer, }
                }
                spawns first_spawn by first_covenant {
                    outputs { left: Peer, }
                }
                spawns second_spawn by second_covenant {
                    outputs { left: Peer, }
                }
                emits emitted: Owner {}
            }

            actor Peer owns PeerState {}
            app Test { actor Owner; actor Peer; }
        "#
        .to_string(),
    )
    .expect("source parses");
    let actor = &module.actors[0];
    let names = reserved_entry_names(actor, &actor.entries[0]).expect("entry namespace is collision-free");
    let roles = names.body_bindings.iter().map(|(name, role)| (name.as_str(), role.description())).collect::<BTreeMap<_, _>>();

    assert_eq!(roles.get("self").map(String::as_str), Some("current actor context"));
    assert_eq!(roles.get("consumed").map(String::as_str), Some("consume handle"));
    assert_eq!(roles.get("emitted").map(String::as_str), Some("emit handle"));
    assert_eq!(roles.get("observed").map(String::as_str), Some("observe root"));
    assert_eq!(roles.get("agent").map(String::as_str), Some("observe `observed` output label"));
    assert_eq!(roles.get("first_spawn").map(String::as_str), Some("spawn root"));
    assert_eq!(roles.get("second_spawn").map(String::as_str), Some("spawn root"));
    assert_eq!(roles.get("left").map(String::as_str), Some("spawn `first_spawn` output label"));
    assert_eq!(roles.get("first_covenant").map(String::as_str), Some("spawn `first_spawn` covenant binding"));
    assert_eq!(roles.get("second_covenant").map(String::as_str), Some("spawn `second_spawn` covenant binding"));
    assert_eq!(roles.get("open_peer").map(String::as_str), Some("observe `observed` open-actor binding"));
    assert_eq!(roles.get("parameter").map(String::as_str), Some("entry parameter"));
}

#[test]
fn registry_rejects_clause_collisions_with_both_roles() {
    let module = crate::compiler::syntax::parser::parse_module(
        PathBuf::from("entry-namespace-collision.ag"),
        r#"
            state OwnerState { int value; }
            state PeerState { int value; }

            actor Owner owns OwnerState {
                entry inspect() consumes { next: Peer, } emits next: Owner {}
            }

            actor Peer owns PeerState {}
            app Test { actor Owner; actor Peer; }
        "#
        .to_string(),
    )
    .expect("source parses");
    let actor = &module.actors[0];
    let err = reserved_entry_names(actor, &actor.entries[0]).expect_err("clause handles share one namespace");

    assert!(err.to_string().contains("emit handle `next` collides with consume handle of the same name"), "unexpected error: {err}");
}

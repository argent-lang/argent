use super::*;
use crate::compiler::loader::{SourceSet, load_inline_program};
use crate::compiler::model::{AppCompilationContext, StaticActorId};
use crate::compiler::syntax::node::{DeclId, ModuleId, SymbolKind};
use crate::compiler::syntax::{ActorDecl, Cardinality, CardinalityBound, EmitOutput, EmitSpec, EntryKind};

fn test_entry_id() -> EntryId {
    EntryId { actor: DeclId::new(ModuleId::new(0), SymbolKind::Actor, 0), index: 0 }
}

fn test_actor_ids(names: &[&str]) -> BTreeMap<String, StaticActorId> {
    names
        .iter()
        .enumerate()
        .map(|(index, name)| (name.to_string(), StaticActorId::InApp(DeclId::new(ModuleId::new(0), SymbolKind::Actor, index))))
        .collect()
}

#[test]
fn bound_route_identity_controls_static_target_validation() {
    let program = load_inline_program(
        std::path::PathBuf::from("bound-route.ag"),
        include_str!("../../../../tests/fixtures/state_layout/function_contexts/app.ag").to_string(),
    )
    .expect("source resolves");
    let model = AppCompilationContext::from_resolved(
        &program,
        None,
        &std::collections::BTreeMap::new(),
        &crate::compiler::model::default_route_planner,
    )
    .expect("model plans");
    let actor = model.actor_by_decl(model.types.names["Routed"]).expect("route actor");
    let entry = actor.entries.iter().find(|entry| entry.name == "advance").expect("route entry");
    let mut route = model
        .entry_model_by_id(crate::compiler::syntax::node::EntryId {
            actor: model.types.names[&actor.name],
            index: actor.entries.iter().position(|candidate| std::ptr::eq(candidate, entry)).expect("entry belongs to actor"),
        })
        .expect("entry model")
        .routes()[0]
        .clone();
    let ResolvedSuccessor::Constructed { actor: display, .. } = &mut route.successor else { panic!("constructed route expected") };
    *display = RouteActorSource::Expanded(StaticActorId::InApp(model.types.names["Foreign"]));
    assert_eq!(
        model
            .route_target_ids_by_id(
                crate::compiler::syntax::node::EntryId {
                    actor: model.types.names[&actor.name],
                    index: actor.entries.iter().position(|candidate| std::ptr::eq(candidate, entry)).expect("entry belongs to actor")
                },
                &route
            )
            .expect("bound route target"),
        [StaticActorId::InApp(model.types.names["Routed"])]
    );
    assert_eq!(
        model.route_static_target_id(&route).expect("bound route actor identity"),
        StaticActorId::InApp(model.types.names["Routed"]),
    );
}

#[test]
fn foreign_routes_retain_group_and_output_identities_with_repeated_labels() {
    let sources = SourceSet::discover_inline(
        std::path::PathBuf::from("foreign-route-identities.ag"),
        include_str!("../../../../tests/fixtures/runtime/context_multiple_genesis_spawns/app.ag").to_string(),
    )
    .expect("source discovers");
    sources
        .with_resolved(|program| {
            let model = AppCompilationContext::from_resolved(
                &program,
                Some("ControllerApp"),
                &std::collections::BTreeMap::new(),
                &crate::compiler::model::default_route_planner,
            )?;
            let actor = model.actor_by_decl(model.types.names["Controller"])?;
            let entry = &actor.entries[0];
            let entry_model = model.entry_model_by_id(crate::compiler::syntax::node::EntryId {
                actor: model.types.names[&actor.name],
                index: actor.entries.iter().position(|candidate| std::ptr::eq(candidate, entry)).expect("entry belongs to actor"),
            })?;
            let body = model.resolution.entry_body(entry_model.id)?;
            let route_ids = body
                .iter()
                .filter_map(|statement| match statement {
                    AuthoredEntryStatement::ForeignBecome { routes, .. } => Some(
                        routes
                            .iter()
                            .map(|route| entry_model.route_output(route.id).expect("bound foreign output"))
                            .collect::<Vec<_>>(),
                    ),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(
                route_ids,
                [
                    vec![
                        (CovenantGroupId::Genesis(0), InteractionId::SpawnedOutput { spawn: 0, output: 0 }),
                        (CovenantGroupId::Genesis(0), InteractionId::SpawnedOutput { spawn: 0, output: 1 }),
                    ],
                    vec![(CovenantGroupId::Genesis(1), InteractionId::SpawnedOutput { spawn: 1, output: 0 })],
                    vec![
                        (CovenantGroupId::Genesis(2), InteractionId::SpawnedOutput { spawn: 2, output: 0 }),
                        (CovenantGroupId::Genesis(2), InteractionId::SpawnedOutput { spawn: 2, output: 1 }),
                    ],
                ]
            );
            Ok(())
        })
        .expect("foreign route identities are planned");
}

#[test]
fn observed_actor_type_source_keeps_nominal_state_identity() {
    let program = load_inline_program(
        std::path::PathBuf::from("observed-actor-type.ag"),
        include_str!("../../../../tests/fixtures/emit/open_observed_state_handle/app.ag").to_string(),
    )
    .expect("source resolves");
    let model = AppCompilationContext::from_resolved(
        &program,
        None,
        &std::collections::BTreeMap::new(),
        &crate::compiler::model::default_route_planner,
    )
    .expect("model plans");
    let actor = model.actor_by_decl(model.types.names["Cell"]).expect("cell actor");
    let entry = actor.entries.iter().find(|entry| entry.name == "advance").expect("advance entry");
    let entry_id = model
        .entry_model_by_id(crate::compiler::syntax::node::EntryId {
            actor: model.types.names[&actor.name],
            index: actor.entries.iter().position(|candidate| std::ptr::eq(candidate, entry)).expect("entry belongs to actor"),
        })
        .expect("bound entry")
        .id;
    let state = model.source_state_id("AgentCapsule").expect("authored state identity");
    let value = clause_actor_type_ref(
        entry_id,
        InteractionId::ObservedInput { observe: 0, input: 0 },
        "self.agent_type",
        actor,
        entry,
        &model,
    )
    .expect("clause resolves")
    .expect("actor-type source");
    assert_eq!(value.state(), &state);
    let ClauseActorTypeRef::StateField { field, .. } = &value else { panic!("state field source expected") };
    assert_eq!(field.state(), &model.source_state_id("CellState").expect("field owner identity"));
    assert_eq!(field.field(), "agent_type");
    assert_eq!(
        clause_actor_type_ref(
            entry_id,
            InteractionId::ObservedInput { observe: 0, input: 0 },
            "copied text is not the source",
            actor,
            entry,
            &model
        )
        .expect("bound clause resolves"),
        Some(value.clone())
    );
    let observe = &entry.observes[0];
    assert_eq!(
        observed_open_state_for_decl(entry_id, actor, entry, observe, &observe.inputs[0], &model).expect("observed target"),
        Some(state)
    );
}

#[test]
fn actor_type_entry_argument_keeps_parameter_position() {
    let program = load_inline_program(
        std::path::PathBuf::from("actor-type-argument.ag"),
        r#"
        state TargetState { int n; }
        state HostState { int n; }
        actor Host owns HostState {
            entry choose(int prefix, actor_type<TargetState> target, cov_id remote)
            observes watched by remote {
                inputs { value: target, }
            }
            emits none { require(n >= prefix); }
        }
        app Test { actor Host; }
        "#
        .to_string(),
    )
    .expect("source resolves");
    let model = AppCompilationContext::from_resolved(
        &program,
        None,
        &std::collections::BTreeMap::new(),
        &crate::compiler::model::default_route_planner,
    )
    .expect("model plans");
    let actor = model.actor_by_decl(model.types.names["Host"]).expect("host actor");
    let entry = &actor.entries[0];
    let entry_id = model
        .entry_model_by_id(crate::compiler::syntax::node::EntryId {
            actor: model.types.names[&actor.name],
            index: actor.entries.iter().position(|candidate| std::ptr::eq(candidate, entry)).expect("entry belongs to actor"),
        })
        .expect("bound entry")
        .id;
    let reference =
        clause_actor_type_ref(entry_id, InteractionId::ObservedInput { observe: 0, input: 0 }, "target", actor, entry, &model)
            .expect("clause resolves")
            .expect("actor-type argument");
    assert!(matches!(reference, ClauseActorTypeRef::EntryArgument { index: 1, ref name, ref state }
        if name == "target" && state == &model.source_state_id("TargetState").expect("target state identity")));
    assert_eq!(
        resolve_observe_covenant_id_source(entry_id, actor, entry, &model, &entry.observes[0]).expect("bound covenant argument"),
        CovenantIdSource::EntryArgument { index: 2 }
    );
    let mut copied_actor = actor.clone();
    copied_actor.state = "TargetState".to_string();
    copied_actor.entries[0].params[1].name = "copied_parameter_name".to_string();
    let copied_entry = &copied_actor.entries[0];
    assert_eq!(
        clause_actor_type_ref(
            entry_id,
            InteractionId::ObservedInput { observe: 0, input: 0 },
            "target",
            &copied_actor,
            copied_entry,
            &model
        )
        .expect("bound parameter position survives copied name"),
        Some(reference)
    );
    assert_eq!(
        resolve_observe_covenant_id_source(entry_id, &copied_actor, copied_entry, &model, &copied_entry.observes[0])
            .expect("bound covenant position survives copied state name"),
        CovenantIdSource::EntryArgument { index: 2 }
    );
}

#[test]
fn bound_local_selector_identity_controls_route_expansion() {
    let program = load_inline_program(
        std::path::PathBuf::from("bound-selector.ag"),
        r#"
        state Game { int n; }
        state Other { int n; }
        actor A owns Game { entry hold() emits none { require(n >= 0); } }
        actor B owns Game { entry hold() emits none { require(n >= 0); } }
        actor enum Move { A; B; }
        actor Mux owns Game {
            entry choose(Move target) emits next: Move {
                unrestricted(next.value);
                Game next_state = { n: n + 1 };
                become next <- target(next_state);
            }
            entry choose_b() emits next: Move {
                unrestricted(next.value);
                Game next_state = { n: n + 1 };
                actor_type<Game> target = Move::B;
                become next <- target(next_state);
            }
        }
        app Test { actor A; actor B; actor Mux; }
        "#
        .to_string(),
    )
    .expect("source resolves");
    let mut model = AppCompilationContext::from_resolved(
        &program,
        None,
        &std::collections::BTreeMap::new(),
        &crate::compiler::model::default_route_planner,
    )
    .expect("model plans");
    let actor = model.actor_by_decl(model.types.names["Mux"]).expect("mux actor");
    for (entry_name, expected) in [("choose", vec!["A", "B"]), ("choose_b", vec!["B"])] {
        let entry = actor.entries.iter().find(|entry| entry.name == entry_name).expect("route entry");
        let entry_model = model
            .entry_model_by_id(crate::compiler::syntax::node::EntryId {
                actor: model.types.names[&actor.name],
                index: actor.entries.iter().position(|candidate| std::ptr::eq(candidate, entry)).expect("entry belongs to actor"),
            })
            .expect("entry model");
        let expanded = model
            .expanded_routes_by_id(crate::compiler::syntax::node::EntryId {
                actor: model.types.names[&actor.name],
                index: actor.entries.iter().position(|candidate| std::ptr::eq(candidate, entry)).expect("entry belongs to actor"),
            })
            .expect("selector expansion resolves")
            .into_iter()
            .map(|route| match route.successor {
                ResolvedSuccessor::Constructed { actor: RouteActorSource::Expanded(actor), .. } => {
                    model.static_actor_reference(&actor).expect("expanded actor resolves")
                }
                _ => panic!("selector route must expand to a concrete actor"),
            })
            .collect::<Vec<_>>();
        assert_eq!(expanded, expected);
        let mut route = entry_model.routes()[0].clone();
        let ResolvedSuccessor::Constructed { actor: display, bound: Some(bound), .. } = &mut route.successor else {
            panic!("constructed route must be bound")
        };
        let BoundRouteActor::Local(local) = bound.actor_target else { panic!("route must bind a local selector") };
        assert_eq!(entry_model.template_selectors()["target"].binding, Some(local));
        let witness = model
            .witness_plan_by_id(crate::compiler::syntax::node::EntryId {
                actor: model.types.names[&actor.name],
                index: actor.entries.iter().position(|candidate| std::ptr::eq(candidate, entry)).expect("entry belongs to actor"),
            })
            .expect("selector witness plan");
        assert_eq!(witness.selectors[0].binding, local);
        assert_eq!(
            witness.selectors[0].family_id,
            model.route_families.iter().find(|family| family.state_id == model.types.names["Game"]).expect("Game route family").id
        );
        let mut selector = entry_model.template_selectors()["target"].clone();
        selector.state = "Unrelated".to_string();
        assert_eq!(
            selector.source_state(&model).expect("bound selector source state"),
            model.source_state_id("Game").expect("Game state")
        );
        let expected_ids = expected.iter().map(|name| StaticActorId::InApp(model.types.names[*name])).collect::<Vec<_>>();
        selector.variants = vec!["Mux".to_string()];
        selector.fixed_actor = Some("Mux".to_string());
        assert_eq!(selector.route_actor_ids().expect("bound selector targets"), expected_ids);
        assert_eq!(
            selector.variant_actor_ids().expect("bound selector domain"),
            [StaticActorId::InApp(model.types.names["A"]), StaticActorId::InApp(model.types.names["B"])]
        );
        *display = RouteActorSource::Expanded(StaticActorId::InApp(model.types.names["Mux"]));
        assert_eq!(
            model
                .route_target_ids_by_id(
                    crate::compiler::syntax::node::EntryId {
                        actor: model.types.names[&actor.name],
                        index: actor
                            .entries
                            .iter()
                            .position(|candidate| std::ptr::eq(candidate, entry))
                            .expect("entry belongs to actor")
                    },
                    &route
                )
                .expect("bound local route target"),
            expected_ids
        );
        let ResolvedSuccessor::Constructed { bound: Some(bound), .. } = &mut route.successor else { unreachable!() };
        bound.actor_target = BoundRouteActor::Local(LocalId { index: local.index + 1, ..local });
        let error = model
            .route_target_ids_by_id(
                crate::compiler::syntax::node::EntryId {
                    actor: model.types.names[&actor.name],
                    index: actor.entries.iter().position(|candidate| std::ptr::eq(candidate, entry)).expect("entry belongs to actor"),
                },
                &route,
            )
            .expect_err("a different local must not inherit the selector domain");
        assert!(error.to_string().contains("local without an actor selector"), "{error}");
    }
    model.types.actor_states.insert(model.types.names["B"], model.types.names["Other"]);
    let error = crate::compiler::model::infer_direct_routes(
        &model.actor_models,
        &model.app_actors,
        &model.types,
        &crate::compiler::model::default_route_planner,
    )
    .expect_err("the route adapter must use bound variant state identities");
    assert!(error.to_string().contains("targets a different source state"), "{error}");
}

#[test]
fn reassigned_actor_enum_locals_keep_their_full_route_domain() {
    for assignment in [
        "target = Move::A;",
        "if (choose_a) { target = Move::A; }",
        "{ target = Move::A; }",
        "for (i, 0, 1, 1) { target = Move::A; }",
        "if (false) { target = Move::A; }",
    ] {
        let program = load_inline_program(
            std::path::PathBuf::from("reassigned-selector.ag"),
            format!(
                r#"
                    state Game {{ int n; }}
                    actor A owns Game {{ entry hold() emits none {{ require(n >= 0); }} }}
                    actor B owns Game {{ entry hold() emits none {{ require(n >= 10); }} }}
                    actor enum Move {{ A; B; }}
                    actor Mux owns Game {{
                        fn unrelated(int target) -> int {{ target = target + 1; return target; }}
                        entry choose(bool choose_a) emits next: Move {{
                            Move target = Move::B;
                            {assignment}
                            unrestricted(next.value);
                            become next <- target(Game {{ n: n + 1 }});
                        }}
                        entry fixed() emits next: Move {{
                            Move target = Move::B;
                            unrestricted(next.value);
                            become next <- target(Game {{ n: n + 1 }});
                        }}
                        entry other() emits none {{ int target = 0; target = 1; require(target == 1); }}
                    }}
                    app Test {{ actor A; actor B; actor Mux; }}
                "#,
            ),
        )
        .expect("selector assignments resolve");
        let model =
            AppCompilationContext::from_resolved(&program, None, &BTreeMap::new(), &crate::compiler::model::default_route_planner)
                .expect("reassigned selectors are planned");
        let actor = model.types.names["Mux"];
        let dynamic = model.entry_model_by_id(EntryId { actor, index: 0 }).expect("dynamic entry");
        let selector = &dynamic.template_selectors()["target"];
        assert_eq!(selector.fixed_actor, None, "{assignment}");
        assert_eq!(selector.fixed_index, None, "{assignment}");
        assert_eq!(
            selector.route_actor_ids().expect("dynamic route domain"),
            [StaticActorId::InApp(model.types.names["A"]), StaticActorId::InApp(model.types.names["B"])],
            "{assignment}"
        );
        let fixed = model.entry_model_by_id(EntryId { actor, index: 1 }).expect("fixed entry");
        let selector = &fixed.template_selectors()["target"];
        assert_eq!(selector.fixed_actor.as_deref(), Some("B"), "writes to different bindings do not affect this local");
        assert_eq!(selector.fixed_index, Some(1));
        assert_eq!(selector.route_actor_ids().expect("fixed route domain"), [StaticActorId::InApp(model.types.names["B"])]);
    }
}

#[test]
fn retained_selector_initializer_uses_bound_ast_expression() {
    let source_text = r#"
        state Game { int n; }
        actor A owns Game { entry hold() emits none { require(n >= 0); } }
        actor B owns Game { entry hold() emits none { require(n >= 10 && n < 100); } }
        actor enum Move { A; B; }
        actor Mux owns Game {
            entry choose(int choice) emits next: Move {
                actor_type<Game> target = (Move[choice]);
                unrestricted(next.value);
                Game next_state = { n: n + 1 };
                become next <- target(next_state);
            }
        }
        app Test { actor A; actor B; actor Mux; }
        "#
    .to_string();
    let sources =
        SourceSet::discover_inline(std::path::PathBuf::from("retained-selector.ag"), source_text.clone()).expect("source discovers");
    sources
        .with_resolved(|program| {
            let model = AppCompilationContext::from_resolved(
                &program,
                None,
                &std::collections::BTreeMap::new(),
                &crate::compiler::model::default_route_planner,
            )?;
            let actor = model.actor_by_decl(model.types.names["Mux"])?;
            let entry = &actor.entries[0];
            let selector = &model
                .entry_model_by_id(crate::compiler::syntax::node::EntryId {
                    actor: model.types.names[&actor.name],
                    index: actor.entries.iter().position(|candidate| std::ptr::eq(candidate, entry)).expect("entry belongs to actor"),
                })?
                .template_selectors()["target"];
            assert!(selector.binding.is_some());
            assert_eq!(
                selector.route_actor_ids()?,
                [StaticActorId::InApp(model.types.names["A"]), StaticActorId::InApp(model.types.names["B"])]
            );
            Ok(())
        })
        .expect("retained selector AST binds without reparsing initializer text");
    let out_dir = std::env::temp_dir().join(format!("argent-retained-selector-{}", std::process::id()));
    crate::build_inline("retained-selector.ag", source_text, &out_dir)
        .expect("retained selector also lowers into a compiled contract");
    std::fs::remove_dir_all(out_dir).expect("remove selector test output");
}

#[test]
fn explicitly_enum_typed_copies_remain_selectors_and_compile() {
    let source = r#"
        state Game { int n; }
        actor A owns Game { entry hold() emits none { require(n >= 0); } }
        actor B owns Game { entry hold() emits none { require(n > 1); } }
        actor enum Move { A; B; }
        actor Mux owns Game {
            entry choose() emits next: Move {
                Move original = Move::B;
                Move copied = original;
                Game next_state = { n: n + 1 };
                unrestricted(next.value);
                become next <- copied(next_state);
            }
        }
        app Test { actor A; actor B; actor Mux; }
    "#;
    SourceSet::discover_inline(std::path::PathBuf::from("enum-selector-copy.ag"), source.to_string())
        .unwrap()
        .with_resolved(|program| {
            let model = AppCompilationContext::from_resolved(
                &program,
                None,
                &std::collections::BTreeMap::new(),
                &crate::compiler::model::default_route_planner,
            )?;
            let entry =
                model.entry_model_by_id(crate::compiler::syntax::node::EntryId { actor: model.types.names["Mux"], index: 0 })?;
            assert!(entry.handle_value_initializers().is_empty(), "enum locals are not ordinary handle value declarations");
            assert!(entry.template_selectors().contains_key("original"));
            assert!(entry.template_selectors().contains_key("copied"));
            Ok(())
        })
        .expect("enum-typed copy retains its existing selector classification");
    let output = tempfile::tempdir().expect("enum copy output directory created");
    crate::build_inline("enum-selector-copy.ag", source, output.path()).expect("enum-typed selector copy compiles through Sil");
}

#[test]
fn selector_initializer_uses_bound_enum_value_of_an_earlier_local() {
    let valid = r#"
        state Game { int n; }
        actor A owns Game { entry hold() emits none { require(n >= 0); } }
        actor B owns Game { entry hold() emits none { require(n > 1); } }
        actor enum Move { A; B; }
        actor Mux owns Game {
            entry choose(Move target) emits next: Move {
                Move copied = target;
                Game next_state = { n: n + 1 };
                unrestricted(next.value);
                become next <- copied(next_state);
            }
        }
        app Test { actor A; actor B; actor Mux; }
        "#;
    let out_dir = std::env::temp_dir().join(format!("argent-enum-copy-{}", std::process::id()));
    crate::build_inline("enum-copy.ag", valid.to_string(), &out_dir).expect("bound enum copy compiles");
    std::fs::remove_dir_all(out_dir).expect("remove enum copy output");

    let invalid = valid.replace("Move copied = target;", "int raw = 0; Move copied = raw;");
    let sources = SourceSet::discover_inline(std::path::PathBuf::from("unbound-enum-copy.ag"), invalid).expect("source discovers");
    let error = sources
        .with_resolved(|program| {
            AppCompilationContext::from_resolved(
                &program,
                None,
                &std::collections::BTreeMap::new(),
                &crate::compiler::model::default_route_planner,
            )
            .map(|_| ())
        })
        .expect_err("an integer local has no actor enum identity");
    assert!(error.to_string().contains("without an actor enum initializer"), "{error}");
}

#[test]
fn bound_selector_rejects_distinct_declarations_with_the_same_display_label() {
    let program = load_inline_program(
        std::path::PathBuf::from("nominal-selector.ag"),
        r#"
        state Game { int n; }
        state Other { int n; }
        actor A owns Game { entry hold() emits none { require(n >= 0); } }
        actor B owns Game { entry hold() emits none { require(n >= 0); } }
        actor enum Move { A; B; }
        actor enum OtherMove { A; B; }
        actor Mux owns Game {
            entry choose(Move target) emits next: Move {
                unrestricted(next.value);
                Game next_state = { n: n + 1 };
                become next <- target(next_state);
            }
        }
        app Test { actor A; actor B; actor Mux; }
        "#
        .to_string(),
    )
    .expect("source resolves");
    let mut model = AppCompilationContext::from_resolved(
        &program,
        None,
        &std::collections::BTreeMap::new(),
        &crate::compiler::model::default_route_planner,
    )
    .expect("model plans");
    let move_id = model.types.names["Move"];
    let other_enum = model.types.names["OtherMove"];
    let other_state = model.types.names["Other"];
    model.types.display_names.insert(other_enum, "Move".to_string());
    model.types.display_names.insert(other_state, "Game".to_string());
    model.types.display_names.insert(model.types.names["A"], "B".to_string());
    let actor = model.actor_by_decl(model.types.names["Mux"]).expect("Mux actor");
    let entry_model = model
        .entry_model_by_id(crate::compiler::syntax::node::EntryId {
            actor: model.types.names[&actor.name],
            index: actor
                .entries
                .iter()
                .position(|candidate| std::ptr::eq(candidate, &actor.entries[0]))
                .expect("entry belongs to actor"),
        })
        .expect("entry model");

    let wrong_enum = entry_model
        .bound_selector("target", other_enum, None, ResolvedTypeBase::ActorEnum(move_id), &model.types, &BTreeMap::new())
        .expect_err("equal display labels cannot make distinct actor enums interchangeable");
    assert!(wrong_enum.to_string().contains("declares actor enum value"), "{wrong_enum}");

    let wrong_state = entry_model
        .bound_selector("target", move_id, None, ResolvedTypeBase::ActorHandle(other_state), &model.types, &BTreeMap::new())
        .expect_err("equal display labels cannot make distinct states interchangeable");
    assert!(wrong_state.to_string().contains("declares actor handle"), "{wrong_state}");

    let actor_ids = model.app_actors.iter().map(|name| (name.clone(), StaticActorId::InApp(model.types.names[name]))).collect();
    let selector = entry_model
        .bound_selector("target", move_id, None, ResolvedTypeBase::ActorEnum(move_id), &model.types, &actor_ids)
        .expect("display alias does not change bound selector targets");
    assert_eq!(
        selector.route_actor_ids().expect("bound actor domain"),
        &[StaticActorId::InApp(model.types.names["A"]), StaticActorId::InApp(model.types.names["B"])]
    );
}

#[test]
fn local_shadow_cannot_impersonate_actor_enum_selector() {
    let sources = SourceSet::discover_inline(
        std::path::PathBuf::from("shadowed-selector.ag"),
        r#"
        state Game { int n; }
        actor A owns Game { entry hold() emits none { require(n >= 0); } }
        actor B owns Game { entry hold() emits none { require(n >= 0); } }
        actor enum Move { A; B; }
        actor Mux owns Game {
            entry choose(int choice) emits next: Move {
                int Move = 0;
                actor_type<Game> target = Move[choice];
                unrestricted(next.value);
                Game next_state = { n: n + 1 };
                become next <- target(next_state);
            }
        }
        app Test { actor A; actor B; actor Mux; }
        "#
        .to_string(),
    )
    .expect("source discovers");
    let error = sources
        .with_resolved(|program| {
            AppCompilationContext::from_resolved(
                &program,
                None,
                &std::collections::BTreeMap::new(),
                &crate::compiler::model::default_route_planner,
            )
            .map(|_| ())
        })
        .expect_err("a bound local cannot stand in for an actor enum declaration");
    assert!(error.to_string().contains("without an actor enum initializer"), "unexpected error: {error}");
}

#[test]
fn models_covenant_groups_and_preserves_source_nodes() {
    let constants =
        load_inline_program("entry-constants.ag".into(), "const int MAX = 3;".to_string()).expect("constant source resolves");
    let names = constants.selected_declaration_names(None, &[]).expect("constant is selected");
    let consts = ConstResolver::from_resolved(&constants, &names);
    let range = || Cardinality::Range { minimum: CardinalityBound::Literal(0), maximum: CardinalityBound::Const("MAX".to_string()) };
    let entry = EntryDecl {
        kind: EntryKind::Leader,
        name: "step".to_string(),
        params: Vec::new(),
        consumes: vec![ConsumeDecl { name: "peer".to_string(), actor: "Peer".to_string(), cardinality: range() }],
        observes: vec![ObserveDecl {
            name: "remote".to_string(),
            covenant_expr: "self.remote_id".to_string(),
            inputs: vec![ObservedActorDecl {
                name: "before".to_string(),
                actor: "Remote".to_string(),
                open_state: None,
                cardinality: range(),
            }],
            outputs: vec![ObservedActorDecl {
                name: "after".to_string(),
                actor: "Remote".to_string(),
                open_state: None,
                cardinality: range(),
            }],
        }],
        spawns: vec![SpawnDecl {
            name: "launch".to_string(),
            covenant: "child".to_string(),
            outputs: vec![SpawnOutputDecl {
                name: "child".to_string(),
                actor: "Child".to_string(),
                cardinality: range(),
                group_index: 0,
            }],
        }],
        emits: EmitSpec::Outputs(vec![EmitOutput {
            name: "next".to_string(),
            actors: vec!["Move".to_string()],
            cardinality: range(),
            auth_index: 0,
        }]),
        routes: Vec::new(),
        terminal_route_sets: Vec::new(),
    };
    let selectors = BTreeMap::from([(
        "target".to_string(),
        TemplateSelector {
            name: "target".to_string(),
            binding: None,
            actor_enum: "Move".to_string(),
            state: "Game".to_string(),
            variants: vec!["Pawn".to_string(), "King".to_string()],
            fixed_actor: Some("King".to_string()),
            fixed_index: Some(1),
            targets: None,
        },
    )]);
    let actor = test_actor();
    let model = EntryModel::new(test_entry_id(), &actor, &entry, selectors, &consts).expect("entry cardinalities resolve");
    let cardinalities = model
        .groups()
        .flat_map(|group| group.inputs().iter().chain(group.outputs()))
        .map(EntryInteraction::cardinality)
        .collect::<Vec<_>>();
    assert_eq!(cardinalities, vec![ResolvedCardinality::Range { minimum: 0, maximum: 3 }; 5]);
    let locations = model
        .groups()
        .flat_map(|group| group.inputs().iter().chain(group.outputs()))
        .map(EntryInteraction::location)
        .collect::<Vec<_>>();
    assert_eq!(locations, vec![InteractionLocation::Range { start: 0, singleton_count: 0 }; 5]);

    let InteractionSource::Consume(consume) = model.current().inputs()[0].source() else {
        panic!("current input must retain its consume declaration");
    };
    assert!(std::ptr::eq(consume, &entry.consumes[0]));
    assert!(matches!(consume.cardinality, Cardinality::Range { .. }));
    assert_eq!(model.current().inputs()[0].handle(), "peer");
    let InteractionSource::CurrentOutput(output) = model.current().outputs()[0].source() else {
        panic!("current output must retain its emits declaration");
    };
    let EmitSpec::Outputs(outputs) = &entry.emits else {
        panic!("test entry must have named outputs");
    };
    assert!(std::ptr::eq(output, &outputs[0]));
    assert!(matches!(output.cardinality, Cardinality::Range { .. }));
    assert_eq!(model.current().outputs()[0].handle(), "next");
    assert!(matches!(model.current().outputs()[0].target(), ActorTarget::UnresolvedStatic(names) if names == &["Move"]));
    let observe_group = model.existing_groups().next().expect("observe group");
    assert!(std::ptr::eq(observe_group.observe().expect("observe source"), &entry.observes[0]));
    let InteractionSource::ObserveInput(observed) = observe_group.inputs()[0].source() else {
        panic!("observe input must retain its source declaration");
    };
    assert!(std::ptr::eq(observed, &entry.observes[0].inputs[0]));
    assert!(matches!(observed.cardinality, Cardinality::Range { .. }));
    assert_eq!(observe_group.inputs()[0].handle(), "before");
    assert!(matches!(observe_group.inputs()[0].target(), ActorTarget::Source(expr) if expr == "Remote"));

    let spawn_group = model.genesis_groups().next().expect("spawn group");
    assert!(std::ptr::eq(spawn_group.spawn().expect("spawn source"), &entry.spawns[0]));
    let InteractionSource::SpawnOutput(output) = spawn_group.outputs()[0].source() else {
        panic!("spawn output must retain its source declaration");
    };
    assert!(std::ptr::eq(output, &entry.spawns[0].outputs[0]));
    assert!(matches!(output.cardinality, Cardinality::Range { .. }));
    assert_eq!(spawn_group.outputs()[0].handle(), "child");
    assert!(matches!(spawn_group.outputs()[0].target(), ActorTarget::Source(expr) if expr == "Child"));

    assert_eq!(model.template_selectors()["target"].fixed_actor.as_deref(), Some("King"));
}

#[test]
fn actor_targets_keep_source_expressions_out_of_static_planning() {
    let program = load_inline_program(
        std::path::PathBuf::from("observed-target-binding.ag"),
        include_str!("../../../../tests/fixtures/emit/observed_template_witnesses/app.ag").to_string(),
    )
    .expect("source resolves");
    let model = AppCompilationContext::from_resolved(
        &program,
        None,
        &std::collections::BTreeMap::new(),
        &crate::compiler::model::default_route_planner,
    )
    .expect("model plans");
    let actor = model.actor_by_decl(model.types.names["Local"]).expect("local actor");
    let entry = &actor.entries[0];
    let foreign = StaticActorId::InApp(model.types.names["Foreign"]);
    let planned = model
        .entry_model_by_id(crate::compiler::syntax::node::EntryId {
            actor: model.types.names[&actor.name],
            index: actor.entries.iter().position(|candidate| std::ptr::eq(candidate, entry)).expect("entry belongs to actor"),
        })
        .expect("entry model");
    assert_eq!(planned.existing_groups().next().expect("observe group").inputs()[0].target().single_static_actor(), Some(&foreign));

    let mut copied_actor = actor.clone();
    copied_actor.entries[0].observes[0].inputs[0].actor = "copied_text_changed".to_string();
    let copied_entry = &copied_actor.entries[0];
    let actor_ids = test_actor_ids(&["Foreign", "Local"]);
    let mut copied = EntryModel::new(
        planned.id,
        &copied_actor,
        copied_entry,
        BTreeMap::new(),
        &ConstResolver::from_resolved(&program, &BTreeMap::new()),
    )
    .expect("copied entry model");
    copied.bind_actor_targets(model.resolution, &model.types, &actor_ids).expect("bound targets");
    assert_eq!(copied.existing_groups().next().expect("observe group").inputs()[0].target().single_static_actor(), Some(&foreign));
}

#[test]
fn current_emit_domain_uses_bound_declarations_after_source_copy() {
    let program = load_inline_program(
        std::path::PathBuf::from("current-target-binding.ag"),
        include_str!("../../../../examples/route_state_body_choice.ag").to_string(),
    )
    .expect("source resolves");
    let mut model = AppCompilationContext::from_resolved(
        &program,
        None,
        &std::collections::BTreeMap::new(),
        &crate::compiler::model::default_route_planner,
    )
    .expect("model plans");
    let actor = model.actor_by_decl(model.types.names["Mux"]).expect("Mux actor");
    let planned = model
        .entry_model_by_id(crate::compiler::syntax::node::EntryId {
            actor: model.types.names[&actor.name],
            index: actor
                .entries
                .iter()
                .position(|candidate| std::ptr::eq(candidate, &actor.entries[0]))
                .expect("entry belongs to actor"),
        })
        .expect("entry model");
    let mut copied_actor = actor.clone();
    let EmitSpec::Outputs(outputs) = &mut copied_actor.entries[0].emits else { panic!("Mux has named outputs") };
    outputs[0].actors[0] = "source_text_changed".to_string();
    let actor_ids = model
        .types
        .names
        .iter()
        .filter(|(_, id)| id.kind() == SymbolKind::Actor)
        .map(|(name, id)| (name.clone(), StaticActorId::InApp(*id)))
        .collect();
    let mut copied = EntryModel::new(
        planned.id,
        &copied_actor,
        &copied_actor.entries[0],
        BTreeMap::new(),
        &ConstResolver::from_resolved(&program, &BTreeMap::new()),
    )
    .expect("copied entry model");
    copied.bind_actor_targets(model.resolution, &model.types, &actor_ids).expect("bound targets");
    assert_eq!(
        copied.current().outputs()[0].target().static_actors().cloned().collect::<Vec<_>>(),
        [StaticActorId::InApp(model.types.names["Pawn"]), StaticActorId::InApp(model.types.names["Knight"])]
    );
    model.types.display_names.insert(model.types.names["Pawn"], "Knight".to_string());
    copied.bind_actor_targets(model.resolution, &model.types, &actor_ids).expect("display alias cannot change identity");
    assert_eq!(
        copied.current().outputs()[0].target().static_actors().cloned().collect::<Vec<_>>(),
        [StaticActorId::InApp(model.types.names["Pawn"]), StaticActorId::InApp(model.types.names["Knight"])]
    );
}

#[test]
fn models_named_and_empty_emit_domains() {
    let entry = EntryDecl {
        kind: EntryKind::Leader,
        name: "step".to_string(),
        params: Vec::new(),
        consumes: Vec::new(),
        observes: Vec::new(),
        spawns: Vec::new(),
        emits: EmitSpec::Outputs(vec![
            EmitOutput { name: "first".to_string(), actors: vec!["Pawn".to_string()], cardinality: Cardinality::One, auth_index: 0 },
            EmitOutput { name: "second".to_string(), actors: vec!["Move".to_string()], cardinality: Cardinality::One, auth_index: 1 },
        ]),
        routes: Vec::new(),
        terminal_route_sets: Vec::new(),
    };
    let actor = test_actor();
    let constants = load_inline_program("empty-constants.ag".into(), String::new()).expect("empty source resolves");
    let consts = ConstResolver::from_resolved(&constants, &BTreeMap::new());
    let model = EntryModel::new(test_entry_id(), &actor, &entry, BTreeMap::new(), &consts).expect("entry cardinalities resolve");
    let EmitSpec::Outputs(outputs) = &entry.emits else {
        panic!("test entry must have named outputs");
    };
    let InteractionSource::CurrentOutput(first) = model.current().outputs()[0].source() else {
        panic!("named output must retain its emit output");
    };
    let InteractionSource::CurrentOutput(second) = model.current().outputs()[1].source() else {
        panic!("named output must retain its emit output");
    };
    assert!(std::ptr::eq(first, &outputs[0]));
    assert!(std::ptr::eq(second, &outputs[1]));
    assert_eq!(model.current().outputs()[0].handle(), "first");
    assert!(matches!(model.current().outputs()[0].target(), ActorTarget::UnresolvedStatic(names) if names == &["Pawn"]));
    assert_eq!(model.current().outputs()[1].handle(), "second");
    assert!(matches!(model.current().outputs()[1].target(), ActorTarget::UnresolvedStatic(names) if names == &["Move"]));

    let mut empty_entry = entry.clone();
    empty_entry.emits = EmitSpec::None;
    let empty_model =
        EntryModel::new(test_entry_id(), &actor, &empty_entry, BTreeMap::new(), &consts).expect("entry cardinalities resolve");
    assert!(empty_model.current().outputs().is_empty());
}

#[test]
fn resolves_range_bounds_from_int_constants() {
    let cardinality = Cardinality::Range {
        minimum: CardinalityBound::Const("MIN".to_string()),
        maximum: CardinalityBound::Const("MAX".to_string()),
    };

    assert_eq!(
        resolve_test_cardinality(&cardinality, "const int MIN = 1 /* fixed */; const int MAX = 3;").expect("int constants resolve"),
        ResolvedCardinality::Range { minimum: 1, maximum: 3 }
    );
}

#[test]
fn plans_locations_before_within_and_after_a_range() {
    fn plan<const N: usize>(items: [(&str, &Cardinality); N]) -> Vec<InteractionLocation> {
        plan_interaction_locations(items, "Actor", "step", "section").expect("one range has a location plan")
    }

    let one = Cardinality::One;
    let range = Cardinality::Range { minimum: CardinalityBound::Literal(0), maximum: CardinalityBound::Literal(3) };

    assert_eq!(plan([("first", &one), ("second", &one)]), [InteractionLocation::FromStart(0), InteractionLocation::FromStart(1)]);
    assert_eq!(
        plan([("many", &range), ("second", &one), ("third", &one)]),
        [
            InteractionLocation::Range { start: 0, singleton_count: 2 },
            InteractionLocation::FromEnd(2),
            InteractionLocation::FromEnd(1),
        ]
    );
    assert_eq!(
        plan([("first", &one), ("many", &range), ("third", &one)]),
        [
            InteractionLocation::FromStart(0),
            InteractionLocation::Range { start: 1, singleton_count: 2 },
            InteractionLocation::FromEnd(1),
        ]
    );
    assert_eq!(
        plan([("first", &one), ("second", &one), ("many", &range)]),
        [
            InteractionLocation::FromStart(0),
            InteractionLocation::FromStart(1),
            InteractionLocation::Range { start: 2, singleton_count: 2 },
        ]
    );
}

#[test]
fn rejects_multiple_ranges_in_one_section() {
    let range = Cardinality::Range { minimum: CardinalityBound::Literal(0), maximum: CardinalityBound::Literal(3) };
    let err = plan_interaction_locations([("first", &range), ("second", &range)], "Actor", "step", "consumes")
        .expect_err("one section cannot derive two range lengths");

    assert_eq!(err.message, "entry `Actor::step` `consumes` supports at most one range, found `first` and `second`");
}

#[test]
fn rejects_invalid_resolved_range_bounds() {
    let cases = [
        (
            Cardinality::Range { minimum: CardinalityBound::Const("MISSING".to_string()), maximum: CardinalityBound::Literal(1) },
            "",
            "references unknown constant `MISSING`",
        ),
        (
            Cardinality::Range { minimum: CardinalityBound::Const("BOUND".to_string()), maximum: CardinalityBound::Literal(1) },
            "const bool BOUND = true;",
            "bound `BOUND` must have type `int`",
        ),
        (
            Cardinality::Range { minimum: CardinalityBound::Const("BOUND".to_string()), maximum: CardinalityBound::Literal(2) },
            "const int BOUND = true;",
            "bound `BOUND` must be initialized with a valid `int` literal",
        ),
        (
            Cardinality::Range { minimum: CardinalityBound::Const("BOUND".to_string()), maximum: CardinalityBound::Literal(2) },
            "const int BOUND = -1;",
            "must have non-negative bounds",
        ),
        (
            Cardinality::Range { minimum: CardinalityBound::Literal(3), maximum: CardinalityBound::Literal(2) },
            "",
            "minimum 3 exceeds maximum 2",
        ),
        (
            Cardinality::Range {
                minimum: CardinalityBound::Literal(0),
                maximum: CardinalityBound::Literal(MAX_ENTRY_RANGE_CARDINALITY + 1),
            },
            "",
            "maximum 513 exceeds compiler limit 512",
        ),
    ];

    for (cardinality, source, expected) in cases {
        let err = resolve_test_cardinality(&cardinality, source).expect_err("invalid range bounds must be rejected");
        assert!(err.to_string().contains(expected), "unexpected error: {err}");
    }
}

#[test]
fn accepts_the_maximum_entry_range_cardinality() {
    let cardinality =
        Cardinality::Range { minimum: CardinalityBound::Literal(0), maximum: CardinalityBound::Literal(MAX_ENTRY_RANGE_CARDINALITY) };

    assert_eq!(
        resolve_test_cardinality(&cardinality, "").expect("the compiler limit is inclusive"),
        ResolvedCardinality::Range { minimum: 0, maximum: MAX_ENTRY_RANGE_CARDINALITY }
    );
}

fn resolve_test_cardinality(cardinality: &Cardinality, source: &str) -> Result<ResolvedCardinality> {
    let program = load_inline_program("range-constants.ag".into(), source.to_string())?;
    let names = program.selected_declaration_names(None, &[])?;
    let consts = ConstResolver::from_resolved(&program, &names);
    let entry = EntryDecl {
        kind: EntryKind::Leader,
        name: "step".to_string(),
        params: Vec::new(),
        consumes: Vec::new(),
        observes: Vec::new(),
        spawns: Vec::new(),
        emits: EmitSpec::None,
        routes: Vec::new(),
        terminal_route_sets: Vec::new(),
    };
    ResolvedCardinality::resolve(cardinality, "Actor", &entry, "items", &consts)
}

fn test_actor() -> ActorDecl {
    ActorDecl { name: "Source".to_string(), state: "Game".to_string(), functions: Vec::new(), entries: Vec::new() }
}

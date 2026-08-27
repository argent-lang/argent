use super::*;
use crate::compiler::syntax::{ActorDecl, Cardinality, CardinalityBound, ConstDecl, EmitOutput, EmitSpec, EntryBody, EntryKind};

#[test]
fn models_covenant_groups_and_preserves_source_nodes() {
    let max = ConstDecl { ty: TypeRef::new("int"), name: "MAX".to_string(), value: "3".to_string() };
    let range = || Cardinality::Range { minimum: CardinalityBound::Literal(0), maximum: CardinalityBound::Const("MAX".to_string()) };
    let mut entry = EntryDecl {
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
        body: EntryBody::default(),
        routes: Vec::new(),
        terminal_route_sets: Vec::new(),
    };
    set_entry_body(&mut entry, "become next <- target(next);");
    let selectors = BTreeMap::from([(
        "target".to_string(),
        TemplateSelector {
            name: "target".to_string(),
            actor_enum: "Move".to_string(),
            state: "Game".to_string(),
            variants: vec!["Pawn".to_string(), "King".to_string()],
            selector_expr: "selector".to_string(),
            fixed_actor: Some("King".to_string()),
        },
    )]);
    let actor_enums = BTreeMap::from([(
        "Move".to_string(),
        ActorEnumInfo { name: "Move".to_string(), state: "Game".to_string(), variants: vec!["Pawn".to_string(), "King".to_string()] },
    )]);

    let actor = test_actor();
    let model =
        EntryModel::new(&actor, &entry, &actor_enums, selectors, &ConstResolver::new(&[&max])).expect("entry cardinalities resolve");
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
    assert_eq!(model.current().outputs()[0].target().actors().collect::<Vec<_>>(), ["Pawn", "King"]);
    let observe_group = model.existing_groups().next().expect("observe group");
    assert!(std::ptr::eq(observe_group.observe().expect("observe source"), &entry.observes[0]));
    let InteractionSource::ObserveInput(observed) = observe_group.inputs()[0].source() else {
        panic!("observe input must retain its source declaration");
    };
    assert!(std::ptr::eq(observed, &entry.observes[0].inputs[0]));
    assert!(matches!(observed.cardinality, Cardinality::Range { .. }));
    assert_eq!(observe_group.inputs()[0].handle(), "before");
    assert_eq!(observe_group.inputs()[0].target().static_actors().collect::<Vec<_>>(), ["Remote"]);

    let spawn_group = model.genesis_groups().next().expect("spawn group");
    assert!(std::ptr::eq(spawn_group.spawn().expect("spawn source"), &entry.spawns[0]));
    let InteractionSource::SpawnOutput(output) = spawn_group.outputs()[0].source() else {
        panic!("spawn output must retain its source declaration");
    };
    assert!(std::ptr::eq(output, &entry.spawns[0].outputs[0]));
    assert!(matches!(output.cardinality, Cardinality::Range { .. }));
    assert_eq!(spawn_group.outputs()[0].handle(), "child");
    assert_eq!(spawn_group.outputs()[0].target().static_actors().collect::<Vec<_>>(), ["Child"]);

    assert!(matches!(
        &model.expanded_routes()[0].successor,
        ResolvedSuccessor::Constructed { actor, .. } if actor == "King"
    ));
    let app_actors = AppActors::new(["Source", "Peer", "Remote", "Child", "Pawn", "King"].into_iter().map(str::to_string).collect());
    assert_eq!(
        model.actor_template_uses("Source", &app_actors),
        ActorTemplateUses {
            reads: ["Peer", "Remote"].into_iter().map(str::to_string).collect(),
            writes: ["Remote", "Child"].into_iter().map(str::to_string).collect(),
        }
    );
}

#[test]
fn actor_targets_keep_source_expressions_out_of_static_planning() {
    let entry = EntryDecl {
        kind: EntryKind::Leader,
        name: "step".to_string(),
        params: Vec::new(),
        consumes: Vec::new(),
        observes: Vec::new(),
        spawns: Vec::new(),
        emits: EmitSpec::None,
        body: EntryBody::default(),
        routes: Vec::new(),
        terminal_route_sets: Vec::new(),
    };
    let observe = ObserveDecl {
        name: "remote".to_string(),
        covenant_expr: "self.remote_id".to_string(),
        inputs: vec![ObservedActorDecl {
            name: "before".to_string(),
            actor: "Foreign".to_string(),
            open_state: Some("ForeignState".to_string()),
            cardinality: Cardinality::One,
        }],
        outputs: Vec::new(),
    };

    let source = ActorTarget::source_or_static(&entry, "self.foreign_type");
    assert_eq!(source.actors().collect::<Vec<_>>(), ["self.foreign_type"]);
    assert!(source.static_actors().next().is_none());

    let open = ActorTarget::observed(&entry, &observe, &observe.inputs[0]);
    assert_eq!(open.actors().collect::<Vec<_>>(), ["Foreign"]);
    assert!(open.static_actors().next().is_none());

    assert_eq!(ActorTarget::static_actor("Foreign").static_actors().collect::<Vec<_>>(), ["Foreign"]);
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
        body: EntryBody::default(),
        routes: Vec::new(),
        terminal_route_sets: Vec::new(),
    };
    let actor_enums = BTreeMap::from([(
        "Move".to_string(),
        ActorEnumInfo { name: "Move".to_string(), state: "Game".to_string(), variants: vec!["Pawn".to_string(), "King".to_string()] },
    )]);

    let actor = test_actor();
    let model =
        EntryModel::new(&actor, &entry, &actor_enums, BTreeMap::new(), &ConstResolver::new(&[])).expect("entry cardinalities resolve");
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
    assert_eq!(model.current().outputs()[0].target().actors().collect::<Vec<_>>(), ["Pawn"]);
    assert_eq!(model.current().outputs()[1].handle(), "second");
    assert_eq!(model.current().outputs()[1].target().actors().collect::<Vec<_>>(), ["Pawn", "King"]);

    let mut empty_entry = entry.clone();
    empty_entry.emits = EmitSpec::None;
    let empty_model = EntryModel::new(&actor, &empty_entry, &actor_enums, BTreeMap::new(), &ConstResolver::new(&[]))
        .expect("entry cardinalities resolve");
    assert!(empty_model.current().outputs().is_empty());
}

#[test]
fn resolves_range_bounds_from_int_constants() {
    let minimum = ConstDecl { ty: TypeRef::new("int"), name: "MIN".to_string(), value: "1 /* fixed */".to_string() };
    let maximum = ConstDecl { ty: TypeRef::new("int"), name: "MAX".to_string(), value: "3".to_string() };
    let cardinality = Cardinality::Range {
        minimum: CardinalityBound::Const("MIN".to_string()),
        maximum: CardinalityBound::Const("MAX".to_string()),
    };

    assert_eq!(
        resolve_test_cardinality(&cardinality, &[&minimum, &maximum]).expect("int constants resolve"),
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
    let wrong_type = ConstDecl { ty: TypeRef::new("bool"), name: "BOUND".to_string(), value: "1".to_string() };
    let expression = ConstDecl { ty: TypeRef::new("int"), name: "BOUND".to_string(), value: "1 + 1".to_string() };
    let negative = ConstDecl { ty: TypeRef::new("int"), name: "BOUND".to_string(), value: "-1".to_string() };
    let cases = [
        (
            Cardinality::Range { minimum: CardinalityBound::Const("MISSING".to_string()), maximum: CardinalityBound::Literal(1) },
            Vec::new(),
            "references unknown constant `MISSING`",
        ),
        (
            Cardinality::Range { minimum: CardinalityBound::Const("BOUND".to_string()), maximum: CardinalityBound::Literal(1) },
            vec![&wrong_type],
            "bound `BOUND` must have type `int`",
        ),
        (
            Cardinality::Range { minimum: CardinalityBound::Const("BOUND".to_string()), maximum: CardinalityBound::Literal(2) },
            vec![&expression],
            "bound `BOUND` must be initialized with a valid `int` literal",
        ),
        (
            Cardinality::Range { minimum: CardinalityBound::Const("BOUND".to_string()), maximum: CardinalityBound::Literal(2) },
            vec![&negative],
            "must have non-negative bounds",
        ),
        (
            Cardinality::Range { minimum: CardinalityBound::Literal(3), maximum: CardinalityBound::Literal(2) },
            Vec::new(),
            "minimum 3 exceeds maximum 2",
        ),
        (
            Cardinality::Range {
                minimum: CardinalityBound::Literal(0),
                maximum: CardinalityBound::Literal(MAX_ENTRY_RANGE_CARDINALITY + 1),
            },
            Vec::new(),
            "maximum 513 exceeds compiler limit 512",
        ),
    ];

    for (cardinality, consts, expected) in cases {
        let err = resolve_test_cardinality(&cardinality, &consts).expect_err("invalid range bounds must be rejected");
        assert!(err.to_string().contains(expected), "unexpected error: {err}");
    }
}

#[test]
fn accepts_the_maximum_entry_range_cardinality() {
    let cardinality =
        Cardinality::Range { minimum: CardinalityBound::Literal(0), maximum: CardinalityBound::Literal(MAX_ENTRY_RANGE_CARDINALITY) };

    assert_eq!(
        resolve_test_cardinality(&cardinality, &[]).expect("the compiler limit is inclusive"),
        ResolvedCardinality::Range { minimum: 0, maximum: MAX_ENTRY_RANGE_CARDINALITY }
    );
}

fn resolve_test_cardinality(cardinality: &Cardinality, consts: &[&ConstDecl]) -> Result<ResolvedCardinality> {
    let entry = EntryDecl {
        kind: EntryKind::Leader,
        name: "step".to_string(),
        params: Vec::new(),
        consumes: Vec::new(),
        observes: Vec::new(),
        spawns: Vec::new(),
        emits: EmitSpec::None,
        body: EntryBody::default(),
        routes: Vec::new(),
        terminal_route_sets: Vec::new(),
    };
    ResolvedCardinality::resolve(cardinality, "Actor", &entry, "items", &ConstResolver::new(consts))
}

fn test_actor() -> ActorDecl {
    ActorDecl { name: "Source".to_string(), state: "Game".to_string(), functions: Vec::new(), entries: Vec::new() }
}

fn set_entry_body(entry: &mut EntryDecl, source: &str) {
    let body = EntryBody::new(source).expect("test entry body parses");
    let analysis = crate::compiler::syntax::body::routes::analyze_entry_routes(&body).expect("test entry routes analyze");
    entry.body = body;
    entry.routes = analysis.routes;
    entry.terminal_route_sets = analysis.terminal_route_sets;
}

use super::*;
use crate::compiler::syntax::{EmitOutput, EmitSpec, EntryBody, EntryKind};

#[test]
fn models_covenant_groups_and_preserves_source_nodes() {
    let entry = EntryDecl {
        kind: EntryKind::Leader,
        name: "step".to_string(),
        params: Vec::new(),
        consumes: vec![ConsumeDecl { name: "peer".to_string(), actor: "Peer".to_string() }],
        observes: vec![ObserveDecl {
            name: "remote".to_string(),
            covenant_expr: "self.remote_id".to_string(),
            inputs: vec![ObservedActorDecl { name: "before".to_string(), actor: "Remote".to_string(), open_state: None }],
            outputs: vec![ObservedActorDecl { name: "after".to_string(), actor: "Remote".to_string(), open_state: None }],
        }],
        spawns: vec![SpawnDecl {
            name: "launch".to_string(),
            covenant: "child".to_string(),
            outputs: vec![SpawnOutputDecl { name: "child".to_string(), actor: "Child".to_string(), group_index: 0 }],
        }],
        emits: EmitSpec::Outputs(vec![EmitOutput { name: "next".to_string(), actors: vec!["Move".to_string()], auth_index: 0 }]),
        body: EntryBody::default(),
        routes: vec![RouteCall { output: "next".to_string(), actor: "target".to_string(), state: "next".to_string() }],
        terminal_route_sets: Vec::new(),
    };
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

    let model = EntryModel::new(&entry, &actor_enums, selectors);

    let InteractionSource::Consume(consume) = model.current().inputs()[0].source() else {
        panic!("current input must retain its consume declaration");
    };
    assert!(std::ptr::eq(consume, &entry.consumes[0]));
    assert_eq!(model.current().inputs()[0].handle(), "peer");
    assert_eq!(model.current().inputs()[0].index(), 0);
    let InteractionSource::CurrentOutput(output) = model.current().outputs()[0].source() else {
        panic!("current output must retain its emits declaration");
    };
    let EmitSpec::Outputs(outputs) = &entry.emits else {
        panic!("test entry must have named outputs");
    };
    assert!(std::ptr::eq(output, &outputs[0]));
    assert_eq!(model.current().outputs()[0].handle(), "next");
    assert_eq!(model.current().outputs()[0].index(), 0);
    assert_eq!(model.current().outputs()[0].target().actors().collect::<Vec<_>>(), ["Pawn", "King"]);
    let observe_group = model.existing_groups().next().expect("observe group");
    assert!(std::ptr::eq(observe_group.observe().expect("observe source"), &entry.observes[0]));
    let InteractionSource::ObserveInput(observed) = observe_group.inputs()[0].source() else {
        panic!("observe input must retain its source declaration");
    };
    assert!(std::ptr::eq(observed, &entry.observes[0].inputs[0]));
    assert_eq!(observe_group.inputs()[0].handle(), "before");
    assert_eq!(observe_group.inputs()[0].index(), 0);
    assert_eq!(observe_group.inputs()[0].target().static_actors().collect::<Vec<_>>(), ["Remote"]);

    let spawn_group = model.genesis_groups().next().expect("spawn group");
    assert!(std::ptr::eq(spawn_group.spawn().expect("spawn source"), &entry.spawns[0]));
    let InteractionSource::SpawnOutput(output) = spawn_group.outputs()[0].source() else {
        panic!("spawn output must retain its source declaration");
    };
    assert!(std::ptr::eq(output, &entry.spawns[0].outputs[0]));
    assert_eq!(spawn_group.outputs()[0].handle(), "child");
    assert_eq!(spawn_group.outputs()[0].index(), 0);
    assert_eq!(spawn_group.outputs()[0].target().static_actors().collect::<Vec<_>>(), ["Child"]);

    assert_eq!(model.expanded_routes()[0].actor, "King");
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
            EmitOutput { name: "first".to_string(), actors: vec!["Pawn".to_string()], auth_index: 0 },
            EmitOutput { name: "second".to_string(), actors: vec!["Move".to_string()], auth_index: 1 },
        ]),
        body: EntryBody::default(),
        routes: Vec::new(),
        terminal_route_sets: Vec::new(),
    };
    let actor_enums = BTreeMap::from([(
        "Move".to_string(),
        ActorEnumInfo { name: "Move".to_string(), state: "Game".to_string(), variants: vec!["Pawn".to_string(), "King".to_string()] },
    )]);

    let model = EntryModel::new(&entry, &actor_enums, BTreeMap::new());
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
    assert_eq!(model.current().outputs()[0].index(), 0);
    assert_eq!(model.current().outputs()[0].target().actors().collect::<Vec<_>>(), ["Pawn"]);
    assert_eq!(model.current().outputs()[1].handle(), "second");
    assert_eq!(model.current().outputs()[1].index(), 1);
    assert_eq!(model.current().outputs()[1].target().actors().collect::<Vec<_>>(), ["Pawn", "King"]);

    let mut empty_entry = entry.clone();
    empty_entry.emits = EmitSpec::None;
    let empty_model = EntryModel::new(&empty_entry, &actor_enums, BTreeMap::new());
    assert!(empty_model.current().outputs().is_empty());
}

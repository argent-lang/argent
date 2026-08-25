use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::compiler::syntax::parser::parse_module;
use crate::compiler::syntax::{Program, TypeRef};

use super::*;

fn program(source: &str) -> Program {
    let path = PathBuf::from("state-layout-plan-test.ag");
    let module = parse_module(path.clone(), source.to_string()).expect("test source parses");
    Program { root: path, modules: vec![module] }
}

#[test]
fn contract_plans_keep_authored_types_named_and_record_context_eligibility() {
    let program = program(include_str!("../../../../tests/fixtures/state_layout/function_contexts/app.ag"));
    let model = Model::from_program(&program).expect("function context fixture plans");
    let shared = SourceStateId::new("SharedState");

    let aligned = model.state_lowering("Aligned").expect("Aligned lowering exists");
    let aligned_shared = aligned.source_representation(&shared).expect("SharedState is planned for Aligned");
    assert_eq!(aligned_shared.sil_type(), &SilStateType::Source(shared.clone()));
    assert!(aligned_shared.active_state_eligible());

    let routed = model.state_lowering("Routed").expect("Routed lowering exists");
    let routed_shared = routed.source_representation(&shared).expect("SharedState is planned for Routed");
    assert_eq!(routed_shared.sil_type(), &SilStateType::Source(shared.clone()));
    assert!(!routed_shared.active_state_eligible());

    let aligned_target = aligned.target_for_actor("Aligned").expect("active target exists");
    let routed_target = aligned.target_for_actor("Routed").expect("Routed target exists");
    assert_eq!(aligned_target.source(), routed_target.source());
    assert!(aligned_target.storage_to_physical().is_identity());
    assert!(!routed_target.storage_to_physical().is_identity());
    assert!(!routed_target.active_compatible());
    assert_eq!(
        routed.active().physical().fields().iter().map(LayoutField::sil_name).collect::<Vec<_>>(),
        ["gen__foreign_template", "left", "right"]
    );
    assert!(matches!(
        routed.active().physical().fields()[0].id(),
        PhysicalFieldId::Generated(GeneratedFieldId::Template(actor)) if actor.actor() == "Foreign"
    ));
}

#[test]
fn compatible_foreign_template_stays_named_until_state_equivalence_is_selected() {
    let program = program(
        r#"
            state SharedState { int count; }

            actor First owns SharedState {}
            actor Second owns SharedState {}

            app Test {
                actor First;
                actor Second;
            }
        "#,
    );
    let model = Model::from_program(&program).expect("compatible actors plan");
    let lowering = model.state_lowering("First").expect("First lowering exists");
    let second = lowering.target_for_actor("Second").expect("Second target exists");

    assert!(second.active_compatible());
    assert!(second.source_to_storage().is_identity());
    assert!(second.storage_to_physical().is_identity());
    assert_eq!(second.sil_type(), &SilStateType::Source(SourceStateId::new("SharedState")));
    assert_ne!(second.sil_type(), &SilStateType::State);

    let output = lowering.output_type_for_actor("Second").expect("Second output target exists");
    assert_eq!(output.target(), second.id());
    assert_eq!(output.canonical_target(), second.id());
    assert_eq!(output.sil_type(), &SilStateType::State);
}

#[test]
fn nominal_source_identity_is_independent_of_shared_storage_compatibility() {
    let program = program(
        r#"
            state Capsule {
                virtual detail;
                int count;
            }
            state Detail { int value; }
            state FirstView expands Capsule { detail: Detail; }
            state SecondView expands Capsule { detail: Detail; }

            actor First owns FirstView {}
            actor Second owns SecondView {}

            app Test {
                actor First;
                actor Second;
            }
        "#,
    );
    let model = Model::from_program(&program).expect("shared storage views plan");
    let lowering = model.state_lowering("First").expect("First lowering exists");
    let second = lowering.target_for_actor("Second").expect("Second target exists");
    let detail = lowering.active().source().field_id("detail").expect("source detail field is indexed");
    let stored_detail = lowering.active().source_to_storage().storage_field(detail).expect("source detail maps to storage");
    let physical_detail =
        lowering.active().storage_to_physical().physical_field(stored_detail).expect("stored detail maps to physical state");

    assert!(second.active_compatible());
    assert!(!second.source_to_storage().is_identity());
    assert!(second.storage_to_physical().is_identity());
    assert!(!second.has_source_identity(&SourceStateId::new("FirstView")));
    assert!(second.has_source_identity(&SourceStateId::new("SecondView")));
    assert_eq!(second.sil_type(), &SilStateType::StoragePhysical(SourceStateId::new("SecondView")));
    assert_eq!(detail.field(), "detail");
    assert_eq!(stored_detail.field(), "detail");
    assert!(lowering.active().physical().field(physical_detail).is_some());
}

#[test]
fn open_actor_type_targets_have_a_state_keyed_storage_cut() {
    let program = program(
        r#"
            state RemoteState { int value; }
            state LocalState { actor_type<RemoteState> remote; }

            actor Local owns LocalState {
                entry inspect() emits none {
                    require(1 == 1);
                }
            }

            app Test { actor Local; }
        "#,
    );
    let model = Model::from_program(&program).expect("open actor type plans");
    let lowering = model.state_lowering("Local").expect("Local lowering exists");
    let remote = SourceStateId::new("RemoteState");
    let target = lowering.open_state_target(&remote).expect("open state target exists");

    assert_eq!(target.id(), &PhysicalTargetId::OpenState(remote.clone()));
    assert_eq!(target.sil_type(), &SilStateType::Source(remote.clone()));
    assert!(target.storage_to_physical().is_identity());
    assert!(!target.active_compatible());

    let output = lowering.output_type_for_open_state(&remote).expect("open output type exists");
    assert_eq!(output.sil_type(), &SilStateType::Source(remote));
    assert_ne!(output.sil_type(), &SilStateType::State);
}

#[test]
fn same_source_open_output_does_not_inherit_active_generated_fields() {
    let program = program(
        r#"
            state SharedState {
                actor_type<SharedState> peer;
                int count;
            }

            actor Current owns SharedState {
                entry send() emits next: Peer {
                    SharedState next_state = SharedState {
                        peer: peer,
                        count: count + 1,
                    };
                    unrestricted(next.value);
                    become next <- Peer(next_state);
                }
            }

            actor Peer owns SharedState {
                entry hold() emits none { require(count >= 0); }
            }

            app Test {
                actor Current;
                actor Peer;
            }
        "#,
    );
    let model = Model::from_program(&program).expect("same-source open target plans");
    let lowering = model.state_lowering("Current").expect("Current lowering exists");
    let shared = SourceStateId::new("SharedState");
    let target = lowering.open_state_target(&shared).expect("same-source open target exists");

    assert!(lowering.active().physical().fields().iter().any(|field| matches!(field.id(), PhysicalFieldId::Generated(_))));
    assert!(target.storage_to_physical().is_identity());
    assert!(target.physical().fields().iter().all(|field| matches!(field.id(), PhysicalFieldId::Storage(_))));
    assert!(!target.active_compatible());

    let output = lowering.output_type_for_open_state(&shared).expect("same-source open output type exists");
    assert_eq!(output.sil_type(), &SilStateType::Source(shared));
    assert_ne!(output.sil_type(), &SilStateType::State);
}

#[test]
fn physical_compatibility_distinguishes_equal_width_generated_roles() {
    let field = |actor: &str| LayoutField {
        id: PhysicalFieldId::Generated(GeneratedFieldId::Template(CompiledActorId {
            app: "Test".to_string(),
            actor: actor.to_string(),
        })),
        sil_name: "gen__template".to_string(),
        ty: TypeRef::array("byte", 32),
        sil_type: "byte[32]".to_string(),
        packed_len: 32,
    };
    let first = PhysicalStateLayout { fields: vec![field("First")] };
    let second = PhysicalStateLayout { fields: vec![field("Second")] };

    assert_eq!(first.fields()[0].packed_len(), second.fields()[0].packed_len());
    assert_eq!(first.fields()[0].sil_type(), second.fields()[0].sil_type());
    assert!(!first.is_sil_compatible_with(&second));
}

#[test]
fn dynamic_actor_domains_reject_incompatible_semantic_layouts() {
    let actor = |name: &str| CompiledActorId { app: "Test".to_string(), actor: name.to_string() };
    let target_id = |name: &str| PhysicalTargetId::Actor(actor(name));
    let plan = |name: &str| {
        let id = target_id(name);
        TargetPhysicalPlan {
            id: id.clone(),
            source: SourceStateId::new("SharedState"),
            source_to_storage: SourceStorageRelation::Identity { fields: Vec::new() },
            storage_to_physical: StoragePhysicalRelation::Augmented {
                generated_fields: vec![GeneratedFieldId::Template(actor(name))],
                storage_to_physical: Vec::new(),
            },
            physical: PhysicalStateLayout {
                fields: vec![LayoutField {
                    id: PhysicalFieldId::Generated(GeneratedFieldId::Template(actor(name))),
                    sil_name: "gen__template".to_string(),
                    ty: TypeRef::array("byte", 32),
                    sil_type: "byte[32]".to_string(),
                    packed_len: 32,
                }],
            },
            sil_type: SilStateType::TargetPhysical(id),
            active_compatible: false,
        }
    };
    let plans = BTreeMap::from([("First", plan("First")), ("Second", plan("Second"))])
        .into_values()
        .map(|plan| (plan.id.clone(), plan))
        .collect::<BTreeMap<_, _>>();
    let variants = vec![target_id("First"), target_id("Second")];
    let domain =
        PhysicalTargetId::ActorDomain { state: SourceStateId::new("SharedState"), actors: vec![actor("First"), actor("Second")] };
    let active = plans[&target_id("First")].physical.clone();

    let err = canonical_domain_plan(&domain, &variants, &active, &plans).expect_err("semantic role mismatch is rejected");
    assert!(err.to_string().contains("semantic physical layout"), "unexpected error: {err}");
}

#[test]
fn actor_domain_output_records_its_target_and_canonical_type_owner() {
    let program = program(include_str!("../../../../examples/route_state_body_choice.ag"));
    let model = Model::from_program(&program).expect("selector example plans");
    let lowering = model.state_lowering("Mux").expect("Mux lowering exists");
    let variants = vec!["Pawn".to_string(), "Knight".to_string()];
    let output =
        lowering.output_type_for_actor_domain(&SourceStateId::new("BoardState"), &variants).expect("selector domain output exists");

    assert!(
        matches!(output.target(), PhysicalTargetId::ActorDomain { state, actors } if state.as_str() == "BoardState" && actors.len() == 2)
    );
    assert!(matches!(output.canonical_target(), PhysicalTargetId::Actor(actor) if actor.actor() == "Pawn"));
    assert_eq!(output.sil_type(), &SilStateType::State);
}

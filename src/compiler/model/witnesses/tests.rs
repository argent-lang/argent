use std::path::PathBuf;

use crate::compiler::loader::load_inline_program;
use crate::compiler::model::{AppCompilationContext, SourceFieldId};

use super::*;

#[test]
fn expansion_witness_keeps_nominal_field_and_memory_sources() {
    let program = load_inline_program(
        PathBuf::from("expansion-witness.ag"),
        include_str!("../../../../tests/fixtures/emit/state_expansion/app.ag").to_string(),
    )
    .expect("fixture resolves");
    let model = AppCompilationContext::from_resolved(
        &program,
        None,
        &std::collections::BTreeMap::new(),
        &crate::compiler::model::default_route_planner,
    )
    .expect("expansion plans");
    let actor = model.actor_by_decl(model.types.names["Forager"]).expect("Forager exists");
    let spec = &model.state_expansion_witnesses_by_id(model.types.names[&actor.name]).expect("expansion witnesses")[0];
    let state = model.source_state_id("ForagerState").expect("state identity");
    let memory = model.source_state_id("ForagerStrategy").expect("memory identity");
    assert_eq!(spec.state, state.as_str());
    assert_eq!(spec.memory_state, memory.as_str());
    assert_eq!(spec.field_id, SourceFieldId::new(state, "strategy"));
    assert_eq!(spec.memory_source, memory);
}

#[test]
fn observed_and_spawned_actor_type_share_first_provider_and_ordered_roles() {
    let program = load_inline_program(
        PathBuf::from("shared-witness-plan.ag"),
        include_str!("../../../../tests/fixtures/runtime/context_shared_actor_witness/app.ag").to_string(),
    )
    .expect("fixture resolves");
    let model = AppCompilationContext::from_resolved(
        &program,
        Some("SharedActorWitness"),
        &std::collections::BTreeMap::new(),
        &crate::compiler::model::default_route_planner,
    )
    .expect("selected app plans");
    let controller = model.actor_by_decl(model.types.names["Controller"]).expect("Controller exists");
    let advance = controller.entries.iter().find(|entry| entry.name == "advance").expect("advance exists");
    let plan = model
        .witness_plan_by_id(crate::compiler::syntax::node::EntryId {
            actor: model.types.names[&controller.name],
            index: controller.entries.iter().position(|candidate| std::ptr::eq(candidate, advance)).expect("entry belongs to actor"),
        })
        .expect("witness plan exists");

    let anchor_id = model
        .entry_model_by_id(crate::compiler::syntax::node::EntryId {
            actor: model.types.names[&controller.name],
            index: controller.entries.iter().position(|candidate| std::ptr::eq(candidate, advance)).expect("entry belongs to actor"),
        })
        .expect("entry model exists")
        .existing_groups()
        .flat_map(|group| group.inputs())
        .find(|input| input.handle() == "anchor")
        .expect("anchor input interaction")
        .id();
    let anchor_input = model
        .input_plan_by_id(crate::compiler::syntax::node::EntryId {
            actor: model.types.names[&controller.name],
            index: controller.entries.iter().position(|candidate| std::ptr::eq(candidate, advance)).expect("entry belongs to actor"),
        })
        .expect("input plan exists")
        .observed(anchor_id)
        .expect("anchor input");
    let crate::compiler::model::InteractionId::ObservedInput { observe, input } = anchor_id else {
        panic!("anchor is an observed input")
    };
    assert!(
        model
            .input_plan_by_id(crate::compiler::syntax::node::EntryId {
                actor: model.types.names[&controller.name],
                index: controller
                    .entries
                    .iter()
                    .position(|candidate| std::ptr::eq(candidate, advance))
                    .expect("entry belongs to actor")
            })
            .expect("input plan exists")
            .observed(crate::compiler::model::InteractionId::ObservedInput { observe, input: input + 100 })
            .is_err(),
    );
    assert!(matches!(
        &anchor_input.observed_witness.as_ref().expect("observed input descriptor").template_source,
        ObservedTemplateSource::FixedInApp(id) if model.types.display_names[id] == "Anchor"
    ));
    assert_eq!(
        model
            .entry_output_plan_by_id(crate::compiler::syntax::node::EntryId {
                actor: model.types.names[&controller.name],
                index: controller
                    .entries
                    .iter()
                    .position(|candidate| std::ptr::eq(candidate, advance))
                    .expect("entry belongs to actor")
            })
            .expect("output plan exists")
            .observed_witness(
                model
                    .entry_model_by_id(crate::compiler::syntax::node::EntryId {
                        actor: model.types.names[&controller.name],
                        index: controller
                            .entries
                            .iter()
                            .position(|candidate| std::ptr::eq(candidate, advance))
                            .expect("entry belongs to actor")
                    })
                    .expect("entry model exists")
                    .existing_groups()
                    .flat_map(|group| group.outputs())
                    .find(|output| output.handle() == "pair")
                    .expect("pair output interaction")
                    .id(),
            )
            .expect("pair output")
            .template_source,
        ObservedTemplateSource::ActorTypeValue,
    );

    assert_eq!(plan.actor_type_source_templates.len(), 1);
    assert!(matches!(
        &plan.actor_type_source_templates[0].provider,
        ActorTypeSourceWitnessProvider::Observed(spec)
            if spec.observe == "existing" && spec.handle == "pair" && spec.side == ObservedActorSide::Output
    ));
    assert_eq!(plan.spawn_outputs.len(), 1);
    assert_eq!(plan.templates[0].id, StaticActorId::InApp(model.types.names["Anchor"]));
    assert_eq!(plan.roles.len(), 5);
    assert_eq!(plan.roles[0], WitnessRole::Template { index: 0, component: WitnessComponent::Prefix });
    assert_eq!(plan.roles[3], WitnessRole::ActorType { index: 0, component: WitnessComponent::Prefix });
    assert_eq!(plan.roles[4], WitnessRole::ActorType { index: 0, component: WitnessComponent::Suffix });
    assert_eq!(plan.role_type(plan.roles[0]), WitnessAbiType::Int);
    assert_eq!(plan.role_type(plan.roles[3]), WitnessAbiType::Bytes);
}

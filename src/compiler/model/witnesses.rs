//! Entry witness sharing, providers, and ordered ABI roles.

use std::collections::{BTreeMap, BTreeSet};

use crate::compiler::resolve::{ActorSourceBinding, LocalId};
use crate::compiler::syntax::node::{DeclId, EntryId, RootSlot};
use crate::compiler::syntax::{ActorDecl, EntryDecl, ObserveDecl, ObservedActorDecl};
use crate::error::{ArgentError, Result};

use super::link::LinkedActorId;
use super::{
    AppCompilationContext, ClauseActorTypeRef, InteractionId, InteractionSource, SourceFieldId, SourceStateId, StaticActorId,
    StaticActorTarget, clause_actor_type_ref, observed_is_dynamic_binding, observed_open_state_for_decl, packed_field_len,
};

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum TemplateWitnessForm {
    Bytes,
    Len,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum TemplateWitnessSource {
    Field,
    FamilyTable { family_id: String, offset: usize },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TemplateWitnessSpec {
    pub(crate) id: StaticActorId,
    pub(crate) actor: String,
    pub(crate) form: TemplateWitnessForm,
    pub(crate) source: TemplateWitnessSource,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RouteFamilyWitnessSpec {
    pub(crate) family_id: String,
    pub(crate) byte_len: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TemplateSelectorWitnessSpec {
    pub(crate) binding: LocalId,
    pub(crate) name: String,
    pub(crate) actor_enum: String,
    pub(crate) variants: Vec<String>,
    pub(crate) family_id: String,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum ObservedActorSide {
    Input,
    Output,
}

/// Semantic source of an observed actor's template; lowering only materializes it.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum ObservedTemplateSource {
    FixedInApp(DeclId),
    FixedLinked(LinkedActorId),
    DynamicBinding,
    ActorTypeValue,
    Witness,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct ObservedActorWitnessSpec {
    pub(crate) observe: String,
    pub(crate) side: ObservedActorSide,
    pub(crate) handle: String,
    pub(crate) actor: String,
    pub(crate) source: Option<ClauseActorTypeRef>,
    pub(crate) dynamic_binding: bool,
    pub(crate) template_source: ObservedTemplateSource,
}

impl ObservedActorWitnessSpec {
    pub(crate) fn for_decl(
        entry_id: EntryId,
        actor: &ActorDecl,
        entry: &EntryDecl,
        observe: &ObserveDecl,
        side: ObservedActorSide,
        observed: &ObservedActorDecl,
        model: &AppCompilationContext<'_>,
    ) -> Result<Self> {
        let dynamic_binding = observed_is_dynamic_binding(entry_id, observe, observed, model)?;
        let source = if dynamic_binding {
            None
        } else {
            let interaction = model.entry_model_by_id(entry_id)?.observed_interaction_id(observe, observed)?;
            clause_actor_type_ref(entry_id, interaction, &observed.actor, actor, entry, model)?
        };
        let template_source = match model.static_observed_actor_target(entry_id, actor, entry, observe, observed)? {
            Some(StaticActorTarget::InApp(id)) => ObservedTemplateSource::FixedInApp(id),
            Some(StaticActorTarget::CrossApp(target)) => {
                ObservedTemplateSource::FixedLinked(LinkedActorId { app: target.app.clone(), actor: target.actor.clone() })
            }
            None if dynamic_binding => ObservedTemplateSource::DynamicBinding,
            None if source.is_some() => ObservedTemplateSource::ActorTypeValue,
            None => ObservedTemplateSource::Witness,
        };
        Ok(Self {
            observe: observe.name.clone(),
            side,
            handle: observed.name.clone(),
            actor: observed.actor.clone(),
            source,
            dynamic_binding,
            template_source,
        })
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct SpawnActorWitnessSpec {
    pub(crate) id: InteractionId,
    pub(crate) spawn: String,
    pub(crate) handle: String,
    pub(crate) actor: String,
    pub(crate) source: Option<ClauseActorTypeRef>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ActorTypeSourceWitnessProvider {
    Observed(ObservedActorWitnessSpec),
    Spawn(SpawnActorWitnessSpec),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ActorTypeSourceWitnessSpec {
    pub(crate) source: ClauseActorTypeRef,
    pub(crate) form: TemplateWitnessForm,
    pub(crate) provider: ActorTypeSourceWitnessProvider,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StateExpansionWitnessSpec {
    pub(crate) state: String,
    pub(crate) field: String,
    pub(crate) memory_state: String,
    pub(crate) field_id: SourceFieldId,
    pub(crate) memory_source: SourceStateId,
    pub(crate) packed_len: usize,
}

impl StateExpansionWitnessSpec {
    pub(crate) fn for_actor(actor_id: DeclId, model: &AppCompilationContext<'_>) -> Result<Vec<Self>> {
        let state = *model.types.actor_states.get(&actor_id).ok_or_else(|| ArgentError::new("selected actor has no bound state"))?;
        let source = model.source_state_id_by_decl(state)?;
        model
            .state_by_source(&source)?
            .expansion
            .as_ref()
            .map(|expansion| {
                expansion
                    .digests
                    .iter()
                    .enumerate()
                    .map(|(index, digest)| {
                        let memory_source =
                            model.source_state_id_by_decl(model.bound_state_use(state, RootSlot::DigestState(index))?)?;
                        let packed_len = model
                            .state_by_source(&memory_source)?
                            .fields
                            .iter()
                            .try_fold(0usize, |sum, field| packed_field_len(&field.ty).map(|len| sum + len))?;
                        Ok(Self {
                            state: source.as_str().to_string(),
                            field: digest.field.clone(),
                            memory_state: memory_source.as_str().to_string(),
                            field_id: SourceFieldId::new(source.clone(), &digest.field),
                            memory_source,
                            packed_len,
                        })
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()
            .map(Option::unwrap_or_default)
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct ObservedOutputFieldWitnessSpec {
    pub(crate) observe: String,
    pub(crate) handle: String,
    pub(crate) state: String,
    pub(crate) field: String,
    pub(crate) field_id: SourceFieldId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WitnessComponent {
    Prefix,
    Suffix,
    TemplateHash,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WitnessRole {
    Template { index: usize, component: WitnessComponent },
    RouteFamily { index: usize },
    Selector { index: usize, component: WitnessComponent },
    Observed { index: usize, component: WitnessComponent },
    SpawnIndex { index: usize },
    ActorType { index: usize, component: WitnessComponent },
    StateExpansion { index: usize },
    ObservedOutputField { index: usize },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WitnessAbiType {
    Bytes,
    Int,
    FixedBytes(usize),
}

#[derive(Debug)]
pub(crate) struct WitnessPlan {
    pub(crate) templates: Vec<TemplateWitnessSpec>,
    pub(crate) families: Vec<RouteFamilyWitnessSpec>,
    pub(crate) selectors: Vec<TemplateSelectorWitnessSpec>,
    pub(crate) observed_actors: Vec<ObservedActorWitnessSpec>,
    pub(crate) spawn_outputs: Vec<SpawnActorWitnessSpec>,
    pub(crate) actor_type_source_templates: Vec<ActorTypeSourceWitnessSpec>,
    pub(crate) state_expansions: Vec<StateExpansionWitnessSpec>,
    pub(crate) observed_output_fields: Vec<ObservedOutputFieldWitnessSpec>,
    pub(crate) imported_templates: Vec<LinkedActorId>,
    pub(crate) roles: Vec<WitnessRole>,
}

impl WitnessPlan {
    /// Select one shared witness set and assign its ABI order.
    pub(crate) fn new(entry_id: EntryId, actor: &ActorDecl, entry: &EntryDecl, model: &AppCompilationContext<'_>) -> Result<Self> {
        #[derive(Eq, Ord, PartialEq, PartialOrd)]
        enum ObservedWitnessKey {
            DynamicBinding(ActorSourceBinding),
            ActorType(ClauseActorTypeRef),
            Actor(StaticActorId),
            Unbound(InteractionId),
        }

        let entry_model = model.entry_model_by_id(entry_id)?;
        let source_id = entry_model.id.actor;
        let uses = model.entry_template_uses(entry_id)?;
        let input_plan = model.input_plan_by_id(entry_id)?;
        let optional_input_actors = entry_model
            .current()
            .inputs()
            .iter()
            .filter(|interaction| interaction.cardinality().range_bounds().is_some_and(|(minimum, _)| minimum == 0))
            .flat_map(|interaction| interaction.target().static_actors().cloned())
            .collect::<BTreeSet<_>>();
        let mut required = uses.reads.union(&uses.writes).cloned().collect::<BTreeSet<_>>();
        let mut templates = Vec::new();
        for (actor_id, target) in model.app_actors.iter_with_ids() {
            let id = StaticActorId::InApp(actor_id);
            if required.remove(&id) {
                templates.push(TemplateWitnessSpec {
                    id: id.clone(),
                    actor: target.to_string(),
                    form: if uses.writes.contains(&id) && !uses.reads.contains(&id) {
                        TemplateWitnessForm::Bytes
                    } else {
                        TemplateWitnessForm::Len
                    },
                    source: TemplateWitnessSource::Field,
                });
            }
        }
        for id in required {
            templates.push(TemplateWitnessSpec {
                form: if uses.writes.contains(&id) && !uses.reads.contains(&id) {
                    TemplateWitnessForm::Bytes
                } else {
                    TemplateWitnessForm::Len
                },
                actor: model.static_actor_reference(&id)?,
                id,
                source: TemplateWitnessSource::Field,
            });
        }
        for spec in &mut templates {
            if spec.id != StaticActorId::InApp(source_id) && optional_input_actors.contains(&spec.id) && uses.writes.contains(&spec.id)
            {
                let target = model
                    .state_lowering_by_id(source_id)?
                    .target_for_actor(&spec.id)
                    .ok_or_else(|| ArgentError::new(format!("actor `{}` has no input target for `{}`", actor.name, spec.actor)))?;
                if input_plan.first_authenticated_template(target.id()).is_none() {
                    spec.form = TemplateWitnessForm::Bytes;
                }
            }
        }
        let mut family_specs = BTreeMap::<String, RouteFamilyWitnessSpec>::new();
        for target in &uses.writes {
            let StaticActorId::InApp(target_id) = target else { continue };
            if *target_id == source_id {
                continue;
            }
            let transition = model.route_transitions.get(&(source_id, *target_id)).ok_or_else(|| {
                ArgentError::new(format!(
                    "entry model has no route transition from `{}` to in-app target `{}`",
                    actor.name,
                    model.app_actors.name(*target_id).unwrap_or("<unknown>")
                ))
            })?;
            for family_id in &transition.families_to_open {
                let family = model
                    .route_family(family_id)
                    .ok_or_else(|| ArgentError::new(format!("route transition references unknown family `{family_id}`")))?;
                family_specs
                    .entry(family.id.clone())
                    .or_insert(RouteFamilyWitnessSpec { family_id: family.id.clone(), byte_len: family.table_byte_len() });
            }
        }
        let source_state_id = model.types.actor_states[&source_id];
        for spec in &mut templates {
            let StaticActorId::InApp(target_id) = spec.id else { continue };
            let source = model.route_families.iter().find(|family| family.actor_ids.contains(&target_id)).and_then(|family| {
                if !family_specs.contains_key(&family.id)
                    && (family.state_id != source_state_id || !family.table_actor_ids.contains(&target_id))
                {
                    return None;
                }
                family
                    .table_actor_ids
                    .iter()
                    .position(|candidate| *candidate == target_id)
                    .map(|index| TemplateWitnessSource::FamilyTable { family_id: family.id.clone(), offset: index * 32 })
            });
            spec.source = source.unwrap_or(TemplateWitnessSource::Field);
        }
        let selectors = entry_model
            .template_selectors()
            .values()
            .map(|selector| {
                let binding = selector
                    .binding
                    .ok_or_else(|| ArgentError::new(format!("actor selector `{}` has no bound identity", selector.name)))?;
                let variants = selector.variant_actor_ids()?;
                let state = variants.iter().find_map(|variant| match variant {
                    StaticActorId::InApp(id) => model.types.actor_states.get(id),
                    StaticActorId::Linked(_) => None,
                });
                let family = model
                    .route_families
                    .iter()
                    .filter(|family| state == Some(&family.state_id))
                    .find(|family| {
                        variants.iter().all(|variant| match variant {
                            StaticActorId::InApp(id) => family.table_actor_ids.contains(id),
                            StaticActorId::Linked(_) => false,
                        })
                    })
                    .ok_or_else(|| {
                        ArgentError::new(format!(
                            "actor enum `{}` variants are not available as a selector table for state `{}`",
                            selector.actor_enum, selector.state
                        ))
                    })?;
                Ok(TemplateSelectorWitnessSpec {
                    binding,
                    name: selector.name.clone(),
                    actor_enum: selector.actor_enum.clone(),
                    variants: selector.variants.clone(),
                    family_id: family.id.clone(),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let mut observed_actors = Vec::new();
        let mut actor_type_observed = Vec::new();
        for observe in &entry.observes {
            let mut seen = BTreeSet::new();
            for (side, observed) in observe.outputs.iter().map(|output| (ObservedActorSide::Output, output)).chain(
                observe
                    .inputs
                    .iter()
                    .filter(|input| !observe.outputs.iter().any(|output| output.actor == input.actor))
                    .map(|input| (ObservedActorSide::Input, input)),
            ) {
                let (side, observed) = if side == ObservedActorSide::Output {
                    if let Some(input) = observe.inputs.iter().find(|input| input.actor == observed.actor) {
                        (ObservedActorSide::Input, input)
                    } else {
                        (side, observed)
                    }
                } else {
                    (side, observed)
                };
                let spec = ObservedActorWitnessSpec::for_decl(entry_id, actor, entry, observe, side, observed, model)?;
                let interaction = entry_model.observed_interaction_id(observe, observed)?;
                let key = match &spec.template_source {
                    ObservedTemplateSource::DynamicBinding => {
                        if !matches!(interaction, InteractionId::ObservedInput { .. } | InteractionId::ObservedOutput { .. }) {
                            return Err(ArgentError::new("observed witness has no observed actor site"));
                        }
                        let site = interaction.actor_target_site(entry_id, model.resolution)?;
                        let binding = model
                            .resolution
                            .bindings(entry_id.actor)
                            .local_actor_targets
                            .get(&site)
                            .copied()
                            .ok_or_else(|| ArgentError::new("dynamic observed witness has no bound actor source"))?;
                        ObservedWitnessKey::DynamicBinding(binding)
                    }
                    ObservedTemplateSource::ActorTypeValue => ObservedWitnessKey::ActorType(
                        spec.source.clone().ok_or_else(|| ArgentError::new("actor-type witness has no bound source"))?,
                    ),
                    ObservedTemplateSource::FixedInApp(id) => ObservedWitnessKey::Actor(StaticActorId::InApp(*id)),
                    ObservedTemplateSource::FixedLinked(id) => ObservedWitnessKey::Actor(StaticActorId::Linked(id.clone())),
                    ObservedTemplateSource::Witness => ObservedWitnessKey::Unbound(interaction),
                };
                if !seen.insert((side, key)) {
                    continue;
                }
                match &spec.template_source {
                    ObservedTemplateSource::FixedInApp(_) | ObservedTemplateSource::FixedLinked(_) => {}
                    ObservedTemplateSource::ActorTypeValue => actor_type_observed.push(spec),
                    ObservedTemplateSource::DynamicBinding | ObservedTemplateSource::Witness => observed_actors.push(spec),
                }
            }
        }
        let mut spawn_outputs = Vec::new();
        for group in entry_model.genesis_groups() {
            let spawn = group.spawn().expect("genesis group has a spawn declaration");
            for interaction in group.outputs() {
                let InteractionSource::SpawnOutput(output) = interaction.source() else {
                    unreachable!("genesis outputs are spawn outputs");
                };
                let source = if interaction.target().is_source() {
                    clause_actor_type_ref(entry_id, interaction.id(), &output.actor, actor, entry, model)?
                } else {
                    None
                };
                spawn_outputs.push(SpawnActorWitnessSpec {
                    id: interaction.id(),
                    spawn: spawn.name.clone(),
                    handle: output.name.clone(),
                    actor: interaction
                        .target()
                        .artifact_references(model)?
                        .into_iter()
                        .next()
                        .ok_or_else(|| ArgentError::new("spawn output has no actor reference"))?,
                    source,
                });
            }
        }
        let mut seen_sources = BTreeSet::new();
        let mut actor_type_source_templates = Vec::new();
        for spec in &actor_type_observed {
            let source = spec.source.as_ref().expect("selected actor-type observed witness has source");
            let form = if spec.side == ObservedActorSide::Input { TemplateWitnessForm::Len } else { TemplateWitnessForm::Bytes };
            if seen_sources.insert((source.clone(), form)) {
                actor_type_source_templates.push(ActorTypeSourceWitnessSpec {
                    source: source.clone(),
                    form,
                    provider: ActorTypeSourceWitnessProvider::Observed(spec.clone()),
                });
            }
        }
        for spec in &spawn_outputs {
            let Some(source) = &spec.source else { continue };
            if seen_sources.insert((source.clone(), TemplateWitnessForm::Bytes)) {
                actor_type_source_templates.push(ActorTypeSourceWitnessSpec {
                    source: source.clone(),
                    form: TemplateWitnessForm::Bytes,
                    provider: ActorTypeSourceWitnessProvider::Spawn(spec.clone()),
                });
            }
        }
        let state_expansions = model.state_expansion_witnesses_by_id(source_id)?.to_vec();
        let mut observed_output_fields = Vec::new();
        let mut seen_fields = BTreeSet::new();
        for observe in &entry.observes {
            for output in &observe.outputs {
                let Some(state_id) = observed_open_state_for_decl(entry_id, actor, entry, observe, output, model)? else {
                    continue;
                };
                let state = model.storage_state_by_source(&state_id)?;
                for field in &state.fields {
                    if field.virtual_slot {
                        let spec = ObservedOutputFieldWitnessSpec {
                            observe: observe.name.clone(),
                            handle: output.name.clone(),
                            state: state_id.as_str().to_string(),
                            field: field.name.clone(),
                            field_id: SourceFieldId::new(state_id.clone(), &field.name),
                        };
                        if seen_fields.insert(spec.clone()) {
                            observed_output_fields.push(spec);
                        }
                    }
                }
            }
        }
        let imported_templates = entry_model
            .existing_groups()
            .chain(entry_model.genesis_groups())
            .flat_map(|group| group.inputs().iter().chain(group.outputs()))
            .flat_map(|interaction| interaction.target().static_actors())
            .filter_map(|target| match target {
                StaticActorId::Linked(id) => Some(id.clone()),
                StaticActorId::InApp(_) => None,
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let mut plan = Self {
            templates,
            families: family_specs.into_values().collect(),
            selectors,
            observed_actors,
            spawn_outputs,
            actor_type_source_templates,
            state_expansions,
            observed_output_fields,
            imported_templates,
            roles: Vec::new(),
        };
        plan.assign_roles(source_id);
        Ok(plan)
    }

    fn assign_roles(&mut self, source_id: DeclId) {
        let pair = [WitnessComponent::Prefix, WitnessComponent::Suffix];
        for (index, spec) in self.templates.iter().enumerate() {
            if spec.id != StaticActorId::InApp(source_id) {
                self.roles.extend(pair.map(|component| WitnessRole::Template { index, component }));
            }
        }
        for index in 0..self.families.len() {
            self.roles.push(WitnessRole::RouteFamily { index });
        }
        for index in 0..self.selectors.len() {
            self.roles.extend(pair.map(|component| WitnessRole::Selector { index, component }));
        }
        for (index, spec) in self.observed_actors.iter().enumerate() {
            self.roles.extend(pair.map(|component| WitnessRole::Observed { index, component }));
            if spec.side == ObservedActorSide::Input && spec.dynamic_binding {
                self.roles.push(WitnessRole::Observed { index, component: WitnessComponent::TemplateHash });
            }
        }
        for index in 0..self.spawn_outputs.len() {
            self.roles.push(WitnessRole::SpawnIndex { index });
        }
        for index in 0..self.actor_type_source_templates.len() {
            self.roles.extend(pair.map(|component| WitnessRole::ActorType { index, component }));
        }
        for index in 0..self.state_expansions.len() {
            self.roles.push(WitnessRole::StateExpansion { index });
        }
        for index in 0..self.observed_output_fields.len() {
            self.roles.push(WitnessRole::ObservedOutputField { index });
        }
    }

    pub(crate) fn template(&self, target: &StaticActorId) -> Option<&TemplateWitnessSpec> {
        self.templates.iter().find(|spec| &spec.id == target)
    }

    pub(crate) fn spawn_output(&self, id: InteractionId) -> Result<&SpawnActorWitnessSpec> {
        self.spawn_outputs
            .iter()
            .find(|spec| spec.id == id)
            .ok_or_else(|| ArgentError::new(format!("missing spawn output witness descriptor for `{id:?}`")))
    }

    pub(crate) fn role_type(&self, role: WitnessRole) -> WitnessAbiType {
        match role {
            WitnessRole::Template { index, .. } => match self.templates[index].form {
                TemplateWitnessForm::Bytes => WitnessAbiType::Bytes,
                TemplateWitnessForm::Len => WitnessAbiType::Int,
            },
            WitnessRole::RouteFamily { index } => WitnessAbiType::FixedBytes(self.families[index].byte_len),
            WitnessRole::Selector { .. } => WitnessAbiType::Bytes,
            WitnessRole::Observed { index, component: WitnessComponent::TemplateHash } => {
                let _ = index;
                WitnessAbiType::FixedBytes(32)
            }
            WitnessRole::Observed { index, .. } => match self.observed_actors[index].side {
                ObservedActorSide::Input => WitnessAbiType::Int,
                ObservedActorSide::Output => WitnessAbiType::Bytes,
            },
            WitnessRole::SpawnIndex { .. } => WitnessAbiType::Int,
            WitnessRole::ActorType { index, .. } => match self.actor_type_source_templates[index].form {
                TemplateWitnessForm::Bytes => WitnessAbiType::Bytes,
                TemplateWitnessForm::Len => WitnessAbiType::Int,
            },
            WitnessRole::StateExpansion { index } => WitnessAbiType::FixedBytes(self.state_expansions[index].packed_len),
            WitnessRole::ObservedOutputField { .. } => WitnessAbiType::FixedBytes(32),
        }
    }
}

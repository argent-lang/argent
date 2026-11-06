//! Selected-app semantic context and domain models shared by planning and code generation.

use std::collections::BTreeMap;

use crate::artifact::{AppDependencyArtifact, EntryRefArtifact};
use crate::compiler::resolve::{Binding, ResolvedModules, ResolvedName};
use crate::compiler::syntax::node::{DeclId, EntryId, RootSlot, SourceNodeCursor, SymbolKind};
use crate::compiler::syntax::{ActorDecl, ConstDecl, EntryDecl, FunctionDecl, ObserveDecl, ObservedActorDecl, StateDecl};
use crate::error::{ArgentError, Result};

use self::link::{LinkedActor, LinkedActorId};

mod actor;
mod actor_value_policy;
mod actor_values;
mod build;
mod consts;
mod entry;
mod inputs;
mod layout;
pub(crate) mod link;
mod outputs;
mod routes;
mod source_values;
mod types;
mod validate;
mod witnesses;

pub(crate) use actor::ActorModel;
pub(crate) use actor_values::LinkedActorValuePlan;
pub(crate) use consts::{ConstIntError, ConstResolver};
pub(crate) use entry::{
    ActorTarget, ActorTemplateUses, ClauseActorTypeRef, CovenantGroup, CovenantGroupId, CovenantIdSource, EntryInteraction,
    EntryModel, InteractionId, InteractionLocation, InteractionSource, ResolvedRoute, ResolvedSuccessor, TemplateSelector,
    clause_actor_type_ref, observed_is_dynamic_binding, observed_open_bindings, observed_open_state_for_decl,
    resolve_observe_covenant_id_source, spawn_target_state,
};
pub(crate) use inputs::{
    CurrentInputGroupPolicy, EntryInputPlan, EntryInputReferenceId, InputAuthentication, InputFieldAvailability, InputReferenceOrigin,
    InputReferenceRequirement,
};
pub(crate) use layout::{
    ContractStateLowering, GeneratedFieldId, OutputPhysicalTypePlan, PhysicalFieldId, PhysicalStateLayout, PhysicalTargetId,
    SilStateType, SourceFieldId, SourceStateId, SourceStorageRelation, TargetPhysicalPlan, build_contract_state_lowerings,
    packed_field_len,
};
pub(crate) use outputs::{ActorOutputPlan, EntryOutputPlan, GeneratedFieldSource, OutputProofRequirement, OutputTargetPlan};
pub(crate) use routes::{
    CompilerRoutePlan, CompilerRoutePlanner, CompilerRouteTransition, RouteFamily, RouteRootLeaf, default_route_planner,
    infer_direct_routes,
};
pub(crate) use types::BoundRouteActor;
pub(crate) use types::{
    ActorValuePlan, CallableId, CallableSignaturePlan, FixedArrayLength, PlannedStateValue, ResolvedType, ResolvedTypeBase,
    StateValueShape, TypeTable,
};
pub(crate) use witnesses::{
    ActorTypeSourceWitnessProvider, ObservedActorSide, ObservedActorWitnessSpec, ObservedOutputFieldWitnessSpec,
    ObservedTemplateSource, SpawnActorWitnessSpec, StateExpansionWitnessSpec, TemplateWitnessForm, TemplateWitnessSource,
    WitnessAbiType, WitnessComponent, WitnessPlan, WitnessRole,
};

/// The selected application's source declarations, linked dependencies, routes, and state plans.
#[derive(Debug)]
pub(crate) struct AppCompilationContext<'a> {
    pub(crate) resolution: &'a ResolvedModules<'a>,
    pub(crate) app_name: String,
    pub(crate) types: TypeTable,
    pub(crate) declaration_origins: BTreeMap<String, link::DeclarationOrigin>,
    /// Direct artifacts used to link the selected app.
    pub(crate) app_dependencies: Vec<AppDependencyArtifact>,
    pub(crate) app_actors: AppActors,
    pub(crate) route_families: Vec<RouteFamily>,
    pub(crate) consts: Vec<(DeclId, &'a ConstDecl)>,
    pub(crate) functions: Vec<(DeclId, &'a FunctionDecl)>,
    pub(crate) states: BTreeMap<String, &'a StateDecl>,
    pub(crate) linked_states: BTreeMap<String, StateDecl>,
    state_names_by_source: BTreeMap<SourceStateId, String>,
    state_decl_ids_by_source: BTreeMap<SourceStateId, DeclId>,
    storage_source_by_source: BTreeMap<SourceStateId, SourceStateId>,
    pub(crate) linked_field_sources: BTreeMap<SourceFieldId, SourceStateId>,
    pub(crate) actors_by_name: BTreeMap<String, &'a ActorDecl>,
    pub(crate) linked_actors: BTreeMap<LinkedActorId, LinkedActor>,
    pub(crate) linked_actor_values: LinkedActorValuePlan,
    pub(crate) linked_actor_names: BTreeMap<String, LinkedActorId>,
    pub(crate) actor_enums: BTreeMap<String, ActorEnumInfo>,
    pub(crate) actor_models: BTreeMap<DeclId, ActorModel<'a>>,
    pub(crate) actor_value_plans: BTreeMap<DeclId, ActorValuePlan>,
    pub(crate) input_plans: BTreeMap<EntryId, EntryInputPlan>,
    pub(crate) output_plans: BTreeMap<DeclId, ActorOutputPlan>,
    pub(crate) entry_output_plans: BTreeMap<EntryId, EntryOutputPlan>,
    pub(crate) witness_plans: BTreeMap<EntryId, WitnessPlan>,
    pub(crate) state_expansion_witnesses_by_actor: BTreeMap<DeclId, Vec<StateExpansionWitnessSpec>>,
    /// Delegate entries that establish each actor as a leader actor.
    pub(crate) leader_for: BTreeMap<DeclId, Vec<EntryRefArtifact>>,
    /// The planned route commitment cut carried by each app actor.
    pub(crate) route_leaves_by_actor: BTreeMap<DeclId, Vec<RouteRootLeaf>>,
    pub(crate) route_transitions: BTreeMap<(DeclId, DeclId), CompilerRouteTransition>,
    /// Contract-local state representation plans built after route planning.
    pub(crate) state_lowering_by_actor: BTreeMap<DeclId, ContractStateLowering>,
}

/// Ordered membership of the selected application's actor domain.
#[derive(Debug)]
pub(crate) struct AppActors {
    actors: Vec<String>,
    ids: Vec<DeclId>,
    by_name: BTreeMap<String, DeclId>,
}

impl AppActors {
    /// Build ordered and membership views of the selected app's actors.
    pub(crate) fn new(actors: Vec<(DeclId, String)>) -> Self {
        let ids = actors.iter().map(|(id, _)| *id).collect();
        let by_name = actors.iter().map(|(id, name)| (name.clone(), *id)).collect();
        let actors = actors.into_iter().map(|(_, name)| name).collect::<Vec<_>>();
        Self { actors, ids, by_name }
    }

    /// Iterate actors in app declaration order.
    pub(crate) fn iter(&self) -> impl Iterator<Item = &String> {
        self.actors.iter()
    }

    /// Iterate selected actor identities with their artifact names in app order.
    pub(crate) fn iter_with_ids(&self) -> impl Iterator<Item = (DeclId, &str)> {
        self.ids.iter().copied().zip(self.actors.iter().map(String::as_str))
    }

    /// Return whether an actor belongs to the selected app.
    pub(crate) fn contains(&self, actor: &str) -> bool {
        self.by_name.contains_key(actor)
    }

    pub(crate) fn name(&self, id: DeclId) -> Option<&str> {
        self.ids.iter().position(|candidate| *candidate == id).map(|index| self.actors[index].as_str())
    }

    /// Return whether `target` is the source actor in a singleton app.
    pub(crate) fn is_singleton_actor_self_target(&self, source_actor: DeclId, target: DeclId) -> bool {
        self.ids.as_slice() == [source_actor] && source_actor == target
    }
}

/// An actor enum resolved to one state domain and its ordered variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActorEnumInfo {
    pub(crate) name: String,
    pub(crate) state: String,
    pub(crate) variants: Vec<String>,
}

/// A compiler-known actor target resolved without changing route membership.
#[derive(Clone, Copy, Debug)]
pub(crate) enum StaticActorTarget<'m> {
    /// An actor in the selected application's routing domain.
    InApp(DeclId),
    /// An imported actor whose template stays outside the local route graph.
    CrossApp(&'m LinkedActor),
}

/// Semantic identity of a fixed actor used by input and output plans.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum StaticActorId {
    InApp(DeclId),
    Linked(LinkedActorId),
}

impl<'m> StaticActorTarget<'m> {
    pub(crate) fn id(self) -> StaticActorId {
        match self {
            Self::InApp(id) => StaticActorId::InApp(id),
            Self::CrossApp(actor) => StaticActorId::Linked(LinkedActorId { app: actor.app.clone(), actor: actor.actor.clone() }),
        }
    }
}

impl AppCompilationContext<'_> {
    /// Look up a selected actor's state lowering by its bound declaration.
    pub(crate) fn state_lowering_by_id(&self, actor: DeclId) -> Result<&ContractStateLowering> {
        self.state_lowering_by_actor.get(&actor).ok_or_else(|| {
            let name = self.app_actors.name(actor).unwrap_or("<unknown>");
            ArgentError::new(format!("missing state lowering environment for actor `{name}`"))
        })
    }

    /// Resolve a local or linked state declaration.
    pub(crate) fn state(&self, name: &str) -> Result<&StateDecl> {
        self.states
            .get(name)
            .copied()
            .or_else(|| self.linked_states.get(name))
            .ok_or_else(|| ArgentError::new(format!("unknown state `{name}`")))
    }

    /// Resolve a local or linked state to its verified declaration provenance.
    pub(crate) fn source_state_id(&self, name: &str) -> Result<SourceStateId> {
        self.state(name)?;
        let origin = self
            .declaration_origins
            .get(name)
            .ok_or_else(|| ArgentError::new(format!("missing declaration identity for state `{name}`")))?;
        Ok(SourceStateId::from_origin(name, origin.clone()))
    }

    /// Convert a resolved source declaration to its nominal state identity.
    pub(crate) fn source_state_id_by_decl(&self, id: DeclId) -> Result<SourceStateId> {
        if id.kind() != SymbolKind::State {
            return Err(ArgentError::new("resolved source identity is not a state declaration"));
        }
        let origin = link::DeclarationOrigin::Source {
            path: self.resolution.declaration_path(id).to_path_buf(),
            kind: SymbolKind::State,
            index: id.index,
        };
        let source = SourceStateId::from_origin(String::new(), origin.clone());
        let name = self
            .state_names_by_source
            .get(&source)
            .ok_or_else(|| ArgentError::new("resolved state has no selected source identity"))?;
        Ok(SourceStateId::from_origin(name, origin))
    }

    /// Read a state reference already bound at a declaration site.
    pub(crate) fn bound_state_use(&self, owner: DeclId, slot: RootSlot) -> Result<DeclId> {
        let cursor = SourceNodeCursor::new(owner, slot);
        let site = self
            .resolution
            .nodes()
            .find(&cursor.address)
            .ok_or_else(|| ArgentError::new("state reference has no indexed source site"))?;
        match self.resolution.bindings(owner).sites.get(&site) {
            Some(Binding::Source(ResolvedName::Declaration(state))) if state.kind() == SymbolKind::State => Ok(*state),
            _ => Err(ArgentError::new("state reference has no bound source declaration")),
        }
    }

    /// Resolve a state through its nominal provenance, not only its display name.
    pub(crate) fn state_by_source(&self, source: &SourceStateId) -> Result<&StateDecl> {
        if let Some(name) = self.state_names_by_source.get(source) {
            return self.state(name);
        }
        self.state(source.as_str())?;
        Err(ArgentError::new(format!("state `{}` has conflicting source identity", source.as_str())))
    }

    pub(crate) fn storage_state_by_source(&self, source: &SourceStateId) -> Result<&StateDecl> {
        self.state_by_source(self.storage_source_id(source))
    }

    /// Resolve a selected actor's storage state through its bound declaration.
    pub(crate) fn storage_state_for_actor(&self, actor: DeclId) -> Result<&StateDecl> {
        let state = *self.types.actor_states.get(&actor).ok_or_else(|| ArgentError::new("selected actor has no bound state"))?;
        let source = self.source_state_id_by_decl(state)?;
        self.storage_state_by_source(&source)
    }

    /// Follow a completed expansion relation by nominal source identity.
    pub(crate) fn storage_source_id<'s>(&'s self, source: &'s SourceStateId) -> &'s SourceStateId {
        self.storage_source_by_source.get(source).unwrap_or(source)
    }

    /// Return a selected source declaration ID when the state belongs to this build.
    pub(crate) fn state_decl_id_by_source(&self, source: &SourceStateId) -> Option<DeclId> {
        self.state_decl_ids_by_source.get(source).copied()
    }

    /// Iterate the verified nominal identities of local and linked states.
    pub(crate) fn state_sources(&self) -> impl Iterator<Item = &SourceStateId> {
        self.state_names_by_source.keys()
    }

    /// Retrieve a selected actor through its resolved declaration identity.
    pub(crate) fn actor_by_decl(&self, id: DeclId) -> Result<&ActorDecl> {
        self.actor_models
            .get(&id)
            .map(ActorModel::source)
            .ok_or_else(|| ArgentError::new("unknown selected actor declaration identity"))
    }

    /// Return the route family containing an actor.
    pub(crate) fn route_family_for_actor_id(&self, actor: DeclId) -> Option<&RouteFamily> {
        self.route_families.iter().find(|family| family.actor_ids.contains(&actor))
    }

    /// Resolve a route family by its artifact ID.
    pub(crate) fn route_family(&self, family_id: &str) -> Option<&RouteFamily> {
        self.route_families.iter().find(|family| family.id == family_id)
    }

    /// Return delegate entries for which an actor is the leader.
    pub(crate) fn leader_for(&self, actor: DeclId) -> &[EntryRefArtifact] {
        self.leader_for.get(&actor).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Return whether an actor leads at least one delegate entry.
    pub(crate) fn is_leader_actor(&self, actor: DeclId) -> bool {
        !self.leader_for(actor).is_empty()
    }

    /// Resolve a fixed selected-app or linked actor without changing routing.
    pub(crate) fn static_actor_target(&self, expression: &str) -> Option<StaticActorTarget<'_>> {
        let reference = expression.trim();
        self.app_actors
            .by_name
            .get(reference)
            .copied()
            .map(StaticActorTarget::InApp)
            .or_else(|| self.linked_actor(reference).map(StaticActorTarget::CrossApp))
    }

    /// Render a bound fixed actor for contract-local witness naming.
    pub(crate) fn static_actor_reference(&self, id: &StaticActorId) -> Result<String> {
        match id {
            StaticActorId::InApp(id) => self
                .app_actors
                .name(*id)
                .map(str::to_string)
                .ok_or_else(|| ArgentError::new("output proof references an unknown selected-app actor")),
            StaticActorId::Linked(id) => self
                .linked_actors
                .get(id)
                .map(|actor| format!("{}::{}", actor.app, actor.actor))
                .ok_or_else(|| ArgentError::new("output proof references an unknown linked actor")),
        }
    }

    /// Iterate selected and linked fixed actors by their bound identities.
    pub(crate) fn static_actor_ids(&self) -> impl Iterator<Item = StaticActorId> + '_ {
        self.app_actors
            .ids
            .iter()
            .copied()
            .map(StaticActorId::InApp)
            .chain(self.linked_actors.keys().cloned().map(StaticActorId::Linked))
    }

    /// Resolve a bound actor target to its authored state identity.
    pub(crate) fn static_actor_source_state(&self, id: &StaticActorId) -> Result<SourceStateId> {
        match id {
            StaticActorId::InApp(id) => {
                let state =
                    self.types.actor_states.get(id).ok_or_else(|| ArgentError::new("unknown selected-app actor state identity"))?;
                self.source_state_id_by_decl(*state)
            }
            StaticActorId::Linked(id) => {
                let state = &self.linked_actors.get(id).ok_or_else(|| ArgentError::new("unknown linked actor identity"))?.state;
                self.source_state_id(state)
            }
        }
    }

    /// An open observed actor requires a runtime template rather than a fixed actor target.
    pub(crate) fn static_observed_actor_target(
        &self,
        entry_id: EntryId,
        actor: &ActorDecl,
        entry: &EntryDecl,
        observe: &ObserveDecl,
        observed: &ObservedActorDecl,
    ) -> Result<Option<StaticActorTarget<'_>>> {
        if observed_open_state_for_decl(entry_id, actor, entry, observe, observed, self)?.is_some() {
            return Ok(None);
        }
        let entry_model = self.entry_model_by_id(entry_id)?;
        let id = entry_model.observed_interaction_id(observe, observed)?;
        let interaction = entry_model
            .existing_groups()
            .flat_map(|group| group.inputs().iter().chain(group.outputs()))
            .find(|interaction| interaction.id() == id)
            .ok_or_else(|| ArgentError::new("observed actor has no normalized interaction"))?;
        Ok(self.resolve_static_actor_target(interaction.target()))
    }

    /// Resolve a normalized singleton static target.
    pub(crate) fn resolve_static_actor_target(&self, target: &ActorTarget) -> Option<StaticActorTarget<'_>> {
        match target.single_static_actor()? {
            StaticActorId::InApp(id) => self.actor_by_decl(*id).ok().map(|_| StaticActorTarget::InApp(*id)),
            StaticActorId::Linked(id) => self.linked_actors.get(id).map(StaticActorTarget::CrossApp),
        }
    }

    /// Collect shared local and imported actor-template uses for one entry.
    pub(crate) fn entry_template_uses(&self, id: EntryId) -> Result<ActorTemplateUses> {
        let entry_model = self.entry_model_by_id(id)?;
        let mut uses = entry_model.actor_template_uses(entry_model.id.actor, &self.app_actors);

        // Current interactions are restricted to the selected app; only external
        // covenant groups can reference imported actors.
        for group in entry_model.existing_groups().chain(entry_model.genesis_groups()) {
            for interaction in group.inputs() {
                for target in interaction.target().static_actors() {
                    if matches!(target, StaticActorId::Linked(_)) {
                        uses.reads.insert(target.clone());
                    }
                }
            }
            for interaction in group.outputs() {
                for target in interaction.target().static_actors() {
                    if matches!(target, StaticActorId::Linked(_)) {
                        uses.writes.insert(target.clone());
                    }
                }
            }
        }
        Ok(uses)
    }
}

impl<'a> AppCompilationContext<'a> {
    /// Visit completed entries in the selected app's declared actor order.
    pub(crate) fn entries_in_app_order(&self) -> impl Iterator<Item = (&ActorDecl, &EntryModel<'a>)> {
        self.app_actors.ids.iter().flat_map(|id| {
            let actor = &self.actor_models[id];
            actor.entries().map(move |entry| (actor.source(), entry))
        })
    }

    /// Look up a selected actor's state-value requirements by declaration.
    pub(crate) fn actor_value_plan_by_id(&self, actor: DeclId) -> Result<&ActorValuePlan> {
        self.actor_value_plans.get(&actor).ok_or_else(|| {
            let name = self.app_actors.name(actor).unwrap_or("<unknown>");
            ArgentError::new(format!("missing value plan for actor `{name}`"))
        })
    }

    pub(crate) fn input_plan_by_id(&self, id: EntryId) -> Result<&EntryInputPlan> {
        self.input_plans.get(&id).ok_or_else(|| ArgentError::new(format!("missing input plan for entry `{id:?}`")))
    }

    pub(crate) fn output_plan_by_id(&self, actor: DeclId) -> Result<&ActorOutputPlan> {
        self.output_plans.get(&actor).ok_or_else(|| {
            let name = self.app_actors.name(actor).unwrap_or("<unknown>");
            ArgentError::new(format!("missing output plan for actor `{name}`"))
        })
    }

    pub(crate) fn witness_plan_by_id(&self, id: EntryId) -> Result<&WitnessPlan> {
        self.witness_plans.get(&id).ok_or_else(|| ArgentError::new(format!("missing witness plan for entry `{id:?}`")))
    }

    pub(crate) fn entry_output_plan_by_id(&self, id: EntryId) -> Result<&EntryOutputPlan> {
        self.entry_output_plans.get(&id).ok_or_else(|| ArgentError::new(format!("missing output proof plan for entry `{id:?}`")))
    }

    pub(crate) fn state_expansion_witnesses_by_id(&self, actor: DeclId) -> Result<&[StateExpansionWitnessSpec]> {
        self.state_expansion_witnesses_by_actor.get(&actor).map(Vec::as_slice).ok_or_else(|| {
            let name = self.app_actors.name(actor).unwrap_or("<unknown>");
            ArgentError::new(format!("missing state expansion witness plan for actor `{name}`"))
        })
    }

    /// Resolve the normalized model for one source entry.
    pub(crate) fn entry_model_by_id(&self, id: EntryId) -> Result<&EntryModel<'a>> {
        self.actor_models
            .get(&id.actor)
            .and_then(|actor| actor.entry_by_id(id))
            .ok_or_else(|| ArgentError::new("unknown selected entry identity"))
    }

    /// Return a linked actor by its source reference.
    pub(crate) fn linked_actor(&self, name: &str) -> Option<&LinkedActor> {
        self.linked_actors.get(self.linked_actor_names.get(name)?)
    }

    /// Expand selector routes from a bound entry identity.
    pub(crate) fn expanded_routes_by_id(&self, id: EntryId) -> Result<Vec<ResolvedRoute>> {
        let entry_model = self.entry_model_by_id(id)?;
        let mut expanded = Vec::new();
        for route in entry_model.routes() {
            let ResolvedSuccessor::Constructed { arity, bound, .. } = &route.successor else {
                expanded.push(route.clone());
                continue;
            };
            let selector = match bound.map(|value| value.actor_target) {
                Some(types::BoundRouteActor::Local(local)) => entry_model.selector_for_local(local),
                Some(types::BoundRouteActor::Selector(_)) | None => None,
                Some(types::BoundRouteActor::Fixed(_) | types::BoundRouteActor::Linked(_) | types::BoundRouteActor::Expression(_)) => {
                    None
                }
            };
            if let Some(selector) = selector {
                for id in selector.route_actor_ids()? {
                    expanded.push(ResolvedRoute {
                        id: route.id,
                        output: route.output.clone(),
                        successor: ResolvedSuccessor::Constructed {
                            actor: entry::RouteActorSource::Expanded(id.clone()),
                            arity: *arity,
                            bound: *bound,
                        },
                    });
                }
            } else {
                expanded.push(route.clone());
            }
        }
        Ok(expanded)
    }

    /// Resolve a fixed constructed route to its semantic actor identity.
    pub(crate) fn route_static_target_id(&self, route: &ResolvedRoute) -> Result<StaticActorId> {
        let ResolvedSuccessor::Constructed { bound: Some(bound), .. } = &route.successor else {
            return Err(ArgentError::new("output proof has no bound constructed route"));
        };
        match bound.actor_target {
            types::BoundRouteActor::Fixed(id) => Ok(StaticActorId::InApp(id)),
            types::BoundRouteActor::Linked(member) => {
                let id = LinkedActorId::from_member(member, self.resolution);
                self.linked_actors
                    .contains_key(&id)
                    .then_some(StaticActorId::Linked(id))
                    .ok_or_else(|| ArgentError::new("linked route has no actor identity"))
            }
            types::BoundRouteActor::Selector(_) | types::BoundRouteActor::Local(_) | types::BoundRouteActor::Expression(_) => {
                Err(ArgentError::new("output proof requires one fixed actor target"))
            }
        }
    }

    /// Expand a bound entry route to its concrete targets.
    pub(crate) fn route_target_ids_by_id(&self, id: EntryId, route: &ResolvedRoute) -> Result<Vec<StaticActorId>> {
        let entry_model = self.entry_model_by_id(id)?;
        let actor = self.actor_by_decl(id.actor)?;
        let entry = entry_model.source();
        let ResolvedSuccessor::Constructed { bound, .. } = &route.successor else {
            return Ok(vec![StaticActorId::InApp(entry_model.id.actor)]);
        };
        let bound = bound
            .ok_or_else(|| ArgentError::new(format!("entry `{}::{}` has an unbound constructed successor", actor.name, entry.name)))?;
        match bound.actor_target {
            types::BoundRouteActor::Fixed(id) => Ok(vec![StaticActorId::InApp(id)]),
            types::BoundRouteActor::Linked(member) => {
                let id = LinkedActorId::from_member(member, self.resolution);
                self.linked_actors
                    .contains_key(&id)
                    .then_some(id)
                    .map(StaticActorId::Linked)
                    .map(|id| vec![id])
                    .ok_or_else(|| ArgentError::new("linked route has no actor identity"))
            }
            types::BoundRouteActor::Selector(id) => self
                .types
                .enum_variants
                .get(&id)
                .map(|variants| variants.iter().copied().map(StaticActorId::InApp).collect())
                .ok_or_else(|| ArgentError::new(format!("entry `{}::{}` has an unbound route selector", actor.name, entry.name))),
            types::BoundRouteActor::Local(local) => entry_model
                .selector_for_local(local)
                .ok_or_else(|| {
                    ArgentError::new(format!(
                        "entry `{}::{}` routes through a local without an actor selector",
                        actor.name, entry.name
                    ))
                })?
                .route_actor_ids()
                .map(<[StaticActorId]>::to_vec),
            types::BoundRouteActor::Expression(_) => Err(ArgentError::new(format!(
                "entry `{}::{}` has a route target without a resolved actor or selector",
                actor.name, entry.name
            ))),
        }
    }
}

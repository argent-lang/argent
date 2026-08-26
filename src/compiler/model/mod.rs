//! Source-backed compiler models shared by validation, planning, and code generation.

use std::collections::{BTreeMap, BTreeSet};

use crate::artifact::{AppDependencyArtifact, EntryRefArtifact};
use crate::compiler::naming::to_snake;
use crate::compiler::syntax::{ActorDecl, ConstDecl, EntryDecl, FunctionDecl, StateDecl, TypeRef};
use crate::error::{ArgentError, Result};
use crate::routing::{CommitmentNode, RouteGraph, RoutePlan as PlannerRoutePlan, SelectorRequirement, route_plan};

use self::link::LinkedActor;

mod actor;
mod build;
mod entry;
mod layout;
pub(crate) mod link;
mod validate;

#[cfg(test)]
mod tests;

pub(crate) use actor::ActorModel;
pub(crate) use entry::{
    ActorTarget, ActorTemplateUses, ClauseActorTypeRef, CovenantGroup, CovenantIdSource, EntryInteraction, EntryModel,
    InteractionSource, ResolvedRoute, ResolvedSuccessor, TemplateSelector, actor_enum_variant_const_expr, clause_actor_type_ref,
    observed_is_dynamic_binding, observed_open_bindings, observed_open_state_for_decl, parse_actor_enum_selector,
    parse_actor_enum_variant, resolve_observe_covenant_id_source, source_actor_type_state_for_expr, spawn_target_state,
};
pub(crate) use layout::{
    ContractStateLowering, GeneratedFieldId, OutputPhysicalTypePlan, PhysicalFieldId, PhysicalStateLayout, PhysicalTargetId,
    SilStateType, SourceStateId, TargetPhysicalPlan, build_contract_state_lowerings, packed_field_len,
};

/// The selected application's compiler-wide source and routing model.
#[derive(Debug)]
pub(crate) struct Model<'a> {
    pub(crate) app_name: String,
    /// Direct artifacts used to link the selected app.
    pub(crate) app_dependencies: Vec<AppDependencyArtifact>,
    pub(crate) app_actors: AppActors,
    pub(crate) route_families: Vec<RouteFamily>,
    pub(crate) consts: Vec<&'a ConstDecl>,
    pub(crate) functions: Vec<&'a FunctionDecl>,
    pub(crate) states: BTreeMap<String, &'a StateDecl>,
    pub(crate) linked_states: BTreeMap<String, StateDecl>,
    pub(crate) actors_by_name: BTreeMap<String, &'a ActorDecl>,
    pub(crate) linked_actor_decls: BTreeMap<String, ActorDecl>,
    pub(crate) linked_actors: BTreeMap<String, LinkedActor>,
    pub(crate) actor_enums: BTreeMap<String, ActorEnumInfo>,
    pub(crate) actors: Vec<&'a ActorDecl>,
    pub(crate) actor_models: BTreeMap<&'a str, ActorModel<'a>>,
    /// Delegate entries that establish each actor as a leader actor.
    pub(crate) leader_for: BTreeMap<String, Vec<EntryRefArtifact>>,
    /// The planned route commitment cut carried by each app actor.
    pub(crate) route_leaves_by_actor: BTreeMap<String, Vec<RouteRootLeaf>>,
    pub(crate) route_transitions: BTreeMap<(String, String), CompilerRouteTransition>,
    /// Contract-local state representation plans built after route planning.
    pub(crate) state_lowering_by_actor: BTreeMap<String, ContractStateLowering>,
}

/// Ordered membership of the selected application's actor domain.
#[derive(Debug)]
pub(crate) struct AppActors {
    actors: Vec<String>,
    members: BTreeSet<String>,
}

impl AppActors {
    /// Build ordered and membership views of the selected app's actors.
    pub(crate) fn new(actors: Vec<String>) -> Self {
        let members = actors.iter().cloned().collect();
        Self { actors, members }
    }

    /// Iterate actors in app declaration order.
    pub(crate) fn iter(&self) -> impl Iterator<Item = &String> {
        self.actors.iter()
    }

    /// Return whether an actor belongs to the selected app.
    pub(crate) fn contains(&self, actor: &str) -> bool {
        self.members.contains(actor)
    }

    /// Return the selected app's actor set.
    pub(crate) fn members(&self) -> &BTreeSet<String> {
        &self.members
    }

    /// Return whether `target` is the source actor in a singleton app.
    pub(crate) fn is_singleton_actor_self_target(&self, source_actor: &str, target: &str) -> bool {
        self.actors.len() == 1 && self.actors[0] == source_actor && target == source_actor
    }
}

/// An actor enum resolved to one state domain and its ordered variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActorEnumInfo {
    pub(crate) name: String,
    pub(crate) state: String,
    pub(crate) variants: Vec<String>,
}

/// A state-local actor family represented by one ordered route table.
///
/// Entry actors remain direct while table actors are committed in table order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RouteFamily {
    pub(crate) id: String,
    pub(crate) state: String,
    pub(crate) rep: String,
    pub(crate) actors: Vec<String>,
    pub(crate) entry_actors: Vec<String>,
    pub(crate) table_actors: Vec<String>,
}

impl RouteFamily {
    /// Return the actor representing this family.
    pub(crate) fn rep(&self) -> &str {
        &self.rep
    }

    /// Return family actors whose templates remain direct.
    pub(crate) fn direct_template_actors(&self) -> &[String] {
        &self.entry_actors
    }

    /// Return family actors committed in the route table.
    pub(crate) fn table_actors(&self) -> &[String] {
        &self.table_actors
    }

    /// Return the serialized byte length of the route table.
    pub(crate) fn table_byte_len(&self) -> usize {
        self.table_actors().len() * 32
    }
}

/// One selected root in an actor-carried route commitment.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum RouteRootLeaf {
    Actor(String),
    Family(String),
}

/// A compiler-known actor target resolved without changing route membership.
#[derive(Clone, Copy, Debug)]
pub(crate) enum StaticActorTarget<'m> {
    /// An actor in the selected application's routing domain.
    InApp(&'m ActorDecl),
    /// An imported actor whose template stays outside the local route graph.
    CrossApp(&'m LinkedActor),
}

impl<'m> StaticActorTarget<'m> {
    /// Return the target's source state.
    pub(crate) fn state(&self) -> &str {
        match self {
            Self::InApp(actor) => &actor.state,
            Self::CrossApp(actor) => &actor.state,
        }
    }

    /// Return the stable artifact reference used by shared template witnesses.
    pub(crate) fn artifact_reference(&self) -> String {
        match self {
            Self::InApp(actor) => actor.name.clone(),
            Self::CrossApp(actor) => format!("{}::{}", actor.app, actor.actor),
        }
    }

    /// Return the selected-app actor, if this target belongs to it.
    pub(crate) fn in_app_actor(self) -> Option<&'m ActorDecl> {
        match self {
            Self::InApp(actor) => Some(actor),
            Self::CrossApp(_) => None,
        }
    }

    /// Compare canonical actor identity across local and imported spellings.
    pub(crate) fn same_actor(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::InApp(left), Self::InApp(right)) => left.name == right.name,
            (Self::CrossApp(left), Self::CrossApp(right)) => left.app == right.app && left.actor == right.actor,
            (Self::InApp(_), Self::CrossApp(_)) | (Self::CrossApp(_), Self::InApp(_)) => false,
        }
    }
}

impl Model<'_> {
    /// Return the immutable state lowering environment for one emitted actor.
    pub(crate) fn state_lowering(&self, actor: &str) -> Result<&ContractStateLowering> {
        self.state_lowering_by_actor
            .get(actor)
            .ok_or_else(|| ArgentError::new(format!("missing state lowering environment for actor `{actor}`")))
    }

    /// Resolve a local or linked state declaration.
    pub(crate) fn state(&self, name: &str) -> Result<&StateDecl> {
        self.states
            .get(name)
            .copied()
            .or_else(|| self.linked_states.get(name))
            .ok_or_else(|| ArgentError::new(format!("unknown state `{name}`")))
    }

    /// Resolve the physical state stored for a source state.
    pub(crate) fn storage_state_name(&self, name: &str) -> Result<String> {
        let state = self.state(name)?;
        Ok(state.expansion.as_ref().map_or_else(|| name.to_string(), |expansion| expansion.base.clone()))
    }

    /// Return the physical state declaration stored for a source state.
    pub(crate) fn storage_state(&self, name: &str) -> Result<&StateDecl> {
        self.state(&self.storage_state_name(name)?)
    }

    /// Return whether a local or linked state is available.
    pub(crate) fn has_state(&self, name: &str) -> bool {
        self.states.contains_key(name) || self.linked_states.contains_key(name)
    }

    /// Iterate local followed by linked state declarations.
    pub(crate) fn all_states(&self) -> impl Iterator<Item = &StateDecl> {
        self.states.values().copied().chain(self.linked_states.values())
    }

    /// Resolve a local or linked actor declaration.
    pub(crate) fn actor(&self, name: &str) -> Result<&ActorDecl> {
        self.actors_by_name
            .get(name)
            .copied()
            .or_else(|| self.linked_actor_decls.get(name))
            .ok_or_else(|| ArgentError::new(format!("unknown actor `{name}`")))
    }

    /// Return the physical state carried by an actor template.
    pub(crate) fn actor_state(&self, name: &str) -> Result<&StateDecl> {
        let actor = self.actor(name)?;
        self.storage_state(&actor.state)
    }

    /// Return the route family containing an actor.
    pub(crate) fn route_family_for_actor(&self, actor: &str) -> Option<&RouteFamily> {
        self.route_families.iter().find(|family| family.actors.iter().any(|member| member == actor))
    }

    /// Resolve a route family by its artifact ID.
    pub(crate) fn route_family(&self, family_id: &str) -> Option<&RouteFamily> {
        self.route_families.iter().find(|family| family.id == family_id)
    }

    /// Resolve the planned cut transition between two app actors.
    pub(crate) fn route_transition(&self, source: &str, target: &str) -> Option<&CompilerRouteTransition> {
        self.route_transitions.get(&(source.to_string(), target.to_string()))
    }

    /// Return route families whose actors carry one state type.
    pub(crate) fn route_families_for_state(&self, state: &str) -> Vec<&RouteFamily> {
        self.route_families.iter().filter(|family| family.state == state).collect()
    }

    /// Return delegate entries for which an actor is the leader.
    pub(crate) fn leader_for(&self, actor: &str) -> &[EntryRefArtifact] {
        self.leader_for.get(actor).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Return whether an actor leads at least one delegate entry.
    pub(crate) fn is_leader_actor(&self, actor: &str) -> bool {
        !self.leader_for(actor).is_empty()
    }

    /// Return whether a type names an actor enum.
    pub(crate) fn is_actor_enum_type(&self, ty: &TypeRef) -> bool {
        ty.array.is_none() && self.actor_enums.contains_key(&ty.name)
    }

    /// Expand fixed actors and actor-enum domains to concrete actor names.
    pub(crate) fn expand_actor_refs(&self, refs: &[String]) -> Vec<String> {
        refs.iter()
            .flat_map(|actor| {
                self.actor_enums.get(actor).map_or_else(|| vec![actor.clone()], |actor_enum| actor_enum.variants.clone())
            })
            .collect()
    }

    /// Resolve a fixed selected-app or linked actor without changing routing.
    pub(crate) fn static_actor_target(&self, expression: &str) -> Option<StaticActorTarget<'_>> {
        let reference = expression.trim();
        self.app_actors
            .contains(reference)
            .then(|| self.actors_by_name.get(reference).copied())
            .flatten()
            .map(StaticActorTarget::InApp)
            .or_else(|| self.linked_actors.get(reference).map(StaticActorTarget::CrossApp))
    }

    /// Resolve a normalized singleton static target.
    pub(crate) fn resolve_static_actor_target(&self, target: &ActorTarget) -> Option<StaticActorTarget<'_>> {
        target.single_static_actor().and_then(|actor| self.static_actor_target(actor))
    }

    /// Collect shared local and imported actor-template uses for one entry.
    pub(crate) fn entry_template_uses(&self, actor: &ActorDecl, entry: &EntryDecl) -> Result<ActorTemplateUses> {
        let entry_model = self.entry_model(actor, entry)?;
        let mut uses = entry_model.actor_template_uses(&actor.name, &self.app_actors);

        let insert_linked = |target: &str, actors: &mut BTreeSet<String>| {
            if let Some(target @ StaticActorTarget::CrossApp(_)) = self.static_actor_target(target) {
                actors.insert(target.artifact_reference());
            }
        };

        // Current interactions are restricted to the selected app; only external
        // covenant groups can reference imported actors.
        for group in entry_model.existing_groups().chain(entry_model.genesis_groups()) {
            for interaction in group.inputs() {
                for target in interaction.target().static_actors() {
                    insert_linked(target, &mut uses.reads);
                }
            }
            for interaction in group.outputs() {
                for target in interaction.target().static_actors() {
                    insert_linked(target, &mut uses.writes);
                }
            }
        }
        Ok(uses)
    }
}

impl<'a> Model<'a> {
    /// Resolve an actor model in the selected application.
    pub(crate) fn actor_model(&self, actor: &str) -> Result<&ActorModel<'a>> {
        self.actor_models.get(actor).ok_or_else(|| ArgentError::new(format!("unknown app actor `{actor}`")))
    }

    /// Resolve the normalized model for one source entry.
    pub(crate) fn entry_model(&self, actor: &ActorDecl, entry: &EntryDecl) -> Result<&EntryModel<'a>> {
        self.actor_model(&actor.name)?
            .entry(&entry.name)
            .ok_or_else(|| ArgentError::new(format!("unknown entry model `{}::{}`", actor.name, entry.name)))
    }

    /// Return a linked actor by its source reference.
    pub(crate) fn linked_actor(&self, name: &str) -> Option<&LinkedActor> {
        self.linked_actors.get(name)
    }

    /// Return selector routes visible to one entry.
    pub(crate) fn template_selectors_for_entry(
        &self,
        actor: &ActorDecl,
        entry: &EntryDecl,
    ) -> Result<BTreeMap<String, TemplateSelector>> {
        Ok(self.entry_model(actor, entry)?.template_selectors().clone())
    }

    /// Expand one body-selected route to its concrete targets.
    pub(crate) fn route_targets(&self, actor: &ActorDecl, entry: &EntryDecl, route: &ResolvedRoute) -> Result<Vec<String>> {
        let ResolvedSuccessor::Constructed { actor: target, .. } = &route.successor else {
            return Ok(vec![actor.name.clone()]);
        };
        let selectors = self.template_selectors_for_entry(actor, entry)?;
        Ok(selectors.get(target).map_or_else(|| vec![target.clone()], TemplateSelector::route_actors))
    }
}

/// Operations that transform one actor's route cut into another's.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CompilerRouteTransition {
    pub(crate) families_to_open: Vec<String>,
    pub(crate) families_to_pack: Vec<String>,
}

/// Compiler route families, actor cuts, and transitions derived for one app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompilerRoutePlan {
    pub(crate) families: Vec<RouteFamily>,
    pub(crate) leaves_by_actor: BTreeMap<String, Vec<RouteRootLeaf>>,
    pub(crate) transitions: BTreeMap<(String, String), CompilerRouteTransition>,
}

/// Injection point between compiler route modeling and generic route planning.
pub(crate) type CompilerRoutePlanner =
    dyn Fn(&RouteGraph, &BTreeMap<String, Vec<String>>, &[SelectorRequirement]) -> Result<PlannerRoutePlan>;

pub(crate) fn default_route_planner(
    graph: &RouteGraph,
    domains: &BTreeMap<String, Vec<String>>,
    selectors: &[SelectorRequirement],
) -> Result<PlannerRoutePlan> {
    route_plan(graph, domains, selectors).map_err(|err| ArgentError::new(err.to_string()))
}

pub(crate) fn infer_direct_routes<'a>(
    actor_models: &BTreeMap<&'a str, ActorModel<'a>>,
    app_actors: &AppActors,
    route_planner: &CompilerRoutePlanner,
) -> Result<CompilerRoutePlan> {
    let mut graph = RouteGraph::default();
    let mut domains = BTreeMap::<String, Vec<String>>::new();
    let mut selector_requirements = Vec::new();
    let mut transition_pairs = BTreeSet::new();

    for actor_name in app_actors.iter() {
        let actor_model = actor_models.get(actor_name.as_str()).expect("selected app actor has a model");
        let actor = actor_model.source();
        // Route-isolated actors still need an empty cut in the final plan.
        graph.add_actor(actor.name.clone());
        domains.entry(actor.state.clone()).or_default().push(actor.name.clone());
        for entry_model in actor_model.entries() {
            // Selectors constrain the table shape independently of concrete
            // relations contributed by this entry's interaction groups.
            selector_requirements.extend(entry_model.template_selectors().values().map(|selector| SelectorRequirement {
                domain: selector.state.clone(),
                source: actor.name.clone(),
                variants: selector.variants.clone(),
            }));
            for group in entry_model.groups() {
                for interaction in group.inputs() {
                    for target in interaction.target().static_actors() {
                        // A single-actor covenant already authenticates its only
                        // possible template, so its self-input needs no route leaf.
                        if app_actors.contains(target) && !app_actors.is_singleton_actor_self_target(&actor.name, target) {
                            graph.add_consume(actor.name.clone(), target.to_string());
                        }
                    }
                }
                for interaction in group.outputs() {
                    for target_name in interaction.target().static_actors() {
                        if !app_actors.contains(target_name) {
                            continue;
                        }
                        // A self-output adds no dependency edge. Still plan its no-op
                        // cut transition so an actor-enum output may select the current actor.
                        if actor.name != target_name {
                            graph.add_emit(actor.name.clone(), target_name.to_string());
                        }
                        transition_pairs.insert((actor.name.clone(), target_name.to_string()));
                    }
                }
            }
        }
    }

    let plan = route_planner(&graph, &domains, &selector_requirements)?;
    let leaves_by_actor = compiler_route_leaves(&plan)?;
    let transitions = transition_pairs
        .into_iter()
        .map(|(source, target)| {
            let transition = compiler_route_transition(&plan, &source, &target)?;
            Ok(((source, target), transition))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let families = plan
        .families
        .into_iter()
        .map(|family| {
            let table_actors = family.table.iter().cloned().collect::<BTreeSet<_>>();
            let entry_actors = family.members.iter().filter(|actor| !table_actors.contains(*actor)).cloned().collect();
            RouteFamily {
                id: route_template_family_receipt_id(&family.domain, &family.rep),
                state: family.domain,
                actors: family.members,
                entry_actors,
                rep: family.rep,
                table_actors: family.table,
            }
        })
        .collect();

    Ok(CompilerRoutePlan { families, leaves_by_actor, transitions })
}

fn compiler_route_leaves(plan: &PlannerRoutePlan) -> Result<BTreeMap<String, Vec<RouteRootLeaf>>> {
    let mut leaves_by_actor = BTreeMap::new();
    for actor in plan.commitments.cuts.keys() {
        let nodes = plan.commitments.cut_nodes(actor).expect("an actor with a planned cut must resolve its cut nodes");
        let mut leaves = Vec::new();
        for node in nodes {
            leaves.push(compiler_route_leaf(plan, node)?);
        }
        leaves_by_actor.insert(actor.clone(), leaves);
    }
    Ok(leaves_by_actor)
}

fn compiler_route_transition(plan: &PlannerRoutePlan, source: &str, target: &str) -> Result<CompilerRouteTransition> {
    let transition = plan.commitments.cut_transition(source, target).map_err(|err| ArgentError::new(err.to_string()))?;
    let families_to_open =
        transition.branches_to_open.into_iter().map(|branch| compiler_route_family_id(plan, branch)).collect::<Result<Vec<_>>>()?;
    let families_to_pack =
        transition.branches_to_pack.into_iter().map(|branch| compiler_route_family_id(plan, branch)).collect::<Result<Vec<_>>>()?;
    Ok(CompilerRouteTransition { families_to_open, families_to_pack })
}

fn compiler_route_family_id(plan: &PlannerRoutePlan, branch: &CommitmentNode) -> Result<String> {
    let RouteRootLeaf::Family(id) = compiler_route_leaf(plan, branch)? else {
        return Err(ArgentError::new("commitment transition operation must reference a route family branch"));
    };
    Ok(id)
}

fn compiler_route_leaf(plan: &PlannerRoutePlan, node: &CommitmentNode) -> Result<RouteRootLeaf> {
    match node {
        CommitmentNode::Leaf { actor } => Ok(RouteRootLeaf::Actor(actor.clone())),
        CommitmentNode::Branch { children } => {
            let mut table = Vec::new();
            for child in children {
                let CommitmentNode::Leaf { actor } = child else {
                    return Err(ArgentError::new("nested commitment families cannot be lowered by the compiler"));
                };
                table.push(actor.clone());
            }
            let family = plan
                .families
                .iter()
                .find(|family| family.table == table)
                .ok_or_else(|| ArgentError::new(format!("commitment branch {:?} has no matching route family", table)))?;
            Ok(RouteRootLeaf::Family(route_template_family_receipt_id(&family.domain, &family.rep)))
        }
    }
}

fn route_template_family_receipt_id(state: &str, rep_actor: &str) -> String {
    format!("route_family/{state}/{}", to_snake(rep_actor))
}

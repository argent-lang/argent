//! Source-backed compiler models shared by validation, planning, and code generation.

use std::collections::{BTreeMap, BTreeSet};

use crate::artifact::{AppDependencyArtifact, EntryRefArtifact};
use crate::ast::{ActorDecl, ConstDecl, ConsumeDecl, FunctionDecl, StateDecl};
use crate::error::{ArgentError, Result};
use crate::link::LinkedActor;
use crate::naming::to_snake;
use crate::routing::{CommitmentNode, RouteGraph, RoutePlan as PlannerRoutePlan, SelectorRequirement, route_plan};

mod actor;
mod entry;

pub(crate) use actor::ActorModel;
pub(crate) use entry::{
    ActorTemplateUses, CovenantGroup, EntryModel, InteractionSource, TemplateSelector, actor_enum_variant_const_expr,
    parse_actor_enum_selector, parse_actor_enum_variant,
};

/// The selected application's compiler-wide source and routing model.
#[derive(Debug)]
pub(crate) struct Model<'a> {
    pub(crate) app_name: String,
    /// Direct artifacts used to link the selected app.
    pub(crate) app_dependencies: Vec<AppDependencyArtifact>,
    pub(crate) app_actors: Vec<String>,
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
    app_actors: &[String],
    route_planner: &CompilerRoutePlanner,
) -> Result<CompilerRoutePlan> {
    let app_actor_set = app_actors.iter().cloned().collect::<BTreeSet<_>>();
    let mut graph = RouteGraph::default();
    let mut domains = BTreeMap::<String, Vec<String>>::new();
    let mut selector_requirements = Vec::new();
    let mut transition_pairs = BTreeSet::new();

    for actor_name in app_actors {
        let actor_model = actor_models.get(actor_name.as_str()).expect("selected app actor has a model");
        let actor = actor_model.source();
        if app_actor_set.contains(&actor.name) {
            graph.add_actor(actor.name.clone());
            domains.entry(actor.state.clone()).or_default().push(actor.name.clone());
        }
        for entry_model in actor_model.entries() {
            selector_requirements.extend(entry_model.template_selectors().values().map(|selector| SelectorRequirement {
                domain: selector.state.clone(),
                source: actor.name.clone(),
                variants: selector.variants.clone(),
            }));
            for group in std::iter::once(entry_model.current()).chain(entry_model.existing_groups()) {
                for interaction in group.inputs() {
                    for target in interaction.target().concrete_actors() {
                        if app_actor_set.contains(target) && !is_single_actor_self_target(app_actors, actor, target) {
                            graph.add_consume(actor.name.clone(), target.to_string());
                        }
                    }
                }
                for interaction in group.outputs() {
                    for target_name in interaction.target().concrete_actors() {
                        if !app_actor_set.contains(target_name) {
                            continue;
                        }
                        if actor.name != target_name {
                            graph.add_emit(actor.name.clone(), target_name.to_string());
                        }
                        transition_pairs.insert((actor.name.clone(), target_name.to_string()));
                    }
                }
            }

            for group in entry_model.genesis_groups() {
                for interaction in group.outputs() {
                    for target in interaction.target().concrete_actors() {
                        if !app_actor_set.contains(target) {
                            continue;
                        }
                        graph.add_emit(actor.name.clone(), target.to_string());
                        if actor.name != target {
                            transition_pairs.insert((actor.name.clone(), target.to_string()));
                        }
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

pub(crate) fn is_single_actor_self_consume(app_actors: &[String], actor: &ActorDecl, consume: &ConsumeDecl) -> bool {
    is_single_actor_self_target(app_actors, actor, &consume.actor)
}

pub(crate) fn is_single_actor_self_target(app_actors: &[String], actor: &ActorDecl, target: &str) -> bool {
    app_actors.len() == 1 && app_actors[0] == actor.name && target == actor.name
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiler_route_leaves_preserve_packed_and_opened_commitment_nodes() {
        let mut graph = RouteGraph::default();
        graph.add_actor("Knight");
        graph.add_emit("Player", "Mux");
        graph.add_emit("Mux", "Knight");
        graph.add_emit("Mux", "Pawn");
        graph.add_emit("Pawn", "Mux");
        graph.add_emit("Mux", "Settle");
        let domains = BTreeMap::from([
            ("BoardState".to_string(), ["Knight", "Mux", "Pawn"].into_iter().map(str::to_string).collect()),
            ("PlayerState".to_string(), vec!["Player".to_string()]),
            ("SettleState".to_string(), vec!["Settle".to_string()]),
        ]);

        let plan = route_plan(&graph, &domains, &[]).expect("route plan is valid");
        let leaves = compiler_route_leaves(&plan).expect("commitment nodes lower to compiler leaves");

        assert_eq!(
            leaves["Player"],
            [
                RouteRootLeaf::Family("route_family/BoardState/mux".to_string()),
                RouteRootLeaf::Actor("Mux".to_string()),
                RouteRootLeaf::Actor("Settle".to_string()),
            ]
        );
        assert_eq!(
            leaves["Mux"],
            [
                RouteRootLeaf::Actor("Knight".to_string()),
                RouteRootLeaf::Actor("Pawn".to_string()),
                RouteRootLeaf::Actor("Mux".to_string()),
                RouteRootLeaf::Actor("Settle".to_string()),
            ]
        );
        assert!(leaves["Settle"].is_empty());

        let family_id = "route_family/BoardState/mux".to_string();
        assert_eq!(
            compiler_route_transition(&plan, "Player", "Mux").expect("Player can open the Mux family"),
            CompilerRouteTransition { families_to_open: vec![family_id.clone()], families_to_pack: Vec::new() }
        );
        assert_eq!(
            compiler_route_transition(&plan, "Mux", "Player").expect("Mux can pack its family for Player"),
            CompilerRouteTransition { families_to_open: Vec::new(), families_to_pack: vec![family_id] }
        );
    }
}

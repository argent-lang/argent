//! Builds the selected application's compiler model.

use std::collections::{BTreeMap, BTreeSet};

use super::ModelSource;
use crate::artifact::{AppDependencyArtifact, EntryRefArtifact};
use crate::compiler::loader::ResolvedDeclaration;
use crate::compiler::syntax::*;
use crate::error::{ArgentError, Result};

use super::link::{LinkedContext, LinkedDependency};
use super::{
    ActorEnumInfo, ActorModel, AppActors, CompilerRoutePlan, CompilerRoutePlanner, ConstResolver, Model,
    build_contract_state_lowerings, default_route_planner, infer_direct_routes,
};

fn compute_leader_for(actors: &[&ActorDecl]) -> BTreeMap<String, Vec<EntryRefArtifact>> {
    let mut leader_for = BTreeMap::<String, Vec<EntryRefArtifact>>::new();
    for actor in actors {
        for entry in &actor.entries {
            if entry.kind != EntryKind::Delegate {
                continue;
            }
            let Some(leader) = entry.consumes.first() else {
                continue;
            };
            leader_for
                .entry(leader.actor.clone())
                .or_default()
                .push(EntryRefArtifact { actor: actor.name.clone(), entry: entry.name.clone() });
        }
    }
    leader_for
}

impl<'a> Model<'a> {
    pub(crate) fn from_source(program: &'a ModelSource<'_>) -> Result<Self> {
        Self::from_source_with_route_planner(program, &BTreeMap::new(), &default_route_planner)
    }

    pub(crate) fn from_source_linked(
        program: &'a ModelSource<'_>,
        dependencies: &BTreeMap<String, LinkedDependency<'_>>,
    ) -> Result<Self> {
        Self::from_source_with_route_planner(program, dependencies, &default_route_planner)
    }

    pub(crate) fn from_source_with_route_planner(
        program: &'a ModelSource<'_>,
        dependencies: &BTreeMap<String, LinkedDependency<'_>>,
        route_planner: &CompilerRoutePlanner,
    ) -> Result<Self> {
        // collect all program declarations
        let mut consts = Vec::new();
        let mut functions = Vec::new();
        let mut states = BTreeMap::new();
        let mut all_actors = BTreeMap::new();
        let mut actor_enum_decls = BTreeMap::new();
        for declaration in program.declarations() {
            match declaration {
                ResolvedDeclaration::Const(declaration) => consts.push(declaration),
                ResolvedDeclaration::State(declaration) => {
                    states.insert(declaration.name.clone(), declaration);
                }
                ResolvedDeclaration::Function(declaration) => functions.push(declaration),
                ResolvedDeclaration::Actor(declaration) => {
                    all_actors.insert(declaration.name.clone(), declaration);
                }
                ResolvedDeclaration::ActorEnum(declaration) => {
                    actor_enum_decls.insert(declaration.name.clone(), declaration);
                }
                ResolvedDeclaration::App(_) => {}
            }
        }

        let app_name = program.app_name.clone();
        let app_actors = program.actors.clone();
        let const_resolver = ConstResolver::new(&consts);
        let app_actors = AppActors::new(app_actors);

        // actors filtered by the selected app
        let mut actors = Vec::new();
        for name in app_actors.iter() {
            let actor =
                all_actors.get(name).copied().ok_or_else(|| ArgentError::new(format!("app references unknown actor `{name}`")))?;
            if !states.contains_key(&actor.state) {
                return Err(ArgentError::new(format!("actor `{}` owns unknown state `{}`", actor.name, actor.state)));
            }
            actors.push(actor);
        }

        let LinkedContext {
            states: linked_states,
            actor_decls: linked_actor_decls,
            actors: linked_actors,
            actor_enums: linked_actor_enums,
            origins: declaration_origins,
        } = LinkedContext::new(dependencies, program.declaration_origins(), &program.unbound_names, &states, &all_actors)?;
        let mut actor_enums = build_actor_enums(&actor_enum_decls, &all_actors, &states, &app_actors)?;
        for (name, linked) in linked_actor_enums {
            let linked = ActorEnumInfo { name: linked.name, state: linked.state, variants: linked.variants };
            if let Some(local) = actor_enums.insert(name.clone(), linked.clone())
                && local != linked
            {
                return Err(ArgentError::new(format!("imported actor enum `{name}` conflicts with a local actor enum")));
            }
        }
        let actor_models = build_actor_models(&actors, &actor_enums, &const_resolver)?;
        let CompilerRoutePlan { families: route_families, leaves_by_actor: route_leaves_by_actor, transitions: route_transitions } =
            infer_direct_routes(&actor_models, &app_actors, route_planner)?;
        let leader_for = compute_leader_for(&actors);
        let mut model = Self {
            app_name,
            declaration_origins,
            app_dependencies: dependencies
                .iter()
                .map(|(app, linked_dependency)| AppDependencyArtifact {
                    app: app.clone(),
                    artifact_id: linked_dependency.artifact.id.clone(),
                })
                .collect(),
            app_actors,
            route_families,
            consts,
            functions,
            states,
            linked_states,
            actors_by_name: all_actors,
            linked_actor_decls,
            linked_actors,
            actor_enums,
            actors,
            actor_models,
            leader_for,
            route_leaves_by_actor,
            route_transitions,
            state_lowering_by_actor: BTreeMap::new(),
        };
        model.validate()?;
        model.state_lowering_by_actor = build_contract_state_lowerings(&model)?;
        Ok(model)
    }
}

fn build_actor_enums(
    actor_enum_decls: &BTreeMap<String, &ActorEnumDecl>,
    actors_by_name: &BTreeMap<String, &ActorDecl>,
    states: &BTreeMap<String, &StateDecl>,
    app_actors: &AppActors,
) -> Result<BTreeMap<String, ActorEnumInfo>> {
    let mut out = BTreeMap::new();
    for actor_enum in actor_enum_decls.values() {
        if !actor_enum.variants.iter().any(|variant| app_actors.contains(variant)) {
            continue;
        }
        if actors_by_name.contains_key(&actor_enum.name) || states.contains_key(&actor_enum.name) {
            return Err(ArgentError::new(format!("actor enum `{}` conflicts with an actor or state declaration", actor_enum.name)));
        }
        if actor_enum.variants.len() < 2 {
            return Err(ArgentError::new(format!("actor enum `{}` must contain at least two variants", actor_enum.name)));
        }
        let mut seen = BTreeSet::new();
        let mut state = None::<String>;
        for variant in &actor_enum.variants {
            if !seen.insert(variant.as_str()) {
                return Err(ArgentError::new(format!("actor enum `{}` repeats variant `{variant}`", actor_enum.name)));
            }
            if !app_actors.contains(variant) {
                return Err(ArgentError::new(format!(
                    "actor enum `{}` references actor `{variant}` outside the app",
                    actor_enum.name
                )));
            }
            let actor = actors_by_name
                .get(variant)
                .copied()
                .ok_or_else(|| ArgentError::new(format!("actor enum `{}` references unknown actor `{variant}`", actor_enum.name)))?;
            if let Some(expected) = &state {
                if expected != &actor.state {
                    return Err(ArgentError::new(format!(
                        "actor enum `{}` variant `{variant}` owns state `{}`, expected `{expected}`",
                        actor_enum.name, actor.state
                    )));
                }
            } else {
                state = Some(actor.state.clone());
            }
        }
        out.insert(
            actor_enum.name.clone(),
            ActorEnumInfo {
                name: actor_enum.name.clone(),
                state: state.expect("non-empty actor enum has a state"),
                variants: actor_enum.variants.clone(),
            },
        );
    }
    Ok(out)
}

fn build_actor_models<'a>(
    actors: &[&'a ActorDecl],
    actor_enums: &BTreeMap<String, ActorEnumInfo>,
    const_resolver: &ConstResolver<'_>,
) -> Result<BTreeMap<&'a str, ActorModel<'a>>> {
    actors.iter().map(|actor| Ok((actor.name.as_str(), ActorModel::build(actor, actor_enums, const_resolver)?))).collect()
}

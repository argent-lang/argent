//! Builds the selected application's compiler model.

use std::collections::{BTreeMap, BTreeSet};

use crate::artifact::{AppDependencyArtifact, EntryRefArtifact};
use crate::compiler::loader::{ResolvedDeclaration, ResolvedModules};
use crate::compiler::syntax::node::{DeclId, RootSlot, SymbolKind};
use crate::compiler::syntax::*;
use crate::error::{ArgentError, Result};

use super::link::{LinkedContext, LinkedDependency};
use super::{
    ActorEnumInfo, ActorModel, ActorOutputPlan, ActorValuePlan, AppActors, AppCompilationContext, CompilerRoutePlan,
    CompilerRoutePlanner, ConstResolver, EntryInputPlan, EntryOutputPlan, StateExpansionWitnessSpec, StaticActorId, TypeTable,
    WitnessPlan, build_contract_state_lowerings, infer_direct_routes,
};

fn compute_leader_for(
    actor_ids: &[DeclId],
    actor_models: &BTreeMap<DeclId, ActorModel<'_>>,
) -> BTreeMap<DeclId, Vec<EntryRefArtifact>> {
    let mut leader_for = BTreeMap::<DeclId, Vec<EntryRefArtifact>>::new();
    for id in actor_ids {
        let actor_model = &actor_models[id];
        let actor = actor_model.source();
        for entry_model in actor_model.entries() {
            let entry = entry_model.source();
            if entry.kind != EntryKind::Delegate {
                continue;
            }
            let Some(StaticActorId::InApp(leader)) =
                entry_model.current().inputs().first().and_then(|input| input.target().single_static_actor())
            else {
                continue;
            };
            leader_for.entry(*leader).or_default().push(EntryRefArtifact { actor: actor.name.clone(), entry: entry.name.clone() });
        }
    }
    leader_for
}

impl<'a> AppCompilationContext<'a> {
    /// Construct the completed selected-app model from resolved source identities.
    pub(crate) fn from_resolved(
        resolution: &'a ResolvedModules<'a>,
        app_name: Option<&str>,
        dependencies: &BTreeMap<String, LinkedDependency<'_>>,
        route_planner: &CompilerRoutePlanner,
    ) -> Result<Self> {
        let selected_app = resolution.root_app(app_name)?;
        let actor_ids = if let Some(app) = selected_app {
            resolution.app_actor_ids(app)?
        } else {
            resolution.root_declarations().filter(|id| id.kind() == SymbolKind::Actor).collect()
        };
        let declaration_names = resolution.selected_declaration_names(selected_app, &actor_ids)?;
        resolution.validate_actor_captures(&actor_ids)?;
        resolution.validate_authored_state_operations(&declaration_names, &actor_ids)?;
        let types = TypeTable::new(resolution, &declaration_names)?;
        let mut consts = Vec::new();
        let mut functions = Vec::new();
        let mut states = BTreeMap::new();
        let mut local_state_ids = BTreeMap::new();
        let mut state_decl_ids_by_source = BTreeMap::new();
        let mut all_actors = BTreeMap::new();
        let mut actors_by_id = BTreeMap::new();
        let mut actor_enum_decls = BTreeMap::new();
        for id in declaration_names.keys().copied() {
            let declaration = resolution.declaration(id);
            match declaration {
                ResolvedDeclaration::Const(declaration) => consts.push((id, declaration)),
                ResolvedDeclaration::State(declaration) => {
                    let origin = super::link::DeclarationOrigin::Source {
                        path: resolution.declaration_path(id).to_path_buf(),
                        kind: SymbolKind::State,
                        index: id.index,
                    };
                    state_decl_ids_by_source.insert(super::SourceStateId::from_origin(&declaration_names[&id], origin), id);
                    states.insert(declaration_names[&id].clone(), declaration);
                    local_state_ids.insert(declaration_names[&id].clone(), id);
                }
                ResolvedDeclaration::Function(declaration) => functions.push((id, declaration)),
                ResolvedDeclaration::Actor(declaration) => {
                    all_actors.insert(declaration_names[&id].clone(), declaration);
                    actors_by_id.insert(id, declaration);
                }
                ResolvedDeclaration::ActorEnum(declaration) => {
                    actor_enum_decls.insert(id, declaration);
                }
                ResolvedDeclaration::App(_) => {}
            }
        }

        let app_name = selected_app.map(|id| resolution.declaration(id).name().to_string()).unwrap_or_else(|| "ArgentApp".to_string());
        let app_actors = actor_ids.iter().map(|id| (*id, declaration_names[id].clone())).collect();
        let const_resolver = ConstResolver::from_resolved(resolution, &declaration_names);
        let app_actors = AppActors::new(app_actors);

        // actors filtered by the selected app
        for id in &actor_ids {
            let name = &types.display_names[id];
            let actor =
                actors_by_id.get(id).copied().ok_or_else(|| ArgentError::new(format!("app references unknown actor `{name}`")))?;
            let state = types
                .actor_states
                .get(id)
                .ok_or_else(|| ArgentError::new(format!("actor `{name}` owns unknown state `{}`", actor.state)))?;
            if !states.contains_key(&types.display_names[state]) {
                return Err(ArgentError::new(format!("actor `{}` owns unknown state `{}`", actor.name, actor.state)));
            }
        }

        let reserved_names = declaration_names.keys().flat_map(|id| resolution.reserved_local_names(*id)).collect();
        let LinkedContext {
            states: linked_states,
            state_field_sources: linked_field_sources,
            actors: linked_actors,
            actor_names: linked_actor_names,
            actor_enums: linked_actor_enums,
            origins: declaration_origins,
        } = LinkedContext::new(dependencies, &reserved_names, &states, &local_state_ids, &all_actors, resolution, &declaration_names)?;
        let mut state_names_by_source = BTreeMap::new();
        for name in states.keys().chain(linked_states.keys()) {
            let origin = declaration_origins
                .get(name)
                .ok_or_else(|| ArgentError::new(format!("missing declaration identity for state `{name}`")))?;
            state_names_by_source.entry(super::SourceStateId::from_origin(name, origin.clone())).or_insert_with(|| name.clone());
        }
        let mut actor_enums = build_actor_enums(&actor_enum_decls, &all_actors, &states, &app_actors, &types)?;
        for (name, linked) in linked_actor_enums {
            let linked = ActorEnumInfo { name: linked.name, state: linked.state, variants: linked.variants };
            if let Some(local) = actor_enums.insert(name.clone(), linked.clone())
                && local != linked
            {
                return Err(ArgentError::new(format!("imported actor enum `{name}` conflicts with a local actor enum")));
            }
        }
        let mut static_actor_ids =
            linked_actor_names.iter().map(|(name, id)| (name.clone(), StaticActorId::Linked(id.clone()))).collect::<BTreeMap<_, _>>();
        for id in &actor_ids {
            static_actor_ids.insert(types.display_names[id].clone(), StaticActorId::InApp(*id));
        }
        let actor_models = build_actor_models(&actor_ids, &actors_by_id, &const_resolver, &types, resolution, &static_actor_ids)?;
        let CompilerRoutePlan { families: route_families, leaves_by_actor: route_leaves_by_actor, transitions: route_transitions } =
            infer_direct_routes(&actor_models, &app_actors, &types, route_planner)?;
        let leader_for = compute_leader_for(&actor_ids, &actor_models);
        let mut model = Self {
            resolution,
            app_name,
            types,
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
            state_names_by_source,
            state_decl_ids_by_source,
            storage_source_by_source: BTreeMap::new(),
            linked_field_sources,
            actors_by_name: all_actors,
            linked_actors,
            linked_actor_names,
            actor_enums,
            actor_models,
            actor_value_plans: BTreeMap::new(),
            input_plans: BTreeMap::new(),
            output_plans: BTreeMap::new(),
            entry_output_plans: BTreeMap::new(),
            witness_plans: BTreeMap::new(),
            state_expansion_witnesses_by_actor: BTreeMap::new(),
            leader_for,
            route_leaves_by_actor,
            route_transitions,
            state_lowering_by_actor: BTreeMap::new(),
        };
        let mut expansion_sources = Vec::new();
        for (source, owner) in &model.state_decl_ids_by_source {
            if model.state_by_source(source)?.expansion.is_some() {
                let storage = model.source_state_id_by_decl(model.bound_state_use(*owner, RootSlot::StateBase)?)?;
                expansion_sources.push((source.clone(), storage));
            }
        }
        for state in model.linked_states.values() {
            if let Some(expansion) = &state.expansion {
                expansion_sources.push((model.source_state_id(&state.name)?, model.source_state_id(&expansion.base)?));
            }
        }
        for (source_id, storage_id) in expansion_sources {
            let source = source_id.as_str().to_string();
            if let Some(previous) = model.storage_source_by_source.insert(source_id, storage_id.clone())
                && previous != storage_id
            {
                return Err(ArgentError::new(format!("state `{source}` has conflicting storage source identities")));
            }
        }
        model.validate_pre_layout()?;
        model.state_lowering_by_actor = build_contract_state_lowerings(&model)?;
        model.actor_value_plans = model
            .app_actors
            .iter_with_ids()
            .map(|(id, _)| Ok((id, ActorValuePlan::new(id, model.actor_by_decl(id)?, &model)?)))
            .collect::<Result<_>>()?;
        model.types.body_values = model.types.plan_body_values(&model, resolution)?;
        let (co_spent_sites, digest_operands, actor_field_uses) = model.types.plan_expression_sites(&model)?;
        model.types.co_spent_sites = co_spent_sites;
        model.types.digest_operands = digest_operands;
        model.types.actor_field_uses = actor_field_uses;
        model.output_plans = model
            .app_actors
            .iter_with_ids()
            .map(|(id, _)| Ok((id, ActorOutputPlan::new(id, model.actor_by_decl(id)?, &model)?)))
            .collect::<Result<_>>()?;
        model.input_plans = model
            .entries_in_app_order()
            .map(|(actor, entry)| Ok((entry.id, EntryInputPlan::new(entry.id, actor, entry.source(), &model)?)))
            .collect::<Result<_>>()?;
        model.entry_output_plans = model
            .entries_in_app_order()
            .map(|(actor, entry)| Ok((entry.id, EntryOutputPlan::new(entry.id, actor, entry.source(), &model)?)))
            .collect::<Result<_>>()?;
        model.state_expansion_witnesses_by_actor = model
            .app_actors
            .iter_with_ids()
            .map(|(id, _)| Ok((id, StateExpansionWitnessSpec::for_actor(id, &model)?)))
            .collect::<Result<_>>()?;
        model.witness_plans = model
            .entries_in_app_order()
            .map(|(actor, entry)| Ok((entry.id, WitnessPlan::new(entry.id, actor, entry.source(), &model)?)))
            .collect::<Result<_>>()?;
        model.validate_complete()?;
        Ok(model)
    }
}

fn build_actor_enums(
    actor_enum_decls: &BTreeMap<DeclId, &ActorEnumDecl>,
    actors_by_name: &BTreeMap<String, &ActorDecl>,
    states: &BTreeMap<String, &StateDecl>,
    app_actors: &AppActors,
    types: &TypeTable,
) -> Result<BTreeMap<String, ActorEnumInfo>> {
    let mut out = BTreeMap::new();
    for (enum_id, actor_enum) in actor_enum_decls {
        let variants = types
            .enum_variants
            .get(enum_id)
            .ok_or_else(|| ArgentError::new(format!("actor enum `{}` has no bound variants", actor_enum.name)))?;
        if !variants.iter().any(|variant| app_actors.name(*variant).is_some()) {
            continue;
        }
        if actors_by_name.contains_key(&actor_enum.name) || states.contains_key(&actor_enum.name) {
            return Err(ArgentError::new(format!("actor enum `{}` conflicts with an actor or state declaration", actor_enum.name)));
        }
        if actor_enum.variants.len() < 2 {
            return Err(ArgentError::new(format!("actor enum `{}` must contain at least two variants", actor_enum.name)));
        }
        if variants.len() != actor_enum.variants.len() {
            return Err(ArgentError::new(format!("actor enum `{}` has incomplete bound variants", actor_enum.name)));
        }
        let mut seen = BTreeSet::new();
        let mut state = None::<DeclId>;
        for (variant, variant_id) in actor_enum.variants.iter().zip(variants) {
            if !seen.insert(*variant_id) {
                return Err(ArgentError::new(format!("actor enum `{}` repeats variant `{variant}`", actor_enum.name)));
            }
            if app_actors.name(*variant_id).is_none() {
                return Err(ArgentError::new(format!(
                    "actor enum `{}` references actor `{variant}` outside the app",
                    actor_enum.name
                )));
            }
            let actor_state = types
                .actor_states
                .get(variant_id)
                .ok_or_else(|| ArgentError::new(format!("actor enum `{}` references unknown actor `{variant}`", actor_enum.name)))?;
            if let Some(expected) = state {
                if expected != *actor_state {
                    return Err(ArgentError::new(format!(
                        "actor enum `{}` variant `{variant}` owns state `{}`, expected `{}`",
                        actor_enum.name, types.display_names[actor_state], types.display_names[&expected]
                    )));
                }
            } else {
                state = Some(*actor_state);
            }
        }
        out.insert(
            actor_enum.name.clone(),
            ActorEnumInfo {
                name: actor_enum.name.clone(),
                state: types.display_names[&state.expect("non-empty actor enum has a state")].clone(),
                variants: actor_enum.variants.clone(),
            },
        );
    }
    Ok(out)
}

fn build_actor_models<'a>(
    selected_actor_ids: &[DeclId],
    actors_by_id: &BTreeMap<DeclId, &'a ActorDecl>,
    const_resolver: &ConstResolver,
    types: &TypeTable,
    resolution: &ResolvedModules<'_>,
    actor_ids: &BTreeMap<String, StaticActorId>,
) -> Result<BTreeMap<DeclId, ActorModel<'a>>> {
    selected_actor_ids
        .iter()
        .map(|id| {
            let actor = actors_by_id.get(id).copied().ok_or_else(|| ArgentError::new("selected actor has no declaration"))?;
            let state = types.actor_states[id];
            let mut model = ActorModel::build(*id, state, actor, const_resolver)?;
            model.attach_bound_routes(types, resolution, actor_ids)?;
            Ok((*id, model))
        })
        .collect()
}

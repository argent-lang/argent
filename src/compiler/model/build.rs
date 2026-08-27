//! Builds the selected application's compiler model.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use crate::artifact::{AppDependencyArtifact, Artifact, EntryRefArtifact};
use crate::compiler::syntax::word;
use crate::compiler::syntax::*;
use crate::error::{ArgentError, Result};

use super::link::{LinkedContext, link_imported_actors};
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
    pub(crate) fn from_program(program: &'a Program) -> Result<Self> {
        Self::from_program_selected(program, None, &BTreeMap::new())
    }

    #[cfg(test)]
    pub(crate) fn from_program_app(program: &'a Program, app_name: &str) -> Result<Self> {
        Self::from_program_selected(program, Some(app_name), &BTreeMap::new())
    }

    pub(crate) fn from_program_app_linked(
        program: &'a Program,
        app_name: &str,
        dependencies: &BTreeMap<String, &Artifact>,
    ) -> Result<Self> {
        Self::from_program_selected(program, Some(app_name), dependencies)
    }

    fn from_program_selected(
        program: &'a Program,
        app_name: Option<&str>,
        dependencies: &BTreeMap<String, &Artifact>,
    ) -> Result<Self> {
        Self::from_program_selected_with_route_planner(program, app_name, dependencies, &default_route_planner)
    }

    #[cfg(test)]
    pub(crate) fn from_program_with_route_planner(program: &'a Program, route_planner: &CompilerRoutePlanner) -> Result<Self> {
        Self::from_program_selected_with_route_planner(program, None, &BTreeMap::new(), route_planner)
    }

    fn from_program_selected_with_route_planner(
        program: &'a Program,
        app_name: Option<&str>,
        dependencies: &BTreeMap<String, &Artifact>,
        route_planner: &CompilerRoutePlanner,
    ) -> Result<Self> {
        validate_unique_apps(program)?;
        let consts = collect_consts(program)?;
        let const_resolver = ConstResolver::new(&consts);
        let functions = collect_functions(program)?;
        let states = collect_states(program)?;
        let all_actors = collect_actors(program)?;
        let actor_enum_decls = collect_actor_enums(program)?;

        let app = select_root_app(program, app_name)?;
        // app_actors define the selected app's actor domain.
        let (app_name, app_actors) = if let Some(app) = app {
            (app.name.clone(), app.actors.clone())
        } else {
            ("ArgentApp".to_string(), all_actors.keys().cloned().collect())
        };
        let app_actors = AppActors::new(app_actors);
        validate_direct_actor_imports(program, &app_name, &app_actors)?;

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
        } = link_imported_actors(program, dependencies, &states, &all_actors)?;
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
            app_dependencies: dependencies
                .iter()
                .map(|(app, artifact)| AppDependencyArtifact { app: app.clone(), artifact_id: artifact.id.clone() })
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

fn collect_consts(program: &Program) -> Result<Vec<&ConstDecl>> {
    let mut seen = BTreeMap::new();
    let mut consts = Vec::new();
    for module in &program.modules {
        for ct in &module.consts {
            reject_duplicate_top_level(word::CONST, &ct.name, &module.path, &mut seen)?;
            consts.push(ct);
        }
    }
    Ok(consts)
}

fn collect_functions(program: &Program) -> Result<Vec<&FunctionDecl>> {
    let mut seen = BTreeMap::new();
    let mut functions = Vec::new();
    for module in &program.modules {
        for function in &module.functions {
            reject_duplicate_top_level(word::FN, &function.name, &module.path, &mut seen)?;
            functions.push(function);
        }
    }
    Ok(functions)
}

fn collect_states(program: &Program) -> Result<BTreeMap<String, &StateDecl>> {
    let mut seen = BTreeMap::new();
    let mut states = BTreeMap::new();
    for module in &program.modules {
        for state in &module.states {
            reject_duplicate_top_level(word::STATE, &state.name, &module.path, &mut seen)?;
            states.insert(state.name.clone(), state);
        }
    }
    Ok(states)
}

fn collect_actors(program: &Program) -> Result<BTreeMap<String, &ActorDecl>> {
    let mut seen = BTreeMap::new();
    let mut actors = BTreeMap::new();
    for module in &program.modules {
        for actor in &module.actors {
            reject_duplicate_top_level(word::ACTOR, &actor.name, &module.path, &mut seen)?;
            actors.insert(actor.name.clone(), actor);
        }
    }
    Ok(actors)
}

fn collect_actor_enums(program: &Program) -> Result<BTreeMap<String, &ActorEnumDecl>> {
    let mut seen = BTreeMap::new();
    let mut actor_enums = BTreeMap::new();
    for module in &program.modules {
        for actor_enum in &module.actor_enums {
            reject_duplicate_top_level("actor enum", &actor_enum.name, &module.path, &mut seen)?;
            actor_enums.insert(actor_enum.name.clone(), actor_enum);
        }
    }
    Ok(actor_enums)
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

fn validate_unique_apps(program: &Program) -> Result<()> {
    let mut seen = BTreeMap::new();
    for module in &program.modules {
        for app in &module.apps {
            reject_duplicate_top_level(word::APP, &app.name, &module.path, &mut seen)?;
        }
    }
    Ok(())
}

fn select_root_app<'a>(program: &'a Program, app_name: Option<&str>) -> Result<Option<&'a AppDecl>> {
    let root = program
        .modules
        .iter()
        .find(|module| module.path == program.root)
        .ok_or_else(|| ArgentError::at(&program.root, "root module is missing from the loaded program"))?;

    if let Some(app_name) = app_name {
        return root
            .apps
            .iter()
            .find(|app| app.name == app_name)
            .map(Some)
            .ok_or_else(|| ArgentError::at(&program.root, format!("root module has no app named `{app_name}`")));
    }

    match root.apps.as_slice() {
        [] => Ok(None),
        [app] => Ok(Some(app)),
        apps => Err(ArgentError::at(
            &program.root,
            format!(
                "root module declares multiple apps ({}); select one with `--app <name>`",
                apps.iter().map(|app| app.name.as_str()).collect::<Vec<_>>().join(", ")
            ),
        )),
    }
}

fn validate_direct_actor_imports(program: &Program, app_name: &str, app_actors: &AppActors) -> Result<()> {
    for module in &program.modules {
        let base = module.path.parent().ok_or_else(|| ArgentError::at(&module.path, "module path has no parent"))?;
        for import in &module.imports {
            let Import::Actor { actor, path } = import else {
                continue;
            };
            let source = fs::canonicalize(base.join(path)).map_err(|err| ArgentError::at(&module.path, err.to_string()))?;
            let imported = program
                .modules
                .iter()
                .find(|candidate| candidate.path == source)
                .ok_or_else(|| ArgentError::at(&module.path, format!("direct actor import source `{path}` was not loaded")))?;
            if !imported.actors.iter().any(|candidate| candidate.name == *actor) {
                return Err(ArgentError::at(
                    &module.path,
                    format!("direct actor import `{actor}` does not name an actor declared by `{path}`"),
                ));
            }
            if app_actors.contains(actor) {
                continue;
            }

            let defining_apps =
                imported.apps.iter().filter(|app| app.actors.contains(actor)).map(|app| app.name.as_str()).collect::<Vec<_>>();
            let suggestion = match defining_apps.as_slice() {
                [defining_app] => format!("; use `import actor {defining_app}::{actor} from \"{path}\";`"),
                _ => "; add it to the selected app or import it through its defining app".to_string(),
            };
            return Err(ArgentError::at(
                &module.path,
                format!("direct actor import `{actor}` is not part of selected app `{app_name}`{suggestion}"),
            ));
        }
    }
    Ok(())
}

fn reject_duplicate_top_level<'a>(kind: &str, name: &str, path: &'a Path, seen: &mut BTreeMap<String, &'a Path>) -> Result<()> {
    if let Some(first_path) = seen.insert(name.to_string(), path) {
        return Err(ArgentError::new(format!(
            "duplicate top-level {kind} `{name}` in `{}`; first declared in `{}`",
            path.display(),
            first_path.display()
        )));
    }
    Ok(())
}

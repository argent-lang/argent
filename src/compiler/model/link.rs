//! Links imported app artifacts into the selected application's model.
//!
//! Linked interfaces, templates, states, and actor enums become model inputs.

use std::collections::{BTreeMap, BTreeSet};

use crate::artifact::*;
use crate::compiler::loader::SymbolKind;
use crate::compiler::syntax::*;
use crate::error::{ArgentError, Result};

#[derive(Debug, Clone)]
pub(crate) struct LinkedActor {
    pub app: String,
    pub actor: String,
    pub state: String,
    pub interface: ActorInterfaceArtifact,
    pub template: ActorTemplateArtifact,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LinkedActorEnum {
    pub name: String,
    pub state: String,
    pub variants: Vec<String>,
}

/// identity carried privately between builds
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum DeclarationOrigin {
    Source { path: std::path::PathBuf, kind: SymbolKind, index: usize },
    Dependency { app: String, name: String, kind: SymbolKind },
}

pub(crate) struct LinkedDependency<'a> {
    pub artifact: &'a Artifact,
    pub origins: &'a BTreeMap<String, DeclarationOrigin>,
}

pub(crate) struct LinkedContext {
    pub states: BTreeMap<String, StateDecl>,
    pub actor_decls: BTreeMap<String, ActorDecl>,
    pub actors: BTreeMap<String, LinkedActor>,
    pub actor_enums: BTreeMap<String, LinkedActorEnum>,
    pub origins: BTreeMap<String, DeclarationOrigin>,
}

impl LinkedContext {
    pub(super) fn new(
        dependencies: &BTreeMap<String, LinkedDependency<'_>>,
        origins: BTreeMap<String, DeclarationOrigin>,
        unbound_names: &BTreeSet<String>,
        local_states: &BTreeMap<String, &StateDecl>,
        local_actors: &BTreeMap<String, &ActorDecl>,
    ) -> Result<Self> {
        let mut context = Self {
            states: BTreeMap::new(),
            actor_decls: BTreeMap::new(),
            actors: BTreeMap::new(),
            actor_enums: BTreeMap::new(),
            origins,
        };
        let mut names_by_origin =
            context.origins.iter().map(|(name, origin)| (origin.clone(), name.clone())).collect::<BTreeMap<_, _>>();

        // for each linked dependencies, link state, actors and actor enums declarations to the context
        for (app, dependency) in dependencies {
            let artifact = dependency.artifact;
            if artifact.app != *app {
                return Err(ArgentError::new(format!("linked artifact for app `{app}` declares app `{}`", artifact.app)));
            }
            artifact
                .check_consistency()
                .map_err(|err| ArgentError::new(format!("linked app `{app}` has an invalid artifact: {err}")))?;

            let mut names = BTreeMap::new();
            for (name, kind) in artifact
                .argent
                .states
                .iter()
                .map(|state| (&state.name, SymbolKind::State))
                .chain(artifact.argent.actor_enums.iter().map(|item| (&item.name, SymbolKind::ActorEnum)))
            {
                let origin = dependency.origins.get(name).cloned().unwrap_or_else(|| DeclarationOrigin::Dependency {
                    app: app.clone(),
                    name: name.clone(),
                    kind,
                });
                let model_name = if let Some(name) = names_by_origin.get(&origin) {
                    name.clone()
                } else {
                    let mut candidate = name.clone();
                    let mut suffix = 0;
                    while context.origins.contains_key(&candidate) || unbound_names.contains(&candidate) {
                        suffix += 1;
                        candidate = format!("Argent__linked__{suffix}__{name}");
                    }
                    names_by_origin.insert(origin.clone(), candidate.clone());
                    context.origins.insert(candidate.clone(), origin);
                    candidate
                };
                names.insert(name.clone(), model_name);
            }
            for interface in &artifact.argent.interfaces.exports {
                let actor_name = &interface.actor;
                if interface.app != *app {
                    return Err(ArgentError::new(format!("app `{app}` has no exported interface for actor `{actor_name}`")));
                }
                let reference = format!("{app}::{actor_name}");
                if local_actors.contains_key(&reference) {
                    return Err(ArgentError::new(format!(
                        "imported actor reference `{reference}` conflicts with a local actor declaration"
                    )));
                }
                let actor = artifact
                    .argent
                    .actors
                    .iter()
                    .find(|actor| &actor.name == actor_name)
                    .ok_or_else(|| ArgentError::new(format!("app `{app}` does not export actor `{actor_name}`")))?;
                let template = artifact
                    .argent
                    .template_plan
                    .templates
                    .iter()
                    .find(|template| &template.actor == actor_name)
                    .ok_or_else(|| ArgentError::new(format!("app `{app}` has no template receipt for actor `{actor_name}`")))?;

                // add linked actor's state, and its potential sub-states (expansion)
                context.import_state_closure(artifact, &actor.state, &names, local_states)?;

                let state = names[&actor.state].clone();
                context.actor_decls.insert(
                    reference.clone(),
                    ActorDecl { name: reference.clone(), state: state.clone(), functions: Vec::new(), entries: Vec::new() },
                );
                context.actors.insert(
                    reference,
                    LinkedActor {
                        app: app.clone(),
                        actor: actor_name.clone(),
                        state,
                        interface: interface.clone(),
                        template: template.actor_type_handle.template.clone(),
                    },
                );
            }
            for actor_enum in &artifact.argent.actor_enums {
                let name = names[&actor_enum.name].clone();
                let state = names.get(&actor_enum.state).cloned().ok_or_else(|| {
                    ArgentError::new(format!("linked app `{app}` does not describe enum state `{}`", actor_enum.state))
                })?;
                if !context.states.contains_key(&state) && !local_states.contains_key(&state) {
                    continue;
                }
                let variants = actor_enum
                    .variants
                    .iter()
                    .map(|actor| {
                        // Imported enum variants already carry their defining app.
                        if actor.contains("::") {
                            return actor.clone();
                        }
                        dependency
                            .origins
                            .get(actor)
                            .and_then(|origin| names_by_origin.get(origin))
                            .cloned()
                            .unwrap_or_else(|| format!("{app}::{actor}"))
                    })
                    .collect();
                let linked = LinkedActorEnum { name: name.clone(), state, variants };
                if let Some(previous) = context.actor_enums.insert(name.clone(), linked.clone())
                    && previous != linked
                {
                    return Err(ArgentError::new(format!("linked apps provide conflicting actor enum definitions for `{name}`")));
                }
            }
        }
        Ok(context)
    }

    /// Import and remap all state-bearing edges before comparing shared declarations.
    fn import_state_closure(
        &mut self,
        artifact: &Artifact,
        root: &str,
        names: &BTreeMap<String, String>,
        local_states: &BTreeMap<String, &StateDecl>,
    ) -> Result<()> {
        let states = artifact.argent.states.iter().map(|state| (state.name.as_str(), state)).collect::<BTreeMap<_, _>>();
        let expansions =
            artifact.argent.state_expansions.iter().map(|expansion| (expansion.state.as_str(), expansion)).collect::<BTreeMap<_, _>>();
        let enums = artifact.argent.actor_enums.iter().map(|item| (item.name.as_str(), item)).collect::<BTreeMap<_, _>>();
        let mapped_state = |name: &str| {
            names
                .get(name)
                .filter(|_| states.contains_key(name))
                .cloned()
                .ok_or_else(|| ArgentError::new(format!("linked app `{}` does not describe state `{name}`", artifact.app)))
        };
        let mut pending = vec![root.to_string()];
        let mut visited = BTreeSet::new();
        while let Some(name) = pending.pop() {
            if !visited.insert(name.clone()) {
                continue;
            }
            let state = states
                .get(name.as_str())
                .ok_or_else(|| ArgentError::new(format!("linked app `{}` does not describe state `{name}`", artifact.app)))?;
            let expansion = expansions.get(name.as_str()).copied();
            let mut fields = if expansion.is_some() {
                Vec::new()
            } else {
                state.fields.iter().map(linked_field_decl).collect::<Result<Vec<_>>>()?
            };
            for field in &mut fields {
                if let Some(target) = states.get(field.ty.name.as_str()) {
                    pending.push(target.name.clone());
                } else if let Some(actor_enum) = enums.get(field.ty.name.as_str()) {
                    pending.push(actor_enum.state.clone());
                }
                if let Some(actor_state) = &mut field.ty.actor_state {
                    pending.push(actor_state.clone());
                    *actor_state = mapped_state(actor_state)?;
                }
                if !field.ty.is_builtin() {
                    field.ty.name = names.get(&field.ty.name).cloned().ok_or_else(|| {
                        ArgentError::new(format!("linked app `{}` does not describe type `{}`", artifact.app, field.ty.name))
                    })?;
                }
            }
            let expansion = expansion
                .map(|expansion| -> Result<_> {
                    pending.push(expansion.base.clone());
                    pending.extend(expansion.digests.iter().map(|digest| digest.state.clone()));
                    Ok(StateExpansionDecl {
                        base: mapped_state(&expansion.base)?,
                        digests: expansion
                            .digests
                            .iter()
                            .map(|digest| {
                                Ok(StateDigestExpansionDecl { field: digest.field.clone(), state: mapped_state(&digest.state)? })
                            })
                            .collect::<Result<_>>()?,
                    })
                })
                .transpose()?;
            let model_name = names[&name].clone();
            let decl = StateDecl { name: model_name.clone(), fields, expansion };
            if let Some(local) = local_states.get(&model_name) {
                if !same_state_decl(local, &decl) {
                    return Err(ArgentError::new(format!(
                        "linked app `{}` state `{name}` conflicts with its imported source declaration",
                        artifact.app
                    )));
                }
            } else if let Some(previous) = self.states.insert(model_name.clone(), decl.clone())
                && !same_state_decl(&previous, &decl)
            {
                return Err(ArgentError::new(format!("linked apps provide conflicting state definitions for `{model_name}`")));
            }
        }
        Ok(())
    }
}

fn linked_field_decl(field: &ArgentFieldArtifact) -> Result<FieldDecl> {
    Ok(FieldDecl { ty: linked_field_type(field)?, name: field.name.clone(), virtual_slot: field.virtual_slot })
}

fn linked_field_type(field: &ArgentFieldArtifact) -> Result<TypeRef> {
    if let Some(source) = &field.source_type {
        return Ok(TypeRef {
            name: source.name.clone(),
            array: source.array.map(|array| match array {
                SourceArrayArtifact::Dynamic => ArrayDim::Dynamic,
                SourceArrayArtifact::Fixed(len) => ArrayDim::Fixed(len),
            }),
            actor_state: source.actor_state.clone(),
        });
    }
    linked_type_ref(&field.ty)
}

fn linked_type_ref(ty: &TypeArtifact) -> Result<TypeRef> {
    let scalar = |name: &str| Ok(TypeRef::new(name));
    match ty {
        TypeArtifact::Int => scalar("int"),
        TypeArtifact::Temporal => scalar("temporal"),
        TypeArtifact::Bool => scalar("bool"),
        TypeArtifact::Byte => scalar("byte"),
        TypeArtifact::Bytes => Ok(TypeRef::dynamic_array("byte")),
        TypeArtifact::Text => scalar("string"),
        TypeArtifact::Pubkey => scalar("pubkey"),
        TypeArtifact::Sig => scalar("sig"),
        TypeArtifact::Datasig => scalar("datasig"),
        TypeArtifact::FixedBytes { len } => Ok(TypeRef::array("byte", *len)),
        TypeArtifact::FixedArray { item, len } => {
            let item = linked_type_ref(item)?;
            if item.array.is_some() || item.actor_state.is_some() {
                return Err(ArgentError::new("linked artifacts cannot expose nested array state fields"));
            }
            Ok(TypeRef::array(item.name, *len))
        }
        TypeArtifact::DynamicArray { item } => {
            let item = linked_type_ref(item)?;
            if item.array.is_some() || item.actor_state.is_some() {
                return Err(ArgentError::new("linked artifacts cannot expose nested array state fields"));
            }
            Ok(TypeRef::dynamic_array(item.name))
        }
        TypeArtifact::Struct { name } => scalar(name),
    }
}

fn same_state_decl(left: &StateDecl, right: &StateDecl) -> bool {
    left.name == right.name
        && left.fields.len() == right.fields.len()
        && left
            .fields
            .iter()
            .zip(&right.fields)
            .all(|(left, right)| left.name == right.name && left.ty == right.ty && left.virtual_slot == right.virtual_slot)
        && match (&left.expansion, &right.expansion) {
            (None, None) => true,
            (Some(left), Some(right)) => {
                left.base == right.base
                    && left.digests.len() == right.digests.len()
                    && left
                        .digests
                        .iter()
                        .zip(&right.digests)
                        .all(|(left, right)| left.field == right.field && left.state == right.state)
            }
            _ => false,
        }
}

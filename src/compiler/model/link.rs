//! Links imported app artifacts into the selected application's model.
//!
//! Linked interfaces, templates, states, and actor enums become model inputs.

use std::collections::{BTreeMap, BTreeSet};

use crate::artifact::*;
use crate::compiler::loader::SymbolKind;
use crate::compiler::resolve::{AppMember, ResolvedModules, ResolvedName};
use crate::compiler::syntax::node::DeclId;
use crate::compiler::syntax::*;
use crate::error::{ArgentError, Result};

use super::{SourceFieldId, SourceStateId};

#[derive(Debug, Clone)]
pub(crate) struct LinkedActor {
    pub app: String,
    pub actor: String,
    pub state: String,
    pub interface: ActorInterfaceArtifact,
    pub template: ActorTemplateArtifact,
}

/// Portable identity of an actor exported by a linked app.
#[derive(Debug, Clone, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct LinkedActorId {
    pub(crate) app: String,
    pub(crate) actor: String,
}

impl LinkedActorId {
    /// Translate a bound source member to its portable linked-artifact identity.
    pub(crate) fn from_member(member: AppMember, resolution: &ResolvedModules<'_>) -> Self {
        Self {
            app: resolution.declaration(member.app).name().to_string(),
            actor: resolution.declaration(member.actor).name().to_string(),
        }
    }
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

/// Declaration identities of a pinned artifact import, keyed by the artifact's own names.
///
/// The artifact is declared as a source module (states, actor enums and exported actors, each in artifact order), so these are
/// the identities its declarations have in every importing app: an importer that uses the artifact's
/// state links it to the same declaration the artifact reports.
pub(crate) fn artifact_origins(path: &std::path::Path, artifact: &Artifact) -> BTreeMap<String, DeclarationOrigin> {
    let states = artifact.argent.states.iter().map(|state| (&state.name, SymbolKind::State));
    let actor_enums = artifact.argent.actor_enums.iter().map(|item| (&item.name, SymbolKind::ActorEnum));
    let actors = artifact.argent.interfaces.exports.iter().map(|interface| (&interface.actor, SymbolKind::Actor));
    let mut indexes = BTreeMap::<SymbolKind, usize>::new();
    states
        .chain(actor_enums)
        .chain(actors)
        .map(|(name, kind)| {
            let index = indexes.entry(kind).or_default();
            let origin = DeclarationOrigin::Source { path: path.to_path_buf(), kind, index: *index };
            *index += 1;
            (name.clone(), origin)
        })
        .collect()
}

pub(crate) struct LinkedDependency<'a> {
    pub artifact: &'a Artifact,
    pub origins: &'a BTreeMap<String, DeclarationOrigin>,
}

type LocalStateSource<'a, 'src> = (&'a BTreeMap<String, DeclId>, &'a ResolvedModules<'src>, &'a BTreeMap<DeclId, String>);

pub(crate) struct LinkedContext {
    pub states: BTreeMap<String, StateDecl>,
    pub state_field_sources: BTreeMap<SourceFieldId, SourceStateId>,
    pub actors: BTreeMap<LinkedActorId, LinkedActor>,
    pub actor_names: BTreeMap<String, LinkedActorId>,
    pub actor_enums: BTreeMap<String, LinkedActorEnum>,
    pub origins: BTreeMap<String, DeclarationOrigin>,
}

impl LinkedContext {
    pub(super) fn new(
        dependencies: &BTreeMap<String, LinkedDependency<'_>>,
        reserved_names: &BTreeSet<String>,
        local_states: &BTreeMap<String, &StateDecl>,
        local_state_ids: &BTreeMap<String, DeclId>,
        local_actors: &BTreeMap<String, &ActorDecl>,
        resolution: &ResolvedModules<'_>,
        display_names: &BTreeMap<DeclId, String>,
    ) -> Result<Self> {
        let origins = display_names
            .iter()
            .map(|(id, name)| {
                (
                    name.clone(),
                    DeclarationOrigin::Source {
                        path: resolution.declaration_path(*id).to_path_buf(),
                        kind: id.kind(),
                        index: id.index,
                    },
                )
            })
            .collect();
        let mut context = Self {
            states: BTreeMap::new(),
            state_field_sources: BTreeMap::new(),
            actors: BTreeMap::new(),
            actor_names: BTreeMap::new(),
            actor_enums: BTreeMap::new(),
            origins,
        };
        let mut names_by_origin =
            context.origins.iter().map(|(name, origin)| (origin.clone(), name.clone())).collect::<BTreeMap<_, _>>();
        let mut actor_enum_apps = BTreeMap::new();

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
                    while context.origins.contains_key(&candidate) || reserved_names.contains(&candidate) {
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
                context.import_state_closure(
                    artifact,
                    &actor.state,
                    &names,
                    local_states,
                    Some((local_state_ids, resolution, display_names)),
                )?;

                let state = names[&actor.state].clone();
                let id = LinkedActorId { app: app.clone(), actor: actor_name.clone() };
                context.actor_names.insert(reference, id.clone());
                context.actors.insert(
                    id,
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
                let previous_app = actor_enum_apps.entry(name.clone()).or_insert(app);
                if let Some(previous) = context.actor_enums.insert(name.clone(), linked.clone())
                    && previous != linked
                {
                    return Err(ArgentError::new(format!(
                        "actor enum `{name}` is provided through both `{previous_app}` and `{app}`; \
                         linking the same enum declaration through different apps is not supported"
                    )));
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
        local_source: Option<LocalStateSource<'_, '_>>,
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
            let owner_origin = self
                .origins
                .get(&model_name)
                .ok_or_else(|| ArgentError::new(format!("linked state `{model_name}` has no declaration origin")))?;
            let owner = SourceStateId::from_origin(model_name.clone(), owner_origin.clone());
            for field in &decl.fields {
                let Some(target_origin) = self.origins.get(&field.ty.name) else { continue };
                if !matches!(
                    target_origin,
                    DeclarationOrigin::Source { kind: SymbolKind::State, .. }
                        | DeclarationOrigin::Dependency { kind: SymbolKind::State, .. }
                ) {
                    continue;
                }
                let target = SourceStateId::from_origin(field.ty.name.clone(), target_origin.clone());
                let key = SourceFieldId::new(owner.clone(), &field.name);
                if let Some(previous) = self.state_field_sources.insert(key, target.clone())
                    && previous != target
                {
                    return Err(ArgentError::new(format!(
                        "linked state `{model_name}` field `{}` has conflicting source identities",
                        field.name
                    )));
                }
            }
            if let Some(local) = local_states.get(&model_name) {
                let (local_state_ids, resolution, display_names) = local_source
                    .ok_or_else(|| ArgentError::new(format!("linked state `{model_name}` has no local source bindings")))?;
                if !same_source_state_decl(local, &decl, local_state_ids[&model_name], resolution, display_names) {
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

pub(crate) fn linked_field_decl(field: &ArgentFieldArtifact) -> Result<FieldDecl> {
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

/// Compare an authored state with its linked artifact after resolving source type references.
fn same_source_state_decl(
    source: &StateDecl,
    linked: &StateDecl,
    owner: DeclId,
    resolution: &ResolvedModules<'_>,
    display_names: &BTreeMap<DeclId, String>,
) -> bool {
    let name = |source: &str| match resolution.bindings(owner).names.get(source) {
        Some(ResolvedName::Declaration(id)) => display_names.get(id).cloned().unwrap_or_else(|| source.to_string()),
        _ => source.to_string(),
    };
    display_names.get(&owner) == Some(&linked.name)
        && source.fields.len() == linked.fields.len()
        && source.fields.iter().zip(&linked.fields).all(|(left, right)| {
            left.name == right.name
                && left.virtual_slot == right.virtual_slot
                && left.ty.array == right.ty.array
                && if left.ty.is_builtin() { left.ty.name == right.ty.name } else { name(&left.ty.name) == right.ty.name }
                && left.ty.actor_state.as_deref().map(name) == right.ty.actor_state
        })
        && match (&source.expansion, &linked.expansion) {
            (None, None) => true,
            (Some(left), Some(right)) => {
                name(&left.base) == right.base
                    && left.digests.len() == right.digests.len()
                    && left
                        .digests
                        .iter()
                        .zip(&right.digests)
                        .all(|(left, right)| left.field == right.field && name(&left.state) == right.state)
            }
            _ => false,
        }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linked_nested_fields_retain_distinct_source_origins() {
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("clock after epoch").as_nanos();
        let directory = std::env::temp_dir().join(format!("argent-linked-field-origins-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("test directory created");
        let mut artifacts = Vec::new();
        for (app, field) in [("Left", "int amount"), ("Right", "bool active")] {
            let path = directory.join(format!("{app}.ag"));
            std::fs::write(
                &path,
                format!(
                    "state Inner {{ {field}; }} state S {{ Inner nested; }} state R {{ int nonce; }} \
                     actor A owns R {{ entry hold() emits none {{}} }} app {app} {{ actor A; }}"
                ),
            )
            .expect("source written");
            artifacts.push((app, crate::build_file(&path, directory.join(format!("out-{app}"))).expect("dependency builds")));
        }
        let mut origins = BTreeMap::new();
        for (app, _) in &artifacts {
            for state in ["Inner", "S"] {
                origins.insert(
                    format!("{app}{state}"),
                    DeclarationOrigin::Dependency { app: (*app).to_string(), name: state.to_string(), kind: SymbolKind::State },
                );
            }
        }
        let mut context = LinkedContext {
            states: BTreeMap::new(),
            state_field_sources: BTreeMap::new(),
            actors: BTreeMap::new(),
            actor_names: BTreeMap::new(),
            actor_enums: BTreeMap::new(),
            origins,
        };
        for (app, artifact) in &artifacts {
            let names = [("Inner".to_string(), format!("{app}Inner")), ("S".to_string(), format!("{app}S"))].into_iter().collect();
            context.import_state_closure(artifact, "S", &names, &BTreeMap::new(), None).expect("nested state closure imports");
        }
        let owners =
            ["Left", "Right"].map(|app| SourceStateId::from_origin(format!("{app}S"), context.origins[&format!("{app}S")].clone()));
        let targets = owners
            .iter()
            .map(|owner| context.state_field_sources[&SourceFieldId::new(owner.clone(), "nested")].clone())
            .collect::<Vec<_>>();
        assert_ne!(targets[0], targets[1]);
        assert_eq!(targets[0], SourceStateId::from_origin("LeftInner", context.origins["LeftInner"].clone()));
        assert_eq!(targets[1], SourceStateId::from_origin("RightInner", context.origins["RightInner"].clone()));
        std::fs::remove_dir_all(directory).expect("test directory removed");
    }
}

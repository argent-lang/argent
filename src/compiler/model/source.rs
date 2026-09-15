//! Temporary adapter from immutable source bindings to the string-based model.
//!
//! This is the only module that assigns compatibility names and rewrites bound
//! source. Remove it when Model and Sil lowering consume source nodes and IDs.
use std::collections::{BTreeMap, BTreeSet};

use super::link::DeclarationOrigin;
use crate::compiler::loader::{ActorSite, DeclId, ModuleId, ResolvedDeclaration, ResolvedModules, ResolvedName, SymbolKind, TextSite};
use crate::compiler::syntax::*;
use crate::error::{ArgentError, Result};

#[derive(Debug)]
pub(crate) struct ModelSource<'a> {
    program: &'a ResolvedModules,
    modules: Vec<Module>,
    names: BTreeMap<DeclId, String>,
    declarations: BTreeSet<DeclId>,
    /// Local and unresolved names that imported declarations must not reuse.
    pub(super) unbound_names: BTreeSet<String>,
    pub(super) app_name: String,
    pub(super) actors: Vec<String>,
}

impl<'a> ModelSource<'a> {
    pub(crate) fn new(program: &'a ResolvedModules, app_name: Option<&str>) -> Result<Self> {
        let app = program.root_app(app_name)?;
        let actors = if let Some(app) = app {
            program.app_actor_ids(app)?
        } else {
            program.root_declarations().filter(|id| id.kind() == SymbolKind::Actor).collect()
        };
        let declarations = program.app_declarations(app)?;
        let mut names = BTreeMap::new();
        let mut occupied = BTreeSet::new();
        // Actor names are app re-export
        for id in &actors {
            let name = program.declaration(*id).name().to_string();
            if !occupied.insert(name.clone()) {
                return Err(ArgentError::new(format!("selected app exports actor name `{name}` more than once")));
            }
            names.insert(*id, name);
        }
        let root_bindings = program.root_declarations().collect::<BTreeSet<_>>();
        let mut ordered = declarations.iter().copied().collect::<Vec<_>>();
        ordered.sort_by(|left, right| {
            (!root_bindings.contains(left), program.declaration_path(*left), left.kind(), left.index).cmp(&(
                !root_bindings.contains(right),
                program.declaration_path(*right),
                right.kind(),
                right.index,
            ))
        });
        let authored_names = declarations.iter().map(|id| program.declaration(*id).name().to_string()).collect::<BTreeSet<_>>();
        for id in ordered {
            if names.contains_key(&id) {
                continue;
            }
            let declaration = program.declaration(id);
            let source_name = declaration.name();
            let foreign_locals = declarations
                .iter()
                .filter(|owner| program.declaration_path(**owner) != program.declaration_path(id))
                .flat_map(|owner| program.bindings(*owner).unbound_names.iter())
                .collect::<BTreeSet<_>>();
            let mut name = source_name.to_string();
            if occupied.contains(&name) || foreign_locals.contains(&name) {
                let mut suffix = 1;
                loop {
                    name = format!("Argent__{suffix}__{source_name}");
                    if !occupied.contains(&name) && !authored_names.contains(&name) && !foreign_locals.contains(&name) {
                        break;
                    }
                    suffix += 1;
                }
            }
            occupied.insert(name.clone());
            names.insert(id, name);
        }
        let app_name = app.map(|id| program.declaration(id).name().to_string()).unwrap_or_else(|| "ArgentApp".to_string());
        let actors = actors.into_iter().map(|id| names[&id].clone()).collect();
        let unbound_names = declarations.iter().flat_map(|id| program.bindings(*id).unbound_names.iter().cloned()).collect();
        let mut source = Self { program, modules: Vec::new(), names, declarations, unbound_names, app_name, actors };
        for index in 0..program.modules().len() {
            source.modules.push(source.bind_module(ModuleId::new(index))?);
        }
        Ok(source)
    }

    pub(super) fn declarations(&self) -> impl Iterator<Item = ResolvedDeclaration<'_>> {
        self.declarations.iter().map(|id| ResolvedModules::declaration_in(&self.modules, *id))
    }

    /// Declaration identity survives module loading order and compatibility renaming.
    pub(super) fn declaration_origins(&self) -> BTreeMap<String, DeclarationOrigin> {
        self.names
            .iter()
            .map(|(id, name)| {
                (
                    name.clone(),
                    DeclarationOrigin::Source {
                        path: self.program.declaration_path(*id).to_path_buf(),
                        kind: id.kind(),
                        index: id.index,
                    },
                )
            })
            .collect()
    }

    fn name(&self, owner: DeclId, source: &str) -> String {
        self.target_name(self.program.bindings(owner).names[source])
    }

    fn actor_target(&self, owner: DeclId, site: ActorSite, source: &str) -> String {
        let bindings = self.program.bindings(owner);
        if bindings.local_actor_targets.contains(&site) { source.to_string() } else { self.target_name(bindings.actor_targets[&site]) }
    }

    fn target_name(&self, target: ResolvedName) -> String {
        match target {
            ResolvedName::Declaration(id) => self.names[&id].clone(),
            ResolvedName::AppMember(member) => {
                format!("{}::{}", self.program.declaration(member.app).name(), self.program.declaration(member.actor).name())
            }
            ResolvedName::Module(_) => unreachable!("module namespaces are not bound value references"),
        }
    }

    fn text(&self, owner: DeclId, site: TextSite, source: &str) -> String {
        let mut text = source.to_string();
        let mut references = self.program.bindings(owner).text[&site].iter().collect::<Vec<_>>();
        references.sort_by_key(|reference| reference.span.start);
        for reference in references.into_iter().rev() {
            text.replace_range(reference.span.start..reference.span.end, &self.target_name(reference.target));
        }
        text
    }

    fn ty(&self, owner: DeclId, source: &TypeRef) -> TypeRef {
        let mut ty = source.clone();
        if !source.is_builtin() {
            ty.name = self.name(owner, &source.name);
        }
        ty.actor_state = source.actor_state.as_ref().map(|state| self.name(owner, state));
        ty
    }

    fn cardinality(&self, owner: DeclId, cardinality: &mut Cardinality) {
        if let Cardinality::Range { minimum, maximum } = cardinality {
            for bound in [minimum, maximum] {
                if let CardinalityBound::Const(name) = bound {
                    *name = self.name(owner, name);
                }
            }
        }
    }

    fn bind_module(&self, module_id: ModuleId) -> Result<Module> {
        let source = &self.program.modules()[module_id.index()];
        let mut module = source.clone();

        for (index, (source, bound)) in source.consts.iter().zip(&mut module.consts).enumerate() {
            let id = DeclId::new(module_id, SymbolKind::Const, index);
            if !self.declarations.contains(&id) {
                continue;
            }
            bound.name = self.names[&id].to_string();
            bound.ty = self.ty(id, &source.ty);
            bound.value = self.text(id, TextSite::Value, &source.value);
        }

        for (index, (source, bound)) in source.states.iter().zip(&mut module.states).enumerate() {
            let id = DeclId::new(module_id, SymbolKind::State, index);
            if !self.declarations.contains(&id) {
                continue;
            }
            bound.name = self.names[&id].to_string();
            for (source_field, bound_field) in source.fields.iter().zip(&mut bound.fields) {
                bound_field.ty = self.ty(id, &source_field.ty);
            }
            if let (Some(source_expansion), Some(bound_expansion)) = (&source.expansion, &mut bound.expansion) {
                bound_expansion.base = self.name(id, &source_expansion.base);
                for (source_digest, bound_digest) in source_expansion.digests.iter().zip(&mut bound_expansion.digests) {
                    bound_digest.state = self.name(id, &source_digest.state);
                }
            }
        }

        for (index, (source, bound)) in source.functions.iter().zip(&mut module.functions).enumerate() {
            let id = DeclId::new(module_id, SymbolKind::Function, index);
            if !self.declarations.contains(&id) {
                continue;
            }
            *bound = self.function(id, 0, source);
        }

        for (index, (source, bound)) in source.actors.iter().zip(&mut module.actors).enumerate() {
            let id = DeclId::new(module_id, SymbolKind::Actor, index);
            if !self.declarations.contains(&id) {
                continue;
            }
            bound.name = self.names[&id].to_string();
            bound.state = self.name(id, &source.state);
            for (index, (source_function, bound_function)) in source.functions.iter().zip(&mut bound.functions).enumerate() {
                *bound_function = self.function(id, index, source_function);
            }
            for (index, (source_entry, bound_entry)) in source.entries.iter().zip(&mut bound.entries).enumerate() {
                *bound_entry = self.entry(id, index, source_entry)?;
            }
        }

        for (index, (source, bound)) in source.actor_enums.iter().zip(&mut module.actor_enums).enumerate() {
            let id = DeclId::new(module_id, SymbolKind::ActorEnum, index);
            if !self.declarations.contains(&id) {
                continue;
            }
            bound.name = self.names[&id].to_string();
            bound.variants = source.variants.iter().map(|variant| self.name(id, variant)).collect();
        }

        for (index, (source, bound)) in source.apps.iter().zip(&mut module.apps).enumerate() {
            let id = DeclId::new(module_id, SymbolKind::App, index);
            if !self.declarations.contains(&id) {
                continue;
            }
            bound.actors = source.actors.iter().map(|actor| self.name(id, actor)).collect();
        }

        Ok(module)
    }

    fn function(&self, owner: DeclId, index: usize, source: &FunctionDecl) -> FunctionDecl {
        let mut function = source.clone();
        if owner.kind() == SymbolKind::Function {
            function.name = self.names[&owner].clone();
        }
        for param in &mut function.params {
            param.ty = self.ty(owner, &param.ty);
        }
        function.return_ty = source.return_ty.as_ref().map(|ty| self.ty(owner, ty));
        function.body = self.text(owner, TextSite::Function(index), &source.body);
        function
    }

    fn entry(&self, owner: DeclId, index: usize, source: &EntryDecl) -> Result<EntryDecl> {
        let mut entry = source.clone();
        for (source_param, bound_param) in source.params.iter().zip(&mut entry.params) {
            bound_param.ty = self.ty(owner, &source_param.ty);
        }
        for (source_consume, bound_consume) in source.consumes.iter().zip(&mut entry.consumes) {
            bound_consume.actor = self.name(owner, &source_consume.actor);
            self.cardinality(owner, &mut bound_consume.cardinality);
        }
        for (observe_index, (source_observe, bound_observe)) in source.observes.iter().zip(&mut entry.observes).enumerate() {
            bound_observe.covenant_expr =
                self.text(owner, TextSite::Observe { entry: index, observe: observe_index }, &source_observe.covenant_expr);
            let inputs = source_observe
                .inputs
                .iter()
                .zip(&mut bound_observe.inputs)
                .enumerate()
                .map(|(input, pair)| (ActorSite::ObserveInput { entry: index, observe: observe_index, input }, pair));
            let outputs = source_observe
                .outputs
                .iter()
                .zip(&mut bound_observe.outputs)
                .enumerate()
                .map(|(output, pair)| (ActorSite::ObserveOutput { entry: index, observe: observe_index, output }, pair));
            for (site, (source_actor, bound_actor)) in inputs.chain(outputs) {
                bound_actor.actor = self.actor_target(owner, site, &source_actor.actor);
                bound_actor.open_state = source_actor.open_state.as_deref().map(|state| self.name(owner, state));
                self.cardinality(owner, &mut bound_actor.cardinality);
            }
        }
        for (spawn_index, (source_spawn, bound_spawn)) in source.spawns.iter().zip(&mut entry.spawns).enumerate() {
            for (output_index, (source_output, bound_output)) in source_spawn.outputs.iter().zip(&mut bound_spawn.outputs).enumerate()
            {
                bound_output.actor = self.actor_target(
                    owner,
                    ActorSite::Spawn { entry: index, spawn: spawn_index, output: output_index },
                    &source_output.actor,
                );
                self.cardinality(owner, &mut bound_output.cardinality);
            }
        }
        if let (EmitSpec::Outputs(source_outputs), EmitSpec::Outputs(bound_outputs)) = (&source.emits, &mut entry.emits) {
            for (source_output, bound_output) in source_outputs.iter().zip(bound_outputs) {
                bound_output.actors = source_output.actors.iter().map(|actor| self.name(owner, actor)).collect();
                self.cardinality(owner, &mut bound_output.cardinality);
            }
        }
        let body = self.text(owner, TextSite::Entry(index), source.body.text());
        if body != source.body.text() {
            entry.body = EntryBody::new(body)?;
            let route_analysis = body::routes::analyze_entry_routes(&entry.body)?;
            entry.routes = route_analysis.routes;
            entry.terminal_route_sets = route_analysis.terminal_route_sets;
        }
        Ok(entry)
    }
}

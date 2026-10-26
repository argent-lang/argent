//! Finds what names refer to across modules and imports.
//! Keeps the source modules unchanged and records the results for later compiler steps.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::artifact::Artifact;
use crate::compiler::syntax::lexer::RESERVED_GENERATED_MODULE_NAME_PREFIX;
use crate::compiler::syntax::node::{
    ChildEdge, DeclId, EntryId, ModuleId, NodeId, RootSlot, SourceNodeCursor, SourceNodeIndex, SymbolKind,
};
use crate::compiler::syntax::source::Origin;
use crate::compiler::syntax::*;
use crate::error::{ArgentError, Result};

#[derive(Debug, Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct AppMember {
    pub app: DeclId,
    pub actor: DeclId,
}

#[derive(Debug, Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum ResolvedName {
    Module(ModuleId),
    Declaration(DeclId),
    AppMember(AppMember),
}

pub(crate) enum ResolvedDeclaration<'a> {
    Const(&'a ConstDecl),
    State(&'a StateDecl),
    Function(&'a FunctionDecl),
    Actor(&'a ActorDecl),
    ActorEnum(&'a ActorEnumDecl),
    App(&'a AppDecl),
}

#[derive(Debug, Clone)]
pub(super) struct ResolvedImport {
    pub target: ModuleId,
    pub alias: Option<String>,
}

mod bindings;
mod namespaces;
pub(crate) use bindings::{ActorSourceBinding, Binding, ClauseReference, DeclarationBindings, LocalId};
use namespaces::NamespaceTable;

#[cfg(test)]
mod tests;

#[derive(Debug, Clone)]
struct SourceStorage<'src> {
    program: &'src Program<'src>,
}

impl SourceStorage<'_> {
    fn module(&self, id: ModuleId) -> &Module {
        &self.program.modules[id.index()].legacy
    }

    fn module_count(&self) -> usize {
        self.program.modules.len()
    }

    fn source_path(&self, id: ModuleId) -> &std::path::Path {
        &self.program.modules[id.index()].source.display_path
    }

    fn source_text(&self, id: ModuleId) -> &str {
        &self.program.modules[id.index()].source.text
    }

    fn nodes(&self) -> &SourceNodeIndex {
        &self.program.nodes
    }

    fn type_use(&self, id: NodeId) -> Option<&ArgentTypeUse> {
        self.program.modules[id.module.index()].type_uses.get(&id)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedModules<'src> {
    storage: SourceStorage<'src>,
    imports: Arc<Vec<Vec<ResolvedImport>>>,
    view: ModuleView,
    /// Names available in each module, including imports. Indexed by `ModuleId`.
    exports: Arc<NamespaceTable>,
    /// What references inside each declaration refer to.
    bindings: Arc<BTreeMap<DeclId, DeclarationBindings>>,
    /// Published app artifacts behind artifact-backed modules (`import "./app/artifact.json" id "...";`).
    artifacts: Arc<BTreeMap<ModuleId, Artifact>>,
}

#[derive(Debug, Clone)]
struct ModuleView {
    root: ModuleId,
    reachable: Vec<ModuleId>,
}

impl<'src> ResolvedModules<'src> {
    pub(super) fn from_program(
        program: &'src Program<'src>,
        root: ModuleId,
        imports: Vec<Vec<ResolvedImport>>,
        artifacts: BTreeMap<ModuleId, Artifact>,
    ) -> Result<Self> {
        let storage = SourceStorage { program };
        if storage.module_count() != imports.len() || root.index() >= storage.module_count() {
            return Err(ArgentError::new("invalid loaded module graph"));
        }

        let exports = NamespaceTable::new(&storage, &imports)?;
        let mut resolved = Self {
            storage,
            imports: Arc::new(imports),
            view: ModuleView { root, reachable: Vec::new() },
            exports: Arc::new(exports),
            bindings: Arc::new(BTreeMap::new()),
            artifacts: Arc::new(artifacts),
        };
        resolved.view = resolved.module_view(root);
        for module_index in 0..resolved.storage.module_count() {
            let source = resolved.storage.module(ModuleId::new(module_index));
            for (kind, declaration_count) in [
                (SymbolKind::Const, source.consts.len()),
                (SymbolKind::State, source.states.len()),
                (SymbolKind::Function, source.functions.len()),
                (SymbolKind::Actor, source.actors.len()),
                (SymbolKind::ActorEnum, source.actor_enums.len()),
                (SymbolKind::App, source.apps.len()),
            ] {
                for index in 0..declaration_count {
                    let id = DeclId::new(ModuleId::new(module_index), kind, index);
                    let bindings = DeclarationBindings::resolve(&resolved, &program.modules[module_index], id)?;
                    Arc::get_mut(&mut resolved.bindings)
                        .expect("binding map is uniquely owned during resolution")
                        .insert(id, bindings);
                }
            }
        }
        Ok(resolved)
    }

    fn wrong_kind(&self, module: ModuleId, reference: &str, expected: &[SymbolKind], actual: SymbolKind) -> ArgentError {
        self.exports.wrong_kind(self, module, reference, expected, actual)
    }

    pub(crate) fn declaration(&self, id: DeclId) -> ResolvedDeclaration<'_> {
        Self::declaration_in_module(self.storage.module(id.module), id)
    }

    fn declaration_in_module(module: &Module, id: DeclId) -> ResolvedDeclaration<'_> {
        match id.kind {
            SymbolKind::Const => ResolvedDeclaration::Const(&module.consts[id.index]),
            SymbolKind::State => ResolvedDeclaration::State(&module.states[id.index]),
            SymbolKind::Function => ResolvedDeclaration::Function(&module.functions[id.index]),
            SymbolKind::Actor => ResolvedDeclaration::Actor(&module.actors[id.index]),
            SymbolKind::ActorEnum => ResolvedDeclaration::ActorEnum(&module.actor_enums[id.index]),
            SymbolKind::App => ResolvedDeclaration::App(&module.apps[id.index]),
        }
    }

    pub(crate) fn root_path(&self) -> &std::path::Path {
        &self.storage.module(self.view.root).path
    }

    pub(crate) fn module_paths(&self) -> impl Iterator<Item = &std::path::Path> {
        self.view.reachable.iter().map(|id| self.storage.source_path(*id))
    }

    pub(crate) fn module_id_for_path(&self, path: &std::path::Path) -> Option<ModuleId> {
        (0..self.storage.module_count()).find(|&index| self.storage.module(ModuleId::new(index)).path == path).map(ModuleId::new)
    }

    pub(crate) fn with_root(&self, root: ModuleId) -> Self {
        let mut view = self.clone();
        view.view = self.module_view(root);
        view
    }

    fn module_view(&self, root: ModuleId) -> ModuleView {
        let mut reachable = Vec::new();
        let mut seen = BTreeSet::new();
        let mut pending = vec![root];
        while let Some(module) = pending.pop() {
            if !seen.insert(module) {
                continue;
            }
            reachable.push(module);
            pending.extend(self.imports[module.index()].iter().rev().map(|import| import.target));
        }
        ModuleView { root, reachable }
    }

    fn resolve(&self, from_module: ModuleId, reference: &str) -> Result<ResolvedName> {
        let segments = reference.split("::").collect::<Vec<_>>();
        if segments.is_empty() {
            return Err(ArgentError::at(&self.storage.module(from_module).path, "invalid empty reference"));
        }
        self.exports.bind_export_path(self, from_module, &segments, reference)
    }

    pub(crate) fn bindings(&self, id: DeclId) -> &DeclarationBindings {
        &self.bindings[&id]
    }

    pub(crate) fn declaration_path(&self, id: DeclId) -> &std::path::Path {
        self.storage.source_path(ModuleId::new(self.storage.nodes().source(id).0))
    }

    pub(crate) fn source_text(&self, module: ModuleId) -> &str {
        self.storage.source_text(module)
    }

    pub(crate) fn type_use(&self, id: NodeId) -> Option<&ArgentTypeUse> {
        self.storage.type_use(id)
    }

    /// Return the constant expression retained by the parsed source.
    pub(crate) fn const_expression(&self, id: DeclId) -> Result<silverscript_lang::ast::Expr<'_>> {
        Ok(self.storage.program.modules[id.module.index()].const_values[id.index].clone())
    }

    pub(crate) fn entry_body(&self, id: EntryId) -> Result<&[AuthoredEntryStatement<'_>]> {
        let cursor = SourceNodeCursor::new(id.actor, RootSlot::Entry(id.index)).child(ChildEdge::Body);
        let site = self
            .nodes()
            .find(&cursor.address)
            .ok_or_else(|| ArgentError::at(self.declaration_path(id.actor), "missing indexed authored entry body"))?;
        self.storage.program.modules[id.actor.module.index()]
            .entry_bodies
            .get(&site)
            .map(Vec::as_slice)
            .ok_or_else(|| ArgentError::at(self.declaration_path(id.actor), "missing retained authored entry body"))
    }

    /// Read a constructed route's expressions from their indexed authored sites.
    pub(crate) fn route_texts(&self, id: EntryId, route: RouteId) -> Result<(&str, &str)> {
        let (actor, state) = self
            .nodes()
            .route_sites(id, route)
            .ok_or_else(|| ArgentError::new("constructed route has no indexed authored expressions"))?;
        let text = self.source_text(id.actor.module);
        let source = |site| match self.nodes().node(site).origin {
            Origin::Authored { start, end, .. } => {
                text.get(start..end).ok_or_else(|| ArgentError::new("constructed route expression is outside its source"))
            }
            Origin::Generated { .. } => Err(ArgentError::new("constructed route expression has no authored source")),
        };
        Ok((source(actor)?, source(state)?))
    }

    /// Return the parsed body of a global or actor helper.
    pub(crate) fn function_body(&self, owner: DeclId, member: Option<usize>) -> Result<&[silverscript_lang::ast::Statement<'_>]> {
        let root = member.map_or(RootSlot::Declaration, RootSlot::ActorFunction);
        let cursor = SourceNodeCursor::new(owner, root).child(ChildEdge::Body);
        let site = self
            .nodes()
            .find(&cursor.address)
            .ok_or_else(|| ArgentError::at(self.declaration_path(owner), "missing indexed authored function body"))?;
        self.storage.program.modules[owner.module.index()]
            .function_bodies
            .get(&site)
            .map(Vec::as_slice)
            .ok_or_else(|| ArgentError::at(self.declaration_path(owner), "missing retained authored function body"))
    }

    pub(crate) fn root_declarations(&self) -> impl Iterator<Item = DeclId> + '_ {
        self.exports.exports[self.view.root.index()].values().filter_map(|resolved| match resolved {
            ResolvedName::Declaration(id) => Some(*id),
            ResolvedName::Module(_) | ResolvedName::AppMember(_) => None,
        })
    }

    /// Collects the starting declarations and everything they reference, directly or indirectly.
    pub(crate) fn declaration_closure(&self, roots: impl IntoIterator<Item = DeclId>) -> BTreeSet<DeclId> {
        let mut declarations = BTreeSet::new();
        let mut pending = roots.into_iter().collect::<Vec<_>>();
        while let Some(id) = pending.pop() {
            if !declarations.insert(id) {
                continue;
            }
            if let Some(bindings) = self.bindings.get(&id) {
                pending.extend(bindings.declarations.iter().copied());
            }
        }
        declarations
    }

    pub(crate) fn root_app(&self, app_name: Option<&str>) -> Result<Option<DeclId>> {
        let root = self.storage.module(self.view.root);
        if let Some(app_name) = app_name {
            return root
                .apps
                .iter()
                .position(|app| app.name == app_name)
                .map(|index| Some(DeclId::new(self.view.root, SymbolKind::App, index)))
                .ok_or_else(|| ArgentError::at(&root.path, format!("root module has no app named `{app_name}`")));
        }
        match root.apps.as_slice() {
            [] => Ok(None),
            [_] => Ok(Some(DeclId::new(self.view.root, SymbolKind::App, 0))),
            apps => Err(ArgentError::at(
                &root.path,
                format!(
                    "root module declares multiple apps ({}); select one with `--app <name>`",
                    apps.iter().map(|app| app.name.as_str()).collect::<Vec<_>>().join(", ")
                ),
            )),
        }
    }

    pub(crate) fn app_actor_ids(&self, app: DeclId) -> Result<Vec<DeclId>> {
        if app.kind != SymbolKind::App {
            return Err(ArgentError::new("declaration is not an app"));
        }
        let ResolvedDeclaration::App(app_decl) = self.declaration(app) else {
            unreachable!("app declaration ID resolves to an app");
        };
        let actors = app_decl
            .actors
            .iter()
            .map(|reference| match self.resolve(app.module, reference)? {
                ResolvedName::Declaration(actor) if actor.kind == SymbolKind::Actor => Ok(actor),
                ResolvedName::Declaration(actor) => Err(self.wrong_kind(app.module, reference, &[SymbolKind::Actor], actor.kind)),
                ResolvedName::AppMember(_) => Err(ArgentError::at(
                    &self.storage.module(app.module).path,
                    format!("app member `{reference}` cannot belong directly to another app"),
                )),
                ResolvedName::Module(_) => Err(ArgentError::at(
                    &self.storage.module(app.module).path,
                    format!("module namespace `{reference}` cannot belong to an app"),
                )),
            })
            .collect::<Result<Vec<_>>>()?;
        let mut names = BTreeSet::new();
        for actor in &actors {
            let name = self.declaration(*actor).name().to_string();
            if !names.insert(name.clone()) {
                return Err(ArgentError::new(format!("selected app exports actor name `{name}` more than once")));
            }
        }
        Ok(actors)
    }

    pub(crate) fn referenced_app_members(&self, app: DeclId) -> Result<BTreeSet<AppMember>> {
        let declarations = self.app_declarations(Some(app))?;
        Ok(declarations.into_iter().flat_map(|id| self.bindings[&id].apps.iter().copied()).collect())
    }

    /// Collects root constants, states, functions and actor enums, plus the selected app's actors
    /// and everything they reference. Without an app, includes all actors available in the root module.
    pub(crate) fn app_declarations(&self, app: Option<DeclId>) -> Result<BTreeSet<DeclId>> {
        let mut roots =
            self.root_declarations().filter(|id| !matches!(id.kind(), SymbolKind::Actor | SymbolKind::App)).collect::<Vec<_>>();
        if let Some(app) = app {
            roots.extend(self.app_actor_ids(app)?);
        } else {
            roots.extend(self.root_declarations().filter(|id| id.kind() == SymbolKind::Actor));
        }
        Ok(self.declaration_closure(roots))
    }

    /// Assign distinct Sil display names to declarations in a selected source closure.
    pub(crate) fn selected_declaration_names(&self, app: Option<DeclId>, actors: &[DeclId]) -> Result<BTreeMap<DeclId, String>> {
        let declarations = self.app_declarations(app)?;
        let reserved_names = declarations.iter().map(|id| (*id, self.reserved_local_names(*id))).collect::<BTreeMap<_, _>>();
        let mut names = BTreeMap::new();
        let mut occupied = BTreeSet::new();
        for id in actors {
            let name = self.declaration(*id).name().to_string();
            occupied.insert(name.clone());
            names.insert(*id, name);
        }
        let root_bindings = self.root_declarations().collect::<BTreeSet<_>>();
        let mut ordered = declarations.iter().copied().collect::<Vec<_>>();
        ordered.sort_by(|left, right| {
            (!root_bindings.contains(left), self.declaration_path(*left), left.kind(), left.index).cmp(&(
                !root_bindings.contains(right),
                self.declaration_path(*right),
                right.kind(),
                right.index,
            ))
        });
        let authored_names = declarations.iter().map(|id| self.declaration(*id).name().to_string()).collect::<BTreeSet<_>>();
        for id in ordered {
            if names.contains_key(&id) {
                continue;
            }
            let declaration = self.declaration(id);
            let source_name = declaration.name();
            let foreign_locals = declarations
                .iter()
                .filter(|owner| self.declaration_path(**owner) != self.declaration_path(id))
                .flat_map(|owner| reserved_names[owner].iter())
                .collect::<BTreeSet<_>>();
            let mut name = source_name.to_string();
            if occupied.contains(&name) || foreign_locals.contains(&name) {
                let mut suffix = 1;
                loop {
                    name = format!("{RESERVED_GENERATED_MODULE_NAME_PREFIX}{suffix}__{source_name}");
                    if !occupied.contains(&name) && !authored_names.contains(&name) && !foreign_locals.contains(&name) {
                        break;
                    }
                    suffix += 1;
                }
            }
            occupied.insert(name.clone());
            names.insert(id, name);
        }
        Ok(names)
    }

    /// Names owned by local scopes or runtime syntax that selected Sil declarations cannot capture.
    pub(crate) fn reserved_local_names(&self, owner: DeclId) -> BTreeSet<String> {
        let bindings = self.bindings(owner);
        let mut names = bindings.local_names.values().cloned().collect::<BTreeSet<_>>();
        for (site, binding) in &bindings.sites {
            if !matches!(binding, Binding::Builtin | Binding::RuntimeRoot) {
                continue;
            }
            if let Origin::Authored { start, end, .. } = self.nodes().node(*site).origin
                && let Some(name) = self.source_text(site.module).get(start..end)
            {
                names.insert(name.to_string());
            }
        }
        if let ResolvedDeclaration::Actor(actor) = self.declaration(owner) {
            names.extend(actor.functions.iter().map(|function| function.name.clone()));
            let Some(ResolvedName::Declaration(mut state_id)) = bindings.names.get(&actor.state).copied() else {
                return names;
            };
            let mut seen = BTreeSet::new();
            while seen.insert(state_id) {
                let ResolvedDeclaration::State(state) = self.declaration(state_id) else { break };
                names.extend(state.fields.iter().map(|field| field.name.clone()));
                let Some(expansion) = &state.expansion else { break };
                let Some(ResolvedName::Declaration(base)) = self.bindings(state_id).names.get(&expansion.base).copied() else {
                    break;
                };
                state_id = base;
            }
        }
        names
    }

    pub(crate) fn app_source(&self, app: DeclId) -> (&std::path::Path, &AppDecl) {
        let ResolvedDeclaration::App(declaration) = self.declaration(app) else {
            unreachable!("app declaration ID resolves to an app");
        };
        (&self.storage.module(app.module).path, declaration)
    }

    pub(crate) fn module(&self, id: ModuleId) -> &Module {
        self.storage.module(id)
    }

    pub(crate) fn nodes(&self) -> &SourceNodeIndex {
        self.storage.nodes()
    }

    pub(crate) fn root_module(&self) -> &Module {
        self.storage.module(self.view.root)
    }

    /// The published artifact behind `module`, when it was loaded from a pinned artifact import.
    pub(crate) fn artifact(&self, module: ModuleId) -> Option<&Artifact> {
        self.artifacts.get(&module)
    }
}

impl SymbolKind {
    fn description(self) -> &'static str {
        match self {
            Self::Const => "a constant",
            Self::State => "a state",
            Self::Function => "a function",
            Self::Actor => "an actor",
            Self::ActorEnum => "an actor enum",
            Self::App => "an app",
        }
    }
}

impl ResolvedDeclaration<'_> {
    pub(crate) fn name(&self) -> &str {
        match self {
            Self::Const(item) => &item.name,
            Self::State(item) => &item.name,
            Self::Function(item) => &item.name,
            Self::Actor(item) => &item.name,
            Self::ActorEnum(item) => &item.name,
            Self::App(item) => &item.name,
        }
    }
}

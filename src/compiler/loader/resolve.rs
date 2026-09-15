//! Finds what names refer to across modules and imports.
//! Keeps the source modules unchanged and records the results for later compiler steps.

use std::collections::{BTreeMap, BTreeSet};

use crate::compiler::naming::is_identifier;
use crate::compiler::syntax::*;
use crate::error::{ArgentError, Result};

/// Index of a module in the loaded modules array.
#[derive(Debug, Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct ModuleId(usize);

#[derive(Debug, Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum SymbolKind {
    Const,
    State,
    Function,
    Actor,
    ActorEnum,
    App,
}

#[derive(Debug, Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct DeclId {
    module: ModuleId,
    kind: SymbolKind,
    /// Index within the module's list for this kind.
    pub(crate) index: usize,
}

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
pub(crate) use bindings::{ActorSite, DeclarationBindings, TextSite};

#[derive(Debug, Clone)]
pub(crate) struct ResolvedModules {
    source_modules: Vec<Module>,
    root: ModuleId,
    /// Names available in each module, including imports. Indexed by `ModuleId`.
    exports: Vec<BTreeMap<String, ResolvedName>>,
    /// What references inside each declaration refer to.
    bindings: BTreeMap<DeclId, DeclarationBindings>,
}

impl ModuleId {
    pub(crate) fn new(index: usize) -> Self {
        Self(index)
    }

    pub(crate) fn index(self) -> usize {
        self.0
    }
}

impl DeclId {
    pub(crate) fn new(module: ModuleId, kind: SymbolKind, index: usize) -> Self {
        Self { module, kind, index }
    }

    pub(crate) fn kind(self) -> SymbolKind {
        self.kind
    }
}

impl ResolvedModules {
    pub(super) fn from_loaded(modules: Vec<Module>, root: ModuleId, imports: Vec<Vec<ResolvedImport>>) -> Result<Self> {
        if modules.len() != imports.len() || root.index() >= modules.len() {
            return Err(ArgentError::new("invalid loaded module graph"));
        }

        let exports = Self::build_exports(&modules, &imports)?;
        let mut resolved = Self { source_modules: modules, root, exports, bindings: BTreeMap::new() };
        for module_index in 0..resolved.source_modules.len() {
            let source = &resolved.source_modules[module_index];
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
                    let bindings = DeclarationBindings::resolve(&resolved, id)?;
                    resolved.bindings.insert(id, bindings);
                }
            }
        }
        Ok(resolved)
    }

    /// Collects names available in each module and rejects names that refer to different things.
    fn build_exports(modules: &[Module], imports: &[Vec<ResolvedImport>]) -> Result<Vec<BTreeMap<String, ResolvedName>>> {
        let mut candidates = vec![BTreeMap::<String, BTreeSet<ResolvedName>>::new(); modules.len()];

        for (module_index, module) in modules.iter().enumerate() {
            let module_id = ModuleId::new(module_index);
            let mut insert_declaration = |name: &str, kind, index| {
                candidates[module_index]
                    .entry(name.to_string())
                    .or_default()
                    .insert(ResolvedName::Declaration(DeclId::new(module_id, kind, index)));
            };
            for (index, item) in module.consts.iter().enumerate() {
                insert_declaration(&item.name, SymbolKind::Const, index);
            }
            for (index, item) in module.states.iter().enumerate() {
                insert_declaration(&item.name, SymbolKind::State, index);
            }
            for (index, item) in module.functions.iter().enumerate() {
                insert_declaration(&item.name, SymbolKind::Function, index);
            }
            for (index, item) in module.actors.iter().enumerate() {
                insert_declaration(&item.name, SymbolKind::Actor, index);
            }
            for (index, item) in module.actor_enums.iter().enumerate() {
                insert_declaration(&item.name, SymbolKind::ActorEnum, index);
            }
            for (index, item) in module.apps.iter().enumerate() {
                insert_declaration(&item.name, SymbolKind::App, index);
            }

            let mut aliases = BTreeSet::new();
            for import in &imports[module_index] {
                if let Some(alias) = &import.alias {
                    if !aliases.insert(alias.as_str()) {
                        return Err(ArgentError::at(&module.path, format!("module alias `{alias}` is imported more than once")));
                    }
                    candidates[module_index].entry(alias.clone()).or_default().insert(ResolvedName::Module(import.target));
                }
            }
        }

        // Repeat until imports of imports have also been included.
        loop {
            let mut next_candidates = candidates.clone();
            for (module_index, module_imports) in imports.iter().enumerate() {
                // For each unaliased import, add the imported module's resolved names to this module.
                for import in module_imports.iter().filter(|import| import.alias.is_none()) {
                    for (name, possible_targets) in &candidates[import.target.index()] {
                        next_candidates[module_index].entry(name.clone()).or_default().extend(possible_targets);
                    }
                }
            }
            if next_candidates == candidates {
                break;
            }
            candidates = next_candidates;
        }

        let mut exports = Vec::with_capacity(candidates.len());
        for (module_index, module_candidates) in candidates.into_iter().enumerate() {
            let mut module_exports = BTreeMap::new();
            for (name, possible_targets) in module_candidates {
                let Some(target) = possible_targets.iter().next().copied() else {
                    continue;
                };
                if possible_targets.len() > 1 {
                    return Err(ArgentError::at(
                        &modules[module_index].path,
                        format!("ambiguous export `{name}` in module namespace"),
                    ));
                }
                module_exports.insert(name, target);
            }
            exports.push(module_exports);
        }
        Ok(exports)
    }

    /// Follows a name such as `library::app::Actor` from the module where it is used.
    fn bind_export_path(&self, from_module: ModuleId, segments: &[&str], reference: &str) -> Result<ResolvedName> {
        let mut current_module = from_module;
        for (index, segment) in segments.iter().enumerate() {
            if !is_identifier(segment) {
                return Err(ArgentError::at(
                    &self.source_modules[from_module.index()].path,
                    format!("invalid qualified reference `{reference}`"),
                ));
            }
            let target = self.exports[current_module.index()].get(*segment).copied().ok_or_else(|| {
                ArgentError::at(
                    &self.source_modules[from_module.index()].path,
                    format!("unknown export `{segment}` while resolving `{reference}`"),
                )
            })?;
            if index + 1 == segments.len() {
                return Ok(target);
            }
            match target {
                ResolvedName::Module(imported_module) => current_module = imported_module,
                ResolvedName::Declaration(app) if app.kind == SymbolKind::App && index + 2 == segments.len() => {
                    return self.resolve_app_member(from_module, app, segments[index + 1], reference);
                }
                ResolvedName::Declaration(_) | ResolvedName::AppMember(_) => {
                    return Err(ArgentError::at(
                        &self.source_modules[from_module.index()].path,
                        format!("export `{segment}` is not a namespace while resolving `{reference}`"),
                    ));
                }
            }
        }
        Err(ArgentError::at(&self.source_modules[from_module.index()].path, format!("invalid qualified reference `{reference}`")))
    }

    fn resolve_app_member(&self, from_module: ModuleId, app: DeclId, actor_name: &str, reference: &str) -> Result<ResolvedName> {
        let ResolvedDeclaration::App(app_decl) = self.declaration(app) else {
            unreachable!("app declaration ID resolves to an app");
        };
        let mut matching_actor = None;
        for actor_reference in &app_decl.actors {
            let actor_segments = actor_reference.split("::").collect::<Vec<_>>();
            let ResolvedName::Declaration(actor) = self.bind_export_path(app.module, &actor_segments, actor_reference)? else {
                return Err(ArgentError::at(
                    &self.source_modules[app.module.index()].path,
                    format!("app `{}` member `{actor_reference}` does not name a local actor", app_decl.name),
                ));
            };
            if actor.kind != SymbolKind::Actor {
                continue;
            }
            let ResolvedDeclaration::Actor(actor_decl) = self.declaration(actor) else {
                unreachable!("actor declaration ID resolves to an actor");
            };
            if actor_decl.name == actor_name && matching_actor.replace(actor).is_some() {
                return Err(ArgentError::at(
                    &self.source_modules[app.module.index()].path,
                    format!("app `{}` exports actor name `{actor_name}` more than once", app_decl.name),
                ));
            }
        }
        matching_actor.map(|actor| ResolvedName::AppMember(AppMember { app, actor })).ok_or_else(|| {
            ArgentError::at(
                &self.source_modules[from_module.index()].path,
                format!("app `{}` has no actor member `{actor_name}` while resolving `{reference}`", app_decl.name),
            )
        })
    }

    fn wrong_kind(&self, module: ModuleId, reference: &str, expected: &[SymbolKind], actual: SymbolKind) -> ArgentError {
        let expected = expected.iter().map(|kind| kind.description()).collect::<Vec<_>>().join(" or ");
        ArgentError::at(
            &self.source_modules[module.index()].path,
            format!("reference `{reference}` names {}, expected {expected}", actual.description()),
        )
    }

    pub(crate) fn declaration(&self, id: DeclId) -> ResolvedDeclaration<'_> {
        Self::declaration_in(&self.source_modules, id)
    }

    pub(crate) fn declaration_in(modules: &[Module], id: DeclId) -> ResolvedDeclaration<'_> {
        let module = &modules[id.module.index()];
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
        &self.source_modules[self.root.index()].path
    }

    pub(crate) fn module_paths(&self) -> impl Iterator<Item = &std::path::Path> {
        self.source_modules.iter().map(|module| module.path.as_path())
    }

    fn resolve(&self, from_module: ModuleId, reference: &str) -> Result<ResolvedName> {
        let segments = reference.split("::").collect::<Vec<_>>();
        if segments.is_empty() {
            return Err(ArgentError::at(&self.source_modules[from_module.index()].path, "invalid empty reference"));
        }
        self.bind_export_path(from_module, &segments, reference)
    }

    pub(crate) fn bindings(&self, id: DeclId) -> &DeclarationBindings {
        &self.bindings[&id]
    }

    pub(crate) fn declaration_path(&self, id: DeclId) -> &std::path::Path {
        &self.source_modules[id.module.index()].path
    }

    pub(crate) fn root_declarations(&self) -> impl Iterator<Item = DeclId> + '_ {
        self.exports[self.root.index()].values().filter_map(|resolved| match resolved {
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
        let root = &self.source_modules[self.root.index()];
        if let Some(app_name) = app_name {
            return root
                .apps
                .iter()
                .position(|app| app.name == app_name)
                .map(|index| Some(DeclId::new(self.root, SymbolKind::App, index)))
                .ok_or_else(|| ArgentError::at(&root.path, format!("root module has no app named `{app_name}`")));
        }
        match root.apps.as_slice() {
            [] => Ok(None),
            [_] => Ok(Some(DeclId::new(self.root, SymbolKind::App, 0))),
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
        app_decl
            .actors
            .iter()
            .map(|reference| match self.resolve(app.module, reference)? {
                ResolvedName::Declaration(actor) if actor.kind == SymbolKind::Actor => Ok(actor),
                ResolvedName::Declaration(actor) => Err(self.wrong_kind(app.module, reference, &[SymbolKind::Actor], actor.kind)),
                ResolvedName::AppMember(_) => Err(ArgentError::at(
                    &self.source_modules[app.module.index()].path,
                    format!("app member `{reference}` cannot belong directly to another app"),
                )),
                ResolvedName::Module(_) => Err(ArgentError::at(
                    &self.source_modules[app.module.index()].path,
                    format!("module namespace `{reference}` cannot belong to an app"),
                )),
            })
            .collect()
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

    pub(super) fn app_source(&self, app: DeclId) -> (&std::path::Path, &AppDecl) {
        let ResolvedDeclaration::App(declaration) = self.declaration(app) else {
            unreachable!("app declaration ID resolves to an app");
        };
        (&self.source_modules[app.module.index()].path, declaration)
    }

    pub(crate) fn modules(&self) -> &[Module] {
        &self.source_modules
    }

    pub(crate) fn root_module(&self) -> &Module {
        &self.source_modules[self.root.index()]
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

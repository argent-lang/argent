//! Loads Argent modules and plans source-app dependency graphs.
//!
//! Filesystem, inline, and standard-library sources become syntax programs here.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::compiler::syntax::Import;
use crate::compiler::syntax::parser::parse_module;
use crate::error::{ArgentError, Result};

use self::resolve::ResolvedImport;
pub(crate) use self::resolve::{
    ActorSite, DeclId, ModuleId, ResolvedDeclaration, ResolvedModules, ResolvedName, SymbolKind, TextSite,
};
use self::stdlib::{is_standard_module, load_standard_module};

mod resolve;
pub(crate) mod stdlib;

#[cfg(test)]
mod tests;

pub fn load_program(root: impl AsRef<Path>) -> Result<ResolvedModules> {
    let mut loader = Loader::default();
    let root = loader.load_module(root.as_ref())?;
    loader.finish(root)
}

pub fn load_inline_program(root: PathBuf, source: String) -> Result<ResolvedModules> {
    let module = parse_module(root, source)?;
    let imports = module.imports.clone();
    let mut loader = Loader::default();
    let root = loader.insert_module(module);
    loader.load_inline_imports(root, imports)?;
    loader.finish(root)
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) struct SourceApp {
    pub source: PathBuf,
    pub app: String,
}

/// Load one source app and its app dependencies in dependency-first order.
///
/// Each `(canonical source path, app name)` pair appears once. The requested
/// root app is the last item.
pub(crate) fn load_app_graph(root: impl AsRef<Path>, app: &str) -> Result<Vec<(SourceApp, Vec<SourceApp>, ResolvedModules)>> {
    let program = load_program(root)?;
    plan_app_graph(program, app)
}

pub(crate) fn plan_app_graph(program: ResolvedModules, app: &str) -> Result<Vec<(SourceApp, Vec<SourceApp>, ResolvedModules)>> {
    let root = program.root_path().to_path_buf();
    let mut planner = AppGraphPlanner::default();
    planner.programs.insert(root.clone(), program);
    planner.visit(SourceApp { source: root, app: app.to_string() })?;
    Ok(planner.order)
}

#[derive(Default)]
struct Loader {
    modules: Vec<crate::compiler::syntax::Module>,
    imports: Vec<Vec<ResolvedImport>>,
    module_ids: BTreeMap<PathBuf, ModuleId>,
}

impl Loader {
    fn load_module(&mut self, path: &Path) -> Result<ModuleId> {
        let canonical = fs::canonicalize(path).map_err(|err| ArgentError::at(path, err.to_string()))?;
        if let Some(module) = self.module_ids.get(&canonical).copied() {
            return Ok(module);
        }

        let source = fs::read_to_string(&canonical).map_err(|err| ArgentError::at(&canonical, err.to_string()))?;
        let module = parse_module(canonical.clone(), source)?;
        let base = canonical.parent().ok_or_else(|| ArgentError::at(&canonical, "module path has no parent"))?.to_path_buf();
        let imports = module.imports.clone();
        let module = self.insert_module(module);

        for import in imports {
            let target = self.load_import(&base, &import.path)?;
            self.imports[module.index()].push(ResolvedImport { target, alias: import.alias });
        }

        Ok(module)
    }

    fn load_inline_imports(&mut self, module: ModuleId, imports: Vec<Import>) -> Result<()> {
        for Import { path, alias } in imports {
            if is_standard_module(&path) {
                let target = self.load_standard_module(&path)?;
                self.imports[module.index()].push(ResolvedImport { target, alias });
            } else {
                return Err(ArgentError::at(
                    &self.modules[module.index()].path,
                    format!("inline source cannot import filesystem module `{path}`"),
                ));
            }
        }
        Ok(())
    }

    fn load_import(&mut self, base: &Path, path: &str) -> Result<ModuleId> {
        if is_standard_module(path) { self.load_standard_module(path) } else { self.load_module(&base.join(path)) }
    }

    fn load_standard_module(&mut self, path: &str) -> Result<ModuleId> {
        let module_path = PathBuf::from(path);
        if let Some(module) = self.module_ids.get(&module_path).copied() {
            return Ok(module);
        }
        let module = load_standard_module(path)?;
        let imports = module.imports.clone();
        let module = self.insert_module(module);
        for import in imports {
            if is_standard_module(&import.path) {
                let target = self.load_standard_module(&import.path)?;
                self.imports[module.index()].push(ResolvedImport { target, alias: import.alias });
            } else {
                return Err(ArgentError::new(format!("Argent standard module `{}` cannot import a filesystem module", import.path)));
            }
        }
        Ok(module)
    }

    fn insert_module(&mut self, module: crate::compiler::syntax::Module) -> ModuleId {
        let id = ModuleId::new(self.modules.len());
        self.module_ids.insert(module.path.clone(), id);
        self.modules.push(module);
        self.imports.push(Vec::new());
        id
    }

    fn finish(self, root: ModuleId) -> Result<ResolvedModules> {
        ResolvedModules::from_loaded(self.modules, root, self.imports)
    }
}

#[derive(Clone, Copy)]
enum Visit {
    Active(usize),
    Complete,
}

#[derive(Default)]
struct AppGraphPlanner {
    programs: BTreeMap<PathBuf, ResolvedModules>,
    app_sources: BTreeMap<String, PathBuf>,
    visits: BTreeMap<SourceApp, Visit>,
    stack: Vec<SourceApp>,
    order: Vec<(SourceApp, Vec<SourceApp>, ResolvedModules)>,
}

impl AppGraphPlanner {
    fn visit(&mut self, app: SourceApp) -> Result<()> {
        if let Some(previous) = self.app_sources.insert(app.app.clone(), app.source.clone())
            && previous != app.source
        {
            return Err(ArgentError::new(format!(
                "app `{}` is imported from both `{}` and `{}`",
                app.app,
                previous.display(),
                app.source.display()
            )));
        }
        match self.visits.get(&app).copied() {
            Some(Visit::Complete) => return Ok(()),
            Some(Visit::Active(start)) => {
                let cycle = self.stack[start..]
                    .iter()
                    .chain(std::iter::once(&app))
                    .map(|app| app.app.as_str())
                    .collect::<Vec<_>>()
                    .join(" -> ");
                return Err(ArgentError::new(format!("app import cycle: {cycle}")));
            }
            None => {}
        }

        self.visits.insert(app.clone(), Visit::Active(self.stack.len()));
        self.stack.push(app.clone());

        let program = self.load_source(&app.source)?;
        if program.root_path() != app.source {
            return Err(ArgentError::at(&app.source, "app source is not the root module"));
        }
        let Some(selected_app) = program.root_app(Some(&app.app))? else {
            return Err(ArgentError::at(&app.source, format!("source does not declare app `{}`", app.app)));
        };
        let dependencies = program
            .referenced_app_members(selected_app)?
            .into_iter()
            .map(|member| {
                let (source, app) = program.app_source(member.app);
                SourceApp { source: source.to_path_buf(), app: app.name.clone() }
            })
            .collect::<BTreeSet<_>>();
        for dependency in &dependencies {
            self.visit(dependency.clone())?;
        }

        self.stack.pop();
        self.visits.insert(app.clone(), Visit::Complete);
        self.order.push((app, dependencies.into_iter().collect(), program));
        Ok(())
    }

    fn load_source(&mut self, source: &Path) -> Result<ResolvedModules> {
        if let Some(program) = self.programs.get(source) {
            return Ok(program.clone());
        }
        let program = load_program(source)?;
        self.programs.insert(source.to_path_buf(), program.clone());
        Ok(program)
    }
}

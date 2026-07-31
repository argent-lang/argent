use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::compiler::syntax::parser::parse_module;
use crate::compiler::syntax::{Import, Program};
use crate::error::{ArgentError, Result};
use crate::stdlib::{is_standard_module, load_standard_module};

pub fn load_program(root: impl AsRef<Path>) -> Result<Program> {
    let root = root.as_ref().to_path_buf();
    let canonical_root = fs::canonicalize(&root).map_err(|err| ArgentError::at(&root, err.to_string()))?;
    let mut loader = Loader::default();
    loader.load_module(&canonical_root)?;
    Ok(Program { root: canonical_root, modules: loader.modules })
}

pub fn load_inline_program(root: PathBuf, source: String) -> Result<Program> {
    let module = parse_module(root.clone(), source)?;
    let imports = module.imports.clone();
    let mut loader = Loader::default();
    loader.modules.push(module);
    loader.load_inline_imports(imports)?;
    Ok(Program { root, modules: loader.modules })
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
pub(crate) fn load_app_graph(root: impl AsRef<Path>, app: &str) -> Result<Vec<(SourceApp, Vec<SourceApp>, Program)>> {
    let program = load_program(root)?;
    plan_app_graph(program, app)
}

pub(crate) fn plan_app_graph(program: Program, app: &str) -> Result<Vec<(SourceApp, Vec<SourceApp>, Program)>> {
    let root = program.root.clone();
    let mut planner = AppGraphPlanner::default();
    planner.programs.insert(root.clone(), program);
    planner.visit(SourceApp { source: root, app: app.to_string() })?;
    Ok(planner.order)
}

#[derive(Default)]
struct Loader {
    visited: BTreeSet<PathBuf>,
    visited_standard: BTreeSet<String>,
    modules: Vec<crate::compiler::syntax::Module>,
}

impl Loader {
    fn load_module(&mut self, path: &Path) -> Result<()> {
        let canonical = fs::canonicalize(path).map_err(|err| ArgentError::at(path, err.to_string()))?;
        if !self.visited.insert(canonical.clone()) {
            return Ok(());
        }

        let source = fs::read_to_string(&canonical).map_err(|err| ArgentError::at(&canonical, err.to_string()))?;
        let module = parse_module(canonical.clone(), source)?;
        let base = canonical.parent().ok_or_else(|| ArgentError::at(&canonical, "module path has no parent"))?.to_path_buf();
        let imports = module.imports.clone();
        self.modules.push(module);

        for import in imports {
            self.load_import(&base, import)?;
        }

        Ok(())
    }

    fn load_inline_imports(&mut self, imports: Vec<Import>) -> Result<()> {
        for import in imports {
            if let Import::Module { path } = import
                && is_standard_module(&path)
            {
                self.load_standard_module(&path)?;
            }
        }
        Ok(())
    }

    fn load_import(&mut self, base: &Path, import: Import) -> Result<()> {
        match import {
            Import::Module { path } if is_standard_module(&path) => self.load_standard_module(&path),
            Import::Module { path } | Import::Actor { path, .. } => self.load_module(&base.join(path)),
            Import::AppActor { .. } | Import::App { .. } => Ok(()),
        }
    }

    fn load_standard_module(&mut self, path: &str) -> Result<()> {
        if !self.visited_standard.insert(path.to_string()) {
            return Ok(());
        }
        let module = load_standard_module(path)?;
        let imports = module.imports.clone();
        self.modules.push(module);
        for import in imports {
            match import {
                Import::Module { path } if is_standard_module(&path) => self.load_standard_module(&path)?,
                Import::Module { path } | Import::Actor { path, .. } => {
                    return Err(ArgentError::new(format!("Argent standard module `{path}` cannot import a filesystem module")));
                }
                Import::AppActor { .. } | Import::App { .. } => {
                    return Err(ArgentError::new("Argent standard modules cannot import apps"));
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum Visit {
    Active(usize),
    Complete,
}

#[derive(Default)]
struct AppGraphPlanner {
    programs: BTreeMap<PathBuf, Program>,
    app_sources: BTreeMap<String, PathBuf>,
    visits: BTreeMap<SourceApp, Visit>,
    stack: Vec<SourceApp>,
    order: Vec<(SourceApp, Vec<SourceApp>, Program)>,
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
        let selected_app = root_app(&program, &app)?;
        let dependencies = app_dependencies(&program, selected_app)?;
        for dependency in &dependencies {
            self.visit(dependency.clone())?;
        }

        self.stack.pop();
        self.visits.insert(app.clone(), Visit::Complete);
        self.order.push((app, dependencies.into_iter().collect(), program));
        Ok(())
    }

    fn load_source(&mut self, source: &Path) -> Result<Program> {
        if let Some(program) = self.programs.get(source) {
            return Ok(program.clone());
        }
        let program = load_program(source)?;
        self.programs.insert(source.to_path_buf(), program.clone());
        Ok(program)
    }
}

fn app_dependencies(program: &Program, selected_app: &crate::compiler::syntax::AppDecl) -> Result<BTreeSet<SourceApp>> {
    let mut dependencies = BTreeSet::<SourceApp>::new();
    let mut app_sources = BTreeMap::<String, PathBuf>::new();
    let referenced_apps = qualified_app_references(program, selected_app);
    for module in &program.modules {
        let base = module.path.parent().ok_or_else(|| ArgentError::at(&module.path, "module path has no parent"))?;
        for import in &module.imports {
            match import {
                Import::AppActor { app, path, .. } | Import::App { app, path } => {
                    let source = canonical_source(&base.join(path))?;
                    insert_app_dependency(&mut dependencies, &mut app_sources, SourceApp { source, app: app.clone() }, &module.path)?;
                }
                Import::Module { path } if !is_standard_module(path) => {
                    let source = canonical_source(&base.join(path))?;
                    let imported = program
                        .modules
                        .iter()
                        .find(|candidate| candidate.path == source)
                        .ok_or_else(|| ArgentError::at(&module.path, format!("module import source `{path}` was not loaded")))?;
                    for app in imported.apps.iter().filter(|app| referenced_apps.contains(&app.name)) {
                        insert_app_dependency(
                            &mut dependencies,
                            &mut app_sources,
                            SourceApp { source: source.clone(), app: app.name.clone() },
                            &module.path,
                        )?;
                    }
                }
                Import::Module { .. } | Import::Actor { .. } => {}
            }
        }
    }
    Ok(dependencies)
}

fn qualified_app_references(program: &Program, selected_app: &crate::compiler::syntax::AppDecl) -> BTreeSet<String> {
    let actors =
        program.modules.iter().flat_map(|module| &module.actors).map(|actor| (actor.name.as_str(), actor)).collect::<BTreeMap<_, _>>();
    let mut apps = BTreeSet::new();
    // Consumes and emits are always in-app. Only observations and spawns can
    // introduce a foreign static actor dependency.
    for actor in selected_app.actors.iter().filter_map(|name| actors.get(name.as_str())) {
        for entry in &actor.entries {
            for observe in &entry.observes {
                for observed in observe.inputs.iter().chain(&observe.outputs) {
                    insert_qualified_app(&mut apps, &observed.actor);
                }
            }
            for spawn in &entry.spawns {
                for output in &spawn.outputs {
                    insert_qualified_app(&mut apps, &output.actor);
                }
            }
        }
    }
    apps
}

fn insert_qualified_app(apps: &mut BTreeSet<String>, actor: &str) {
    if let Some((app, actor)) = actor.split_once("::")
        && !app.is_empty()
        && !actor.is_empty()
    {
        apps.insert(app.to_string());
    }
}

fn insert_app_dependency(
    dependencies: &mut BTreeSet<SourceApp>,
    app_sources: &mut BTreeMap<String, PathBuf>,
    dependency: SourceApp,
    importing_module: &Path,
) -> Result<()> {
    if let Some(previous) = app_sources.insert(dependency.app.clone(), dependency.source.clone())
        && previous != dependency.source
    {
        return Err(ArgentError::at(
            importing_module,
            format!("app `{}` is imported from both `{}` and `{}`", dependency.app, previous.display(), dependency.source.display()),
        ));
    }
    dependencies.insert(dependency);
    Ok(())
}

fn root_app<'a>(program: &'a Program, source_app: &SourceApp) -> Result<&'a crate::compiler::syntax::AppDecl> {
    let root = program
        .modules
        .iter()
        .find(|module| module.path == source_app.source)
        .ok_or_else(|| ArgentError::at(&source_app.source, "app source is not the root module"))?;
    root.apps
        .iter()
        .find(|app| app.name == source_app.app)
        .ok_or_else(|| ArgentError::at(&source_app.source, format!("source does not declare app `{}`", source_app.app)))
}

fn canonical_source(path: &Path) -> Result<PathBuf> {
    fs::canonicalize(path).map_err(|err| ArgentError::at(path, err.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_graph_orders_and_deduplicates_diamond_dependencies() {
        let temp = temp_dir("diamond");
        write_app(&temp.join("shared.ag"), "", "SharedApp", "Shared");
        write_app(&temp.join("left.ag"), "import app SharedApp from \"./shared.ag\";", "LeftApp", "Left");
        write_app(&temp.join("right.ag"), "import actor SharedApp::Shared from \"./shared.ag\";", "RightApp", "Right");
        write_app(
            &temp.join("root.ag"),
            "import app LeftApp from \"./left.ag\";\nimport app RightApp from \"./right.ag\";",
            "RootApp",
            "Root",
        );

        let graph = load_app_graph(temp.join("root.ag"), "RootApp").expect("app graph loads");
        assert_eq!(
            graph.iter().map(|(app, _, _)| app.app.as_str()).collect::<Vec<_>>(),
            ["SharedApp", "LeftApp", "RightApp", "RootApp"]
        );
        assert_eq!(graph.iter().filter(|(app, _, _)| app.app == "SharedApp").count(), 1);
        let (_, root_dependencies, root_program) = graph.last().unwrap();
        assert_eq!(root_dependencies.iter().map(|dependency| dependency.app.as_str()).collect::<Vec<_>>(), ["LeftApp", "RightApp"]);
        assert_eq!(root_program.modules.len(), 1, "app imports do not enter the importing program");

        let _ = fs::remove_dir_all(temp);
    }

    #[test]
    fn app_graph_infers_apps_declared_by_module_imports() {
        let temp = temp_dir("module-app");
        write_app(&temp.join("asset.ag"), "", "AssetApp", "Asset");
        fs::write(
            temp.join("controller.ag"),
            r#"
import "./asset.ag";

state ControllerState {}

actor Controller owns ControllerState {
    entry inspect(cov_id asset_id)
    observes asset by asset_id {
        inputs {
            src: AssetApp::Asset,
        }
    }
    emits none {}
}

app ControllerApp {
    actor Controller;
}
"#,
        )
        .expect("controller source written");

        let graph = load_app_graph(temp.join("controller.ag"), "ControllerApp").expect("module app dependency graph loads");
        assert_eq!(graph.iter().map(|(app, _, _)| app.app.as_str()).collect::<Vec<_>>(), ["AssetApp", "ControllerApp"]);
        let (_, dependencies, program) = graph.last().unwrap();
        assert_eq!(dependencies.iter().map(|dependency| dependency.app.as_str()).collect::<Vec<_>>(), ["AssetApp"]);
        assert_eq!(program.modules.len(), 2, "ordinary imports retain shared source declarations");

        let _ = fs::remove_dir_all(temp);
    }

    #[test]
    fn module_apps_without_qualified_references_remain_shared_source() {
        let temp = temp_dir("shared-module-app");
        write_app(&temp.join("shared.ag"), "", "SharedApp", "Shared");
        write_app(&temp.join("root.ag"), "import \"./shared.ag\";", "RootApp", "Root");

        let graph = load_app_graph(temp.join("root.ag"), "RootApp").expect("shared source module loads");
        assert_eq!(graph.iter().map(|(app, _, _)| app.app.as_str()).collect::<Vec<_>>(), ["RootApp"]);
        assert!(graph[0].1.is_empty());
        assert_eq!(graph[0].2.modules.len(), 2);

        let _ = fs::remove_dir_all(temp);
    }

    #[test]
    fn app_graph_reports_the_complete_cycle() {
        let temp = temp_dir("cycle");
        write_app(&temp.join("a.ag"), "import app BApp from \"./b.ag\";", "AApp", "A");
        write_app(&temp.join("b.ag"), "import app CApp from \"./c.ag\";", "BApp", "B");
        write_app(&temp.join("c.ag"), "import app AApp from \"./a.ag\";", "CApp", "C");

        let err = load_app_graph(temp.join("a.ag"), "AApp").expect_err("app cycle is rejected");
        assert!(err.to_string().contains("app import cycle: AApp -> BApp -> CApp -> AApp"), "unexpected error: {err}");

        let _ = fs::remove_dir_all(temp);
    }

    #[test]
    fn app_graph_rejects_one_namespace_from_two_sources() {
        let temp = temp_dir("namespace-collision");
        write_app(&temp.join("first.ag"), "", "AssetApp", "First");
        write_app(&temp.join("second.ag"), "", "AssetApp", "Second");
        write_app(
            &temp.join("controller.ag"),
            "import app AssetApp from \"./first.ag\";\nimport app AssetApp from \"./second.ag\";",
            "CtrlApp",
            "Ctrl",
        );

        let err = load_app_graph(temp.join("controller.ag"), "CtrlApp").expect_err("ambiguous app namespace is rejected");
        assert!(err.to_string().contains("app `AssetApp` is imported from both"), "unexpected error: {err}");

        let _ = fs::remove_dir_all(temp);
    }

    #[test]
    fn app_graph_rejects_namespace_conflicts_across_branches() {
        let temp = temp_dir("transitive-namespace-collision");
        write_app(&temp.join("first.ag"), "", "SharedApp", "First");
        write_app(&temp.join("second.ag"), "", "SharedApp", "Second");
        write_app(&temp.join("left.ag"), "import app SharedApp from \"./first.ag\";", "LeftApp", "Left");
        write_app(&temp.join("right.ag"), "import app SharedApp from \"./second.ag\";", "RightApp", "Right");
        write_app(
            &temp.join("root.ag"),
            "import app LeftApp from \"./left.ag\";\nimport app RightApp from \"./right.ag\";",
            "RootApp",
            "Root",
        );

        let err = load_app_graph(temp.join("root.ag"), "RootApp").expect_err("one app namespace must have one source");
        assert!(err.to_string().contains("app `SharedApp` is imported from both"), "unexpected error: {err}");

        let _ = fs::remove_dir_all(temp);
    }

    fn temp_dir(name: &str) -> PathBuf {
        let temp = std::env::temp_dir().join(format!("argent-app-graph-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(&temp).expect("temp directory is created");
        temp
    }

    fn write_app(path: &Path, imports: &str, app: &str, actor: &str) {
        fs::write(
            path,
            format!(
                r#"
{imports}

state {actor}State {{}}

actor {actor} owns {actor}State {{}}

app {app} {{
    actor {actor};
}}
"#
            ),
        )
        .expect("app source is written");
    }
}

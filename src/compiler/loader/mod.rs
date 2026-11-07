//! Discovers and retains the source graph before parsing declarations.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::artifact::Artifact;
use crate::compiler::syntax::Program;
use crate::compiler::syntax::lexer::{Token, lex_argent_source};
use crate::compiler::syntax::node::SourceNodeIndex;
pub(crate) use crate::compiler::syntax::node::{ModuleId, SymbolKind};
use crate::compiler::syntax::parser::{DiscoveredImport, discover_imports, parse_module};
use crate::compiler::syntax::source::{SourceFile, SourceId};
use crate::error::{ArgentError, Result};

use self::artifact::{check_pin, declaration_source, is_artifact_path, read_artifact};
use self::stdlib::{is_standard_module, standard_source};
use super::resolve::ResolvedImport;
pub(crate) use super::resolve::{ResolvedDeclaration, ResolvedModules};

mod artifact;
pub(crate) mod stdlib;

#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;
#[cfg(test)]
pub(crate) use test_support::{load_app_graph, load_inline_program, load_program};

pub(crate) fn plan_app_graph<'src>(program: ResolvedModules<'src>, app: &str) -> Result<super::app_graph::AppGraphPlan<'src>> {
    super::app_graph::AppGraphPlan::new(program, app)
}

#[derive(Debug, Default)]
pub(crate) struct SourceSet {
    pub(crate) files: Vec<SourceFile>,
    tokens: Vec<Vec<Token>>,
    imports: Vec<Vec<DiscoveredImport>>,
    edges: Vec<Vec<ResolvedImport>>,
    source_ids: BTreeMap<SourceIdentity, SourceId>,
    /// Published app artifacts behind artifact-backed sources (`import "./app/artifact.json" id "...";`).
    artifacts: BTreeMap<SourceId, Artifact>,
    root: Option<SourceId>,
}

#[derive(Debug, Eq, Ord, PartialEq, PartialOrd)]
enum SourceIdentity {
    File(PathBuf),
    Standard(String),
    Inline,
}

impl SourceSet {
    pub(crate) fn discover_file(path: &Path) -> Result<Self> {
        if is_artifact_path(path) {
            return Err(ArgentError::at(path, "a published app artifact is not Argent source; import it from a source module"));
        }
        let mut sources = Self::default();
        sources.root = Some(sources.load_file(path)?);
        Ok(sources)
    }

    pub(crate) fn discover_inline(path: PathBuf, text: String) -> Result<Self> {
        let mut sources = Self::default();
        let root = sources.insert_source(SourceIdentity::Inline, path, text)?;
        sources.root = Some(root);
        let imports = sources.imports[root.0].clone();
        for discovered in imports {
            let import = discovered.import;
            if import.id.is_some() {
                return Err(pin_without_artifact(&sources.files[root.0].display_path, &import.path));
            }
            if !is_standard_module(&import.path) {
                return Err(ArgentError::at(
                    &sources.files[root.0].display_path,
                    format!("inline source cannot import filesystem module `{}`", import.path),
                ));
            }
            let target = sources.load_standard(&import.path)?;
            sources.edges[root.0].push(ResolvedImport { target: ModuleId::new(target.0), alias: import.alias });
        }
        Ok(sources)
    }

    fn load_file(&mut self, path: &Path) -> Result<SourceId> {
        let canonical = fs::canonicalize(path).map_err(|err| ArgentError::at(path, err.to_string()))?;
        let identity = SourceIdentity::File(canonical.clone());
        if let Some(id) = self.source_ids.get(&identity).copied() {
            return Ok(id);
        }
        let text = fs::read_to_string(&canonical).map_err(|err| ArgentError::at(&canonical, err.to_string()))?;
        let display_path = std::path::absolute(path).map_err(|err| ArgentError::at(path, err.to_string()))?;
        let base = canonical.parent().ok_or_else(|| ArgentError::at(&display_path, "module path has no parent"))?.to_path_buf();
        let id = self.insert_source(identity, display_path, text)?;
        let imports = self.imports[id.0].clone();
        for discovered in imports {
            let import = discovered.import;
            let target = if is_standard_module(&import.path) {
                if import.id.is_some() {
                    return Err(pin_without_artifact(&self.files[id.0].display_path, &import.path));
                }
                self.load_standard(&import.path)?
            } else if is_artifact_path(Path::new(&import.path)) {
                self.load_artifact(&base.join(&import.path), import.id.as_deref())?
            } else {
                if import.id.is_some() {
                    return Err(pin_without_artifact(&self.files[id.0].display_path, &import.path));
                }
                self.load_file(&base.join(&import.path))?
            };
            self.edges[id.0].push(ResolvedImport { target: ModuleId::new(target.0), alias: import.alias });
        }
        Ok(id)
    }

    /// Loads a published app artifact once per path as a declaration-only source; every import of it must pin its id.
    fn load_artifact(&mut self, path: &Path, pin: Option<&str>) -> Result<SourceId> {
        let canonical = fs::canonicalize(path).map_err(|err| ArgentError::at(path, err.to_string()))?;
        let identity = SourceIdentity::File(canonical.clone());
        if let Some(id) = self.source_ids.get(&identity).copied() {
            check_pin(&canonical, &self.artifacts[&id], pin)?;
            return Ok(id);
        }
        let artifact = read_artifact(&canonical)?;
        check_pin(&canonical, &artifact, pin)?;
        let display_path = std::path::absolute(path).map_err(|err| ArgentError::at(path, err.to_string()))?;
        let text = declaration_source(&display_path, &artifact)?;
        let id = self.insert_source(identity, display_path, text)?;
        self.artifacts.insert(id, artifact);
        Ok(id)
    }

    fn load_standard(&mut self, path: &str) -> Result<SourceId> {
        let identity = SourceIdentity::Standard(path.to_string());
        if let Some(id) = self.source_ids.get(&identity).copied() {
            return Ok(id);
        }
        let text = standard_source(path)?.to_string();
        let id = self.insert_source(identity, PathBuf::from(path), text)?;
        let imports = self.imports[id.0].clone();
        for discovered in imports {
            let import = discovered.import;
            if import.id.is_some() {
                return Err(pin_without_artifact(Path::new(path), &import.path));
            }
            if !is_standard_module(&import.path) {
                return Err(ArgentError::new(format!("Argent standard module `{}` cannot import a filesystem module", import.path)));
            }
            let target = self.load_standard(&import.path)?;
            self.edges[id.0].push(ResolvedImport { target: ModuleId::new(target.0), alias: import.alias });
        }
        Ok(id)
    }

    fn insert_source(&mut self, identity: SourceIdentity, display_path: PathBuf, text: String) -> Result<SourceId> {
        let id = SourceId(self.files.len());
        let file = SourceFile { id, display_path, text };
        let tokens = lex_argent_source(&file.text).map_err(|err| err.with_path(file.display_path.clone()))?;
        let imports = discover_imports(&file, &tokens)?;
        self.source_ids.insert(identity, id);
        self.files.push(file);
        self.tokens.push(tokens);
        self.imports.push(imports);
        self.edges.push(Vec::new());
        Ok(id)
    }

    pub(crate) fn parse_modules(&self) -> Result<Program<'_>> {
        let mut modules = Vec::with_capacity(self.files.len());
        let mut nodes = SourceNodeIndex::default();
        for file in &self.files {
            let (module, module_nodes) = parse_module(file, &self.tokens[file.id.0], &self.imports[file.id.0])?;
            modules.push(module);
            nodes.push_module(ModuleId::new(file.id.0), module_nodes);
        }
        Ok(Program { modules, nodes })
    }

    pub(crate) fn with_resolved<R>(&self, use_program: impl for<'src> FnOnce(ResolvedModules<'src>) -> Result<R>) -> Result<R> {
        let program = self.parse_modules()?;
        let root = ModuleId::new(self.root.expect("source discovery sets a root").0);
        let artifacts = self.artifacts.iter().map(|(id, artifact)| (ModuleId::new(id.0), artifact.clone())).collect();
        let resolved = ResolvedModules::from_program(&program, root, self.edges.clone(), artifacts)?;
        use_program(resolved)
    }
}

fn pin_without_artifact(importer: &Path, path: &str) -> ArgentError {
    ArgentError::at(importer, format!("an `id` pin applies only to a published app artifact, not to `{path}`"))
}

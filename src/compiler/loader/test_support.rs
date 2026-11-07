//! Owned fixtures for tests that need a resolved program across assertions.

use std::path::{Path, PathBuf};

use super::{ModuleId, ResolvedModules, SourceSet};
use crate::error::Result;

pub(crate) fn load_program(root: impl AsRef<Path>) -> Result<ResolvedModules<'static>> {
    parse_program(SourceSet::discover_file(root.as_ref())?)
}

pub(crate) fn load_inline_program(root: PathBuf, source: String) -> Result<ResolvedModules<'static>> {
    parse_program(SourceSet::discover_inline(root, source)?)
}

pub(crate) fn load_app_graph(root: impl AsRef<Path>, app: &str) -> Result<crate::compiler::app_graph::AppGraphPlan<'static>> {
    crate::compiler::app_graph::AppGraphPlan::new(load_program(root)?, app)
}

fn parse_program(sources: SourceSet) -> Result<ResolvedModules<'static>> {
    // Fixtures outlive their local discovery scope; parsing and resolution are
    // identical to SourceSet::with_resolved after the sources are retained.
    let sources = Box::leak(Box::new(sources));
    let program = sources.parse_modules()?;
    let root = ModuleId::new(sources.root.expect("source discovery sets a root").0);
    let program = Box::leak(Box::new(program));
    let artifacts = sources.artifacts.iter().map(|(id, artifact)| (ModuleId::new(id.0), artifact.clone())).collect();
    ResolvedModules::from_program(program, root, sources.edges.clone(), artifacts)
}

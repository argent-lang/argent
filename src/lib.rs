//! Public compilation and runtime facades for Argent applications.
//!
//! Compiler internals remain private behind file and inline build operations.

use std::{collections::BTreeMap, path::Path};

use compiler::loader;

pub mod artifact;
pub mod builder;
pub mod codec;
mod compiler;
pub mod error;
pub mod inspect;
pub mod routing;

pub use argent_genesis as genesis;
pub use error::{ArgentError, Result};

/// Artifacts compiled together from one source-app dependency graph.
#[derive(Debug, Clone)]
pub struct CompiledAppBundle {
    primary_app: String,
    artifacts: BTreeMap<String, artifact::Artifact>,
}

impl CompiledAppBundle {
    pub fn primary(&self) -> &artifact::Artifact {
        self.artifacts.get(&self.primary_app).expect("compiled bundle contains its primary app")
    }

    pub fn app(&self, app: &str) -> Option<&artifact::Artifact> {
        self.artifacts.get(app)
    }

    pub fn apps(&self) -> impl Iterator<Item = (&str, &artifact::Artifact)> {
        self.artifacts.iter().map(|(app, artifact)| (app.as_str(), artifact))
    }

    /// Build the runtime bundle with the selected app as its primary artifact.
    pub fn runtime_bundle(&self) -> builder::BuilderResult<builder::ArtifactBundle<'_>> {
        let mut bundle = builder::ArtifactBundle::new(self.primary())?;
        for (app, artifact) in &self.artifacts {
            if app != &self.primary_app {
                bundle = bundle.with_artifact(artifact)?;
            }
        }
        Ok(bundle)
    }

    fn into_primary(mut self) -> artifact::Artifact {
        self.artifacts.remove(&self.primary_app).expect("compiled bundle contains its primary app")
    }
}

/// Compile an inline Argent source string and return its artifact.
///
/// `source_label` is used for diagnostics and module identity; it does not
/// need to exist on disk.
pub fn compile_inline(source_label: impl AsRef<Path>, source: impl Into<String>) -> Result<artifact::Artifact> {
    let nonce =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|duration| duration.as_nanos()).unwrap_or_default();
    let out_dir = std::env::temp_dir().join(format!("argent-inline-{}-{nonce}", std::process::id()));
    let artifact = build_inline(source_label, source, &out_dir);
    if artifact.is_ok() {
        let _ = std::fs::remove_dir_all(&out_dir);
    }
    artifact
}

/// Build an inline Argent source string into `out_dir` and return its artifact.
///
/// This writes the same generated files as `argentc build`, including
/// `artifact.json`, `manifest.json`, and generated Silverscript contracts.
pub fn build_inline(
    source_label: impl AsRef<Path>,
    source: impl Into<String>,
    out_dir: impl AsRef<Path>,
) -> Result<artifact::Artifact> {
    let source_label = source_label.as_ref().to_path_buf();
    loader::SourceSet::discover_inline(source_label, source.into())?.with_resolved(|program| {
        let model = compiler::model::AppCompilationContext::from_resolved(
            &program,
            None,
            &BTreeMap::new(),
            &compiler::model::default_route_planner,
        )?;
        compiler::codegen::emit_build_model(&program, &model, out_dir.as_ref())?;
        read_artifact(out_dir.as_ref())
    })
}

/// Build a file-backed Argent app into `out_dir` and return its artifact.
///
/// This is the library equivalent of `argentc build <app.ag> --out <dir>`.
/// Imports are resolved relative to the input file.
pub fn build_file(input: impl AsRef<Path>, out_dir: impl AsRef<Path>) -> Result<artifact::Artifact> {
    let input = input.as_ref();
    let out_dir = out_dir.as_ref();
    loader::SourceSet::discover_file(input)?.with_resolved(|program| {
        let root = program.root_module();
        if let [app] = root.apps.as_slice() {
            let app_name = app.name.clone();
            return Ok(build_app_graph(loader::plan_app_graph(program, &app_name)?, &app_name, out_dir)?.into_primary());
        }
        let model = compiler::model::AppCompilationContext::from_resolved(
            &program,
            None,
            &BTreeMap::new(),
            &compiler::model::default_route_planner,
        )?;
        compiler::codegen::emit_build_model(&program, &model, out_dir)?;
        read_artifact(out_dir)
    })
}

/// Build one named app from a file that declares multiple apps.
///
/// Only apps declared in the input file are selectable. A module import that
/// declares another app keeps its shared source declarations available and
/// exposes that app through `App::Actor`. Explicit app imports also form
/// separate app dependencies.
pub fn build_file_app(input: impl AsRef<Path>, app_name: &str, out_dir: impl AsRef<Path>) -> Result<artifact::Artifact> {
    Ok(build_file_app_bundle(input, app_name, out_dir)?.into_primary())
}

/// Compile one source app and all source-backed app dependencies.
///
/// Dependencies are compiled once in dependency order. Their generated files
/// are written below `out_dir/apps/<AppName>`. The selected app keeps the
/// existing `out_dir` layout.
pub fn build_file_app_bundle(input: impl AsRef<Path>, app_name: &str, out_dir: impl AsRef<Path>) -> Result<CompiledAppBundle> {
    loader::SourceSet::discover_file(input.as_ref())?
        .with_resolved(|program| build_app_graph(loader::plan_app_graph(program, app_name)?, app_name, out_dir.as_ref()))
}

/// Compile the only app declared in a source file, including its dependencies.
///
/// Use [`build_file_app_bundle`] when the file declares more than one app.
pub fn build_file_bundle(input: impl AsRef<Path>, out_dir: impl AsRef<Path>) -> Result<CompiledAppBundle> {
    let input = input.as_ref();
    loader::SourceSet::discover_file(input)?.with_resolved(|program| {
        let [app] = program.root_module().apps.as_slice() else {
            return Err(ArgentError::at(input, "expected exactly one app in the source file; select an app with --app"));
        };
        let app_name = app.name.clone();
        build_app_graph(loader::plan_app_graph(program, &app_name)?, &app_name, out_dir.as_ref())
    })
}

fn build_app_graph(apps: compiler::app_graph::AppGraphPlan<'_>, app_name: &str, out_dir: &Path) -> Result<CompiledAppBundle> {
    let dependency_dir = out_dir.join("apps");
    if dependency_dir.exists() {
        std::fs::remove_dir_all(&dependency_dir)?;
    }

    let mut artifacts = BTreeMap::<String, artifact::Artifact>::new();
    let mut origins = BTreeMap::new();
    for (index, unit) in apps.units().iter().enumerate() {
        let source_app = &unit.source_app;
        let dependencies = &unit.dependencies;
        let program = apps.program_for(unit);
        if let Some(artifact) = program.artifact(unit.root) {
            // A pinned artifact import: the loader checked the artifact and its id, so link it as published.
            let app_out = dependency_dir.join(&source_app.app);
            std::fs::create_dir_all(&app_out)?;
            let json = serde_json::to_string_pretty(artifact).map_err(|err| ArgentError::new(err.to_string()))?;
            std::fs::write(app_out.join("artifact.json"), json)?;
            origins.insert(source_app.app.clone(), compiler::model::link::artifact_origins(&source_app.source, artifact));
            if artifacts.insert(source_app.app.clone(), artifact.clone()).is_some() {
                return Err(ArgentError::new(format!(
                    "app name `{}` occurs more than once in the compiled dependency graph",
                    source_app.app
                )));
            }
            continue;
        }
        let linked = dependencies
            .iter()
            .map(|dependency| {
                artifacts
                    .get(&dependency.app)
                    .map(|artifact| {
                        (
                            dependency.app.clone(),
                            compiler::model::link::LinkedDependency { artifact, origins: &origins[&dependency.app] },
                        )
                    })
                    // shouldn't happen as ordered by dependency order
                    .ok_or_else(|| {
                        ArgentError::new(format!("app `{}` dependency `{}` was not compiled first", source_app.app, dependency.app))
                    })
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        let app_out = if index + 1 == apps.units().len() { out_dir.to_path_buf() } else { dependency_dir.join(&source_app.app) };
        let model = compiler::model::AppCompilationContext::from_resolved(
            &program,
            Some(&source_app.app),
            &linked,
            &compiler::model::default_route_planner,
        )?;
        let app_origins = model.declaration_origins.clone();
        compiler::codegen::emit_build_model(&program, &model, &app_out)?;
        origins.insert(source_app.app.clone(), app_origins);
        let artifact = read_artifact(&app_out)?;
        if artifacts.insert(source_app.app.clone(), artifact).is_some() {
            return Err(ArgentError::new(format!(
                "app name `{}` occurs more than once in the compiled dependency graph",
                source_app.app
            )));
        }
    }

    Ok(CompiledAppBundle { primary_app: app_name.to_string(), artifacts })
}

#[cfg(test)]
mod tests;

fn read_artifact(out_dir: &Path) -> Result<artifact::Artifact> {
    let path = out_dir.join("artifact.json");
    let json = std::fs::read_to_string(&path)?;
    serde_json::from_str(&json).map_err(|err| ArgentError::at(path, err.to_string()))
}

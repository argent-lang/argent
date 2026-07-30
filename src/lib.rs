use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

pub mod artifact;
pub mod ast;
pub mod builder;
pub mod codec;
pub mod emit;
pub mod error;
pub mod inspect;
mod language;
pub mod lexer;
mod link;
pub mod loader;
mod model;
mod naming;
pub mod parser;
pub mod routes;
pub mod routing;
mod stdlib;
mod token_refs;

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
    let program = inline_program(source_label, source.into())?;
    emit::emit_build(&program, out_dir.as_ref())?;
    read_artifact(out_dir.as_ref())
}

/// Build a file-backed Argent app into `out_dir` and return its artifact.
///
/// This is the library equivalent of `argentc build <app.ag> --out <dir>`.
/// Imports are resolved relative to the input file.
pub fn build_file(input: impl AsRef<Path>, out_dir: impl AsRef<Path>) -> Result<artifact::Artifact> {
    let input = input.as_ref();
    let out_dir = out_dir.as_ref();
    let program = loader::load_program(input)?;
    let root = program.modules.iter().find(|module| module.path == program.root).expect("loaded program contains its root module");
    if let [app] = root.apps.as_slice() {
        let app_name = app.name.clone();
        return Ok(build_app_graph(loader::plan_app_graph(program, &app_name)?, &app_name, out_dir)?.into_primary());
    }
    emit::emit_build(&program, out_dir)?;
    read_artifact(out_dir)
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
    let apps = loader::load_app_graph(input.as_ref(), app_name)?;
    build_app_graph(apps, app_name, out_dir.as_ref())
}

fn build_app_graph(
    apps: Vec<(loader::SourceApp, Vec<loader::SourceApp>, ast::Program)>,
    app_name: &str,
    out_dir: &Path,
) -> Result<CompiledAppBundle> {
    let dependency_dir = out_dir.join("apps");
    if dependency_dir.exists() {
        std::fs::remove_dir_all(&dependency_dir)?;
    }

    let mut artifacts = BTreeMap::<String, artifact::Artifact>::new();
    for (index, (source_app, dependencies, program)) in apps.iter().enumerate() {
        let linked = dependencies
            .iter()
            .map(|dependency| {
                artifacts.get(&dependency.app).map(|artifact| (dependency.app.clone(), artifact)).ok_or_else(|| {
                    ArgentError::new(format!("app `{}` dependency `{}` was not compiled first", source_app.app, dependency.app))
                })
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        let app_out = if index + 1 == apps.len() { out_dir.to_path_buf() } else { dependency_dir.join(&source_app.app) };
        emit::emit_build_app_linked(program, &source_app.app, &linked, &app_out)?;
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

fn inline_program(source_label: PathBuf, source: String) -> Result<ast::Program> {
    loader::load_inline_program(source_label, source)
}

fn read_artifact(out_dir: &Path) -> Result<artifact::Artifact> {
    let path = out_dir.join("artifact.json");
    let json = std::fs::read_to_string(&path)?;
    serde_json::from_str(&json).map_err(|err| ArgentError::at(path, err.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const COUNTER_APP: &str = r#"
state CounterState {
    int count;
}

actor Counter owns CounterState {
    entry bump(int delta) emits next: Counter {
        unrestricted(next.value);
        CounterState next = {
            count: count + delta,
        };

        become next <- Counter(next);
    }
}

app CounterApp {
    actor Counter;
}
"#;

    const INVOCATION_UID_APP: &str = r#"
import "std::core";

state IssuerState {
    byte[32] last_uid;
}

actor Issuer owns IssuerState {
    entry issue(byte[] domain) emits next: Issuer {
        unrestricted(next.value);
        byte[32] uid = invocation_uid(domain);
        require(uid == invocation_uid(domain));

        IssuerState next = {
            last_uid: uid,
        };
        become next <- Issuer(next);
    }
}

app IssuerApp {
    actor Issuer;
}
"#;

    const TWO_APPS: &str = r#"
state LeftState {
    int amount;
}

actor Left owns LeftState {
    entry bump() emits next: Left {
        unrestricted(next.value);
        LeftState next = {
            amount: amount + 1,
        };
        become next <- Left(next);
    }
}

state RightState {
    int amount;
}

actor Right owns RightState {
    entry bump() emits next: Right {
        unrestricted(next.value);
        RightState next = {
            amount: amount + 1,
        };
        become next <- Right(next);
    }
}

actor RightAlt owns RightState {
    entry bump() emits next: RightAlt {
        unrestricted(next.value);
        RightState next = {
            amount: amount + 1,
        };
        become next <- RightAlt(next);
    }
}

actor enum RightKind {
    Right;
    RightAlt;
}

app LeftApp {
    actor Left;
}

app RightApp {
    actor Right;
    actor RightAlt;
}
"#;

    #[test]
    fn compile_inline_returns_artifact_without_a_user_output_dir() {
        let artifact = compile_inline("counter.ag", COUNTER_APP).expect("inline app compiles");
        assert_eq!(artifact.app, "CounterApp");
        assert!(artifact.sil_abi.contract("Counter").is_some());
    }

    #[test]
    fn build_inline_writes_outputs_and_returns_artifact() {
        let out_dir = std::env::temp_dir().join(format!("argent-build-inline-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out_dir);

        let artifact = build_inline("counter.ag", COUNTER_APP, &out_dir).expect("inline app builds");

        assert_eq!(artifact.app, "CounterApp");
        assert!(out_dir.join("artifact.json").exists());
        assert!(out_dir.join("sil").join("Counter.sil").exists());

        let _ = std::fs::remove_dir_all(out_dir);
    }

    #[test]
    fn build_inline_loads_explicit_standard_module() {
        let out_dir = std::env::temp_dir().join(format!("argent-build-inline-std-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out_dir);

        let artifact = build_inline("issuer.ag", INVOCATION_UID_APP, &out_dir).expect("inline app imports std::core");
        let sil = std::fs::read_to_string(out_dir.join("sil/Issuer.sil")).expect("generated Issuer Sil exists");

        assert!(artifact.modules.iter().any(|module| module == "std::core"));
        assert!(sil.contains("function invocation_uid(byte[] domain) : byte[32]"), "{sil}");
        assert!(sil.contains("return blake2bWithKey(outpoint, domain);"), "{sil}");
        assert!(sil.contains("byte[32] uid = invocation_uid(domain);"), "{sil}");

        let _ = std::fs::remove_dir_all(out_dir);
    }

    #[test]
    fn build_inline_does_not_load_standard_module_without_import() {
        let out_dir = std::env::temp_dir().join(format!("argent-build-inline-no-std-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out_dir);
        let source = INVOCATION_UID_APP.replace("import \"std::core\";", "");

        let error = build_inline("issuer.ag", source, &out_dir).expect_err("standard function requires an explicit import");
        assert!(error.to_string().contains("failed to compile"), "unexpected error: {error}");

        let _ = std::fs::remove_dir_all(out_dir);
    }

    #[test]
    fn build_file_writes_outputs_and_returns_artifact() {
        let temp = std::env::temp_dir().join(format!("argent-build-file-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(&temp).expect("temp dir created");

        let input = temp.join("counter.ag");
        let out_dir = temp.join("build");
        std::fs::write(&input, COUNTER_APP).expect("source written");

        let artifact = build_file(&input, &out_dir).expect("file app builds");

        assert_eq!(artifact.app, "CounterApp");
        assert!(out_dir.join("artifact.json").exists());
        assert!(out_dir.join("sil").join("Counter.sil").exists());

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn build_file_loads_explicit_standard_module() {
        let temp = std::env::temp_dir().join(format!("argent-build-file-std-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(&temp).expect("temp dir created");

        let input = temp.join("issuer.ag");
        let out_dir = temp.join("build");
        std::fs::write(&input, INVOCATION_UID_APP).expect("source written");

        let artifact = build_file(&input, &out_dir).expect("file app imports std::core");
        assert!(artifact.modules.iter().any(|module| module == "std::core"));
        assert!(out_dir.join("sil/Issuer.sil").exists());

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn build_file_app_selects_one_root_app() {
        let temp = std::env::temp_dir().join(format!("argent-build-file-app-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(&temp).expect("temp dir created");

        let input = temp.join("pair.ag");
        std::fs::write(&input, TWO_APPS).expect("source written");

        let left = build_file_app(&input, "LeftApp", temp.join("left")).expect("left app builds");
        assert_eq!(left.app, "LeftApp");
        assert!(left.sil_abi.contract("Left").is_some());
        assert!(left.sil_abi.contract("Right").is_none());

        let right = build_file_app(&input, "RightApp", temp.join("right")).expect("right app builds");
        assert_eq!(right.app, "RightApp");
        assert!(right.sil_abi.contract("Right").is_some());
        assert!(right.sil_abi.contract("RightAlt").is_some());
        assert!(right.sil_abi.contract("Left").is_none());
        assert!(right.argent.actor_enums.iter().any(|actor_enum| actor_enum.name == "RightKind"));

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn app_qualified_actor_imports_keep_each_selected_app_compilation() {
        let temp = std::env::temp_dir().join(format!("argent-app-qualified-actor-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(&temp).expect("temp dir created");

        std::fs::write(
            temp.join("shared.ag"),
            r#"
state SharedState {
    int count;
}

actor Shared owns SharedState {
    entry merge()
    consumes {
        other: Shared,
    }
    emits next: Shared {
        unrestricted(next.value);
        SharedState next = {
            count: count + other.count,
        };
        become next <- Shared(next);
    }
}

state GuardState {}

actor Guard owns GuardState {
    entry hold() emits next: Guard {
        unrestricted(next.value);
        become next <- Guard(self.state);
    }
}

app SoloApp {
    actor Shared;
}

app CohortApp {
    actor Shared;
    actor Guard;
}
"#,
        )
        .expect("shared actor source written");

        std::fs::write(
            temp.join("controller.ag"),
            r#"
import actor SoloApp::Shared from "./shared.ag";
import app CohortApp from "./shared.ag";

state CtrlState {}

actor Ctrl owns CtrlState {
    entry inspect(cov_id solo_id, cov_id cohort_id)
    observes solo by solo_id {
        inputs {
            src: Shared,
        }
    }
    observes cohort by cohort_id {
        inputs {
            src: CohortApp::Shared,
        }
    }
    emits none {
        SharedState solo_state = solo.inputs.src.state;
        SharedState cohort_state = cohort.inputs.src.state;
        require(solo_state.count >= 0);
        require(cohort_state.count >= 0);
    }
}

app CtrlApp {
    actor Ctrl;
}
"#,
        )
        .expect("controller source written");

        let compiled = build_file_app_bundle(temp.join("controller.ag"), "CtrlApp", temp.join("build"))
            .expect("both app-qualified identities compile in one bundle");
        let solo_actor = compiled
            .app("SoloApp")
            .expect("solo dependency exists")
            .argent
            .template_plan
            .templates
            .iter()
            .find(|template| template.actor == "Shared")
            .expect("solo Shared template exists");
        let cohort_actor = compiled
            .app("CohortApp")
            .expect("cohort dependency exists")
            .argent
            .template_plan
            .templates
            .iter()
            .find(|template| template.actor == "Shared")
            .expect("cohort Shared template exists");
        assert_ne!(
            solo_actor.sil_template_hash, cohort_actor.sil_template_hash,
            "one source actor must compile in each selected app context"
        );
        assert_ne!(
            solo_actor.actor_type_handle.template.hash_hex, cohort_actor.actor_type_handle.template.hash_hex,
            "each app-qualified actor must export its own handle"
        );

        let ctrl_script = &compiled.primary().sil_abi.contract("Ctrl").expect("controller contract exists").compiled.script_hex;
        assert_eq!(ctrl_script.matches(&solo_actor.actor_type_handle.template.hash_hex).count(), 1);
        assert_eq!(ctrl_script.matches(&cohort_actor.actor_type_handle.template.hash_hex).count(), 1);

        let solo_sil = std::fs::read_to_string(temp.join("build/apps/SoloApp/sil/Shared.sil")).expect("solo Shared Sil exists");
        let cohort_sil = std::fs::read_to_string(temp.join("build/apps/CohortApp/sil/Shared.sil")).expect("cohort Shared Sil exists");
        assert!(solo_sil.contains("State other = readInputState("), "{solo_sil}");
        assert!(cohort_sil.contains("State other = readInputStateWithTemplate("), "{cohort_sil}");

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn build_file_app_removes_stale_selected_app_contracts() {
        let temp = std::env::temp_dir().join(format!("argent-build-file-app-clean-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(&temp).expect("temp dir created");

        let input = temp.join("pair.ag");
        let out_dir = temp.join("build");
        std::fs::write(&input, TWO_APPS).expect("source written");

        build_file_app(&input, "LeftApp", &out_dir).expect("left app builds");
        assert!(out_dir.join("sil/Left.sil").exists());

        build_file_app(&input, "RightApp", &out_dir).expect("right app builds");
        assert!(!out_dir.join("sil/Left.sil").exists());
        assert!(out_dir.join("sil/Right.sil").exists());
        assert!(out_dir.join("sil/RightAlt.sil").exists());

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn app_bundle_compiles_transitive_imports_from_dependency_artifacts() {
        let temp = std::env::temp_dir().join(format!("argent-build-transitive-apps-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(&temp).expect("temp dir created");

        std::fs::write(
            temp.join("leaf.ag"),
            r#"
state LeafState {
    int n;
}

actor Leaf owns LeafState {
    entry update() emits next: Leaf {
        unrestricted(next.value);
        LeafState next = {
            n: n + 1,
        };
        become next <- Leaf(next);
    }
}

app LeafApp {
    actor Leaf;
}
"#,
        )
        .expect("leaf source written");
        std::fs::write(
            temp.join("middle.ag"),
            r#"
import actor LeafApp::Leaf from "./leaf.ag";

state MiddleState {
    int n;
}

actor Middle owns MiddleState {
    entry update(cov_id leaf_id)
    observes leaf by leaf_id {
        inputs {
            src: Leaf,
        }
        outputs {
            next: Leaf,
        }
    }
    emits next: Middle {
        unrestricted(next.value);
        LeafState next_leaf = leaf.inputs.src.state;
        require leaf.outputs become {
            next <- Leaf(next_leaf),
        };
        MiddleState next = {
            n: n + 1,
        };
        become next <- Middle(next);
    }
}

app MiddleApp {
    actor Middle;
}
"#,
        )
        .expect("middle source written");
        std::fs::write(
            temp.join("root.ag"),
            r#"
import actor MiddleApp::Middle from "./middle.ag";

state RootState {
    int n;
}

actor Root owns RootState {
    entry update(cov_id middle_id)
    observes middle by middle_id {
        inputs {
            src: Middle,
        }
        outputs {
            next: Middle,
        }
    }
    emits next: Root {
        unrestricted(next.value);
        MiddleState next_middle = middle.inputs.src.state;
        require middle.outputs become {
            next <- Middle(next_middle),
        };
        RootState next = {
            n: n + 1,
        };
        become next <- Root(next);
    }
}

app RootApp {
    actor Root;
}
"#,
        )
        .expect("root source written");

        let out_dir = temp.join("build");
        let compiled =
            build_file_app_bundle(temp.join("root.ag"), "RootApp", &out_dir).expect("transitive app dependency graph compiles");

        assert_eq!(compiled.apps().map(|(app, _)| app).collect::<Vec<_>>(), ["LeafApp", "MiddleApp", "RootApp"]);
        assert!(out_dir.join("apps/LeafApp/artifact.json").is_file());
        assert!(out_dir.join("apps/MiddleApp/artifact.json").is_file());
        let leaf = compiled.app("LeafApp").expect("bundle contains LeafApp");
        let middle = compiled.app("MiddleApp").expect("bundle contains MiddleApp");
        assert!(leaf.dependencies.is_empty());
        assert_eq!(
            middle.dependencies,
            [artifact::AppDependencyArtifact { app: "LeafApp".to_string(), artifact_id: leaf.id.clone() }]
        );
        assert_eq!(
            compiled.primary().dependencies,
            [artifact::AppDependencyArtifact { app: "MiddleApp".to_string(), artifact_id: middle.id.clone() }]
        );
        let middle_handle = compiled
            .app("MiddleApp")
            .expect("bundle contains MiddleApp")
            .argent
            .template_plan
            .templates
            .iter()
            .find(|template| template.actor == "Middle")
            .map(|template| &template.actor_type_handle)
            .expect("Middle exports a source-state handle that contains its Leaf dependency");
        let root_script = &compiled.primary().sil_abi.contract("Root").expect("Root contract exists").compiled.script_hex;
        assert_eq!(
            root_script.matches(&middle_handle.template.hash_hex).count(),
            1,
            "Root embeds the exported Middle source-state template once"
        );
        let runtime_bundle = compiled.runtime_bundle().expect("all transitive artifacts form one runtime bundle");
        builder::TxBuilder::from_bundle(&runtime_bundle).expect("all direct dependency ids match transitively");

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn app_bundle_compiles_a_diamond_dependency_once() {
        let temp = std::env::temp_dir().join(format!("argent-build-diamond-apps-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(&temp).expect("temp dir created");

        std::fs::write(
            temp.join("shared.ag"),
            r#"
state SharedState {
    int n;
}

actor Shared owns SharedState {
    entry hold() emits none {
        require(n >= 0);
    }
}

app SharedApp {
    actor Shared;
}
"#,
        )
        .expect("shared source written");
        std::fs::write(
            temp.join("left.ag"),
            r#"
import app SharedApp from "./shared.ag";

state LeftState {
    int n;
}

actor Left owns LeftState {
    entry inspect(cov_id shared_id)
    observes shared by shared_id {
        inputs {
            src: SharedApp::Shared,
        }
    }
    emits none {
        SharedState current = shared.inputs.src.state;
        require(current.n >= 0);
    }
}

app LeftApp {
    actor Left;
}
"#,
        )
        .expect("left source written");
        std::fs::write(
            temp.join("right.ag"),
            r#"
import actor SharedApp::Shared from "./shared.ag";

state RightState {
    byte tag;
}

actor Right owns RightState {
    entry inspect(cov_id shared_id)
    observes shared by shared_id {
        inputs {
            src: Shared,
        }
    }
    emits none {
        SharedState current = shared.inputs.src.state;
        require(current.n >= 0);
    }
}

app RightApp {
    actor Right;
}
"#,
        )
        .expect("right source written");
        std::fs::write(
            temp.join("root.ag"),
            r#"
import app LeftApp from "./left.ag";
import actor RightApp::Right from "./right.ag";

state RootState {}

actor Root owns RootState {
    entry inspect(cov_id left_id, cov_id right_id)
    observes left by left_id {
        inputs {
            src: LeftApp::Left,
        }
    }
    observes right by right_id {
        inputs {
            src: Right,
        }
    }
    emits none {
        require(1 == 1);
    }
}

app RootApp {
    actor Root;
}
"#,
        )
        .expect("root source written");

        let out_dir = temp.join("build");
        let compiled =
            build_file_app_bundle(temp.join("root.ag"), "RootApp", &out_dir).expect("diamond app dependency graph compiles");
        assert_eq!(compiled.apps().count(), 4);
        for app in ["SharedApp", "LeftApp", "RightApp", "RootApp"] {
            assert!(compiled.app(app).is_some(), "compiled bundle is missing `{app}`");
        }
        assert!(out_dir.join("apps/SharedApp/artifact.json").is_file());
        assert!(out_dir.join("apps/LeftApp/artifact.json").is_file());
        assert!(out_dir.join("apps/RightApp/artifact.json").is_file());

        let shared_handle = compiled
            .app("SharedApp")
            .expect("shared dependency exists")
            .argent
            .template_plan
            .templates
            .iter()
            .find(|template| template.actor == "Shared")
            .map(|template| template.actor_type_handle.template.hash_hex.as_str())
            .expect("shared actor handle exists");
        for (app, actor) in [("LeftApp", "Left"), ("RightApp", "Right")] {
            let branch_script = &compiled
                .app(app)
                .expect("diamond branch artifact exists")
                .sil_abi
                .contract(actor)
                .expect("diamond branch contract exists")
                .compiled
                .script_hex;
            assert_eq!(branch_script.matches(shared_handle).count(), 1);
        }
        compiled.runtime_bundle().expect("all four diamond artifacts form one runtime bundle");

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn module_import_links_qualified_app_actors_and_shares_constants() {
        let temp = std::env::temp_dir().join(format!("argent-build-module-app-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(&temp).expect("temp dir created");

        std::fs::write(
            temp.join("asset.ag"),
            r#"
const byte ASSET_TAG = 0x01;

state AssetState {
    byte tag;
}

actor Asset owns AssetState {
    entry keep() emits next: Asset {
        unrestricted(next.value);
        become next <- Asset(self.state);
    }
}

app AssetApp {
    actor Asset;
}
"#,
        )
        .expect("asset source written");
        std::fs::write(
            temp.join("controller.ag"),
            r#"
import "./asset.ag";

state ControllerState {
    cov_id asset_id;
}

actor Controller owns ControllerState {
    entry update()
    observes asset by self.asset_id {
        inputs {
            src: AssetApp::Asset,
        }
        outputs {
            next: AssetApp::Asset,
        }
    }
    emits next: Controller {
        unrestricted(next.value);
        AssetState current = asset.inputs.src.state;
        require(current.tag == ASSET_TAG);
        require asset.outputs become {
            next <- AssetApp::Asset(current),
        };
        become next <- Controller(self.state);
    }
}

app ControllerApp {
    actor Controller;
}
"#,
        )
        .expect("controller source written");

        let out_dir = temp.join("build");
        let compiled =
            build_file_app_bundle(temp.join("controller.ag"), "ControllerApp", &out_dir).expect("module app dependency compiles");

        assert_eq!(compiled.apps().map(|(app, _)| app).collect::<Vec<_>>(), ["AssetApp", "ControllerApp"]);
        assert!(out_dir.join("apps/AssetApp/artifact.json").is_file());
        let controller_sil = std::fs::read_to_string(out_dir.join("sil/Controller.sil")).expect("controller Sil exists");
        assert!(controller_sil.contains("byte constant ASSET_TAG = 0x01;"), "{controller_sil}");
        let observed = &compiled
            .primary()
            .argent
            .actors
            .iter()
            .find(|actor| actor.name == "Controller")
            .expect("controller artifact exists")
            .entries[0]
            .observes[0]
            .inputs[0];
        assert!(matches!(
            &observed.target,
            artifact::ObservedTargetArtifact::StaticActor { app, actor }
                if app == "AssetApp" && actor == "Asset"
        ));

        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn build_file_requires_selection_for_multiple_root_apps() {
        let temp = std::env::temp_dir().join(format!("argent-build-file-ambiguous-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(&temp).expect("temp dir created");

        let input = temp.join("pair.ag");
        std::fs::write(&input, TWO_APPS).expect("source written");

        let error = build_file(&input, temp.join("build")).expect_err("app selection is required");
        assert!(error.to_string().contains("select one with `--app <name>`"));

        let _ = std::fs::remove_dir_all(temp);
    }
}

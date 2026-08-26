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
fn compile_inline_supports_helpers_without_return_types() {
    let source = r#"
        fn authorize(int value) {
            require(value > 0);
        }

        state CounterState {
            int count;
        }

        actor Counter owns CounterState {
            entry bump(int delta) emits next: Counter {
                authorize(delta);
                unrestricted(next.value);
                CounterState next_state = {
                    count: count + delta,
                };
                become next <- Counter(next_state);
            }
        }

        app CounterApp {
            actor Counter;
        }
    "#;

    let artifact = compile_inline("void-helper.ag", source).expect("void helper compiles through Silverscript");
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
    assert!(sil.contains("function invocation_uid(byte[] gen__glob_domain) : byte[32]"), "{sil}");
    assert!(sil.contains("return blake2bWithKey(byte[](gen__glob_outpoint), gen__glob_domain);"), "{sil}");
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
        become next <- self;
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
    let compiled = build_file_app_bundle(temp.join("root.ag"), "RootApp", &out_dir).expect("transitive app dependency graph compiles");

    assert_eq!(compiled.apps().map(|(app, _)| app).collect::<Vec<_>>(), ["LeafApp", "MiddleApp", "RootApp"]);
    assert!(out_dir.join("apps/LeafApp/artifact.json").is_file());
    assert!(out_dir.join("apps/MiddleApp/artifact.json").is_file());
    let leaf = compiled.app("LeafApp").expect("bundle contains LeafApp");
    let middle = compiled.app("MiddleApp").expect("bundle contains MiddleApp");
    assert!(leaf.dependencies.is_empty());
    assert_eq!(middle.dependencies, [artifact::AppDependencyArtifact { app: "LeafApp".to_string(), artifact_id: leaf.id.clone() }]);
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
    let compiled = build_file_app_bundle(temp.join("root.ag"), "RootApp", &out_dir).expect("diamond app dependency graph compiles");
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
        become next <- self;
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
        become next <- self;
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
    let observed =
        &compiled.primary().argent.actors.iter().find(|actor| actor.name == "Controller").expect("controller artifact exists").entries
            [0]
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

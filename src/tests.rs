use super::*;

const COUNTER_APP: &str = r#"
state CounterState {
    int count;
}

actor Counter owns CounterState {
    entry bump(int delta) emits next: Counter {
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

        IssuerState next_state = {
            last_uid: uid,
        };
        become next <- Issuer(next_state);
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
        LeftState next_state = {
            amount: amount + 1,
        };
        become next <- Left(next_state);
    }
}

state RightState {
    int amount;
}

actor Right owns RightState {
    entry bump() emits next: Right {
        unrestricted(next.value);
        RightState next_state = {
            amount: amount + 1,
        };
        become next <- Right(next_state);
    }
}

actor RightAlt owns RightState {
    entry bump() emits next: RightAlt {
        unrestricted(next.value);
        RightState next_state = {
            amount: amount + 1,
        };
        become next <- RightAlt(next_state);
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
        SharedState next_state = {
            count: count + other.count,
        };
        become next <- Shared(next_state);
    }
}

state GuardState {
    int marker;
}

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

state CtrlState {
    int marker;
}

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
        SharedState solo_state = state(solo.inputs.src);
        SharedState cohort_state = state(cohort.inputs.src);
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
    assert!(solo_sil.contains("State gen__other_state = readInputState("), "{solo_sil}");
    assert!(cohort_sil.contains("State gen__other_state = readInputStateWithTemplate("), "{cohort_sil}");

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
        LeafState next_state = {
            n: n + 1,
        };
        become next <- Leaf(next_state);
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
            leaf_output: Leaf,
        }
    }
    emits next: Middle {
        unrestricted(next.value);
        LeafState next_leaf = state(leaf.inputs.src);
        require leaf.outputs become {
            leaf_output <- Leaf(next_leaf),
        };
        MiddleState next_state = {
            n: n + 1,
        };
        become next <- Middle(next_state);
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
            middle_output: Middle,
        }
    }
    emits next: Root {
        unrestricted(next.value);
        MiddleState next_middle = state(middle.inputs.src);
        require middle.outputs become {
            middle_output <- Middle(next_middle),
        };
        RootState next_state = {
            n: n + 1,
        };
        become next <- Root(next_state);
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
fn linked_authored_state_declarations_follow_the_contract_value_plan() {
    let temp = std::env::temp_dir().join(format!("argent-linked-authored-state-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(&temp).expect("temp dir created");

    std::fs::write(
        temp.join("child.ag"),
        r#"
state ChildStorage {
    int amount;
    virtual detail;
}

state ChildDetail {
    int count;
}

state ChildState expands ChildStorage {
    detail: ChildDetail;
}

state IdentityState {
    int amount;
}

actor Child owns ChildState {
    entry hold() emits none {
        require(amount >= 0);
    }
}

actor Identity owns IdentityState {
    entry hold() emits none {
        require(amount >= 0);
    }
}

app ChildApp {
    actor Child;
    actor Identity;
}
"#,
    )
    .expect("child source written");

    let cases = [
        (
            "entry-params",
            "",
            "",
            r#"
    entry hold(ChildState scalar, ChildState[2] fixed, ChildState[] dynamic) emits none {
        require(scalar.amount >= 0);
        require(fixed[0].amount >= 0);
        require(dynamic.length >= 0);
    }
"#,
            ["ChildState", "ChildDetail"].as_slice(),
            ["ChildState scalar", "ChildState[2] fixed", "ChildState[] dynamic"].as_slice(),
        ),
        (
            "function-signatures",
            r#"
fn global_amount(ChildState value) -> int {
    return value.amount;
}
"#,
            r#"
    fn actor_amount(ChildState value) -> int {
        return value.amount;
    }
"#,
            r#"
    entry hold() emits none {
        require(global_amount(ChildState { amount: 1, detail: ChildDetail { count: 1 } }) == 1);
        require(actor_amount(ChildState { amount: 2, detail: ChildDetail { count: 2 } }) == 2);
    }
"#,
            ["ChildState", "ChildDetail"].as_slice(),
            ["function global_amount(ChildState", "function actor_amount(ChildState"].as_slice(),
        ),
        (
            "global-function-body",
            r#"
fn global_amount(int amount) -> int {
    ChildState value = ChildState {
        amount: amount,
        detail: ChildDetail { count: amount },
    };
    return value.amount;
}
"#,
            "",
            r#"
    entry hold() emits none {
        require(global_amount(1) == 1);
    }
"#,
            ["ChildState", "ChildDetail"].as_slice(),
            ["function global_amount(int", "ChildState gen__glob_value = ChildState"].as_slice(),
        ),
        (
            "actor-function-body",
            "",
            r#"
    fn actor_amount(int amount) -> int {
        ChildState value = ChildState {
            amount: amount,
            detail: ChildDetail { count: amount },
        };
        return value.amount;
    }
"#,
            r#"
    entry hold() emits none {
        require(actor_amount(2) == 2);
    }
"#,
            ["ChildState", "ChildDetail"].as_slice(),
            ["function actor_amount(int", "ChildState value = ChildState"].as_slice(),
        ),
        (
            "constant",
            r#"
const ChildState INITIAL_CHILD = ChildState {
    amount: 1,
    detail: ChildDetail { count: 1 },
};
"#,
            "",
            r#"
    entry hold() emits none {
        require(INITIAL_CHILD.amount == 1);
    }
"#,
            ["ChildState", "ChildDetail"].as_slice(),
            ["ChildState constant INITIAL_CHILD"].as_slice(),
        ),
        (
            "body-local",
            "",
            "",
            r#"
    entry hold() emits none {
        ChildState value = ChildState {
            amount: 1,
            detail: ChildDetail { count: 1 },
        };
        require(value.amount == 1);
    }
"#,
            ["ChildState", "ChildDetail"].as_slice(),
            ["ChildState value = ChildState"].as_slice(),
        ),
        (
            "observed-input",
            "",
            "",
            r#"
    entry hold(cov_id child_id)
    observes source by child_id {
        inputs {
            child: ChildApp::Identity,
        }
    }
    emits none {
        IdentityState value = state(source.inputs.child);
        require(value.amount >= 0);
    }
"#,
            ["IdentityState"].as_slice(),
            ["IdentityState value = gen__source_child_state;"].as_slice(),
        ),
        (
            "inline-route",
            "",
            "",
            r#"
    entry send()
    spawns children by child_id {
        outputs {
            child: ChildApp::Child,
        }
    }
    emits none {
        unrestricted(children.outputs.child.value);
        require children.outputs become {
            child <- ChildApp::Child(ChildState {
                amount: 1,
                detail: ChildDetail { count: 1 },
            }),
        };
    }
"#,
            ["ChildState", "ChildDetail"].as_slice(),
            ["spawned become children.child -> ChildApp::Child"].as_slice(),
        ),
    ];

    for (name, global, member, entry, declarations, expected) in cases {
        let source = format!(
            r#"
import app ChildApp from "./child.ag";

{global}

state LocalState {{
    int nonce;
}}

actor Local owns LocalState {{
{member}
{entry}
}}

app LocalApp {{
    actor Local;
}}
"#
        );
        let local_path = temp.join(format!("local-{name}.ag"));
        std::fs::write(&local_path, source).expect("local source written");

        let out_dir = temp.join(format!("build-{name}"));
        build_file_app_bundle(&local_path, "LocalApp", &out_dir)
            .unwrap_or_else(|err| panic!("linked state used only by {name} must compile: {err}"));
        let sil = std::fs::read_to_string(out_dir.join("sil/Local.sil")).expect("Local Sil exists");
        for declaration in declarations {
            assert!(sil.contains(&format!("struct {declaration} {{")), "{name}: {sil}");
        }
        for expected in expected {
            assert!(sil.contains(expected), "{name} is missing `{expected}`: {sil}");
        }
    }

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
        SharedState current = state(shared.inputs.src);
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
        SharedState current = state(shared.inputs.src);
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
            asset_next: AssetApp::Asset,
        }
    }
    emits next: Controller {
        unrestricted(next.value);
        AssetState current = state(asset.inputs.src);
        require(current.tag == ASSET_TAG);
        require asset.outputs become {
            asset_next <- AssetApp::Asset(current),
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

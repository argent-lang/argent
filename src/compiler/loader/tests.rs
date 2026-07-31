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
    assert_eq!(graph.iter().map(|(app, _, _)| app.app.as_str()).collect::<Vec<_>>(), ["SharedApp", "LeftApp", "RightApp", "RootApp"]);
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

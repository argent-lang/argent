use super::*;
use crate::compiler::model::Model;

#[test]
fn source_bindings_respect_lexical_scopes_and_preserve_authored_text() {
    let program = load_inline_program(
        PathBuf::from("scopes.ag"),
        r#"
        const int LIMIT = 1;
        const int PARAM = 2;
        const int INDEX = 3;
        const int PAIR = 4;
        state Item { int LIMIT; }
        fn scoped(int PARAM) -> int {
            int result = LIMIT;
            { int LIMIT = 5; result = result + LIMIT; }
            for (INDEX, 0, 2, 2) { result = result + INDEX; }
            { int left, int PAIR = pair(); result = result + PAIR; }
            Item item = Item { LIMIT: result };
            result = result + item.LIMIT;
            return result + LIMIT + PARAM + INDEX;
        }
    "#
        .to_string(),
    )
    .expect("source resolves");
    let function = program.root_declarations().find(|id| id.kind() == SymbolKind::Function).unwrap();
    let ResolvedDeclaration::Function(authored) = program.declaration(function) else {
        panic!("function expected");
    };
    let text_before = authored.body.clone();
    let references = &program.bindings(function).text[&TextSite::Function(0)];
    let names = references
        .iter()
        .map(|reference| {
            let ResolvedName::Declaration(id) = reference.target else {
                panic!("declaration expected");
            };
            let name = program.declaration(id).name().to_string();
            // reference span correctness
            assert_eq!(&authored.body[reference.span.start..reference.span.end], name);
            name
        })
        .collect::<Vec<_>>();
    assert_eq!(names, ["LIMIT", "Item", "Item", "LIMIT", "INDEX"]);
    let closure = program.declaration_closure([function]);
    assert!(!closure.iter().any(|id| matches!(program.declaration(*id).name(), "PARAM" | "PAIR")));
    assert_eq!(program.root_module().functions[0].body, text_before);
}

#[test]
fn source_bindings_distinguish_numeric_units_from_declaration_references() {
    for unit in ["seconds", "minutes", "hours", "days", "weeks", "litras", "grains", "kas"] {
        let ty = if matches!(unit, "litras" | "grains" | "kas") { "int" } else { "temporal" };
        let program = load_inline_program(
            PathBuf::from("numeric-units.ag"),
            format!(
                r#"
                const int {unit} = 2;
                state S {{ int count; }}
                actor A owns S {{
                    entry inspect() emits none {{
                        {ty} value = 5 {unit};
                        require(count + {unit} + int(value) >= 0);
                    }}
                }}
            "#
            ),
        )
        .expect("source resolves");
        let actor = program.root_declarations().find(|id| id.kind() == SymbolKind::Actor).unwrap();
        let ResolvedDeclaration::Actor(item) = program.declaration(actor) else {
            panic!("actor expected");
        };
        // all references for the first (only) entry
        let references = &program.bindings(actor).text[&TextSite::Entry(0)];
        // should only contains one, at usage (2nd line)
        assert_eq!(references.len(), 1, "{unit}: only the constant use should bind");
        let body = item.entries[0].body.text();
        let reference = &references[0];
        assert_eq!(&body[reference.span.start..reference.span.end], unit);
        assert!(body[..reference.span.start].ends_with("count + "));
    }
}

#[test]
fn selected_apps_reject_duplicate_actor_exports_instead_of_renaming_them() {
    let temp = temp_dir("duplicate-selected-exports");
    fs::write(temp.join("left.ag"), "state Left {} actor A owns Left {}").unwrap();
    fs::write(temp.join("right.ag"), "state Right {} actor A owns Right {}").unwrap();
    fs::write(
        temp.join("root.ag"),
        r#"
        import "./left.ag" as left;
        import "./right.ag" as right;
        app Test { actor left::A; actor right::A; }
    "#,
    )
    .unwrap();
    let program = load_program(temp.join("root.ag")).expect("module namespaces are distinct");
    let error = crate::compiler::model::ModelSource::new(&program, Some("Test")).expect_err("app exports must be unambiguous");
    assert!(error.to_string().contains("selected app exports actor name `A` more than once"), "{error}");
    fs::remove_dir_all(temp).unwrap();
}

#[test]
fn resolves_transitive_namespaced_reexports() {
    let temp = temp_dir("namespaced-reexport");
    fs::write(temp.join("leaf.ag"), "state Stored {}\n").expect("leaf source written");
    fs::write(temp.join("middle.ag"), "import \"./leaf.ag\" as shared;\n").expect("middle source written");
    fs::write(
        temp.join("root.ag"),
        r#"
import "./middle.ag" as assets;

state Stored {}

state Wrapper {
    assets::shared::Stored value;
}
"#,
    )
    .expect("root source written");

    let program = load_program(temp.join("root.ag")).expect("module graph loads");
    let root = program.root_module();
    let wrapper = root.states.iter().find(|state| state.name == "Wrapper").expect("wrapper state is loaded");
    assert_eq!(wrapper.fields[0].ty.name, "assets::shared::Stored");

    let program_source = crate::compiler::model::ModelSource::new(&program, None).expect("model source adapts");
    let model = Model::from_source(&program_source).expect("resolved declarations build the compiler model");
    let imported_name = model
        .states
        .keys()
        .find(|name| name.ends_with("__Stored"))
        .expect("the namespaced state receives a collision-safe internal name");
    assert_eq!(model.states["Wrapper"].fields[0].ty.name, *imported_name);
    assert!(model.states.contains_key("Stored"), "the root declaration keeps its source name");

    let _ = fs::remove_dir_all(temp);
}

#[test]
fn rejects_ambiguous_open_reexports() {
    let temp = temp_dir("ambiguous-open-reexport");
    fs::write(temp.join("left.ag"), "const int LIMIT = 1;\n").expect("left source written");
    fs::write(temp.join("right.ag"), "const int LIMIT = 2;\n").expect("right source written");
    fs::write(temp.join("root.ag"), "import \"./left.ag\";\nimport \"./right.ag\";\n").expect("root source written");

    let err = load_program(temp.join("root.ag")).expect_err("ambiguous open re-export is rejected");
    assert!(err.to_string().contains("ambiguous export `LIMIT` in module namespace"), "unexpected error: {err}");

    let _ = fs::remove_dir_all(temp);
}

#[test]
fn app_graph_orders_and_deduplicates_diamond_dependencies() {
    let temp = temp_dir("diamond");
    write_app(&temp.join("shared.ag"), "", "SharedApp", "Shared", &[]);
    write_app(&temp.join("left.ag"), "import \"./shared.ag\" as shared;", "LeftApp", "Left", &["shared::SharedApp::Shared"]);
    write_app(&temp.join("right.ag"), "import \"./shared.ag\" as shared;", "RightApp", "Right", &["shared::SharedApp::Shared"]);
    write_app(
        &temp.join("root.ag"),
        "import \"./left.ag\" as left;\nimport \"./right.ag\" as right;",
        "RootApp",
        "Root",
        &["left::LeftApp::Left", "right::RightApp::Right"],
    );

    let graph = load_app_graph(temp.join("root.ag"), "RootApp").expect("app graph loads");
    assert_eq!(graph.iter().map(|(app, _, _)| app.app.as_str()).collect::<Vec<_>>(), ["SharedApp", "LeftApp", "RightApp", "RootApp"]);
    assert_eq!(graph.iter().filter(|(app, _, _)| app.app == "SharedApp").count(), 1);
    let (_, root_dependencies, root_program) = graph.last().unwrap();
    assert_eq!(root_dependencies.iter().map(|dependency| dependency.app.as_str()).collect::<Vec<_>>(), ["LeftApp", "RightApp"]);
    assert_eq!(root_program.module_paths().count(), 4, "module imports retain the complete source graph");

    let _ = fs::remove_dir_all(temp);
}

#[test]
fn app_graph_infers_apps_declared_by_module_imports() {
    let temp = temp_dir("module-app");
    write_app(&temp.join("asset.ag"), "", "AssetApp", "Asset", &[]);
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
    assert_eq!(program.module_paths().count(), 2, "ordinary imports retain shared source declarations");

    let _ = fs::remove_dir_all(temp);
}

#[test]
fn module_apps_without_qualified_references_remain_shared_source() {
    let temp = temp_dir("shared-module-app");
    write_app(&temp.join("shared.ag"), "", "SharedApp", "Shared", &[]);
    write_app(&temp.join("root.ag"), "import \"./shared.ag\";", "RootApp", "Root", &[]);

    let graph = load_app_graph(temp.join("root.ag"), "RootApp").expect("shared source module loads");
    assert_eq!(graph.iter().map(|(app, _, _)| app.app.as_str()).collect::<Vec<_>>(), ["RootApp"]);
    assert!(graph[0].1.is_empty());
    assert_eq!(graph[0].2.module_paths().count(), 2);

    let _ = fs::remove_dir_all(temp);
}

#[test]
fn app_graph_reports_the_complete_cycle() {
    let temp = temp_dir("cycle");
    write_app(&temp.join("a.ag"), "import \"./b.ag\" as b;", "AApp", "A", &["b::BApp::B"]);
    write_app(&temp.join("b.ag"), "import \"./c.ag\" as c;", "BApp", "B", &["c::CApp::C"]);
    write_app(&temp.join("c.ag"), "import \"./a.ag\" as a;", "CApp", "C", &["a::AApp::A"]);

    let err = load_app_graph(temp.join("a.ag"), "AApp").expect_err("app cycle is rejected");
    assert!(err.to_string().contains("app import cycle: AApp -> BApp -> CApp -> AApp"), "unexpected error: {err}");

    let _ = fs::remove_dir_all(temp);
}

#[test]
fn app_graph_rejects_one_namespace_from_two_sources() {
    let temp = temp_dir("namespace-collision");
    write_app(&temp.join("first.ag"), "", "AssetApp", "First", &[]);
    write_app(&temp.join("second.ag"), "", "AssetApp", "Second", &[]);
    write_app(
        &temp.join("controller.ag"),
        "import \"./first.ag\" as first;\nimport \"./second.ag\" as second;",
        "CtrlApp",
        "Ctrl",
        &["first::AssetApp::First", "second::AssetApp::Second"],
    );

    let err = load_app_graph(temp.join("controller.ag"), "CtrlApp").expect_err("ambiguous app namespace is rejected");
    assert!(err.to_string().contains("app `AssetApp` is imported from both"), "unexpected error: {err}");

    let _ = fs::remove_dir_all(temp);
}

#[test]
fn app_graph_rejects_namespace_conflicts_across_branches() {
    let temp = temp_dir("transitive-namespace-collision");
    write_app(&temp.join("first.ag"), "", "SharedApp", "First", &[]);
    write_app(&temp.join("second.ag"), "", "SharedApp", "Second", &[]);
    write_app(&temp.join("left.ag"), "import \"./first.ag\" as shared;", "LeftApp", "Left", &["shared::SharedApp::First"]);
    write_app(&temp.join("right.ag"), "import \"./second.ag\" as shared;", "RightApp", "Right", &["shared::SharedApp::Second"]);
    write_app(
        &temp.join("root.ag"),
        "import \"./left.ag\" as left;\nimport \"./right.ag\" as right;",
        "RootApp",
        "Root",
        &["left::LeftApp::Left", "right::RightApp::Right"],
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

fn write_app(path: &Path, imports: &str, app: &str, actor: &str, dependencies: &[&str]) {
    let entries = dependencies
        .iter()
        .enumerate()
        .map(|(index, dependency)| {
            format!(
                r#"
    entry dependency_{index}(cov_id dependency_id)
    observes dependency by dependency_id {{
        inputs {{
            source: {dependency},
        }}
    }}
    emits none {{}}
"#
            )
        })
        .collect::<String>();
    fs::write(
        path,
        format!(
            r#"
{imports}

state {actor}State {{}}

actor {actor} owns {actor}State {{
{entries}
}}

app {app} {{
    actor {actor};
}}
"#
        ),
    )
    .expect("app source is written");
}

#[test]
fn unknown_bare_body_type_is_rejected() {
    let error = load_inline_program(PathBuf::from("unknown-body-type.ag"), "fn check() { Missing value = { n: 1 }; }".to_string())
        .expect_err("body types must resolve before modeling");
    assert!(error.to_string().contains("unknown export `Missing`"), "{error}");
}

#[test]
fn unresolved_spawn_target_is_rejected() {
    let error = load_inline_program(
        PathBuf::from("unknown-spawn-target.ag"),
        r#"
        state S {}
        actor Root owns S {
            entry launch() spawns children by id { outputs { child: Missing, } } emits none {}
        }
        "#
        .to_string(),
    )
    .expect_err("static spawn targets must resolve before modeling");
    assert!(error.to_string().contains("unknown export `Missing`"), "{error}");
}

#[test]
fn unresolved_qualified_body_reference_is_rejected() {
    let error = load_inline_program(PathBuf::from("unknown-qualified-reference.ag"), "fn check() { missing::value(); }".to_string())
        .expect_err("qualified body references must resolve before modeling");
    assert!(error.to_string().contains("unresolved qualified reference `missing::value`"), "{error}");
}

#[test]
fn unknown_actor_enum_variant_is_rejected() {
    let error = load_inline_program(
        PathBuf::from("unknown-enum-variant.ag"),
        r#"
        state S {}
        actor A owns S {}
        actor B owns S {}
        actor enum Kind { A; B; }
        fn check() { Kind::Missing; }
        "#
        .to_string(),
    )
    .expect_err("enum variants must belong to their declared enum");
    assert!(error.to_string().contains("actor enum `Kind` has no variant `Missing`"), "{error}");
}

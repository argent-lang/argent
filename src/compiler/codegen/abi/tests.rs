use super::*;
use silverscript_lang::compiler::{CompileOptions, compile_contract, sil_abi_artifact_from_compiled};

#[test]
fn canonicalizes_imported_actor_state_references_with_different_layout_root_state() {
    check_imported_state_abi(
        r#"
            state CounterState { int count; }
            state Box { CounterState item; }
            state Nested { Box inner; }

            actor Counter owns CounterState {
                entry check(Nested box) emits none {
                    require(box.inner.item.count == count);
                }
            }
        "#,
        r#"
            import "./counter.ag" as lib;

            state CounterState { bool marker; }

            app Example { actor lib::Counter; }
        "#,
        FieldArtifact { name: "marker".to_string(), ty: TypeArtifact::Bool },
    );
}

#[test]
fn canonicalizes_imported_actor_state_references_with_equal_layout_root_state() {
    check_imported_state_abi(
        r#"
            state CounterState { int count; }
            state Box { CounterState item; }
            state Nested { Box inner; }

            actor Counter owns CounterState {
                entry check(Nested box) emits none {
                    require(box.inner.item.count == count);
                }
            }
        "#,
        r#"
            import "./counter.ag" as lib;

            state CounterState { int count; }

            app Example { actor lib::Counter; }
        "#,
        FieldArtifact { name: "count".to_string(), ty: TypeArtifact::Int },
    );
}

#[test]
fn canonicalizes_qualified_owner_state_references_with_different_layout_root_state() {
    check_imported_state_abi(
        r#"
            state CounterState { int count; }
            state Box { CounterState item; }
            state Nested { Box inner; }
        "#,
        r#"
            import "./counter.ag" as lib;

            state CounterState { bool marker; }

            actor Counter owns lib::CounterState {
                entry check(lib::Nested box) emits none {
                    require(box.inner.item.count == count);
                }
            }

            app Example { actor Counter; }
        "#,
        FieldArtifact { name: "marker".to_string(), ty: TypeArtifact::Bool },
    );
}

#[test]
fn canonicalizes_qualified_owner_state_references_with_equal_layout_root_state() {
    check_imported_state_abi(
        r#"
            state CounterState { int count; }
            state Box { CounterState item; }
            state Nested { Box inner; }
        "#,
        r#"
            import "./counter.ag" as lib;

            state CounterState { int count; }

            actor Counter owns lib::CounterState {
                entry check(lib::Nested box) emits none {
                    require(box.inner.item.count == count);
                }
            }

            app Example { actor Counter; }
        "#,
        FieldArtifact { name: "count".to_string(), ty: TypeArtifact::Int },
    );
}

fn check_imported_state_abi(library: &str, source: &str, expected_root_field: FieldArtifact) {
    let dir = tempfile::tempdir().expect("ABI source directory exists");
    std::fs::write(dir.path().join("counter.ag"), library).expect("library is written");
    let input = dir.path().join("app.ag");
    std::fs::write(&input, source).expect("root is written");
    let out = dir.path().join("build");
    let artifact = crate::build_file(&input, &out).expect("imported state ABI builds");
    let actor = artifact.argent.actors.iter().find(|actor| actor.name == "Counter").expect("Counter exists");
    let canonical = &actor.state;
    assert_ne!(canonical, "CounterState", "imported state must keep its own declaration identity");
    let imported = &artifact.sil_abi.structs[canonical];
    assert_eq!(imported.fields, vec![FieldArtifact { name: "count".to_string(), ty: TypeArtifact::Int }]);
    let root = &artifact.sil_abi.structs["CounterState"];
    assert_eq!(root.fields, vec![expected_root_field], "unrelated root declaration remains intact");
    let boxed = &artifact.sil_abi.structs["Box"];
    assert_eq!(boxed.fields[0].ty, TypeArtifact::Struct { name: canonical.clone() });
    assert_eq!(artifact.sil_abi.structs["Nested"].fields[0].ty, TypeArtifact::Struct { name: "Box".to_string() });
    assert!(!artifact.sil_abi.structs.values().any(struct_references_state));
    assert!(!artifact.sil_abi.structs.keys().any(|name| name.contains("::")));

    let sil = std::fs::read_to_string(out.join("sil/Counter.sil")).expect("emitted Counter Sil exists");
    let args = [SilExpr::int(0)];
    let reparsed = compile_contract(&sil, &args, CompileOptions::default()).expect("emitted Sil compiles");
    let direct = artifact.sil_abi.contract("Counter").expect("Counter ABI exists");
    assert_eq!(reparsed.bytecode, direct.compiled.bytecode);
    assert_eq!(reparsed.template_hash(), direct.compiled.template_hash);
    assert_eq!(reparsed.state_layout.start, direct.compiled.state_span.offset);
    assert_eq!(reparsed.state_layout.len, direct.compiled.state_span.len);
    let source_abi = sil_abi_artifact_from_compiled(&reparsed, &args).expect("source ABI exists");
    assert_eq!(source_abi.contract("Counter").expect("source contract exists").entries, direct.entries);
}

#[test]
fn canonicalizes_state_references_inside_array_types() {
    let mut fixed = TypeArtifact::FixedArray { item: Box::new(TypeArtifact::Struct { name: "State".to_string() }), len: 2 };
    let mut dynamic = TypeArtifact::DynamicArray { item: Box::new(TypeArtifact::Struct { name: "State".to_string() }) };

    assert!(type_references_state(&fixed));
    assert!(type_references_state(&dynamic));
    replace_state_type_ref(&mut fixed, "Details");
    replace_state_type_ref(&mut dynamic, "Details");

    assert!(!type_references_state(&fixed));
    assert!(!type_references_state(&dynamic));
    assert_eq!(fixed, TypeArtifact::FixedArray { item: Box::new(TypeArtifact::Struct { name: "Details".to_string() }), len: 2 });
    assert_eq!(dynamic, TypeArtifact::DynamicArray { item: Box::new(TypeArtifact::Struct { name: "Details".to_string() }) });
}

#[test]
fn sil_abi_merge_rejects_conflicting_structs_and_duplicate_contracts() {
    use crate::compiler::loader::SymbolKind;
    use crate::compiler::model::link::DeclarationOrigin;

    let compile = |contract: &str, field_type: &str| {
        let source = format!(
            r#"pragma silverscript ^0.1.0;
contract {contract}() {{
    struct Shared {{
        {field_type} value;
    }}

    entry hold() {{
        require(true);
    }}
}}
"#
        );
        let compiled = compile_contract(&source, &[], CompileOptions::default()).expect("test Sil compiles");
        sil_abi_artifact_from_compiled(&compiled, &[]).expect("test Sil ABI builds")
    };

    let left = compile("Left", "int");
    let source = AbiStructIdentity::Source(SourceStateId::from_origin(
        "Shared",
        DeclarationOrigin::Dependency { app: "App".to_string(), name: "Shared".to_string(), kind: SymbolKind::State },
    ));
    let sources = BTreeMap::from([("Shared".to_string(), source)]);
    let merged = merge_sil_abi_artifacts(left.clone(), compile("Right", "int"), &mut sources.clone(), sources.clone())
        .expect("identical shared structs merge");
    assert_eq!(merged.contracts.len(), 2);
    assert_eq!(merged.structs.len(), 1);

    let right = compile("Right", "bool");
    let err = merge_sil_abi_artifacts(left.clone(), right, &mut sources.clone(), sources.clone())
        .expect_err("different definitions of Shared must not merge");
    assert!(err.to_string().contains("conflicting Sil struct `Shared`"), "unexpected error: {err}");

    let err = merge_sil_abi_artifacts(left.clone(), left, &mut sources.clone(), sources)
        .expect_err("the same contract must not merge twice");
    assert!(err.to_string().contains("duplicate Sil contract `Left`"), "unexpected error: {err}");
}

#[test]
fn sil_abi_merge_requires_shared_source_for_equal_structs() {
    use crate::compiler::loader::SymbolKind;
    use crate::compiler::model::link::DeclarationOrigin;

    let compile = |contract: &str| {
        let source = format!(
            "pragma silverscript ^0.1.0; contract {contract}() {{ struct Shared {{ int value; }} entry hold() {{ require(true); }} }}"
        );
        let compiled = compile_contract(&source, &[], CompileOptions::default()).expect("test Sil compiles");
        sil_abi_artifact_from_compiled(&compiled, &[]).expect("test Sil ABI builds")
    };
    let source = |app: &str| {
        AbiStructIdentity::Source(SourceStateId::from_origin(
            "Shared",
            DeclarationOrigin::Dependency { app: app.to_string(), name: "Shared".to_string(), kind: SymbolKind::State },
        ))
    };
    let left = compile("Left");
    let right = compile("Right");
    let mut left_sources = BTreeMap::from([("Shared".to_string(), source("First"))]);
    let right_sources = BTreeMap::from([("Shared".to_string(), source("Second"))]);
    let err = merge_sil_abi_artifacts(left.clone(), right.clone(), &mut left_sources, right_sources)
        .expect_err("equal layouts from distinct sources must not merge");
    assert!(err.to_string().contains("distinct semantic identities"), "unexpected error: {err}");

    let err = merge_sil_abi_artifacts(left.clone(), right.clone(), &mut left_sources, BTreeMap::new())
        .expect_err("an unclassified struct must not merge by spelling and layout");
    assert!(err.to_string().contains("no semantic identity"), "unexpected error: {err}");

    let shared_source = SourceStateId::from_origin(
        "Shared",
        DeclarationOrigin::Dependency { app: "First".to_string(), name: "Shared".to_string(), kind: SymbolKind::State },
    );
    let physical = |family: &str| {
        BTreeMap::from([(
            "Shared".to_string(),
            AbiStructIdentity::Physical {
                source: shared_source.clone(),
                fields: vec![PhysicalFieldId::Generated(GeneratedFieldId::RouteFamilyDigest {
                    app: "First".to_string(),
                    family: family.to_string(),
                })],
            },
        )])
    };
    let err = merge_sil_abi_artifacts(left.clone(), right.clone(), &mut physical("one"), physical("two"))
        .expect_err("equal physical field layouts with different generated meaning must not merge");
    assert!(err.to_string().contains("distinct semantic identities"), "unexpected error: {err}");

    let shared = BTreeMap::from([("Shared".to_string(), source("First"))]);
    let merged = merge_sil_abi_artifacts(left, right, &mut left_sources, shared).expect("one source used by two contracts can merge");
    assert_eq!(merged.contracts.len(), 2);
    assert_eq!(merged.structs.len(), 1);
}

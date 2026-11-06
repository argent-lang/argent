//! Source-level handle transfers must retain nominal identity before byte[32] lowering.

use argent::builder::{BuilderError, EntryCall, TxBuilder, TxContext, args, state};
use kaspa_consensus_core::{
    Hash,
    tx::{TransactionId, TransactionOutpoint},
};

const FOREIGN: &str = r#"
state ForeignState { int n; }
state OtherState { int n; }
actor Foreign owns ForeignState { entry check() emits none { require(n >= 0); } }
actor Other owns OtherState { entry check() emits none { require(n > 0); } }
app ForeignApp { actor Foreign; actor Other; }
"#;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Context {
    Entry,
    Global,
    Actor,
}

#[derive(Clone, Copy, Debug)]
enum Sink {
    Declaration,
    Assignment,
    Argument,
    Return,
}

struct Producer {
    name: &'static str,
    setup: &'static str,
    expression: &'static str,
    actor_only: bool,
}

fn producers() -> Vec<Producer> {
    vec![
        Producer { name: "literal", setup: "", expression: "foreign::ForeignApp::ACTOR", actor_only: false },
        Producer { name: "parameter", setup: "", expression: "parameter", actor_only: false },
        Producer { name: "local", setup: "actor_type<SOURCE> original = parameter;", expression: "original", actor_only: false },
        Producer { name: "constant", setup: "", expression: "SOURCE_VALUE", actor_only: false },
        Producer { name: "global-result", setup: "", expression: "produce(parameter)", actor_only: false },
        Producer { name: "actor-result", setup: "", expression: "actor_produce(parameter)", actor_only: true },
        Producer { name: "actor-field", setup: "", expression: "stored", actor_only: true },
        Producer { name: "self-field", setup: "", expression: "self.stored", actor_only: true },
        Producer { name: "struct-field", setup: "", expression: "box.handle", actor_only: false },
        Producer { name: "nested-field", setup: "", expression: "outer.inner.handle", actor_only: false },
        Producer { name: "call-field", setup: "", expression: "box_identity(box).handle", actor_only: false },
        Producer {
            name: "call-binding",
            setup: "(actor_type<SOURCE> called) = produce(parameter);",
            expression: "called",
            actor_only: false,
        },
        Producer {
            name: "destructure",
            setup: "Box { handle: actor_type<SOURCE> extracted } = box;",
            expression: "extracted",
            actor_only: false,
        },
        Producer {
            name: "dynamic-index",
            setup: "actor_type<SOURCE>[] handles; handles = handles.append(parameter);",
            expression: "handles[0]",
            actor_only: false,
        },
        Producer {
            name: "fixed-index",
            setup: "actor_type<SOURCE>[1] handles = byte[32][1] { parameter };",
            expression: "handles[0]",
            actor_only: false,
        },
        Producer {
            name: "named-fixed-index",
            setup: "actor_type<SOURCE>[WIDTH] handles = byte[32][1] { parameter };",
            expression: "handles[0]",
            actor_only: false,
        },
        Producer {
            name: "array-copy-index",
            setup: "actor_type<SOURCE>[] first; first = first.append(parameter); actor_type<SOURCE>[] second = first;",
            expression: "second[0]",
            actor_only: false,
        },
    ]
}

fn fixture_source(producer: &Producer, context: Context, sink: Sink, compatible: bool) -> String {
    let source = if compatible { "foreign::ForeignState" } else { "foreign::OtherState" };
    let actor = if compatible { "Foreign" } else { "Other" };
    let setup = producer.setup.replace("SOURCE", source);
    let expression = producer.expression.replace("ACTOR", actor);
    let statement = match sink {
        Sink::Declaration => format!("actor_type<alias::ForeignState> target = {expression}; require(target == target);"),
        Sink::Assignment => format!(
            "actor_type<alias::ForeignState> target = foreign::ForeignApp::Foreign; target = {expression}; require(target == target);"
        ),
        Sink::Argument => format!("consume({expression});"),
        Sink::Return => format!("return {expression};"),
    };
    let body = format!("{setup} {statement}");
    let parameters = format!("actor_type<{source}> parameter, Box box, Outer outer");
    let result = if matches!(sink, Sink::Return) { " -> actor_type<alias::ForeignState>" } else { "" };
    let helper = format!("fn transfer({parameters}){result} {{ {body} }}");
    let (global, actor_helper, entry) = match context {
        Context::Entry => (String::new(), String::new(), body),
        Context::Global => (helper, String::new(), String::new()),
        Context::Actor => (String::new(), helper, String::new()),
    };
    format!(
        r#"
        import "./foreign.ag" as foreign;
        import "./foreign.ag" as alias;
        const int WIDTH = 1;
        const actor_type<{source}> SOURCE_VALUE = foreign::ForeignApp::{actor};
        state Box {{ actor_type<{source}> handle; }}
        state Outer {{ Box inner; }}
        state HostState {{ actor_type<{source}> stored; }}
        fn produce(actor_type<{source}> original) -> actor_type<{source}> {{ return original; }}
        fn box_identity(Box original) -> Box {{ return original; }}
        fn consume(actor_type<alias::ForeignState> target) {{ require(target == target); }}
        {global}
        actor Host owns HostState {{
            fn actor_produce(actor_type<{source}> original) -> actor_type<{source}> {{ return original; }}
            {actor_helper}
            entry check({parameters}) emits none {{ {entry} }}
        }}
        app HostApp {{ actor Host; }}
    "#
    )
}

#[test]
fn declared_handle_producers_and_transfer_contexts_have_positive_and_negative_coverage() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("foreign.ag"), FOREIGN).unwrap();
    let input = directory.path().join("host.ag");
    let output = directory.path().join("build");
    let mut cases = 0;
    for producer in producers() {
        for context in [Context::Entry, Context::Global, Context::Actor] {
            if producer.actor_only && context == Context::Global {
                continue;
            }
            for sink in [Sink::Declaration, Sink::Assignment, Sink::Argument, Sink::Return] {
                if context == Context::Entry && matches!(sink, Sink::Return) {
                    continue;
                }
                for compatible in [true, false] {
                    std::fs::write(&input, fixture_source(&producer, context, sink, compatible)).unwrap();
                    let label = format!("{} / {context:?} / {sink:?} / compatible={compatible}", producer.name);
                    match argent::build_file(&input, &output) {
                        Ok(artifact) => {
                            assert!(compatible, "nominal mismatch was accepted: {label}");
                            assert!(artifact.sil_abi.contract("Host").is_some());
                        }
                        Err(error) => {
                            assert!(!compatible, "positive source failed: {label}: {error}");
                            assert!(error.to_string().contains("actor_type<"), "missing handle diagnostic: {label}: {error}");
                            assert!(!error.to_string().contains("generated Silverscript"), "late Sil error: {label}: {error}");
                        }
                    }
                    cases += 1;
                }
            }
        }
    }
    assert_eq!(cases, 350, "every supported source/context row must have a positive and a nominal negative");
}

fn local_context_source(context: Context, body: &str) -> String {
    let parameters = "actor_type<foreign::ForeignState> good, actor_type<foreign::OtherState> wrong, bool choose";
    let helper = format!("fn transfer({parameters}) {{ {body} }}");
    let (global, actor, entry) = match context {
        Context::Entry => (String::new(), String::new(), body.to_string()),
        Context::Global => (helper, String::new(), String::new()),
        Context::Actor => (String::new(), helper, String::new()),
    };
    format!(
        r#"
        import "./foreign.ag" as foreign;
        const int WIDTH = 1;
        state Box {{ actor_type<foreign::ForeignState> handle; }}
        state Outer {{ Box inner; }}
        state HostState {{ actor_type<foreign::ForeignState> stored; }}
        fn consume_box(Box box) {{ require(box.handle == box.handle); }}
        {global}
        actor Host owns HostState {{ {actor} entry check({parameters}) emits none {{ {entry} }} }}
        app HostApp {{ actor Host; }}
    "#
    )
}

fn check_source(directory: &std::path::Path, source: String, accepted: bool, label: &str) {
    std::fs::write(directory.join("host.ag"), source).unwrap();
    let result = argent::build_file(directory.join("host.ag"), directory.join("build"));
    if accepted {
        result.unwrap_or_else(|error| panic!("positive {label}: {error}"));
    } else {
        let error = result.expect_err(label);
        assert!(error.to_string().contains("actor_type<"), "{label}: {error}");
        assert!(!error.to_string().contains("generated Silverscript"), "late Sil error for {label}: {error}");
    }
}

#[test]
fn local_handle_arrays_preserve_nominal_identity_and_shape_in_all_body_contexts() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("foreign.ag"), FOREIGN).unwrap();
    let scalar = "foreign::ForeignState";
    let other = "foreign::OtherState";
    for context in [Context::Entry, Context::Global, Context::Actor] {
        for accepted in [true, false] {
            let target = if accepted { scalar } else { other };
            let value = if accepted { "good" } else { "wrong" };
            let cases = [
                (
                    "dynamic-copy",
                    format!(
                        "actor_type<{scalar}>[] original; original = original.append(good); actor_type<{target}>[] copied = original; require(copied[0] == good);"
                    ),
                ),
                (
                    "fixed-copy",
                    format!(
                        "actor_type<{scalar}>[1] original = byte[32][1] {{ good }}; actor_type<{target}>[WIDTH] copied = original; require(copied[0] == good);"
                    ),
                ),
                (
                    "fixed-width",
                    format!(
                        "actor_type<{scalar}>[1] original = byte[32][1] {{ good }}; actor_type<{scalar}>[{}] copied = original; require(copied[0] == good);",
                        if accepted { 1 } else { 2 }
                    ),
                ),
                (
                    "scalar-from-array",
                    format!(
                        "actor_type<{scalar}>[] original; original = original.append(good); actor_type<{scalar}> copied = {}; require(copied == good);",
                        if accepted { "original[0]" } else { "original" }
                    ),
                ),
                (
                    "array-from-scalar",
                    format!(
                        "actor_type<{scalar}>[] original; original = original.append(good); actor_type<{scalar}>[] copied = {}; require(copied[0] == good);",
                        if accepted { "original" } else { "good" }
                    ),
                ),
                (
                    "append",
                    format!("actor_type<{scalar}>[] original; original = original.append({value}); require(original[0] == good);"),
                ),
                (
                    "array-literal",
                    format!("actor_type<{scalar}>[1] original = byte[32][1] {{ {value} }}; require(original[0] == good);"),
                ),
                (
                    "nested-array-literal",
                    format!(
                        "actor_type<{scalar}>[1][1] original = byte[32][1][1] {{ byte[32][1] {{ {value} }} }}; require(original[0][0] == good);"
                    ),
                ),
                (
                    "slice",
                    format!(
                        "actor_type<{scalar}>[] original; original = original.append(good); actor_type<{target}>[] copied = original.slice(0, 1); require(copied[0] == good);"
                    ),
                ),
                (
                    "split-projection",
                    format!(
                        "actor_type<{scalar}>[2] original = byte[32][2] {{ good, good }}; actor_type<{target}>[] copied = original.split(1).0; require(copied[0] == good);"
                    ),
                ),
                (
                    "tuple-split",
                    format!(
                        "actor_type<{scalar}>[2] original = byte[32][2] {{ good, good }}; (actor_type<{target}>[] left, actor_type<{scalar}>[] right) = original.split(1); require(left[0] == right[0]);"
                    ),
                ),
            ];
            for (name, body) in cases {
                check_source(
                    directory.path(),
                    local_context_source(context, &body),
                    accepted,
                    &format!("{name}/{context:?}/{accepted}"),
                );
            }
        }
    }
}

#[test]
fn nested_literals_and_typed_state_destructuring_check_handle_field_types() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("foreign.ag"), FOREIGN).unwrap();
    for accepted in [true, false] {
        let actor = if accepted { "Foreign" } else { "Other" };
        let value = format!("foreign::ForeignApp::{actor}");
        for context in [Context::Entry, Context::Global, Context::Actor] {
            for body in [
                format!("Box box = {{ handle: {value} }}; require(box.handle == good);"),
                format!("Outer outer = {{ inner: Box {{ handle: {value} }} }}; require(outer.inner.handle == good);"),
                format!("consume_box(Box {{ handle: {value} }});"),
                format!("Box box = {{ handle: good }}; box = Box {{ handle: {value} }}; require(box.handle == good);"),
                format!("Box[1] boxes = Box[1] {{ Box {{ handle: {value} }} }}; require(boxes[0].handle == good);"),
            ] {
                check_source(
                    directory.path(),
                    local_context_source(context, &body),
                    accepted,
                    &format!("nested literal/{context:?}/{accepted}"),
                );
            }
        }
        for global in [
            format!("const Box VALUE = Box {{ handle: {value} }};"),
            format!("fn value() -> Box {{ return Box {{ handle: {value} }}; }}"),
        ] {
            let source = local_context_source(Context::Entry, "")
                .replace("actor Host owns HostState", &format!("{global} actor Host owns HostState"));
            check_source(directory.path(), source, accepted, "nested constant/helper return");
        }
        let target = if accepted { "foreign::ForeignState" } else { "foreign::OtherState" };
        for index in ["this.activeInputIndex", "1"] {
            let body = format!(
                "HostState {{ stored: actor_type<{target}> extracted }} = readInputState({index}); require(extracted == good);"
            );
            let mut source_text = local_context_source(Context::Entry, &body);
            if index == "1" {
                source_text = source_text.replace(" emits none", " consumes { peer: Host, } emits none");
            }
            check_source(directory.path(), source_text, accepted, "typed state-read destructuring");
        }
    }
}

#[test]
fn unsupported_handle_array_signatures_and_storage_are_rejected_without_losing_dimensions() {
    for dimension in ["[]", "[2]"] {
        for declaration in [
            format!("entry check(actor_type<S>{dimension} values) emits none {{}}"),
            format!("fn helper(actor_type<S>{dimension} values) {{}} entry check() emits none {{}}"),
            format!("fn helper() -> actor_type<S>{dimension} {{ return value; }} entry check() emits none {{}}"),
        ] {
            let source = format!("state S {{ int n; }} actor Host owns S {{ {declaration} }} app Test {{ actor Host; }}");
            let error =
                argent::compile_inline("handle-array-signature.ag", source).expect_err("signature shapes cannot be silently erased");
            assert!(error.to_string().contains("actor_type arrays are supported only in local body declarations"), "{error}");
        }
        for declaration in [
            format!("fn helper(actor_type<S>{dimension} values) {{}}"),
            format!("fn helper() -> actor_type<S>{dimension} {{ return value; }}"),
            format!("const actor_type<S>{dimension} VALUES = value;"),
            format!("state Container {{ actor_type<S>{dimension} values; }}"),
        ] {
            let source = format!(
                "state S {{ int n; }} {declaration} actor Host owns S {{ entry check() emits none {{}} }} app Test {{ actor Host; }}"
            );
            let error =
                argent::compile_inline("handle-array-storage.ag", source).expect_err("declaration shapes cannot be silently erased");
            assert!(error.to_string().contains("actor_type arrays are supported only in local body declarations"), "{error}");
        }
    }
}

#[test]
fn raw_carriers_and_lexical_bindings_keep_their_existing_semantics() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("foreign.ag"), FOREIGN).unwrap();
    for context in [Context::Entry, Context::Global, Context::Actor] {
        let body = r#"
            actor_type<foreign::ForeignState> raw = byte[32](0x0000000000000000000000000000000000000000000000000000000000000000);
            (byte[] left, byte[] right) = raw.split(16);
            require(left == right);
            if (choose) {
                actor_type<foreign::ForeignState> branch = good;
                require(branch == good);
            } else {
                actor_type<foreign::OtherState> branch = wrong;
                require(branch == wrong);
            }
        "#;
        check_source(directory.path(), local_context_source(context, body), true, "raw carriers and sibling scopes");
        let body =
            "actor_type<foreign::ForeignState> target = good; { actor_type<foreign::OtherState> inner = wrong; target = inner; }";
        check_source(directory.path(), local_context_source(context, body), false, "nested scope assignment");
    }
    let source = local_context_source(Context::Entry, "actor_type<ForeignState> target = good;")
        .replace("state HostState", "state ForeignState { int n; } state HostState");
    check_source(directory.path(), source, false, "same spelling and layout do not replace declaration identity");
}

#[test]
fn interaction_input_fields_keep_their_nominal_handle_types() {
    let directory = tempfile::tempdir().unwrap();
    let foreign = FOREIGN.replace(
        "app ForeignApp { actor Foreign; actor Other; }",
        r#"
        state HolderState { actor_type<ForeignState> saved; }
        actor Holder owns HolderState { entry check() emits none { require(saved == saved); } }
        app ForeignApp { actor Foreign; actor Other; actor Holder; }
        "#,
    );
    std::fs::write(directory.path().join("foreign.ag"), foreign).unwrap();
    let mut cases = 0;
    for (label, clauses, reference) in [
        ("consumed", "consumes { peer: Peer, }", "peer"),
        ("ranged-consumed", "consumes { peers: Peer[1..=2], }", "peers[0]"),
        ("static-observed", "observes remote by remote_id { inputs { peer: Peer, } }", "remote.inputs.peer"),
        ("linked-observed", "observes remote by remote_id { inputs { peer: foreign::ForeignApp::Holder, } }", "remote.inputs.peer"),
        ("parameter-observed", "observes remote by remote_id { inputs { peer: parameter_type, } }", "remote.inputs.peer"),
        ("field-observed", "observes remote by remote_id { inputs { peer: self.peer_type, } }", "remote.inputs.peer"),
        (
            "open-observed",
            "observes remote by remote_id { inputs { peer: actor_type<PeerState> as opened, } outputs { peer: opened, } }",
            "remote.inputs.peer",
        ),
    ] {
        for projection in [format!("{reference}.saved"), format!("state({reference}).saved")] {
            for sink in ["declaration", "assignment", "argument", "append", "struct-literal"] {
                for accepted in [true, false] {
                    let target = if accepted { "ForeignState" } else { "OtherState" };
                    let actor = if accepted { "Foreign" } else { "Other" };
                    let body = match sink {
                        "declaration" => format!("actor_type<alias::{target}> value = {projection}; require(value == value);"),
                        "assignment" => format!(
                            "actor_type<alias::{target}> value = foreign::ForeignApp::{actor}; value = {projection}; require(value == value);"
                        ),
                        "argument" => format!("consume({projection});"),
                        "append" => format!(
                            "actor_type<alias::{target}>[] values; values = values.append({projection}); require(values[0] == values[0]);"
                        ),
                        "struct-literal" => format!("TargetBox box = {{ handle: {projection} }}; require(box.handle == box.handle);"),
                        _ => unreachable!(),
                    };
                    let open_route = if label == "open-observed" {
                        "PeerState next_state = state(remote.inputs.peer); unrestricted(remote.outputs.peer.value); require remote.outputs become { peer <- opened(next_state), };"
                    } else {
                        ""
                    };
                    let source = format!(
                        r#"
                        import "./foreign.ag" as foreign;
                        import "./foreign.ag" as alias;
                        state TargetBox {{ actor_type<alias::{target}> handle; }}
                        state PeerState {{ actor_type<foreign::ForeignState> saved; }}
                        state HostState {{ actor_type<PeerState> peer_type; }}
                        fn consume(actor_type<alias::{target}> value) {{ require(value == value); }}
                        actor Peer owns PeerState {{ entry check() emits none {{ require(saved == saved); }} }}
                        actor Host owns HostState {{
                            entry check(cov_id remote_id, actor_type<PeerState> parameter_type)
                            {clauses} emits none {{ {body} {open_route} }}
                        }}
                        app HostApp {{ actor Host; actor Peer; }}
                    "#
                    );
                    check_source(directory.path(), source, accepted, &format!("{label}/{projection}/{sink}"));
                    cases += 1;
                }
            }
        }
    }
    assert_eq!(cases, 140);
    for accepted in [true, false] {
        let target = if accepted { "PeerState" } else { "foreign::OtherState" };
        let source = format!(
            r#"
            import "./foreign.ag" as foreign;
            state PeerState {{ actor_type<foreign::ForeignState> saved; }}
            state HostState {{ int n; }}
            actor Host owns HostState {{
                entry check(cov_id remote_id)
                observes remote by remote_id {{
                    inputs {{ peer: actor_type<PeerState> as opened, }}
                    outputs {{ peer: opened, }}
                }} emits none {{
                    actor_type<{target}> copied = opened;
                    require(copied == opened);
                    PeerState next_state = state(remote.inputs.peer);
                    unrestricted(remote.outputs.peer.value);
                    require remote.outputs become {{ peer <- opened(next_state), }};
                }}
            }}
            app HostApp {{ actor Host; }}
        "#
        );
        check_source(directory.path(), source, accepted, "open observed binding retains its declared handle type");
    }
}

#[test]
fn literal_expansion_views_do_not_change_the_nominal_type_of_declared_copies() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("foreign.ag"),
        r#"
        state Capsule { int n; virtual detail; }
        state Detail { int value; }
        state Expanded expands Capsule { detail: Detail; }
        state Sibling expands Capsule { detail: Detail; }
        actor Foreign owns Expanded { entry check() emits none { require(n >= 0); } }
        app ForeignApp { actor Foreign; }
    "#,
    )
    .unwrap();
    for (view, accepted) in [("Expanded", true), ("Capsule", true), ("Sibling", false)] {
        let statement = format!("actor_type<foreign::{view}> value = foreign::ForeignApp::Foreign; require(value == value);");
        for context in [Context::Entry, Context::Global, Context::Actor] {
            let helper = format!("fn transfer() {{ {statement} }}");
            let (global, actor, body) = match context {
                Context::Entry => (String::new(), String::new(), statement.clone()),
                Context::Global => (helper, String::new(), String::new()),
                Context::Actor => (String::new(), helper, String::new()),
            };
            let source = format!(
                r#"
                import "./foreign.ag" as foreign;
                state HostState {{ int n; }}
                {global}
                actor Host owns HostState {{ {actor} entry check() emits none {{ {body} }} }}
                app HostApp {{ actor Host; }}
            "#
            );
            check_source(directory.path(), source, accepted, "direct literal authored/base/sibling view");
        }
        for global in [
            format!("const actor_type<foreign::{view}> VALUE = foreign::ForeignApp::Foreign;"),
            format!("fn value() -> actor_type<foreign::{view}> {{ return foreign::ForeignApp::Foreign; }}"),
        ] {
            check_source(
                directory.path(),
                format!(
                    r#"
                import "./foreign.ag" as foreign;
                state HostState {{ int n; }}
                {global}
                actor Host owns HostState {{ entry check() emits none {{}} }}
                app HostApp {{ actor Host; }}
            "#
                ),
                accepted,
                "direct literal constant/return expansion view",
            );
        }
    }
    for (from, to, accepted) in
        [("Expanded", "Expanded", true), ("Capsule", "Capsule", true), ("Expanded", "Capsule", false), ("Capsule", "Expanded", false)]
    {
        let body = format!(
            "actor_type<foreign::{from}> original = foreign::ForeignApp::Foreign; actor_type<foreign::{to}> copied = original; require(copied == copied);"
        );
        check_source(
            directory.path(),
            format!(
                r#"
            import "./foreign.ag" as foreign;
            state HostState {{ int n; }}
            actor Host owns HostState {{ entry check() emits none {{ {body} }} }}
            app HostApp {{ actor Host; }}
        "#
            ),
            accepted,
            "declared copies keep their exact source-state view",
        );
    }
}

#[test]
fn field_array_and_destructured_handles_execute_with_correct_and_stale_values() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("foreign.ag"), FOREIGN).unwrap();
    std::fs::write(
        directory.path().join("host.ag"),
        r#"
        import "./foreign.ag" as foreign;
        state Box { actor_type<foreign::ForeignState> handle; }
        state HostState { actor_type<foreign::ForeignState> stored; }
        actor Host owns HostState {
            entry check(bool use_box) emits none {
                actor_type<foreign::ForeignState> from_field = self.stored;
                actor_type<foreign::ForeignState>[] handles;
                handles = handles.append(from_field);
                actor_type<foreign::ForeignState> selected = handles[0];
                if (use_box) {
                    Box box = { handle: stored };
                    Box { handle: actor_type<foreign::ForeignState> extracted } = box;
                    require(extracted == foreign::ForeignApp::Foreign);
                } else {
                    require(selected == foreign::ForeignApp::Foreign);
                }
            }
        }
        app HostApp { actor Host; }
    "#,
    )
    .unwrap();
    let compiled = argent::build_file_bundle(directory.path().join("host.ag"), directory.path().join("build")).unwrap();
    let bundle = compiled.runtime_bundle().unwrap();
    let builder = TxBuilder::from_bundle(&bundle).unwrap();
    let handle = builder.actor_type_handle("foreign_app::Foreign", "ForeignState").unwrap();
    for use_box in [false, true] {
        for correct in [true, false] {
            let mut stored = handle.clone();
            if !correct {
                stored[0] ^= 1;
            }
            let state = state! { stored: stored };
            let utxo = builder.covenant_utxo("Host", state.clone(), 1_000, 0, false, Some(Hash::from_bytes([0x31; 32]))).unwrap();
            let context = TxContext::new().actor_input(
                "Host",
                state,
                EntryCall::new("check").args(args![use_box]),
                TransactionOutpoint::new(TransactionId::from_bytes([0x32; 32]), 0),
                utxo,
                0,
            );
            let result = builder.build(&context);
            if correct {
                result.expect("correct nominal handle executes across field/array/destructure paths");
            } else {
                assert!(matches!(result, Err(BuilderError::InputScript { input_index: 0, .. })));
            }
        }
    }
}

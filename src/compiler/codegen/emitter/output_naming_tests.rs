use super::*;
use crate::compiler::naming::to_snake;

#[test]
fn output_temporaries_distinguish_covenant_groups() {
    for observed in [true, false] {
        for routed in [false, true] {
            for local in [false, true] {
                let arguments = if observed { "cov_id left_id, cov_id right_id" } else { "" };
                let groups = if routed { ["leftSide", "left_side"] } else { ["left", "right"] };
                let mut clauses = String::new();
                let mut body = String::new();
                for (index, group) in groups.iter().enumerate() {
                    let id = if index == 0 { "left_id" } else { "right_id" };
                    let mut state = if observed {
                        clauses.push_str(&format!(
                            "observes {group} by {id} {{ inputs {{ current: Counter, }} outputs {{ next: Counter, }} }}"
                        ));
                        format!("state({group}.inputs.current)")
                    } else {
                        clauses.push_str(&format!("spawns {group} by {id} {{ outputs {{ next: Counter, }} }}"));
                        body.push_str(&format!("unrestricted({group}.outputs.next.value);"));
                        format!("CounterState {{ count: {} }}", index + 1)
                    };
                    if local {
                        body.push_str(&format!("CounterState {group}_state = {state};"));
                        state = format!("{group}_state");
                    }
                    body.push_str(&format!("require {group}.outputs become {{ next <- Counter({state}), }};"));
                }
                let entry = format!("entry preserve({arguments}) {clauses} emits none {{ {body} }}");
                let case = format!("groups-observed-{observed}-routed-{routed}-local-{local}");
                let source = if routed { routed_counter_source(&entry) } else { plain_counter_source(&entry) };
                let actors = check_output_naming(&case, source);
                let sil = &actors["Coordinator"];
                if !local {
                    assert!(sil.contains("gen__0_route_source_next =") && sil.contains("gen__1_route_source_next ="), "{case}: {sil}");
                }
                if routed {
                    assert!(
                        sil.contains("gen__0_route_physical_next =") && sil.contains("gen__1_route_physical_next ="),
                        "{case}: {sil}"
                    );
                }
            }
        }
    }
}

#[test]
fn current_and_observed_outputs_use_distinct_temporaries() {
    for routed in [false, true] {
        for local in [false, true] {
            for output in ["next", "peer_next"] {
                let (bindings, observed_state, current_state) = if local {
                    (
                        "CounterState left = state(peer.inputs.current); CounterState right = CounterState { count: 0 };",
                        "left",
                        "right",
                    )
                } else {
                    ("", "state(peer.inputs.current)", "CounterState { count: 0 }")
                };
                let entry = format!(
                    "entry preserve(cov_id peer_id)
                    observes peer by peer_id {{ inputs {{ current: Counter, }} outputs {{ {output}: Counter, }} }}
                    emits peerNext: Counter {{
                        {bindings}
                        require peer.outputs become {{ {output} <- Counter({observed_state}), }};
                        unrestricted(peerNext.value);
                        become peerNext <- Counter({current_state});
                    }}"
                );
                let case = format!("current-observed-{output}-routed-{routed}-local-{local}");
                let source = if routed { routed_counter_source(&entry) } else { plain_counter_source(&entry) };
                let actors = check_output_naming(&case, source);
                let sil = &actors["Coordinator"];
                if !local {
                    assert!(
                        sil.contains(&format!("gen__0_route_source_{} =", to_snake(output)))
                            && sil.contains("gen__1_route_source_peer_next ="),
                        "{case}: {sil}"
                    );
                }
                if routed {
                    assert!(
                        sil.contains(&format!("gen__0_route_physical_{} =", to_snake(output)))
                            && sil.contains("gen__1_route_physical_peer_next ="),
                        "{case}: {sil}"
                    );
                }
            }
        }
    }
}

#[test]
fn output_temporaries_distinguish_normalized_labels() {
    for kind in ["current", "observed", "spawned"] {
        for (left, right) in [("nextState", "next_state"), ("HTTPNext", "HttpNext")] {
            let outputs = format!("{left}: Counter, {right}: Counter,");
            let (clauses, emits, prefix, route, input) = match kind {
                "current" => ("consumes { other: Counter, }".to_string(), format!("{{ {outputs} }}"), "", "", "other"),
                "observed" => (
                    format!("observes peer by peer_id {{ inputs {{ current: Counter, }} outputs {{ {outputs} }} }}"),
                    "none".to_string(),
                    "",
                    "require peer.outputs ",
                    "peer.inputs.current",
                ),
                "spawned" => (
                    format!(
                        "observes peer by peer_id {{ inputs {{ current: Counter, }} }}
                        spawns created by created_id {{ outputs {{ {outputs} }} }}"
                    ),
                    "none".to_string(),
                    "created.outputs.",
                    "require created.outputs ",
                    "peer.inputs.current",
                ),
                _ => unreachable!(),
            };
            let value_policy = if kind == "observed" {
                String::new()
            } else {
                format!("unrestricted({prefix}{left}.value); unrestricted({prefix}{right}.value);")
            };
            for routed in [false, true] {
                for (value_kind, value) in [
                    ("input", format!("state({input})")),
                    ("literal", "CounterState { count: 0 }".to_string()),
                    ("helper", "snapshot(0)".to_string()),
                    ("local", "local_state".to_string()),
                ] {
                    let entry = format!(
                        "entry preserve(cov_id peer_id) {clauses} emits {emits} {{
                            CounterState local_state = state({input});
                            {value_policy}
                            {route}become {{ {left} <- Counter({value}), {right} <- Counter({value}), }};
                        }}"
                    );
                    let case = format!("{kind}-{left}-{right}-{value_kind}-routed-{routed}");
                    let source = if routed { routed_counter_source(&entry) } else { plain_counter_source(&entry) };
                    let actors = check_output_naming(&case, source);
                    let sil = &actors["Coordinator"];
                    if value_kind != "local" {
                        assert!(
                            sil.contains(&format!("gen__0_route_source_{} =", to_snake(left)))
                                && sil.contains(&format!("gen__1_route_source_{} =", to_snake(right))),
                            "{case}: {sil}"
                        );
                    }
                    if routed {
                        assert!(
                            sil.contains(&format!("gen__0_route_physical_{} =", to_snake(left)))
                                && sil.contains(&format!("gen__1_route_physical_{} =", to_snake(right))),
                            "{case}: {sil}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn output_temporaries_avoid_input_reads() {
    for (case, entry) in [
        (
            "consumed-input",
            "entry preserve()
            consumes { source_next_counter: Counter, }
            emits next: Counter {
                unrestricted(next.value);
                become next <- Counter(state(source_next_counter));
            }",
        ),
        (
            "observed-input",
            "entry preserve(cov_id peer_id)
            observes source by peer_id { inputs { next_counter: Counter, } }
            emits next: Counter {
                unrestricted(next.value);
                become next <- Counter(state(source.inputs.next_counter));
            }",
        ),
        (
            "other-observed-input",
            "entry preserve(cov_id peer_id, cov_id other_id)
            observes source by peer_id { inputs { peer_next_counter: Counter, } }
            observes peer by other_id { outputs { next: Counter, } }
            emits none {
                require peer.outputs become { next <- Counter(state(source.inputs.peer_next_counter)), };
            }",
        ),
    ] {
        let actors = check_output_naming(case, plain_counter_source(entry));
        assert!(actors["Coordinator"].contains("CounterState gen__0_route_source_next ="), "{case}");
    }
}

#[test]
fn output_temporaries_avoid_later_range_loops() {
    for routed in [false, true] {
        let reset = if routed {
            "entry reset() emits next: Coordinator {
                unrestricted(next.value);
                become next <- Coordinator(CoordinatorState { nonce: count });
            }"
        } else {
            ""
        };
        let case = format!("later-range-loop-routed-{routed}");
        let actors = check_output_naming(
            &case,
            format!(
                "state OutputPosition {{ int count; }}
                state CoordinatorState {{ int nonce; }}
                actor Counter owns OutputPosition {{
                    entry hold() emits none {{ require(count >= 0); }}
                    {reset}
                }}
                actor Coordinator owns CoordinatorState {{
                    entry preserve() emits {{ next: Counter, source_next: Counter[1..=2], }} {{
                        OutputPosition[] values;
                        values = values.append(OutputPosition {{ count: 2 }});
                        unrestricted(next.value);
                        for (i, 0, source_next.length, 2) {{ unrestricted(source_next[i].value); }}
                        become {{
                            next <- Counter(OutputPosition {{ count: 1 }}),
                            source_next <- Counter[](values),
                        }};
                    }}
                }}
                app Example {{ actor Counter; actor Coordinator; }}"
            ),
        );
        let sil = &actors["Coordinator"];
        assert!(sil.contains("gen__0_route_source_next ="), "{case}: {sil}");
        let loop_body = sil.split_once("for (gen__source_next_output_position,").expect("range loop keeps its name").1;
        assert!(loop_body.contains("gen__1_route_source_source_next = values[gen__source_next_output_position]"), "{case}: {sil}");
        if routed {
            assert!(loop_body.contains("gen__1_route_physical_source_next ="), "{case}: {sil}");
        }
    }
}

#[test]
fn output_temporaries_preserve_branch_local_single_evaluation() {
    let entry = "entry preserve(bool choose) emits next: Counter {
        unrestricted(next.value);
        if (choose) {
            become next <- Counter(snapshot(1));
        } else {
            become next <- Counter(snapshot(2));
        }
    }";
    let actors = check_output_naming("branched-output-names", routed_counter_source(entry));
    let sil = &actors["Coordinator"];
    let branch = sil.split_once("if (choose)").expect("condition stays in entry").1;
    let (left, right) = branch.split_once("} else {").expect("both branches remain");
    assert_eq!(sil.matches("snapshot(1)").count(), 1);
    assert_eq!(sil.matches("snapshot(2)").count(), 1);
    assert!(left.contains("gen__0_route_source_next = snapshot(1)"));
    assert!(right.contains("gen__1_route_source_next = snapshot(2)"));
    assert!(left.contains("gen__0_route_physical_next ="));
    assert!(right.contains("gen__1_route_physical_next ="));
}

#[test]
fn output_temporaries_allocate_distinct_ranged_route_sites() {
    let entry = "entry preserve(bool choose) emits { nextState: Counter[1..=2], } {
        CounterState[] values;
        values = values.append(CounterState { count: 1 });
        for (i, 0, nextState.length, 2) { unrestricted(nextState[i].value); }
        if (choose) {
            become nextState <- Counter[](values);
        } else {
            become nextState <- Counter[](values);
        }
    }";
    let actors = check_output_naming("branched-range-output-names", routed_counter_source(entry));
    let sil = &actors["Coordinator"];
    assert_eq!(sil.matches("values[gen__nextState_output_position]").count(), 2);
    assert!(sil.contains("gen__0_route_source_next_state = values["));
    assert!(sil.contains("gen__1_route_source_next_state = values["));
    assert!(sil.contains("gen__0_route_physical_next_state ="));
    assert!(sil.contains("gen__1_route_physical_next_state ="));
}

#[test]
fn output_temporaries_avoid_later_selector_bindings() {
    let actors = check_output_naming(
        "selector-output-names",
        r#"
            state CounterStateSelector { int count; }
            state S { int n; }
            actor enum Choice { Alpha; Beta; }
            actor Counter owns CounterStateSelector { entry hold() emits none { require(count >= 0); } }
            actor Alpha owns S { entry hold() emits none { require(n >= 0); } }
            actor Beta owns S { entry hold() emits none { require(n >= 1); } }
            actor Coordinator owns S {
                entry preserve(int index, cov_id peer_id)
                observes peer by peer_id { outputs { next: Counter, } }
                emits selected: Choice {
                    unrestricted(selected.value);
                    require peer.outputs become { next <- Counter(CounterStateSelector { count: 1 }), };
                    Choice source_peer_next_counter_state = Choice[index];
                    become selected <- source_peer_next_counter_state(S { n: 0 });
                }
            }
            app Example { actor Counter; actor Alpha; actor Beta; actor Coordinator; }
        "#
        .to_string(),
    );
    let sil = &actors["Coordinator"];
    let (before, after) = sil.split_once("int gen__source_peer_next_counter_state_selector =").expect("selector retains its binding");
    assert!(before.contains("gen__0_route_source_next ="), "earlier temporary avoids later selector");
    assert!(after.contains("gen__source_peer_next_counter_state_template ="));
}

#[test]
fn output_temporaries_avoid_expanded_field_bindings() {
    let actors = check_output_naming(
        "expanded-field-output-names",
        r#"
            state Capsule { virtual route_tmp_0; }
            state Details { int source; int physical; }
            state Expanded expands Capsule { route_tmp_0: Details; }
            actor Vault owns Expanded {
                entry preserve() emits next: Vault {
                    unrestricted(next.value);
                    become next <- Vault(Expanded {
                        route_tmp_0: Details {
                            source: route_tmp_0.source,
                            physical: route_tmp_0.physical,
                        },
                    });
                }
            }
            app Example { actor Vault; }
        "#
        .to_string(),
    );
    let sil = &actors["Vault"];
    assert!(sil.contains("int gen__route_tmp_0_source ="));
    assert!(sil.contains("int gen__route_tmp_0_physical ="));
    assert!(sil.contains("Expanded gen__0_route_source_next ="));
    assert!(sil.contains("State gen__0_route_physical_next ="));
}

// check that generated code and the ast compile to the same result
fn check_output_naming(case: &str, source: String) -> BTreeMap<String, String> {
    let sources = crate::compiler::loader::SourceSet::discover_inline(PathBuf::from(format!("{case}.ag")), source)
        .expect("naming source resolves");
    sources
        .with_resolved(|program| {
            let model = AppCompilationContext::from_resolved(
                &program,
                None,
                &BTreeMap::new(),
                &crate::compiler::model::default_route_planner,
            )?;
            let actors = actor_sil_for_model(&model);
            let artifact = emit_artifact(&program, &model)?;
            assert_eq!(actor_sil_for_model(&model), actors, "{case}: naming is deterministic");
            for (actor_id, _) in model.app_actors.iter_with_ids() {
                let actor = model.actor_by_decl(actor_id)?;
                let args = constructor_args_for_actor(actor_id, actor, &model)?;
                let reparsed = compile_contract(&actors[&actor.name], &args, CompileOptions::default())
                    .unwrap_or_else(|err| panic!("{case}: {} source compiles: {err}", actor.name));
                let direct = artifact.sil_abi.contract(&actor.name).expect("direct ABI exists");
                assert_eq!(reparsed.bytecode, direct.compiled.bytecode, "{case}: {} bytecode", actor.name);
                assert_eq!(reparsed.template_hash(), direct.compiled.template_hash, "{case}: {} template", actor.name);
                assert_eq!(reparsed.state_layout.start, direct.compiled.state_span.offset, "{case}: state start");
                assert_eq!(reparsed.state_layout.len, direct.compiled.state_span.len, "{case}: state length");
                let abi = sil_abi_artifact_from_compiled(&reparsed, &args).expect("source ABI exists");
                assert_eq!(abi.contract(&actor.name).expect("source contract exists").entries, direct.entries, "{case}: entry ABI");
            }
            Ok(actors)
        })
        .unwrap_or_else(|err| panic!("{case}: {err}"))
}

fn plain_counter_source(entry: &str) -> String {
    counter_source(entry, "")
}

fn routed_counter_source(entry: &str) -> String {
    counter_source(
        entry,
        r#"
            entry reset() emits next: Coordinator {
                unrestricted(next.value);
                become next <- Coordinator(CoordinatorState { nonce: count });
            }
        "#,
    )
}

fn counter_source(entry: &str, counter_entries: &str) -> String {
    format!(
        r#"
            state CounterState {{ int count; }}
            state CoordinatorState {{ int nonce; }}
            fn snapshot(int count) -> CounterState {{ return CounterState {{ count: count }}; }}
            actor Counter owns CounterState {{
                entry hold() emits none {{ require(count >= 0); }}
                {counter_entries}
            }}
            actor Coordinator owns CoordinatorState {{ {entry} }}
            app Example {{ actor Counter; actor Coordinator; }}
        "#,
    )
}

use super::*;

fn counter_artifact() -> Artifact {
    inline_artifact(
        "inactive-route-template-bytes",
        r#"
            state CounterState { int count; }
            actor Counter owns CounterState {
                entry advance(bool finish) emits next: Counter | Finished {
                    unrestricted(next.value);
                    CounterState updated = { count: count + 1 };
                    if (finish) {
                        become next <- Finished(updated);
                    } else {
                        become next <- Counter(updated);
                    }
                }
            }
            actor Finished owns CounterState {
                entry close() emits none { require(count > 0); }
            }
            app Test { actor Counter; actor Finished; }
        "#,
    )
}

fn assert_counter_signature(artifact: &Artifact, tx: &Transaction, input_index: usize, finish: bool, include_template: bool) {
    let entry = entry_artifact(artifact, "Counter", "advance");
    assert_eq!(entry.hidden_params.len(), 2);
    let template = sil_template(artifact.sil_abi.contract("Finished").expect("Finished contract"));
    let (prefix, suffix) = if include_template { (template.prefix, template.suffix) } else { (Vec::new(), Vec::new()) };
    assert_signature_args(
        artifact,
        "Counter",
        "advance",
        &tx.inputs[input_index].signature_script,
        vec![ArtifactValue::Bool(finish), ArtifactValue::Bytes(prefix), ArtifactValue::Bytes(suffix)],
    );
}

pub(super) fn assert_signature_args(artifact: &Artifact, actor: &str, entry: &str, actual: &[u8], args: Vec<ArtifactValue>) {
    let contract = artifact.sil_abi.contract(actor).expect("contract");
    let sil_entry = contract.entry(entry).expect("entry");
    let entry_script = encode_entry_sig_script(&artifact.sil_abi, actor, contract, entry, sil_entry, &args).expect("arguments encode");
    let expected = pay_to_script_hash_signature_script_with_flags(p2sh_redeem_script(actual), entry_script, covenant_engine_flags())
        .expect("expected P2SH signature script");
    assert_eq!(actual, expected, "exact ABI arguments for {actor}::{entry}");
}

#[test]
fn static_template_bytes_follow_the_actual_successor() {
    let artifact = counter_artifact();
    let builder = TxBuilder::new(&artifact).expect("builder");
    let covenant_id = Hash::from_bytes([0x51; 32]);
    let outpoint = TransactionOutpoint::new(TransactionId::from_bytes([0x52; 32]), 0);
    let utxo = builder.covenant_utxo("Counter", count_state(4), 1_000, 0, false, Some(covenant_id)).expect("input UTXO");
    let context = |finish, successor, count| {
        TxContext::new()
            .actor_input("Counter", count_state(4), EntryCall::new("advance").args(args![finish]), outpoint, utxo.clone(), 0)
            .actor_output(successor, count_state(count), CovenantBinding::new(0, covenant_id), 1_000)
    };

    let continuing = builder.build(&context(false, "Counter", 5)).expect("inactive route prunes automatically");
    assert_counter_signature(&artifact, &continuing, 0, false, false);
    let finished = builder.build(&context(true, "Finished", 5)).expect("active route keeps its template");
    assert_counter_signature(&artifact, &finished, 0, true, true);
    assert!(continuing.inputs[0].signature_script.len() < finished.inputs[0].signature_script.len());

    for invalid in [context(true, "Finished", 6), context(true, "Counter", 5), context(false, "Finished", 5)] {
        assert!(matches!(builder.build(&invalid), Err(BuilderError::InputScript { input_index: 0, .. })));
    }
}

#[test]
fn pruning_is_scoped_to_global_authorizing_input_not_covenant_id() {
    let artifact = counter_artifact();
    let builder = TxBuilder::new(&artifact).expect("builder");
    let covenant_id = Hash::from_bytes([0x53; 32]);
    let utxo = builder.covenant_utxo("Counter", count_state(4), 1_000, 0, false, Some(covenant_id)).expect("actor UTXO");
    let context = TxContext::new()
        .input(
            TransactionOutpoint::new(TransactionId::from_bytes([0x54; 32]), 0),
            UtxoEntry::new(100, ScriptPublicKey::new(0, vec![OpTrue].into()), 0, false, None),
            InputSigScript::Static(Vec::new()),
            0,
        )
        .actor_input(
            "Counter",
            count_state(4),
            EntryCall::new("advance").args(args![false]),
            TransactionOutpoint::new(TransactionId::from_bytes([0x55; 32]), 0),
            utxo.clone(),
            0,
        )
        .actor_input(
            "Counter",
            count_state(4),
            EntryCall::new("advance").args(args![true]),
            TransactionOutpoint::new(TransactionId::from_bytes([0x56; 32]), 0),
            utxo,
            0,
        )
        .actor_output("Counter", count_state(5), CovenantBinding::new(1, covenant_id), 1_000)
        .actor_output("Finished", count_state(5), CovenantBinding::new(2, covenant_id), 1_000)
        .output(ScriptPublicKey::new(0, vec![OpTrue].into()), None, 100);
    let transaction = builder.build(&context).expect("independent transitions batch under one covenant ID");
    assert_counter_signature(&artifact, &transaction, 1, false, false);
    assert_counter_signature(&artifact, &transaction, 2, true, true);
}

#[test]
fn raw_successor_keeps_static_templates() {
    let artifact = counter_artifact();
    let builder = TxBuilder::new(&artifact).expect("builder");
    let covenant_id = Hash::from_bytes([0x57; 32]);
    let utxo = builder.covenant_utxo("Counter", count_state(4), 1_000, 0, false, Some(covenant_id)).expect("input UTXO");
    let output = builder.covenant_utxo("Counter", count_state(5), 1_000, 0, false, Some(covenant_id)).expect("output UTXO");
    let context = TxContext::new()
        .actor_input(
            "Counter",
            count_state(4),
            EntryCall::new("advance").args(args![false]),
            TransactionOutpoint::new(TransactionId::from_bytes([0x58; 32]), 0),
            utxo,
            0,
        )
        .output(output.script_public_key, Some(CovenantBinding::new(0, covenant_id)), 1_000);
    let transaction = builder.build(&context).expect("raw successor remains supported");
    assert_counter_signature(&artifact, &transaction, 0, false, true);
}

#[test]
fn optional_consumes_retain_template_bytes_without_a_matching_successor() {
    let artifact = inline_artifact(
        "optional-consume-template-retention",
        r#"
            state CounterState { int count; }
            actor Counter owns CounterState {
                entry advance(bool finish)
                consumes { peers: Finished[0..=1], }
                emits next: Counter | Finished {
                    unrestricted(next.value);
                    CounterState updated = { count: count + 1 };
                    if (finish) {
                        become next <- Finished(updated);
                    } else {
                        become next <- Counter(updated);
                    }
                }
            }
            actor Finished owns CounterState {
                entry close() emits none { require(count > 0); }
            }
            app Test { actor Counter; actor Finished; }
        "#,
    );
    let builder = TxBuilder::new(&artifact).expect("builder");
    let covenant_id = Hash::from_bytes([0x59; 32]);
    let counter = builder.covenant_utxo("Counter", count_state(4), 1_000, 0, false, Some(covenant_id)).expect("Counter UTXO");
    let peer = builder.covenant_utxo("Finished", count_state(9), 100, 0, false, Some(covenant_id)).expect("Finished UTXO");
    for peer_count in [0, 1] {
        let mut context = TxContext::new().actor_input(
            "Counter",
            count_state(4),
            EntryCall::new("advance").args(args![false]),
            TransactionOutpoint::new(TransactionId::from_bytes([0x5a; 32]), 0),
            counter.clone(),
            0,
        );
        if peer_count != 0 {
            context = context.actor_input(
                "Finished",
                count_state(9),
                "close",
                TransactionOutpoint::new(TransactionId::from_bytes([0x5b; 32]), 0),
                peer.clone(),
                0,
            );
        }
        context = context.actor_output("Counter", count_state(5), CovenantBinding::new(0, covenant_id), 1_000);
        let transaction = builder.build(&context).expect("optional consumes execute");
        // These same bytes supply prefix/suffix lengths when authenticating the consumed range.
        assert_counter_signature(&artifact, &transaction, 0, false, true);
    }
}

#[test]
fn static_spawn_keeps_template_bytes_without_a_matching_continuation() {
    let artifact = inline_artifact(
        "spawn-template-retention",
        r#"
            state CounterState { int count; }
            actor Counter owns CounterState {
                entry advance(bool finish)
                spawns children by children_id { outputs { child: Finished, } }
                emits next: Counter | Finished {
                    unrestricted(children.outputs.child.value);
                    require children.outputs become { child <- Finished(CounterState { count: count }), };
                    unrestricted(next.value);
                    CounterState updated = { count: count + 1 };
                    if (finish) {
                        become next <- Finished(updated);
                    } else {
                        become next <- Counter(updated);
                    }
                }
            }
            actor Finished owns CounterState {
                entry close() emits none { require(count > 0); }
            }
            app Test { actor Counter; actor Finished; }
        "#,
    );
    let builder = TxBuilder::new(&artifact).expect("builder");
    let covenant_id = Hash::from_bytes([0x5c; 32]);
    let counter = builder.covenant_utxo("Counter", count_state(4), 1_000, 0, false, Some(covenant_id)).expect("Counter UTXO");
    let context = TxContext::new()
        .actor_input(
            "Counter",
            count_state(4),
            EntryCall::new("advance").args(args![false]),
            TransactionOutpoint::new(TransactionId::from_bytes([0x5d; 32]), 0),
            counter,
            0,
        )
        .actor_output("Counter", count_state(5), CovenantBinding::new(0, covenant_id), 900)
        .actor_genesis_output(0, "spawn::children", "Finished", count_state(4), 100);
    let transaction = builder.build(&context).expect("static spawn executes");
    let template = sil_template(artifact.sil_abi.contract("Finished").expect("Finished contract"));
    assert_signature_args(
        &artifact,
        "Counter",
        "advance",
        &transaction.inputs[0].signature_script,
        vec![
            ArtifactValue::Bool(false),
            ArtifactValue::Bytes(template.prefix),
            ArtifactValue::Bytes(template.suffix),
            ArtifactValue::Int(1),
        ],
    );
}

#[test]
fn static_observation_keeps_template_bytes_without_a_matching_continuation() {
    let artifact = inline_artifact(
        "observe-template-retention",
        r#"
            state CounterState { int count; }
            actor Counter owns CounterState {
                entry advance(cov_id other_id, bool finish)
                observes other by other_id {
                    inputs { worker: Worker, }
                    outputs { result: Finished, }
                }
                emits next: Counter | Finished {
                    require other.outputs become { result <- Finished(CounterState { count: 9 }), };
                    unrestricted(next.value);
                    CounterState updated = { count: count + 1 };
                    if (finish) {
                        become next <- Finished(updated);
                    } else {
                        become next <- Counter(updated);
                    }
                }
            }
            actor Worker owns CounterState {
                entry finish() emits result: Finished {
                    unrestricted(result.value);
                    become result <- Finished(CounterState { count: 9 });
                }
            }
            actor Finished owns CounterState {
                entry close() emits none { require(count > 0); }
            }
            app Test { actor Counter; actor Worker; actor Finished; }
        "#,
    );
    let builder = TxBuilder::new(&artifact).expect("builder");
    let covenant_id = Hash::from_bytes([0x5e; 32]);
    let other_id = Hash::from_bytes([0x5f; 32]);
    let counter = builder.covenant_utxo("Counter", count_state(4), 1_000, 0, false, Some(covenant_id)).expect("Counter UTXO");
    let worker = builder.covenant_utxo("Worker", count_state(8), 100, 0, false, Some(other_id)).expect("Worker UTXO");
    let context = TxContext::new()
        .actor_input(
            "Counter",
            count_state(4),
            EntryCall::new("advance").args(args![other_id, false]),
            TransactionOutpoint::new(TransactionId::from_bytes([0x60; 32]), 0),
            counter,
            0,
        )
        .actor_input("Worker", count_state(8), "finish", TransactionOutpoint::new(TransactionId::from_bytes([0x61; 32]), 0), worker, 0)
        .actor_output("Counter", count_state(5), CovenantBinding::new(0, covenant_id), 1_000)
        .actor_output("Finished", count_state(9), CovenantBinding::new(1, other_id), 100);
    let transaction = builder.build(&context).expect("static observation executes");
    let finished = sil_template(artifact.sil_abi.contract("Finished").expect("Finished contract"));
    let worker = sil_template(artifact.sil_abi.contract("Worker").expect("Worker contract"));
    assert_signature_args(
        &artifact,
        "Counter",
        "advance",
        &transaction.inputs[0].signature_script,
        vec![
            ArtifactValue::Bytes(other_id.as_bytes().to_vec()),
            ArtifactValue::Bool(false),
            ArtifactValue::Int(worker.prefix.len() as i64),
            ArtifactValue::Int(worker.suffix.len() as i64),
            ArtifactValue::Bytes(finished.prefix),
            ArtifactValue::Bytes(finished.suffix),
        ],
    );
}

#[test]
fn module_qualified_consumes_keep_their_templates() {
    let temp = std::env::temp_dir().join(format!(
        "argent-qualified-consume-omissions-{}-{}",
        std::process::id(),
        ARTIFACT_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&temp).expect("source directory");
    fs::write(
        temp.join("peer.ag"),
        r#"
        state PeerState { int count; }
        actor Peer owns PeerState {
            entry close() emits none { require(count >= 0); }
        }
    "#,
    )
    .expect("peer source");
    fs::write(
        temp.join("app.ag"),
        r#"
        import "./peer.ag" as helpers;
        state CounterState { int count; }
        actor Counter owns CounterState {
            entry advance(bool finish)
            consumes { peers: helpers::Peer[0..=1], }
            emits next: Counter | Finished {
                unrestricted(next.value);
                if (finish) {
                    become next <- Finished(CounterState { count: count + 1 });
                } else {
                    become next <- Counter(CounterState { count: count + 1 });
                }
            }
            entry inspect() consumes { peers: helpers::Peer[0..=1], } emits none { require(count >= 0); }
        }
        actor Finished owns CounterState {
            entry close() emits none { require(count > 0); }
        }
        app Test { actor Counter; actor helpers::Peer; actor Finished; }
    "#,
    )
    .expect("root source");
    let artifact = crate::build_file(temp.join("app.ag"), temp.join("build")).expect("qualified consume compiles");
    let builder = TxBuilder::new(&artifact).expect("builder");
    let covenant_id = Hash::from_bytes([0x62; 32]);
    let counter = builder.covenant_utxo("Counter", count_state(4), 1_000, 0, false, Some(covenant_id)).expect("Counter UTXO");
    let peer = builder.covenant_utxo("Peer", count_state(9), 100, 0, false, Some(covenant_id)).expect("Peer UTXO");
    assert_eq!(entry_artifact(&artifact, "Counter", "advance").consumes[0].actor, "helpers::Peer");
    for peer_count in [0, 1] {
        let context = |entry| {
            let mut context = TxContext::new().actor_input(
                "Counter",
                count_state(4),
                entry,
                TransactionOutpoint::new(TransactionId::from_bytes([0x63; 32]), 0),
                counter.clone(),
                0,
            );
            if peer_count != 0 {
                context = context.actor_input(
                    "Peer",
                    count_state(9),
                    "close",
                    TransactionOutpoint::new(TransactionId::from_bytes([0x64; 32]), 0),
                    peer.clone(),
                    0,
                );
            }
            context
        };
        let transaction = builder
            .build(&context(EntryCall::new("advance").args(args![false])).actor_output(
                "Counter",
                count_state(5),
                CovenantBinding::new(0, covenant_id),
                1_000,
            ))
            .expect("qualified consume executes");
        let peer = sil_template(artifact.sil_abi.contract("Peer").expect("Peer contract"));
        let template = sil_template(artifact.sil_abi.contract("Finished").expect("Finished contract"));
        assert_signature_args(
            &artifact,
            "Counter",
            "advance",
            &transaction.inputs[0].signature_script,
            vec![
                ArtifactValue::Bool(false),
                ArtifactValue::Int(peer.prefix.len() as i64),
                ArtifactValue::Int(peer.suffix.len() as i64),
                ArtifactValue::Bytes(template.prefix),
                ArtifactValue::Bytes(template.suffix),
            ],
        );
        // Entries with no byte witnesses must not try resolving source-facing consume names either.
        builder.build(&context(EntryCall::new("inspect"))).expect("length-only qualified consume executes");
    }
    fs::remove_dir_all(temp).expect("remove test files");
}

#[test]
fn linked_spawn_bytes_remain_outside_current_successor_pruning() {
    let temp = std::env::temp_dir().join(format!(
        "argent-linked-spawn-omissions-{}-{}",
        std::process::id(),
        ARTIFACT_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&temp).expect("source directory");
    fs::write(
        temp.join("child.ag"),
        r#"
        state ChildState { int count; }
        actor Child owns ChildState { entry close() emits none { require(count > 0); } }
        app ChildApp { actor Child; }
    "#,
    )
    .expect("child source");
    fs::write(
        temp.join("app.ag"),
        r#"
        import "./child.ag" as dep;
        state LauncherState { int count; }
        actor Launcher owns LauncherState {
            entry launch()
            spawns children by children_id { outputs { child: dep::ChildApp::Child, } }
            emits next: Launcher {
                unrestricted(children.outputs.child.value);
                require children.outputs become { child <- dep::ChildApp::Child(dep::ChildState { count: count }), };
                unrestricted(next.value);
                become next <- Launcher(LauncherState { count: count + 1 });
            }
        }
        app LauncherApp { actor Launcher; }
    "#,
    )
    .expect("launcher source");
    let compiled =
        crate::build_file_app_bundle(temp.join("app.ag"), "LauncherApp", temp.join("build")).expect("linked source bundle compiles");
    let bundle = compiled.runtime_bundle().expect("runtime bundle");
    let builder = TxBuilder::from_bundle(&bundle).expect("builder");
    let artifact = compiled.primary();
    let covenant_id = Hash::from_bytes([0x65; 32]);
    let launcher = builder.covenant_utxo("Launcher", count_state(4), 1_000, 0, false, Some(covenant_id)).expect("Launcher UTXO");
    let context = TxContext::new()
        .actor_input(
            "Launcher",
            count_state(4),
            "launch",
            TransactionOutpoint::new(TransactionId::from_bytes([0x66; 32]), 0),
            launcher,
            0,
        )
        .actor_output("Launcher", count_state(5), CovenantBinding::new(0, covenant_id), 900)
        .actor_genesis_output(0, "spawn::children", "child_app::Child", count_state(4), 100);
    let transaction = builder.build(&context).expect("linked spawn executes");
    let child = &compiled.app("ChildApp").expect("ChildApp").argent.template_plan.templates[0].actor_type_handle.template;
    assert_signature_args(
        artifact,
        "Launcher",
        "launch",
        &transaction.inputs[0].signature_script,
        vec![ArtifactValue::Bytes(child.prefix.clone()), ArtifactValue::Bytes(child.suffix.clone()), ArtifactValue::Int(1)],
    );
    fs::remove_dir_all(temp).expect("remove test files");
}

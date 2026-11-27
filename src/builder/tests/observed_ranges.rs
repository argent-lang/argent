use super::*;

#[test]
fn observed_input_range_and_trailing_singleton_follow_covenant_order() {
    let artifact = inline_artifact(
        "observe-input-range",
        r#"
            state WatcherState { cov_id remote_id; }
            state AssetState { int amount; }
            state MarkerState { int tag; }

            actor Watcher owns WatcherState {
                entry inspect(int expected_count, int last, int expected_amount)
                observes remote by self.remote_id {
                    inputs {
                        assets: Asset[1..=3],
                        marker: Marker,
                    }
                    outputs {
                        kept: Asset,
                    }
                }
                emits none {
                    require(remote.inputs.assets.length == expected_count);
                    require(remote.inputs.assets[last].amount == expected_amount);
                    require(remote.inputs.assets[last].value == 1_000);
                    require(remote.inputs.assets[last].cov_id == self.remote_id);
                    require(remote.inputs.marker.tag == 99);
                    AssetState first = state(remote.inputs.assets[0]);
                    require remote.outputs become { kept <- Asset(first), };
                }
            }

            actor Asset owns AssetState {
                entry keep() emits next: Asset {
                    require(next.value == self.value);
                    become next <- self;
                }

                entry close() emits none { require(amount > 0); }
            }

            actor Marker owns MarkerState {
                entry keep() emits next: Marker {
                    require(next.value == self.value);
                    become next <- self;
                }

                entry close() emits none { require(tag > 1); }
            }

            app Test { actor Watcher; actor Asset; actor Marker; }
        "#,
    );
    let builder = TxBuilder::new(&artifact).expect("builder accepts observed input cardinality");
    let remote_id = Hash::from_bytes([0x83; 32]);
    let local_id = Hash::from_bytes([0x84; 32]);
    let outpoint = |byte| TransactionOutpoint::new(TransactionId::from_bytes([byte; 32]), 0);
    let op_true = ScriptPublicKey::new(0, vec![OpTrue].into());
    let watcher = state! { remote_id: remote_id };
    let watcher_utxo = builder.covenant_utxo("Watcher", watcher.clone(), 1_000, 0, false, Some(local_id)).unwrap();
    let marker = state! { tag: 99 };
    let marker_utxo = builder.covenant_utxo("Marker", marker.clone(), 1_000, 0, false, Some(remote_id)).unwrap();
    let context = |len: u8| {
        let last = i64::from(len) - 1;
        let mut context = TxContext::new().actor_input(
            "Watcher",
            watcher.clone(),
            EntryCall::new("inspect").args(args![i64::from(len), last, 7 + last]),
            outpoint(0x85),
            watcher_utxo.clone(),
            0,
        );
        for index in 0..len {
            if index == 1 {
                context = context.input(outpoint(0x86), UtxoEntry::new(100, op_true.clone(), 0, false, None), Vec::new(), 0);
            }
            let state = state! { amount: 7 + i64::from(index) };
            let utxo = builder.covenant_utxo("Asset", state.clone(), 1_000, 0, false, Some(remote_id)).unwrap();
            let entry = if index == 0 { "keep" } else { "close" };
            context = context.actor_input("Asset", state, entry, outpoint(0x87 + index), utxo, 0);
        }
        context
            .actor_input("Marker", marker.clone(), "close", outpoint(0x8b), marker_utxo.clone(), 0)
            .output(op_true.clone(), None, 100)
            .actor_output("Asset", state! { amount: 7 }, CovenantBinding::new(1, remote_id), 1_000)
    };

    for len in [1, 2, 3] {
        builder.build(&context(len)).unwrap_or_else(|error| panic!("{len} observed assets: {error}"));
    }
    for (len, found) in [(0, 1), (4, 5)] {
        let error = builder.build(&context(len)).expect_err("observed input count is outside the declared bounds");
        assert!(
            matches!(error, BuilderError::ObservedCardinalityMismatch { observe, side: Side::In, handle, minimum: 2, maximum: 4, found: actual }
            if observe == "remote" && handle == "assets" && actual == found)
        );
    }

    let mut transaction = builder.build(&context(2)).expect("two valid observed assets");
    let asset_utxo = |amount| builder.covenant_utxo("Asset", state! { amount: amount }, 1_000, 0, false, Some(remote_id)).unwrap();
    let mut entries =
        vec![watcher_utxo.clone(), asset_utxo(7), UtxoEntry::new(100, op_true.clone(), 0, false, None), asset_utxo(8), marker_utxo];
    execute_input_with_covenants(&transaction, entries.clone(), 0).expect("observer accepts both valid range members in the VM");

    // Leave the first asset in place. Supply a valid Marker spend as the second member,
    // with the same value, covenant ID, and integer state as the asset it replaces.
    let template = |actor| sil_template(artifact.sil_abi.contract(actor).unwrap());
    let (asset_template, marker_template) = (template("Asset"), template("Marker"));
    assert_eq!(asset_template.prefix.len(), marker_template.prefix.len());
    assert_eq!(asset_template.suffix.len(), marker_template.suffix.len());
    assert_ne!(asset_template.hash, marker_template.hash);
    let replacement_state = state! { tag: 8 };
    let replacement_utxo = builder.covenant_utxo("Marker", replacement_state.clone(), 1_000, 0, false, Some(remote_id)).unwrap();
    let replacement = builder
        .build(&TxContext::new().actor_input("Marker", replacement_state, "close", outpoint(0x88), replacement_utxo.clone(), 0))
        .expect("replacement Marker can be spent");
    transaction.inputs[3].signature_script = replacement.inputs[0].signature_script.clone();
    entries[3] = replacement_utxo;

    // Execute the observer directly so the builder's range checks cannot reject first.
    execute_input_with_covenants(&transaction, entries.clone(), 3).expect("replacement input is valid in the observed transaction");
    let error = execute_input_with_covenants(&transaction, entries, 0)
        .expect_err("observer must authenticate the second range member's template");
    assert_eq!(error, kaspa_txscript_errors::TxScriptError::VerifyError);
}

#[test]
fn linked_observed_ranges_validate_every_state_and_allow_empty_inputs() {
    let output = tempfile::tempdir().expect("fixture output directory");
    let compiled = crate::build_file_app_bundle(
        "tests/fixtures/runtime/context_ranged_observe_outputs/observer.ag",
        "ObserverApp",
        output.path(),
    )
    .expect("linked observed ranges compile");
    let observer_entry = entry_artifact(compiled.primary(), "Observer", "inspect");
    let asset_witnesses = observer_entry
        .hidden_params
        .iter()
        .filter(|param| subject_label(&param.subject) == "AssetApp::Asset")
        .map(|param| param.purpose)
        .collect::<Vec<_>>();
    assert_eq!(
        asset_witnesses,
        [HiddenParamPurposeArtifact::TemplatePrefixBytes, HiddenParamPurposeArtifact::TemplateSuffixBytes],
        "linked output template bytes also supply the optional input range's read lengths"
    );

    let bundle = compiled.runtime_bundle().expect("runtime bundle");
    let builder = TxBuilder::from_bundle(&bundle).expect("builder accepts observed output range");
    let remote_id = Hash::from_bytes([0x98; 32]);
    let local_id = Hash::from_bytes([0x99; 32]);
    let outpoint = |byte| TransactionOutpoint::new(TransactionId::from_bytes([byte; 32]), 0);
    let observer = state! { remote_id: remote_id };
    let observer_utxo = builder.covenant_utxo("Observer", observer.clone(), 6_000, 0, false, Some(local_id)).unwrap();
    let issuer = state! { serial: 42 };
    let issuer_utxo = builder.covenant_utxo("asset_app::Issuer", issuer.clone(), 2_000, 0, false, Some(remote_id)).unwrap();
    let covenant = CovenantBinding::new(1, remote_id);
    let states = |amounts: &[i64]| amounts.iter().map(|&amount| state! { amount: amount }).collect::<Vec<_>>();
    let observer_context =
        |entry| TxContext::new().actor_input("Observer", observer.clone(), entry, outpoint(0x9a), observer_utxo.clone(), 0);
    let context = |observed: &[i64], expected: &[i64], actual: &[i64]| {
        let mut context = observer_context(EntryCall::new("inspect").args(args![observed.len() as i64, states(expected)]))
            .actor_input(
                "asset_app::Issuer",
                issuer.clone(),
                EntryCall::new("split").args(args![states(actual)]),
                outpoint(0x9b),
                issuer_utxo.clone(),
                0,
            );
        for (index, state) in states(observed).into_iter().enumerate() {
            let utxo = builder.covenant_utxo("asset_app::Asset", state.clone(), 1_000, 0, false, Some(remote_id)).unwrap();
            context = context.actor_input("asset_app::Asset", state, "inspect", outpoint(0x9c + index as u8), utxo, 0);
        }
        for (index, state) in states(actual).into_iter().enumerate() {
            if index == 1 {
                context = context.output(ScriptPublicKey::new(0, vec![OpTrue].into()), None, 100);
            }
            context = context.actor_output("asset_app::Asset", state, covenant, 1_000);
        }
        context.actor_output("asset_app::Issuer", issuer.clone(), covenant, 2_000)
    };

    for amounts in [&[][..], &[7], &[7, 8]] {
        builder
            .build(&context(&[], amounts, amounts))
            .unwrap_or_else(|error| panic!("no Asset inputs, {} observed Asset outputs: {error}", amounts.len()));
    }
    builder.build(&context(&[7, 8], &[7, 8], &[7, 8])).expect("optional Asset inputs read using the witnessed template lengths");
    builder.build(&observer_context("absent".into())).expect("the observed covenant may have no transaction members");

    let error = builder.build(&context(&[7, 9], &[7], &[7])).expect_err("observer checks every input state");
    assert!(matches!(error, BuilderError::InputScript { input_index: 0, .. }), "unexpected error: {error}");
    for expected in [&[7][..], &[7, 9]] {
        let error =
            builder.build(&context(&[], expected, &[7, 8])).expect_err("observer checks the array length and all output states");
        assert!(matches!(error, BuilderError::InputScript { input_index: 0, .. }), "unexpected error: {error}");
    }
    let error = builder.build(&context(&[], &[7, 8, 9], &[7, 8, 9])).expect_err("observed output count exceeds its declared bounds");
    assert!(matches!(
        error,
        BuilderError::ObservedCardinalityMismatch { observe, side: Side::Out, handle, minimum: 1, maximum: 3, found: 4 }
            if observe == "remote" && handle == "items"
    ));
}

#[test]
fn ranged_current_and_observed_transitions_keep_their_own_members() {
    let source = r#"
        state CoordinatorState { cov_id remote_id; }
        state ItemState { int debit; int credit; }

        actor Coordinator owns CoordinatorState {
            entry settle(ItemState[] expected_current, ItemState[] expected_remote)
            consumes { current: Item[CURRENT_MINIMUM..=2], }
            observes remote by self.remote_id {
                inputs { items: Watched[1..=4], }
                outputs { next: Watched[0..=5], }
            }
            emits { next_current: Item[1..=3], } {
                require(current.length == expected_current.length);
                require(remote.inputs.items.length == expected_remote.length);

                ItemState[] local_next;
                for (i, 0, current.length, 2) {
                    ItemState found = state(current[i]);
                    require(found.debit == expected_current[i].debit);
                    require(found.credit == expected_current[i].credit);
                    local_next = local_next.append(ItemState { debit: found.debit + 100, credit: found.credit + 200 });
                }
                local_next = local_next.append(ItemState { debit: 113, credit: 223 });

                ItemState[] remote_next;
                for (i, 0, remote.inputs.items.length, 4) {
                    ItemState found = state(remote.inputs.items[i]);
                    require(found.debit == expected_remote[i].debit);
                    require(found.credit == expected_remote[i].credit);
                    remote_next = remote_next.append(ItemState { debit: found.debit + 300, credit: found.credit + 400 });
                }
                remote_next = remote_next.append(ItemState { debit: 353, credit: 503 });

                require remote.outputs become { next <- Watched[](remote_next), };
                unrestricted(next_current[0].value);
                become next_current <- Item[](local_next);
            }
        }

        actor Item owns ItemState {
            delegate participate() consumes { leader: Coordinator, } {}
            entry advance(ItemState[] successors) emits { next: Item[1..=2], } {
                unrestricted(next[0].value);
                become next <- Item[](successors);
            }
        }

        actor OtherItem owns ItemState {
            entry advance(ItemState[] successors) emits { next: OtherItem[1..=2], } {
                require(credit > debit);
                unrestricted(next[0].value);
                become next <- OtherItem[](successors);
            }
        }

        app Test { actor Coordinator; actor Item; actor OtherItem; }
    "#;
    let current_inputs = [[4, 17], [8, 19]];
    let remote_inputs = [[25, 77], [26, 79], [37, 89], [43, 97]];
    let current_outputs = [[104, 217], [108, 219], [113, 223]];
    let remote_outputs = [[325, 477], [326, 479], [337, 489], [343, 497], [353, 503]];
    let state = |[debit, credit]: [i64; 2]| state! { debit: debit, credit: credit };
    let states = |values: &[[i64; 2]]| values.iter().copied().map(state).collect::<Vec<_>>();
    let current_id = Hash::from_bytes([0x9c; 32]);
    let remote_id = Hash::from_bytes([0x9d; 32]);
    let outpoint = |index| TransactionOutpoint::new(TransactionId::from_bytes([0x9e; 32]), index);

    for (watched, current_minimum) in [("Item", 1), ("OtherItem", 1), ("Item", 0)] {
        let source = source.replace("Watched", watched).replace("CURRENT_MINIMUM", &current_minimum.to_string());
        let artifact = inline_artifact("combined-observed-ranges", &source);
        if watched == "Item" {
            let item_witnesses = entry_artifact(&artifact, "Coordinator", "settle")
                .hidden_params
                .iter()
                .map(|param| (subject_label(&param.subject), param.purpose))
                .collect::<Vec<_>>();
            assert_eq!(
                item_witnesses,
                [("Item", HiddenParamPurposeArtifact::TemplatePrefixLen), ("Item", HiddenParamPurposeArtifact::TemplateSuffixLen)],
                "the two groups share one pair of Item template lengths and need no template bytes"
            );
        } else {
            let template_hash = |actor| artifact.sil_abi.contract(actor).unwrap().compiled.template_hash;
            assert_ne!(template_hash("Item"), template_hash(watched));
        }
        let (expected_current, emitted_states) = if current_minimum == 0 {
            // The guaranteed observed range supplies the Item proof even though current has no members.
            (&current_inputs[..0], &current_outputs[2..])
        } else {
            (&current_inputs[..], &current_outputs[..])
        };
        let builder = TxBuilder::new(&artifact).expect("builder accepts the combined ranges");
        let coordinator = state! { remote_id: remote_id };
        let coordinator_utxo = builder.covenant_utxo("Coordinator", coordinator.clone(), 3_000, 0, false, Some(current_id)).unwrap();
        let add_input = |context: TxContext<'static>, actor: &str, values, entry: EntryCall<'static>, covenant_id| {
            let state = state(values);
            let utxo = builder.covenant_utxo(actor, state.clone(), 1_000, 0, false, Some(covenant_id)).unwrap();
            let index = u32::try_from(context.inputs.len()).unwrap();
            context.actor_input(actor, state, entry, outpoint(index), utxo, 0)
        };
        let context = |consumed: &[[i64; 2]], observed: &[[i64; 2]; 4], emitted: &[[i64; 2]], continued: &[[i64; 2]; 5]| {
            let mut context = TxContext::new().actor_input(
                "Coordinator",
                coordinator.clone(),
                EntryCall::new("settle").args(args![states(expected_current), states(&remote_inputs)]),
                outpoint(0),
                coordinator_utxo.clone(),
                0,
            );
            // Interleave remote and current members after the coordinator when both are present.
            let mut remote_authorizers = [0u16; 4];
            // The first remote input authorizes two outputs; each other one authorizes one.
            for (i, (authorizer, &values)) in remote_authorizers.iter_mut().zip(observed).enumerate() {
                *authorizer = u16::try_from(context.inputs.len()).unwrap();
                let outputs = if i == 0 { &continued[..2] } else { &continued[i + 1..i + 2] };
                let entry = EntryCall::new("advance").args(args![states(outputs)]);
                context = add_input(context, watched, values, entry, remote_id);
                if let Some(&values) = consumed.get(i) {
                    context = add_input(context, "Item", values, EntryCall::new("participate"), current_id);
                }
            }
            for (i, &values) in continued.iter().enumerate() {
                let authorizer = remote_authorizers[i.saturating_sub(1)];
                context = context.actor_output(watched, state(values), CovenantBinding::new(authorizer, remote_id), 500);
                if let Some(&values) = emitted.get(i) {
                    context = context.actor_output("Item", state(values), CovenantBinding::new(0, current_id), 500);
                }
            }
            context
        };

        builder
            .build(&context(expected_current, &remote_inputs, emitted_states, &remote_outputs))
            .unwrap_or_else(|error| panic!("combined ranges with {watched} and current minimum {current_minimum}: {error}"));

        if current_minimum == 0 {
            continue;
        }

        for side in ["consumes", "observed inputs", "emits", "observed outputs"] {
            let (mut consumed, mut observed, mut emitted, mut continued) =
                (current_inputs, remote_inputs, current_outputs, remote_outputs);
            match side {
                "consumes" => consumed[1][1] += 1,
                "observed inputs" => observed[3][1] += 1,
                "emits" => emitted[2][1] += 1,
                "observed outputs" => continued[4][1] += 1,
                _ => unreachable!(),
            }
            let error = builder
                .build(&context(&consumed, &observed, &emitted, &continued))
                .expect_err("coordinator must reject the wrong state");
            assert!(matches!(error, BuilderError::InputScript { input_index: 0, .. }), "{watched} / {side}: {error}");
        }
    }
}

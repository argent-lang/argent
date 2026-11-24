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
                entry close() emits none { require(tag == 99); }
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
}

#[test]
fn linked_observed_output_range_validates_every_state() {
    let output = tempfile::tempdir().expect("fixture output directory");
    let compiled = crate::build_file_app_bundle(
        "tests/fixtures/runtime/context_ranged_observe_outputs/observer.ag",
        "ObserverApp",
        output.path(),
    )
    .expect("linked observed output fixture compiles");
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
        "one linked template witness serves the complete range"
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
    let context = |expected: &[i64], actual: &[i64]| {
        let mut context = TxContext::new()
            .actor_input(
                "Observer",
                observer.clone(),
                EntryCall::new("inspect").args(args![states(expected)]),
                outpoint(0x9a),
                observer_utxo.clone(),
                0,
            )
            .actor_input(
                "asset_app::Issuer",
                issuer.clone(),
                EntryCall::new("split").args(args![states(actual)]),
                outpoint(0x9b),
                issuer_utxo.clone(),
                0,
            );
        for (index, state) in states(actual).into_iter().enumerate() {
            if index == 1 {
                context = context.output(ScriptPublicKey::new(0, vec![OpTrue].into()), None, 100);
            }
            context = context.actor_output("asset_app::Asset", state, covenant, 1_000);
        }
        context.actor_output("asset_app::Issuer", issuer.clone(), covenant, 2_000)
    };

    for amounts in [&[][..], &[7], &[7, 8]] {
        builder.build(&context(amounts, amounts)).unwrap_or_else(|error| panic!("observed output length {}: {error}", amounts.len()));
    }
    for expected in [&[7][..], &[7, 9]] {
        let error = builder.build(&context(expected, &[7, 8])).expect_err("observer checks the array length and all output states");
        assert!(matches!(error, BuilderError::InputScript { input_index: 0, .. }), "unexpected error: {error}");
    }
    let error = builder.build(&context(&[7, 8, 9], &[7, 8, 9])).expect_err("observed output count exceeds its declared bounds");
    assert!(matches!(
        error,
        BuilderError::ObservedCardinalityMismatch { observe, side: Side::Out, handle, minimum: 1, maximum: 3, found: 4 }
            if observe == "remote" && handle == "items"
    ));
}

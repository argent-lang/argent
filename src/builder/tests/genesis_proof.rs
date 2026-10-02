use super::*;
use crate::genesis::{
    ArgentGenesisOutput, ArgentGenesisPackage, ArgentGenesisProof, ArgentGenesisProofError, GenesisProofError, GenesisProofLayer,
    GenesisProofPackage, GenesisProofPackageError, SilGenesisProofError,
};

fn outpoint() -> TransactionOutpoint {
    TransactionOutpoint::new(Hash::from_bytes([0x61; 32]), 4)
}

fn expanded_state(nonce: i64) -> BTreeMap<String, ArtifactValue> {
    state! {
        owner_kind: 0,
        owner_id: vec![0x42; 32],
        policy: state! { nonce: nonce },
        balance: 100,
    }
}

fn round_trip_package(package: &GenesisProofPackage) -> GenesisProofPackage {
    let json = package.to_json().expect("package serializes");
    let decoded = GenesisProofPackage::from_json(&json).expect("package deserializes");
    assert_eq!(&decoded, package);
    decoded
}

#[test]
fn authored_proof_data_outlives_its_bundle_and_round_trips_without_artifacts() {
    let (artifact, proof) = {
        let artifact = capsule_route_context_artifact();
        let bundle = ArtifactBundle::new(&artifact).expect("artifact forms a bundle");
        let proof = ArgentGenesisProof::compose(
            &bundle,
            outpoint(),
            vec![ArgentGenesisOutput::new(0, 1_000, "ReserveAsset", expanded_state(7))],
        )
        .expect("proof composes");
        (artifact, proof)
    };
    let json = serde_json::to_string(&proof).expect("proof data serializes without artifacts");
    let decoded: ArgentGenesisProof = serde_json::from_str(&json).expect("proof data deserializes without a bundle");
    assert_eq!(decoded, proof);
    let bundle = ArtifactBundle::new(&artifact).expect("artifact context is supplied separately");
    decoded.verify(&bundle, proof.claimed_covenant_id).expect("restored proof verifies with its context");
}

#[test]
fn authored_package_retains_artifacts_and_rechecks_serialized_claims() {
    let artifact = capsule_route_context_artifact();
    let bundle = ArtifactBundle::new(&artifact).expect("artifact forms a bundle");
    let proof =
        ArgentGenesisProof::compose(&bundle, outpoint(), vec![ArgentGenesisOutput::new(0, 1_000, "ReserveAsset", expanded_state(7))])
            .expect("proof composes");
    let package = GenesisProofPackage::new(ArgentGenesisPackage::new(&bundle, proof.clone()));
    let decoded = round_trip_package(&package);
    let GenesisProofLayer::Argent(authored) = &decoded.proof else {
        panic!("Argent layer is retained");
    };
    assert_eq!(authored.primary, artifact);
    assert!(authored.dependencies.is_empty());
    assert_eq!(authored.proof, proof);
    let restored_bundle = authored.runtime_bundle().expect("runtime context restores");
    assert_eq!(
        authored.proof.sil_proof(&restored_bundle).expect("state materializes"),
        proof.sil_proof(&bundle).expect("original state materializes")
    );
    decoded.verify(proof.claimed_covenant_id).expect("loaded authored proof verifies");
    decoded.verify_argent(proof.claimed_covenant_id).expect("Argent verification checks authored state and context");
    assert!(matches!(
        decoded.verify_argent(Hash::from_bytes([0x99; 32])),
        Err(GenesisProofPackageError::Argent(ArgentGenesisProofError::Sil(SilGenesisProofError::Preimage(
            GenesisProofError::ExpectedCovenantIdMismatch { .. }
        ))))
    ));

    let changes: [fn(&mut ArgentGenesisProof); 7] = [
        |proof| {
            proof.outputs[0].authored_state.insert("balance".to_string(), ArtifactValue::Int(101));
        },
        |proof| {
            proof.outputs[0].authored_state.insert("policy".to_string(), ArtifactValue::Object(state! { nonce: 8 }));
        },
        |proof| {
            proof.outputs[0].actor = "WalletAsset".to_string();
        },
        |proof| {
            proof.outputs[0].value += 1;
        },
        |proof| {
            proof.outputs[0].index += 1;
        },
        |proof| {
            proof.authorizing_outpoint.index += 1;
        },
        |proof| {
            proof.claimed_covenant_id = Hash::from_bytes([0xee; 32]);
        },
    ];
    for change in changes {
        let mut changed = package.clone();
        let GenesisProofLayer::Argent(authored) = &mut changed.proof else {
            panic!("Argent layer is retained");
        };
        change(&mut authored.proof);
        let expected_claim = authored.proof.claimed_covenant_id;
        let decoded = round_trip_package(&changed);
        let GenesisProofLayer::Argent(authored) = &decoded.proof else {
            panic!("Argent layer is retained");
        };
        assert_eq!(authored.proof.claimed_covenant_id, expected_claim);
        assert!(matches!(
            decoded.verify_argent(proof.claimed_covenant_id),
            Err(GenesisProofPackageError::Argent(ArgentGenesisProofError::Sil(SilGenesisProofError::Preimage(
                GenesisProofError::ClaimedCovenantIdMismatch { .. }
            ))))
        ));
    }

    let mut injected = decoded;
    let GenesisProofLayer::Argent(authored) = &mut injected.proof else {
        panic!("Argent layer is retained");
    };
    let role = &artifact
        .argent
        .template_plan
        .runtime_states
        .iter()
        .find(|plan| plan.contract == "ReserveAsset")
        .expect("route context exists")
        .field_roles[0];
    authored.proof.outputs[0].authored_state.insert(role.name.clone(), ArtifactValue::Bytes(vec![0xee; 32]));
    assert!(matches!(
        round_trip_package(&injected).verify_argent(proof.claimed_covenant_id),
        Err(GenesisProofPackageError::Argent(ArgentGenesisProofError::OutputState { source, .. }))
            if matches!(*source, BuilderError::HiddenRuntimeFieldProvided { .. })
    ));
}

#[test]
fn authored_proof_matches_genesis_transaction_with_routes_and_expansions() {
    let artifact = capsule_route_context_artifact();
    let bundle = ArtifactBundle::new(&artifact).expect("artifact forms a bundle");
    let builder = TxBuilder::from_bundle(&bundle).expect("builder accepts bundle");
    let outputs = vec![
        ArgentGenesisOutput::new(0, 2_000, "ReserveAsset", expanded_state(7)),
        ArgentGenesisOutput::new(2, 3_000, "WalletAsset", expanded_state(-5)),
        ArgentGenesisOutput::new(3, 2_000, "ReserveAsset", expanded_state(9)),
    ];
    let proof = ArgentGenesisProof::compose(&bundle, outpoint(), outputs).expect("authored proof composes");
    let sil = proof.sil_proof(&bundle).expect("physical proof materializes");
    assert_eq!(sil.abis, vec![artifact.sil_abi.clone()]);
    assert!(sil.outputs.iter().all(|output| output.abi_index == 0));
    assert_eq!(sil.outputs[1].contract, "WalletAsset");

    // The proof derives route context and expansion digests, not caller-supplied bytes.
    let runtime_plan = artifact
        .argent
        .template_plan
        .runtime_states
        .iter()
        .find(|plan| plan.contract == "ReserveAsset")
        .expect("ReserveAsset has generated route context");
    assert!(!runtime_plan.field_roles.is_empty());
    for role in &runtime_plan.field_roles {
        let expected = crate::artifact::fixed_runtime_context_value(&artifact.argent.template_plan, runtime_plan, role)
            .expect("route commitment derives from the plan");
        assert_eq!(sil.outputs[0].runtime_state[&role.name], ArtifactValue::Bytes(expected));
    }
    // Fixed-width ScriptNum uses sign-magnitude; -5 has its sign bit in the last byte.
    let payloads = [[7, 0, 0, 0, 0, 0, 0, 0], [5, 0, 0, 0, 0, 0, 0, 0x80], [9, 0, 0, 0, 0, 0, 0, 0]];
    for (output, payload) in sil.outputs.iter().zip(payloads) {
        let digest = blake3::hash(&payload).as_bytes().to_vec();
        assert_eq!(output.runtime_state["policy"], ArtifactValue::Bytes(digest));
    }

    let funding_script = ScriptPublicKey::new(0, vec![OpTrue].into());
    let context = TxContext::new()
        .input(outpoint(), UtxoEntry::new(10_000, funding_script.clone(), 0, false, None), Vec::new(), 0)
        .actor_genesis_output(0, "launch::asset", "ReserveAsset", expanded_state(7), 2_000)
        .output(funding_script, None, 1_000)
        .actor_genesis_output(0, "launch::asset", "WalletAsset", expanded_state(-5), 3_000)
        .actor_genesis_output(0, "launch::asset", "ReserveAsset", expanded_state(9), 2_000);
    let transaction = builder.build(&context).expect("genesis transaction executes");
    let preimage = sil.consensus_proof().expect("consensus proof materializes");
    for (authored, indexed) in proof.outputs.iter().zip(&preimage.outputs) {
        let built = builder
            .genesis_output(authored.actor.clone(), authored.authored_state.clone(), authored.value)
            .expect("builder materializes the same actor");
        let launched = &transaction.outputs[indexed.index as usize];
        assert_eq!(indexed.value, built.value);
        assert_eq!(indexed.script_public_key, built.script_public_key);
        assert_eq!(indexed.script_public_key, launched.script_public_key);
        assert_eq!(launched.covenant, Some(CovenantBinding::new(0, proof.claimed_covenant_id)));
    }
    assert!(transaction.outputs[1].covenant.is_none());
    let launched_id = transaction.outputs[0].covenant.expect("genesis output is bound").covenant_id;
    proof.check_consistency(&bundle).expect("proof is consistent");
    proof.verify(&bundle, launched_id).expect("proof agrees with the launched covenant");
}

#[test]
fn authored_proof_rejects_unknown_actors_and_caller_supplied_physical_state() {
    let artifact = capsule_route_context_artifact();
    let bundle = ArtifactBundle::new(&artifact).expect("artifact forms a bundle");
    let compose =
        |actor: &str, state| ArgentGenesisProof::compose(&bundle, outpoint(), vec![ArgentGenesisOutput::new(2, 1_000, actor, state)]);

    assert!(matches!(
        compose("Missing", expanded_state(7)),
        Err(ArgentGenesisProofError::OutputState { output_index: 2, source, .. })
            if matches!(*source, BuilderError::UnknownActor(ref actor) if actor == "Missing")
    ));
    let hidden_field = artifact
        .argent
        .template_plan
        .runtime_states
        .iter()
        .find(|plan| plan.contract == "ReserveAsset")
        .expect("ReserveAsset has a runtime plan")
        .field_roles[0]
        .name
        .clone();
    let mut injected = expanded_state(7);
    injected.insert(hidden_field.clone(), ArtifactValue::Bytes(vec![0xee; 32]));
    assert!(matches!(
        compose("ReserveAsset", injected),
        Err(ArgentGenesisProofError::OutputState { output_index: 2, source, .. })
            if matches!(*source, BuilderError::HiddenRuntimeFieldProvided { ref field, .. } if field == &hidden_field)
    ));

    let mut missing = expanded_state(7);
    missing.remove("balance");
    assert!(matches!(
        compose("ReserveAsset", missing),
        Err(ArgentGenesisProofError::OutputState { source, .. })
            if matches!(*source, BuilderError::Codec(CodecError::MissingField(ref field)) if field == "balance")
    ));
    let mut digest_instead_of_preimage = expanded_state(7);
    digest_instead_of_preimage.insert("policy".to_string(), ArtifactValue::Bytes(vec![0xee; 32]));
    assert!(matches!(
        compose("ReserveAsset", digest_instead_of_preimage),
        Err(ArgentGenesisProofError::OutputState { source, .. })
            if matches!(*source, BuilderError::MissingStateExpansionPreimage { ref field, .. } if field == "policy")
    ));

    let mut wrong_type = expanded_state(7);
    wrong_type.insert("balance".to_string(), ArtifactValue::Bool(true));
    assert!(matches!(
        compose("ReserveAsset", wrong_type),
        Err(ArgentGenesisProofError::Sil(SilGenesisProofError::RuntimeState { output_index: 2, .. }))
    ));
}

#[test]
fn authored_proof_preserves_the_claim_and_checks_an_independent_id() {
    let artifact = capsule_route_context_artifact();
    let bundle = ArtifactBundle::new(&artifact).expect("artifact forms a bundle");
    let proof =
        ArgentGenesisProof::compose(&bundle, outpoint(), vec![ArgentGenesisOutput::new(0, 1_000, "ReserveAsset", expanded_state(7))])
            .expect("proof composes");
    let mut changed = proof.clone();
    changed.outputs[0].authored_state.insert("balance".to_string(), ArtifactValue::Int(101));
    assert_eq!(changed.sil_proof(&bundle).expect("changed state materializes").claimed_covenant_id, proof.claimed_covenant_id);
    assert!(matches!(
        changed.check_consistency(&bundle),
        Err(ArgentGenesisProofError::Sil(SilGenesisProofError::Preimage(GenesisProofError::ClaimedCovenantIdMismatch { .. })))
    ));
    let mut changed_actor = proof.clone();
    changed_actor.outputs[0].actor = "WalletAsset".to_string();
    assert!(matches!(
        changed_actor.verify(&bundle, proof.claimed_covenant_id),
        Err(ArgentGenesisProofError::Sil(SilGenesisProofError::Preimage(GenesisProofError::ClaimedCovenantIdMismatch { .. })))
    ));
    assert!(matches!(
        proof.verify(&bundle, Hash::from_bytes([0x99; 32])),
        Err(ArgentGenesisProofError::Sil(SilGenesisProofError::Preimage(GenesisProofError::ExpectedCovenantIdMismatch { .. })))
    ));
}

#[test]
fn authored_proof_requires_the_dependency_closure_but_keeps_one_app_per_group() {
    let out_dir = std::env::temp_dir().join(format!(
        "argent-genesis-proof-bundle-{}-{}",
        std::process::id(),
        ARTIFACT_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let compiled =
        crate::build_file_app_bundle("tests/fixtures/runtime/context_observed_self_merge/controller.ag", "CtrlApp", &out_dir)
            .expect("app and its observed dependency compile");
    fs::remove_dir_all(out_dir).expect("temporary build directory removed");
    let bundle = compiled.runtime_bundle().expect("artifacts form a runtime bundle");
    let output = ArgentGenesisOutput::new(0, 1_000, "Ctrl", state! { n: 7 });
    let proof = ArgentGenesisProof::compose(&bundle, outpoint(), vec![output.clone()]).expect("complete bundle composes");
    let sil = proof.sil_proof(&bundle).expect("physical proof materializes");
    assert_eq!(sil.abis.len(), 1);
    assert!(sil.abis[0].contract("Ctrl").is_some());
    assert!(sil.abis[0].contract("Asset").is_none());
    proof.verify(&bundle, proof.claimed_covenant_id).expect("dependency templates and primary state agree");

    let package = GenesisProofPackage::new(ArgentGenesisPackage::new(&bundle, proof.clone()));
    let decoded = round_trip_package(&package);
    let GenesisProofLayer::Argent(authored) = &decoded.proof else {
        panic!("Argent layer is retained");
    };
    assert_eq!(authored.dependencies, vec![compiled.app("AssetApp").expect("dependency exists").clone()]);
    decoded.verify(proof.claimed_covenant_id).expect("loaded package restores the dependency closure");

    let mut missing_dependency = package.clone();
    let GenesisProofLayer::Argent(authored) = &mut missing_dependency.proof else {
        panic!("Argent layer is retained");
    };
    authored.dependencies.clear();
    assert!(matches!(
        round_trip_package(&missing_dependency).check_consistency(),
        Err(GenesisProofPackageError::Argent(ArgentGenesisProofError::Bundle(BuilderError::MissingDependencyArtifact { .. })))
    ));

    let mut mismatched_dependency = package.clone();
    let GenesisProofLayer::Argent(authored) = &mut mismatched_dependency.proof else {
        panic!("Argent layer is retained");
    };
    let dependency = &mut authored.dependencies[0];
    dependency.generator.version.push_str("-different");
    dependency.id = dependency.computed_id_hex().expect("changed artifact ID computes");
    assert!(matches!(
        round_trip_package(&mismatched_dependency).verify(proof.claimed_covenant_id),
        Err(GenesisProofPackageError::Argent(ArgentGenesisProofError::Bundle(BuilderError::DependencyArtifactMismatch { .. })))
    ));

    let mut duplicate_dependency = package;
    let GenesisProofLayer::Argent(authored) = &mut duplicate_dependency.proof else {
        panic!("Argent layer is retained");
    };
    authored.dependencies.push(authored.dependencies[0].clone());
    assert!(matches!(
        round_trip_package(&duplicate_dependency).check_consistency(),
        Err(GenesisProofPackageError::Argent(ArgentGenesisProofError::Bundle(BuilderError::DuplicateAppAlias(_))))
    ));

    // Proof actor names resolve only in the primary app; app qualifiers are not paths.
    for actor_name in ["Asset", "asset_app::Asset", "ctrl_app::Ctrl"] {
        let unresolved = ArgentGenesisOutput::new(1, 1_000, actor_name, state! { owner: vec![0x42; 32], amount: 1 });
        assert!(matches!(
            ArgentGenesisProof::compose(&bundle, outpoint(), vec![output.clone(), unresolved]),
            Err(ArgentGenesisProofError::OutputState { output_index: 1, source, .. })
                if matches!(*source, BuilderError::UnknownActor(ref actor) if actor == actor_name)
        ));
    }
    let missing = ArtifactBundle::new(compiled.primary()).expect("primary artifact is individually consistent");
    assert!(matches!(
        ArgentGenesisProof::compose(&missing, outpoint(), vec![output.clone()]),
        Err(ArgentGenesisProofError::Bundle(BuilderError::MissingDependencyArtifact { .. }))
    ));
    assert!(matches!(
        proof.check_consistency(&missing),
        Err(ArgentGenesisProofError::Bundle(BuilderError::MissingDependencyArtifact { .. }))
    ));

    let mut different_dependency = compiled.app("AssetApp").expect("dependency exists").clone();
    different_dependency.generator.version.push_str("-different");
    different_dependency.id = different_dependency.computed_id_hex().expect("new artifact ID computes");
    let mismatched = missing.with_artifact(&different_dependency).expect("individually consistent dependency attaches");
    assert!(matches!(
        ArgentGenesisProof::compose(&mismatched, outpoint(), vec![output]),
        Err(ArgentGenesisProofError::Bundle(BuilderError::DependencyArtifactMismatch { .. }))
    ));
    assert!(matches!(
        proof.verify(&mismatched, proof.claimed_covenant_id),
        Err(ArgentGenesisProofError::Bundle(BuilderError::DependencyArtifactMismatch { .. }))
    ));
}

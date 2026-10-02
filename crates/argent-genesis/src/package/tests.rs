use std::collections::BTreeMap;

use kaspa_consensus_core::{
    Hash,
    tx::{ScriptPublicKey, TransactionOutpoint},
};
use serde_json::json;
use silverscript_abi::ArtifactValue;

use super::{GenesisProofLayer, GenesisProofPackage, GenesisProofPackageError};
use crate::{
    ArgentCovenantBootstrap, ArgentGenesisOutput, ConsensusGenesisProof, GenesisProofError, IndexedGenesisOutput, SilCovenantBootstrap,
};

fn package() -> GenesisProofPackage {
    let proof = ConsensusGenesisProof::compose(
        TransactionOutpoint::new(Hash::from_bytes([0x11; 32]), 7),
        vec![
            IndexedGenesisOutput::new(0, 1_000, ScriptPublicKey::new(0, vec![0x51].into())),
            IndexedGenesisOutput::new(2, 2_000, ScriptPublicKey::new(1, vec![0x52, 0x53].into())),
        ],
    )
    .expect("consensus proof composes");
    GenesisProofPackage::new(proof)
}

#[test]
fn consensus_package_round_trip_retains_every_committed_field() {
    let package = package();
    let json = package.to_json().expect("package serializes");
    let decoded = GenesisProofPackage::from_json(&json).expect("package deserializes");
    assert_eq!(decoded, package);
    let GenesisProofLayer::Consensus(proof) = &decoded.proof else {
        panic!("consensus layer is retained");
    };
    assert!(json.contains("\"kind\": \"consensus\""));
    decoded.check_consistency().expect("decoded proof is consistent");
    decoded.verify(proof.claimed_covenant_id).expect("decoded proof verifies");
    assert!(matches!(
        decoded.verify_argent(proof.claimed_covenant_id),
        Err(GenesisProofPackageError::ArgentLayerRequired { found: "consensus" })
    ));
    assert!(matches!(
        decoded.verify(Hash::from_bytes([0x99; 32])),
        Err(GenesisProofPackageError::Consensus(GenesisProofError::ExpectedCovenantIdMismatch { .. }))
    ));
}

#[test]
fn json_parsing_does_not_replace_or_verify_the_claim() {
    let package = package();
    let mut json = serde_json::to_value(&package).expect("package serializes");
    let changed_claim = Hash::from_bytes([0xee; 32]);
    json["proof"]["value"]["claimed_covenant_id"] = serde_json::to_value(changed_claim).expect("hash serializes");
    let decoded = GenesisProofPackage::from_json(&json.to_string()).expect("JSON is structurally valid");
    let GenesisProofLayer::Consensus(proof) = &decoded.proof else {
        panic!("consensus layer is retained");
    };
    assert_eq!(proof.claimed_covenant_id, changed_claim);
    assert!(matches!(
        decoded.check_consistency(),
        Err(GenesisProofPackageError::Consensus(GenesisProofError::ClaimedCovenantIdMismatch { .. }))
    ));

    // A consistent replacement claim must still fail against the original node ID.
    let mut changed = package.clone();
    let GenesisProofLayer::Consensus(proof) = &mut changed.proof else {
        panic!("consensus layer is retained");
    };
    let original_id = proof.claimed_covenant_id;
    proof.outputs[0].value += 1;
    proof.claimed_covenant_id = proof.computed_covenant_id().expect("changed preimage is valid");
    let decoded = GenesisProofPackage::from_json(&changed.to_json().expect("package serializes")).expect("package deserializes");
    decoded.check_consistency().expect("changed proof is internally consistent");
    assert!(matches!(
        decoded.verify(original_id),
        Err(GenesisProofPackageError::Consensus(GenesisProofError::ExpectedCovenantIdMismatch { .. }))
    ));
}

#[test]
fn unsupported_versions_are_rejected_by_every_checked_operation() {
    let mut package = package();
    package.schema_version = 2;
    let unsupported = |error| matches!(error, GenesisProofPackageError::UnsupportedVersion { expected: 1, found: 2 });
    let json = serde_json::to_string(&package).expect("raw serialization does not check the schema");
    assert!(unsupported(GenesisProofPackage::from_json(&json).expect_err("unsupported version is rejected")));
    assert!(unsupported(package.to_json().expect_err("unsupported version is rejected")));
    assert!(unsupported(package.check_consistency().expect_err("unsupported version is rejected")));
    assert!(unsupported(package.verify(Hash::from_bytes([0; 32])).expect_err("unsupported version is rejected")));
    assert!(unsupported(package.verify_argent(Hash::from_bytes([0; 32])).expect_err("unsupported version is rejected")));
}

#[test]
fn malformed_packages_and_unrecognized_layers_are_rejected() {
    let valid = serde_json::to_value(package()).expect("package serializes");
    let mut unknown_layer = valid.clone();
    unknown_layer["proof"]["kind"] = json!("source");
    let mut extra_field = valid.clone();
    extra_field["cached_proof"] = json!({});
    let mut extra_proof_field = valid.clone();
    extra_proof_field["proof"]["cached_proof"] = json!({});
    let mut malformed_outpoint = valid.clone();
    malformed_outpoint["proof"]["value"]["authorizing_outpoint"] = json!({});
    for value in [json!({}), json!({"schema_version": 1}), unknown_layer, extra_field, extra_proof_field, malformed_outpoint] {
        assert!(matches!(GenesisProofPackage::from_json(&value.to_string()), Err(GenesisProofPackageError::Json(_))));
    }
}

#[test]
fn authored_output_json_preserves_tagged_value_types() {
    let output = ArgentGenesisOutput::new(
        0,
        1_000,
        "Example",
        BTreeMap::from([
            ("integer".to_string(), ArtifactValue::Int(7)),
            ("byte".to_string(), ArtifactValue::Byte(7)),
            ("bytes".to_string(), ArtifactValue::Bytes(vec![7, 8])),
            ("array".to_string(), ArtifactValue::Array(vec![ArtifactValue::Int(7), ArtifactValue::Byte(8)])),
            ("object".to_string(), ArtifactValue::Object(BTreeMap::from([("nested".to_string(), ArtifactValue::Bool(false))]))),
            ("text".to_string(), ArtifactValue::Text("example".to_string())),
        ]),
    );
    let json = silverscript_abi::to_pretty_json(&output).expect("output serializes");
    assert_eq!(serde_json::from_str::<ArgentGenesisOutput>(&json).expect("output deserializes"), output);
}

#[test]
fn bootstraps_reject_unknown_fields_and_published_claims() {
    let base = json!({
        "authorizing_outpoint": TransactionOutpoint::new(Hash::from_bytes([0x11; 32]), 7),
        "outputs": []
    });
    let mut authored = base.clone();
    authored["app"] = json!("Example");
    assert!(serde_json::from_value::<ArgentCovenantBootstrap>(authored.clone()).is_ok());
    assert!(serde_json::from_value::<ArgentCovenantBootstrap>(base.clone()).is_err());
    assert!(serde_json::from_value::<SilCovenantBootstrap>(base.clone()).is_ok());
    assert!(serde_json::from_value::<SilCovenantBootstrap>(authored.clone()).is_err());
    for field in ["claimed_covenant_id", "abis"] {
        let mut sil = base.clone();
        sil[field] = json!(null);
        assert!(serde_json::from_value::<SilCovenantBootstrap>(sil).is_err());
        let mut ag = authored.clone();
        ag[field] = json!(null);
        assert!(serde_json::from_value::<ArgentCovenantBootstrap>(ag).is_err());
    }
}

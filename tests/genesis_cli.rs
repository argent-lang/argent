use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use argent::{
    build_file_bundle,
    genesis::{
        ConsensusGenesisProof, GenesisProofLayer, GenesisProofPackage, IndexedGenesisOutput, SilGenesisOutput, SilGenesisProof,
    },
};
use kaspa_consensus_core::{
    Hash,
    tx::{ScriptPublicKey, TransactionOutpoint},
};
use serde_json::{Value, json};
use silverscript_abi::ArtifactValue;
use tempfile::TempDir;

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_argentc"))
}

fn succeeds(command: &mut Command) -> String {
    let output = command.output().expect("CLI starts");
    assert!(output.status.success(), "CLI failed: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8(output.stdout).expect("CLI prints UTF-8")
}

fn fails(command: &mut Command) -> String {
    let Output { status, stdout, stderr } = command.output().expect("CLI starts");
    assert!(!status.success(), "CLI unexpectedly succeeded: {}", String::from_utf8_lossy(&stdout));
    assert!(!String::from_utf8_lossy(&stdout).contains("covenant ID matches"));
    String::from_utf8(stderr).expect("CLI prints UTF-8")
}

fn fixture(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(path)
}

fn source() -> PathBuf {
    fixture("emit/capsule_route_context/app.ag")
}

fn bootstrap() -> PathBuf {
    fixture("genesis_cli/expanded.json")
}

fn write_json(path: &Path, value: &impl serde::Serialize) {
    fs::write(path, silverscript_abi::to_pretty_json(value).expect("JSON serializes")).expect("JSON writes");
}

fn read_package(path: &Path) -> GenesisProofPackage {
    GenesisProofPackage::from_json(&fs::read_to_string(path).expect("package reads")).expect("package parses")
}

fn compose_source(dir: &TempDir) -> (PathBuf, GenesisProofPackage, Hash) {
    let proof = dir.path().join("proof.json");
    let stdout =
        succeeds(cli().args(["genesis", "compose"]).arg(source()).arg("--bootstrap").arg(bootstrap()).arg("--out").arg(&proof));
    let package = read_package(&proof);
    let GenesisProofLayer::Argent(authored) = &package.proof else {
        panic!("Argent package expected");
    };
    let id = authored.proof.claimed_covenant_id;
    assert!(stdout.contains(&format!("covenant ID: {id}")));
    (proof, package, id)
}

#[test]
fn source_composition_verifies_expansions_routes_and_source_correspondence() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let (path, package, id) = compose_source(&dir);
    let GenesisProofLayer::Argent(authored) = &package.proof else {
        panic!("Argent package expected");
    };
    assert!(!authored.primary.argent.template_plan.runtime_states.iter().all(|state| state.field_roles.is_empty()));
    assert_eq!(authored.proof.outputs.iter().map(|output| output.index).collect::<Vec<_>>(), [2, 5]);
    assert!(matches!(authored.proof.outputs[0].authored_state["policy"], ArtifactValue::Object(_)));

    let stdout = succeeds(cli().args(["genesis", "verify"]).arg(&path).args(["--covenant-id", &id.to_string(), "--require-argent"]));
    assert!(stdout.contains("checked: Argent artifacts, authored states, and derived runtime state"));
    assert!(stdout.contains("source correspondence was not checked"));
    // Recompilation may use a different checkout path; identities exclude source paths.
    let copied_source = dir.path().join("app.ag");
    fs::copy(source(), &copied_source).expect("source copies");
    let stdout = succeeds(
        cli().args(["genesis", "verify"]).arg(&path).args(["--covenant-id", &id.to_string(), "--source"]).arg(&copied_source),
    );
    assert!(stdout.contains("source correspondence matches"));

    let changed = fs::read_to_string(&copied_source).expect("source reads").replace("policy.nonce + 1", "policy.nonce + 2");
    fs::write(&copied_source, changed).expect("source changes");
    let err =
        fails(cli().args(["genesis", "verify"]).arg(&path).args(["--covenant-id", &id.to_string(), "--source"]).arg(&copied_source));
    assert!(err.contains("source primary artifact does not match proof package"), "{err}");
}

#[test]
fn artifact_composition_preserves_embedded_artifacts_and_matches_source_mode() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let compiled = build_file_bundle(source(), dir.path().join("build")).expect("app builds");
    let (source_path, source_package, id) = compose_source(&dir);
    let artifact_path = dir.path().join("build/artifact.json");
    let path = dir.path().join("from-artifact.json");
    succeeds(
        cli()
            .args(["genesis", "compose", "--artifact"])
            .arg(&artifact_path)
            .arg("--bootstrap")
            .arg(bootstrap())
            .arg("--out")
            .arg(&path),
    );
    let package = read_package(&path);
    assert_eq!(package, source_package);
    let GenesisProofLayer::Argent(authored) = &package.proof else {
        panic!("Argent package expected");
    };
    assert_eq!(&authored.primary, compiled.primary());
    succeeds(cli().args(["genesis", "verify"]).arg(&path).args(["--covenant-id", &id.to_string()]));

    let before = fs::read(&source_path).expect("existing package reads");
    fails(
        cli()
            .args(["genesis", "compose", "--artifact"])
            .arg(&artifact_path)
            .arg("--bootstrap")
            .arg(bootstrap())
            .arg("--out")
            .arg(&source_path),
    );
    assert_eq!(fs::read(&source_path).expect("existing package still reads"), before);
}

#[test]
fn verify_rejects_wrong_ids_changed_states_and_changed_claims() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let (path, package, id) = compose_source(&dir);
    let err = fails(cli().args(["genesis", "verify"]).arg(&path).args(["--covenant-id", &Hash::from_bytes([0x99; 32]).to_string()]));
    assert!(err.contains("expected"), "{err}");
    for change in [
        |value: &mut Value| {
            value["proof"]["value"]["proof"]["outputs"][0]["authored_state"]["policy"]["value"]["nonce"]["value"] = json!(99)
        },
        |value: &mut Value| value["proof"]["value"]["proof"]["claimed_covenant_id"] = json!(Hash::from_bytes([0xee; 32])),
    ] {
        let mut value = serde_json::to_value(&package).expect("package serializes");
        change(&mut value);
        let changed = dir.path().join("changed.json");
        write_json(&changed, &value);
        let err = fails(cli().args(["genesis", "verify"]).arg(&changed).args(["--covenant-id", &id.to_string()]));
        assert!(err.contains("but its preimage produces"), "{err}");
    }
}

#[test]
fn sil_only_verification_keeps_independent_abi_units_and_reports_its_scope() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let compile = |threshold| {
        silverscript_lang::compiler::compile_to_sil_abi_artifact(
            &format!(
                "contract Counter(int initial) {{ int amount = initial; entry inspect() {{ require(amount >= {threshold}); }} }}"
            ),
            &[ArtifactValue::Int(0)],
        )
        .expect("Sil contract compiles")
    };
    let proof = SilGenesisProof::compose(
        vec![compile(0), compile(1)],
        TransactionOutpoint::new(Hash::from_bytes([0x31; 32]), 4),
        vec![
            SilGenesisOutput::new(
                0,
                1_000,
                0,
                "Counter",
                std::collections::BTreeMap::from([("amount".into(), ArtifactValue::Int(7))]),
            ),
            SilGenesisOutput::new(
                2,
                2_000,
                1,
                "Counter",
                std::collections::BTreeMap::from([("amount".into(), ArtifactValue::Int(8))]),
            ),
        ],
    )
    .expect("Sil proof composes");
    let id = proof.claimed_covenant_id;
    let package = GenesisProofPackage::new(proof);
    let path = dir.path().join("sil.json");
    write_json(&path, &package);
    let stdout = succeeds(cli().args(["genesis", "verify"]).arg(&path).args(["--covenant-id", &id.to_string()]));
    assert!(stdout.contains("checked: Silverscript ABIs, physical states, and derived scripts"));
    assert!(!stdout.contains("checked: Argent"));
    assert!(stdout.contains("source correspondence was not checked"));
    for option in ["--require-argent", "--source"] {
        let mut command = cli();
        command.args(["genesis", "verify"]).arg(&path).args(["--covenant-id", &id.to_string(), option]);
        if option == "--source" {
            command.arg("not-read.ag");
        }
        let err = fails(&mut command);
        assert!(err.contains("requires an Argent proof package, found `sil`"), "{err}");
    }
    let mut changed = package;
    let GenesisProofLayer::Sil(proof) = &mut changed.proof else {
        panic!("Sil proof expected");
    };
    proof.outputs[1].abi_index = 3;
    write_json(&path, &changed);
    let err = fails(cli().args(["genesis", "verify"]).arg(&path).args(["--covenant-id", &id.to_string()]));
    assert!(err.contains("unknown Sil ABI index 3"), "{err}");
}

#[test]
fn consensus_verification_does_not_claim_contract_or_authored_state_checks() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let proof = ConsensusGenesisProof::compose(
        TransactionOutpoint::new(Hash::from_bytes([0x11; 32]), 7),
        vec![IndexedGenesisOutput::new(2, 1_000, ScriptPublicKey::new(0, vec![0x51].into()))],
    )
    .expect("consensus proof composes");
    let id = proof.claimed_covenant_id;
    let path = dir.path().join("consensus.json");
    write_json(&path, &GenesisProofPackage::new(proof));
    let stdout = succeeds(cli().args(["genesis", "verify"]).arg(&path).args(["--covenant-id", &id.to_string()]));
    assert!(stdout.contains("checked: genesis output scripts, values, indices, and authorizing outpoint"));
    assert!(!stdout.contains("checked: Argent"));
    let err = fails(cli().args(["genesis", "verify"]).arg(&path).args(["--covenant-id", &id.to_string(), "--require-argent"]));
    assert!(err.contains("requires an Argent proof package, found `consensus`"), "{err}");
}

#[test]
fn dependency_closure_is_required_for_artifacts_and_compiled_for_sources() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let source = fixture("runtime/context_static_linked_spawn/launcher.ag");
    let compiled = build_file_bundle(&source, dir.path().join("build")).expect("linked app builds");
    let bootstrap = dir.path().join("genesis.json");
    write_json(
        &bootstrap,
        &json!({
            "app": "LauncherApp",
            "authorizing_outpoint": TransactionOutpoint::new(Hash::from_bytes([0x61; 32]), 4),
            "outputs": [{"index": 0, "value": 1_000, "actor": "Launcher", "authored_state": {
                "launches": {"kind": "int", "value": 0}
            }}]
        }),
    );
    let primary_path = dir.path().join("build/artifact.json");
    let dependency_path = dir.path().join("build/apps/ChildApp/artifact.json");
    let proof = dir.path().join("proof.json");
    let err = fails(
        cli()
            .args(["genesis", "compose", "--artifact"])
            .arg(&primary_path)
            .arg("--bootstrap")
            .arg(&bootstrap)
            .arg("--out")
            .arg(&proof),
    );
    assert!(err.contains("ChildApp"), "{err}");
    assert!(!proof.exists());
    succeeds(
        cli()
            .args(["genesis", "compose", "--artifact"])
            .arg(&primary_path)
            .arg("--dependency")
            .arg(&dependency_path)
            .arg("--bootstrap")
            .arg(&bootstrap)
            .arg("--out")
            .arg(&proof),
    );
    let package = read_package(&proof);
    let GenesisProofLayer::Argent(authored) = &package.proof else {
        panic!("Argent package expected");
    };
    assert_eq!(authored.dependencies, vec![compiled.app("ChildApp").expect("child app exists").clone()]);
    let id = authored.proof.claimed_covenant_id;
    let stdout =
        succeeds(cli().args(["genesis", "verify"]).arg(&proof).args(["--covenant-id", &id.to_string(), "--source"]).arg(&source));
    assert!(stdout.contains("source correspondence matches"));
    let source_proof = dir.path().join("source.json");
    succeeds(cli().args(["genesis", "compose"]).arg(&source).arg("--bootstrap").arg(&bootstrap).arg("--out").arg(&source_proof));
    assert_eq!(read_package(&source_proof), package);

    // A consistent package can attach unused artifacts, but source comparison
    // must still require the exact compiled dependency closure.
    let mut extra = package.clone();
    let GenesisProofLayer::Argent(authored) = &mut extra.proof else {
        panic!("Argent package expected");
    };
    let unrelated =
        build_file_bundle(fixture("emit/capsule_route_context/app.ag"), dir.path().join("unrelated")).expect("unrelated app builds");
    authored.dependencies.push(unrelated.primary().clone());
    extra.verify(id).expect("unused checked artifacts do not change the genesis preimage");
    let extra_path = dir.path().join("extra-dependency.json");
    write_json(&extra_path, &extra);
    let err =
        fails(cli().args(["genesis", "verify"]).arg(&extra_path).args(["--covenant-id", &id.to_string(), "--source"]).arg(&source));
    assert!(err.contains("source dependency artifact IDs do not match proof package"), "{err}");

    let mut dependency = compiled.app("ChildApp").expect("child app exists").clone();
    dependency.generator.version = "different".into();
    dependency.id = dependency.computed_id_hex().expect("changed artifact identity computes");
    write_json(&dependency_path, &dependency);
    let err = fails(
        cli()
            .args(["genesis", "compose", "--artifact"])
            .arg(&primary_path)
            .arg("--dependency")
            .arg(&dependency_path)
            .arg("--bootstrap")
            .arg(&bootstrap)
            .arg("--out")
            .arg(dir.path().join("mismatch.json")),
    );
    assert!(err.contains("requires dependency `ChildApp` artifact") && err.contains("found app `ChildApp` artifact"), "{err}");
}

#[test]
fn composition_requires_bootstrap_app_to_match_primary_app() {
    let dir = tempfile::tempdir().expect("temporary directory");
    build_file_bundle(source(), dir.path().join("build")).expect("app builds");
    let artifact = dir.path().join("build/artifact.json");
    let bootstrap_path = dir.path().join("genesis.json");
    let proof = dir.path().join("not-published.json");
    for (app, expected) in
        [(Some("OtherApp"), "bootstrap app `OtherApp` does not match primary app `Asset`"), (None, "missing field `app`")]
    {
        let mut value: Value = serde_json::from_str(&fs::read_to_string(bootstrap()).expect("bootstrap reads")).expect("JSON parses");
        if let Some(app) = app {
            value["app"] = json!(app);
        } else {
            value.as_object_mut().expect("bootstrap is an object").remove("app");
        }
        write_json(&bootstrap_path, &value);
        for from_artifact in [false, true] {
            let mut command = cli();
            command.args(["genesis", "compose"]);
            if from_artifact {
                command.arg("--artifact").arg(&artifact);
            } else {
                command.arg(source());
            }
            let err = fails(command.arg("--bootstrap").arg(&bootstrap_path).arg("--out").arg(&proof));
            assert!(err.contains(expected), "{err}");
            assert!(!proof.exists());
        }
    }
}

#[test]
fn composition_rejects_invalid_output_order_actors_and_authored_fields() {
    let dir = tempfile::tempdir().expect("temporary directory");
    for change in [
        |value: &mut Value| value["outputs"].as_array_mut().expect("outputs are an array").reverse(),
        |value: &mut Value| value["outputs"] = json!([]),
        |value: &mut Value| value["outputs"][0]["actor"] = json!("Missing"),
        |value: &mut Value| value["outputs"][0]["authored_state"]["balance"] = json!({"kind": "bool", "value": true}),
        |value: &mut Value| value["claimed_covenant_id"] = json!(Hash::from_bytes([0; 32])),
    ] {
        let mut value: Value = serde_json::from_str(&fs::read_to_string(bootstrap()).expect("bootstrap reads")).expect("JSON parses");
        change(&mut value);
        let bootstrap = dir.path().join("invalid.json");
        let proof = dir.path().join("not-published.json");
        write_json(&bootstrap, &value);
        fails(cli().args(["genesis", "compose"]).arg(source()).arg("--bootstrap").arg(&bootstrap).arg("--out").arg(&proof));
        assert!(!proof.exists());
    }
}

#[test]
fn source_with_multiple_apps_requires_selection() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let source_path = dir.path().join("multiple.ag");
    fs::write(
        &source_path,
        format!("{}\napp Secondary {{ actor WalletAsset; }}\n", fs::read_to_string(source()).expect("source reads")),
    )
    .expect("source writes");
    let proof = dir.path().join("proof.json");
    let err = fails(cli().args(["genesis", "compose"]).arg(&source_path).arg("--bootstrap").arg(bootstrap()).arg("--out").arg(&proof));
    assert!(err.contains("select an app with --app"), "{err}");
    succeeds(
        cli()
            .args(["genesis", "compose"])
            .arg(&source_path)
            .args(["--app", "Asset"])
            .arg("--bootstrap")
            .arg(bootstrap())
            .arg("--out")
            .arg(&proof),
    );
}

#[test]
fn malformed_packages_and_unsupported_versions_fail_with_file_context() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let proof = dir.path().join("invalid-proof.json");
    for text in [
        "{",
        r#"{"schema_version":999,"proof":{"kind":"consensus","value":{
        "authorizing_outpoint":{"transactionId":"0000000000000000000000000000000000000000000000000000000000000000","index":0},
        "claimed_covenant_id":"0000000000000000000000000000000000000000000000000000000000000000","outputs":[]}}}"#,
    ] {
        fs::write(&proof, text).expect("invalid package writes");
        let err = fails(cli().args(["genesis", "verify"]).arg(&proof).args(["--covenant-id", &Hash::from_bytes([0; 32]).to_string()]));
        assert!(err.contains("invalid-proof.json"), "{err}");
    }
}

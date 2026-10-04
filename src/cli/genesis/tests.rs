use clap::{Parser, error::ErrorKind};

use super::*;
use crate::{Cli, Command};

#[test]
fn compose_parses_source_artifact_and_sil_abi_modes() {
    let cli = Cli::try_parse_from([
        "argentc",
        "genesis",
        "compose",
        "app.ag",
        "--app",
        "Example",
        "--bootstrap",
        "genesis.json",
        "--out",
        "proof.json",
    ])
    .expect("source composition parses");
    let Command::Genesis(GenesisCommand::Compose(args)) = cli.command else {
        panic!("expected composition");
    };
    assert_eq!(args.source, Some(PathBuf::from("app.ag")));
    assert_eq!(args.app.as_deref(), Some("Example"));
    assert_eq!(args.bootstrap, PathBuf::from("genesis.json"));
    assert!(args.artifact.is_none());

    let cli = Cli::try_parse_from([
        "argentc",
        "genesis",
        "compose",
        "--artifact",
        "primary.json",
        "--dependency",
        "first.json",
        "--dependency",
        "second.json",
        "--bootstrap",
        "genesis.json",
        "--out",
        "proof.json",
    ])
    .expect("artifact composition parses");
    let Command::Genesis(GenesisCommand::Compose(args)) = cli.command else {
        panic!("expected composition");
    };
    assert_eq!(args.artifact, Some(PathBuf::from("primary.json")));
    assert_eq!(args.dependency, [PathBuf::from("first.json"), PathBuf::from("second.json")]);
    assert!(args.source.is_none());

    let cli = Cli::try_parse_from([
        "argentc",
        "genesis",
        "compose",
        "--sil-abi",
        "mint.json",
        "--sil-abi",
        "ticket.json",
        "--bootstrap",
        "genesis.json",
        "--out",
        "proof.json",
    ])
    .expect("Sil ABI composition parses");
    let Command::Genesis(GenesisCommand::Compose(args)) = cli.command else {
        panic!("expected composition");
    };
    assert_eq!(args.sil_abi, [PathBuf::from("mint.json"), PathBuf::from("ticket.json")]);
    assert!(args.source.is_none() && args.artifact.is_none());
}

#[test]
fn compose_rejects_ambiguous_or_incomplete_inputs() {
    for mode in [
        vec![],
        vec!["app.ag", "--artifact", "artifact.json"],
        vec!["--artifact", "artifact.json", "--app", "Example"],
        vec!["app.ag", "--dependency", "dependency.json"],
        vec!["app.ag", "--sil-abi", "abi.json"],
        vec!["--artifact", "artifact.json", "--sil-abi", "abi.json"],
        vec!["--sil-abi", "abi.json", "--app", "Example"],
        vec!["--sil-abi", "abi.json", "--dependency", "dependency.json"],
    ] {
        let mut argv = vec!["argentc", "genesis", "compose"];
        argv.extend(mode);
        argv.extend(["--bootstrap", "genesis.json", "--out", "proof.json"]);
        assert_eq!(Cli::try_parse_from(argv).expect_err("invalid mode is rejected").exit_code(), 2);
    }
    for missing in ["--bootstrap", "--out"] {
        let mut argv = vec!["argentc", "genesis", "compose", "app.ag"];
        if missing != "--bootstrap" {
            argv.extend(["--bootstrap", "genesis.json"]);
        }
        if missing != "--out" {
            argv.extend(["--out", "proof.json"]);
        }
        assert_eq!(Cli::try_parse_from(argv).expect_err("required path is missing").exit_code(), 2);
    }
}

#[test]
fn verify_requires_an_independent_valid_id() {
    for argv in [
        vec!["argentc", "genesis", "verify", "proof.json"],
        vec!["argentc", "genesis", "verify", "proof.json", "--covenant-id", "invalid"],
        vec!["argentc", "genesis", "verify", "--covenant-id", "00"],
    ] {
        assert_eq!(Cli::try_parse_from(argv).expect_err("invalid verification is rejected").exit_code(), 2);
    }
    let id = Hash::from_bytes([0x61; 32]);
    let cli = Cli::try_parse_from([
        "argentc",
        "genesis",
        "verify",
        "proof.json",
        "--covenant-id",
        &id.to_string(),
        "--require-argent",
        "--source",
        "app.ag",
        "--app",
        "Example",
    ])
    .expect("verification parses");
    let Command::Genesis(GenesisCommand::Verify(args)) = cli.command else {
        panic!("expected verification");
    };
    assert_eq!(args.covenant_id, id);
    assert!(args.require_argent);
    assert_eq!(args.proof, Some(PathBuf::from("proof.json")));
    assert!(args.bootstrap.is_none());
    assert_eq!(args.source, Some(PathBuf::from("app.ag")));

    assert!(
        Cli::try_parse_from(["argentc", "genesis", "verify", "proof.json", "--covenant-id", &id.to_string(), "--app", "Example",])
            .is_err()
    );
}

#[test]
fn verify_parses_source_bootstrap_without_a_package() {
    let id = Hash::from_bytes([0x61; 32]);
    let cli = Cli::try_parse_from([
        "argentc",
        "genesis",
        "verify",
        "--source",
        "app.ag",
        "--bootstrap",
        "genesis.json",
        "--covenant-id",
        &id.to_string(),
        "--app",
        "Example",
    ])
    .expect("direct bootstrap verification parses");
    let Command::Genesis(GenesisCommand::Verify(args)) = cli.command else {
        panic!("expected verification");
    };
    assert!(args.proof.is_none());
    assert_eq!(args.bootstrap, Some(PathBuf::from("genesis.json")));
    assert_eq!(args.source, Some(PathBuf::from("app.ag")));
    assert_eq!(args.app.as_deref(), Some("Example"));
    assert_eq!(args.covenant_id, id);
}

#[test]
fn verify_rejects_ambiguous_or_incomplete_inputs() {
    let id = Hash::from_bytes([0x61; 32]).to_string();
    for mode in [
        vec![],
        vec!["--source", "app.ag"],
        vec!["--bootstrap", "genesis.json"],
        vec!["proof.json", "--bootstrap", "genesis.json", "--source", "app.ag"],
    ] {
        let mut argv = vec!["argentc", "genesis", "verify"];
        argv.extend(mode);
        argv.extend(["--covenant-id", &id]);
        assert_eq!(Cli::try_parse_from(argv).expect_err("invalid verification mode is rejected").exit_code(), 2);
    }
    for id in [None, Some("invalid")] {
        let mut argv = vec!["argentc", "genesis", "verify", "--source", "app.ag", "--bootstrap", "genesis.json"];
        if let Some(id) = id {
            argv.extend(["--covenant-id", id]);
        }
        assert_eq!(Cli::try_parse_from(argv).expect_err("direct verification requires a valid independent ID").exit_code(), 2);
    }
}

#[test]
fn genesis_commands_provide_help_without_execution() {
    for argv in [
        vec!["argentc", "genesis", "--help"],
        vec!["argentc", "genesis", "compose", "--help"],
        vec!["argentc", "genesis", "verify", "--help"],
    ] {
        assert_eq!(Cli::try_parse_from(argv).expect_err("help skips execution").kind(), ErrorKind::DisplayHelp);
    }
}

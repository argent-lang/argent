//! Filesystem and compiler orchestration for genesis proofs and bootstraps.

use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use argent::{
    ArgentError, CompiledAppBundle, Result,
    artifact::Artifact,
    build_file_app_bundle, build_file_bundle,
    builder::ArtifactBundle,
    genesis::{ArgentCovenantBootstrap, ArgentGenesisPackage, GenesisProofLayer, GenesisProofPackage, SilCovenantBootstrap},
};
use clap::{ArgGroup, Args, Subcommand};
use kaspa_consensus_core::Hash;
use serde::de::DeserializeOwned;

#[derive(Debug, Subcommand)]
pub(crate) enum GenesisCommand {
    /// Compose a proof from an Argent app or independent Silverscript ABI files.
    Compose(ComposeArgs),
    /// Verify a package or source bootstrap against an independently obtained covenant ID.
    Verify(VerifyArgs),
}

#[derive(Debug, Args)]
#[command(group(ArgGroup::new("input").required(true).args(["source", "artifact", "sil_abi"])))]
pub(crate) struct ComposeArgs {
    /// Argent source file. Imports supply the complete dependency closure.
    #[arg(value_name = "APP.AG")]
    source: Option<PathBuf>,
    /// Primary artifact JSON file, instead of source.
    #[arg(long, value_name = "ARTIFACT.JSON")]
    artifact: Option<PathBuf>,
    /// Select an app from the source file; otherwise it must declare exactly one.
    #[arg(long, requires = "source", conflicts_with_all = ["artifact", "sil_abi"], value_name = "NAME")]
    app: Option<String>,
    /// Dependency artifact JSON file; repeat for the complete dependency closure.
    #[arg(long, requires = "artifact", conflicts_with_all = ["source", "sil_abi"], value_name = "ARTIFACT.JSON")]
    dependency: Vec<PathBuf>,
    /// Independent Silverscript ABI file; repeat in bootstrap ABI-index order.
    #[arg(long, value_name = "ABI.JSON")]
    sil_abi: Vec<PathBuf>,
    /// Initial actors or contracts and their states, authorizing outpoint, and ordered output metadata.
    #[arg(long, value_name = "GENESIS.JSON")]
    bootstrap: PathBuf,
    /// File for the self-contained proof package.
    #[arg(long, value_name = "PROOF.JSON")]
    out: PathBuf,
}

#[derive(Debug, Args)]
#[command(group(ArgGroup::new("input").required(true).args(["proof", "bootstrap"])))]
pub(crate) struct VerifyArgs {
    /// Self-contained Argent, Silverscript, or consensus proof package.
    #[arg(value_name = "PROOF.JSON")]
    proof: Option<PathBuf>,
    /// Argent bootstrap to verify directly with --source, instead of a proof package.
    #[arg(long, requires = "source", value_name = "GENESIS.JSON")]
    bootstrap: Option<PathBuf>,
    /// Covenant ID from a node or another independent source.
    #[arg(long, value_name = "ID")]
    covenant_id: Hash,
    /// Reject lower-level packages that do not check authored state and route context.
    #[arg(long)]
    require_argent: bool,
    /// Compile this app to verify a bootstrap or compare a package's artifact identities.
    #[arg(long, value_name = "APP.AG")]
    source: Option<PathBuf>,
    /// Select an app from --source; otherwise it must declare exactly one.
    #[arg(long, requires = "source", value_name = "NAME")]
    app: Option<String>,
}

pub(crate) fn run(command: GenesisCommand) -> Result<()> {
    match command {
        GenesisCommand::Compose(args) => compose(args),
        GenesisCommand::Verify(args) => verify(args),
    }
}

fn compose(args: ComposeArgs) -> Result<()> {
    let (package, covenant_id) = if !args.sil_abi.is_empty() {
        let bootstrap: SilCovenantBootstrap = read_json(&args.bootstrap)?;
        let abis = args.sil_abi.iter().map(|path| read_json(path)).collect::<Result<Vec<_>>>()?;
        let proof = bootstrap.compose(abis).map_err(|err| ArgentError::new(err.to_string()))?;
        let covenant_id = proof.claimed_covenant_id;
        (GenesisProofPackage::new(proof), covenant_id)
    } else {
        let bootstrap: ArgentCovenantBootstrap = read_json(&args.bootstrap)?;
        let authored = if let Some(source) = &args.source {
            let compiled = compile_source(source, args.app.as_deref())?;
            let bundle = compiled.runtime_bundle().map_err(|err| ArgentError::new(err.to_string()))?;
            compose_package(&bundle, bootstrap)?
        } else {
            let path = args.artifact.as_ref().expect("Clap requires source, artifact, or Sil ABI files");
            let primary: Artifact = read_json(path)?;
            let dependencies = args.dependency.iter().map(|path| read_json::<Artifact>(path)).collect::<Result<Vec<_>>>()?;
            let mut bundle = ArtifactBundle::new(&primary).map_err(|err| ArgentError::at(path, err.to_string()))?;
            for dependency in &dependencies {
                bundle = bundle.with_artifact(dependency).map_err(|err| ArgentError::new(err.to_string()))?;
            }
            compose_package(&bundle, bootstrap)?
        };
        let covenant_id = authored.proof.claimed_covenant_id;
        (GenesisProofPackage::new(authored), covenant_id)
    };
    let json = package.to_json().map_err(|err| ArgentError::new(err.to_string()))?;
    // Do not overwrite a package or another input file by accident.
    let mut output = fs::File::create_new(&args.out).map_err(|err| ArgentError::at(&args.out, err.to_string()))?;
    output.write_all(json.as_bytes()).map_err(|err| ArgentError::at(&args.out, err.to_string()))?;
    println!("wrote {}", args.out.display());
    println!("covenant ID: {covenant_id}");
    Ok(())
}

fn compose_package(bundle: &ArtifactBundle<'_>, bootstrap: ArgentCovenantBootstrap) -> Result<ArgentGenesisPackage> {
    let proof = bootstrap.compose(bundle).map_err(|err| ArgentError::new(err.to_string()))?;
    Ok(ArgentGenesisPackage::new(bundle, proof))
}

fn verify(args: VerifyArgs) -> Result<()> {
    // Mode 1: source + bootstrap, without a proof package.
    // Compile the app and verify the bootstrap against the supplied covenant ID.
    if let Some(path) = &args.bootstrap {
        let bootstrap: ArgentCovenantBootstrap = read_json(path)?;
        let source = args.source.as_ref().expect("Clap requires source for bootstrap verification");
        let compiled = compile_source(source, args.app.as_deref())?;
        let bundle = compiled.runtime_bundle().map_err(|err| ArgentError::new(err.to_string()))?;
        let proof = bootstrap.compose(&bundle).map_err(|err| ArgentError::at(path, err.to_string()))?;
        proof.verify(&bundle, args.covenant_id).map_err(|err| ArgentError::at(path, err.to_string()))?;
        println!("covenant ID matches: {}", args.covenant_id);
        println!("checked: Argent source, authored states, and derived runtime state");
        return Ok(());
    }

    // Mode 2: verify a proof package against the supplied covenant ID.
    // Optional --source also checks that its artifacts match the source.
    let path = args.proof.as_ref().expect("Clap requires a proof package or bootstrap");
    let json = read_text(path)?;
    let package = GenesisProofPackage::from_json(&json).map_err(|err| ArgentError::at(path, err.to_string()))?;
    if args.require_argent || args.source.is_some() {
        package.verify_argent(args.covenant_id)
    } else {
        package.verify(args.covenant_id)
    }
    .map_err(|err| ArgentError::at(path, err.to_string()))?;

    if let Some(source) = &args.source {
        let GenesisProofLayer::Argent(authored) = &package.proof else {
            unreachable!("source comparison requires Argent verification");
        };
        let compiled = compile_source(source, args.app.as_deref())?;
        check_source_artifacts(&compiled, authored).map_err(|err| err.with_path(source))?;
    }

    println!("covenant ID matches: {}", args.covenant_id);
    match &package.proof {
        GenesisProofLayer::Consensus(_) => println!("checked: genesis output scripts, values, indices, and authorizing outpoint"),
        GenesisProofLayer::Sil(_) => println!("checked: Silverscript ABIs, physical states, and derived scripts"),
        GenesisProofLayer::Argent(_) => println!("checked: Argent artifacts, authored states, and derived runtime state"),
    }
    if args.source.is_some() {
        println!("source correspondence matches the primary app and dependency artifact IDs");
    } else {
        println!("source correspondence was not checked");
    }
    Ok(())
}

fn compile_source(source: &Path, app: Option<&str>) -> Result<CompiledAppBundle> {
    let output = tempfile::Builder::new().prefix("argent-genesis-").tempdir()?;
    if let Some(app) = app { build_file_app_bundle(source, app, output.path()) } else { build_file_bundle(source, output.path()) }
}

fn check_source_artifacts(compiled: &CompiledAppBundle, package: &ArgentGenesisPackage) -> Result<()> {
    if compiled.primary().app != package.primary.app || compiled.primary().id != package.primary.id {
        return Err(ArgentError::new(format!(
            "source primary artifact does not match proof package: compiled {} ({}), package {} ({})",
            compiled.primary().app,
            compiled.primary().id,
            package.primary.app,
            package.primary.id,
        )));
    }
    let compiled_dependencies = compiled
        .apps()
        .filter(|(app, _)| *app != compiled.primary().app)
        .map(|(app, artifact)| (app, artifact.id.as_str()))
        .collect::<BTreeMap<_, _>>();
    let packaged_dependencies =
        package.dependencies.iter().map(|artifact| (artifact.app.as_str(), artifact.id.as_str())).collect::<BTreeMap<_, _>>();
    if compiled_dependencies != packaged_dependencies {
        return Err(ArgentError::new("source dependency artifact IDs do not match proof package"));
    }
    Ok(())
}

fn read_text(path: &Path) -> Result<String> {
    fs::read_to_string(path).map_err(|err| ArgentError::at(path, err.to_string()))
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    serde_json::from_str(&read_text(path)?).map_err(|err| ArgentError::at(path, err.to_string()))
}

#[cfg(test)]
mod tests;

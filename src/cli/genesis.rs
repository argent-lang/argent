//! Filesystem and compiler orchestration for portable genesis proof packages.

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
    genesis::{ArgentGenesisOutput, ArgentGenesisPackage, ArgentGenesisProof, GenesisProofLayer, GenesisProofPackage},
};
use clap::{ArgGroup, Args, Subcommand};
use kaspa_consensus_core::{Hash, tx::TransactionOutpoint};
use serde::{Deserialize, de::DeserializeOwned};

#[derive(Debug, Subcommand)]
pub(crate) enum GenesisCommand {
    /// Compose an Argent proof from authored genesis states and source or artifacts.
    Compose(ComposeArgs),
    /// Verify a package against an independently obtained covenant ID.
    Verify(VerifyArgs),
}

#[derive(Debug, Args)]
#[command(group(ArgGroup::new("input").required(true).args(["source", "artifact"])))]
pub(crate) struct ComposeArgs {
    /// Argent source file. Imports supply the complete dependency closure.
    #[arg(value_name = "APP.AG")]
    source: Option<PathBuf>,
    /// Primary artifact JSON file, instead of source.
    #[arg(long, value_name = "ARTIFACT.JSON")]
    artifact: Option<PathBuf>,
    /// Select an app from the source file; otherwise it must declare exactly one.
    #[arg(long, requires = "source", conflicts_with = "artifact", value_name = "NAME")]
    app: Option<String>,
    /// Dependency artifact JSON file; repeat for the complete dependency closure.
    #[arg(long, requires = "artifact", conflicts_with = "source", value_name = "ARTIFACT.JSON")]
    dependency: Vec<PathBuf>,
    /// Initial actors and authored states, authorizing outpoint, and ordered output metadata.
    #[arg(long, value_name = "GENESIS.JSON")]
    bootstrap: PathBuf,
    /// File for the self-contained proof package.
    #[arg(long, value_name = "PROOF.JSON")]
    out: PathBuf,
}

#[derive(Debug, Args)]
pub(crate) struct VerifyArgs {
    /// Self-contained Argent, Silverscript, or consensus proof package.
    #[arg(value_name = "PROOF.JSON")]
    proof: PathBuf,
    /// Covenant ID from a node or another independent source, not from this package.
    #[arg(long, value_name = "ID")]
    covenant_id: Hash,
    /// Reject lower-level packages that do not check authored state and route context.
    #[arg(long)]
    require_argent: bool,
    /// Also recompile this Argent source and compare all artifact identities.
    #[arg(long, value_name = "APP.AG")]
    source: Option<PathBuf>,
    /// Select an app from --source; otherwise it must declare exactly one.
    #[arg(long, requires = "source", value_name = "NAME")]
    app: Option<String>,
}

/// Composition data carries no covenant-ID claim; composition derives it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CovenantBootstrap {
    authorizing_outpoint: TransactionOutpoint,
    outputs: Vec<ArgentGenesisOutput>,
}

pub(crate) fn run(command: GenesisCommand) -> Result<()> {
    match command {
        GenesisCommand::Compose(args) => compose(args),
        GenesisCommand::Verify(args) => verify(args),
    }
}

fn compose(args: ComposeArgs) -> Result<()> {
    let bootstrap: CovenantBootstrap = read_json(&args.bootstrap)?;
    let authored = if let Some(source) = &args.source {
        let compiled = compile_source(source, args.app.as_deref())?;
        let bundle = compiled.runtime_bundle().map_err(|err| ArgentError::new(err.to_string()))?;
        compose_package(&bundle, bootstrap)?
    } else {
        let path = args.artifact.as_ref().expect("Clap requires source or artifact");
        let primary: Artifact = read_json(path)?;
        let dependencies = args.dependency.iter().map(|path| read_json::<Artifact>(path)).collect::<Result<Vec<_>>>()?;
        let mut bundle = ArtifactBundle::new(&primary).map_err(|err| ArgentError::at(path, err.to_string()))?;
        for dependency in &dependencies {
            bundle = bundle.with_artifact(dependency).map_err(|err| ArgentError::new(err.to_string()))?;
        }
        compose_package(&bundle, bootstrap)?
    };
    let covenant_id = authored.proof.claimed_covenant_id;
    let package = GenesisProofPackage::new(authored);
    let json = package.to_json().map_err(|err| ArgentError::new(err.to_string()))?;
    // Do not overwrite a package or another input file by accident.
    let mut output = fs::File::create_new(&args.out).map_err(|err| ArgentError::at(&args.out, err.to_string()))?;
    output.write_all(json.as_bytes()).map_err(|err| ArgentError::at(&args.out, err.to_string()))?;
    println!("wrote {}", args.out.display());
    println!("covenant ID: {covenant_id}");
    Ok(())
}

fn compose_package(bundle: &ArtifactBundle<'_>, bootstrap: CovenantBootstrap) -> Result<ArgentGenesisPackage> {
    let proof = ArgentGenesisProof::compose(bundle, bootstrap.authorizing_outpoint, bootstrap.outputs)
        .map_err(|err| ArgentError::new(err.to_string()))?;
    Ok(ArgentGenesisPackage::new(bundle, proof))
}

fn verify(args: VerifyArgs) -> Result<()> {
    let json = read_text(&args.proof)?;
    let package = GenesisProofPackage::from_json(&json).map_err(|err| ArgentError::at(&args.proof, err.to_string()))?;
    if args.require_argent || args.source.is_some() {
        package.verify_argent(args.covenant_id)
    } else {
        package.verify(args.covenant_id)
    }
    .map_err(|err| ArgentError::at(&args.proof, err.to_string()))?;

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

//! Owned, versioned genesis proof packages without filesystem or compiler access.

use argent_artifact::Artifact;
use argent_runtime::ArtifactBundle;
use kaspa_consensus_core::Hash;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{
    ArgentGenesisProof, ArgentGenesisProofError, ConsensusGenesisProof, GenesisProofError, SilGenesisProof, SilGenesisProofError,
};

pub const GENESIS_PROOF_SCHEMA_VERSION: u32 = 1;

/// A self-contained proof at one selected layer.
///
/// Deserialization is not verification. Compare with an independently obtained
/// covenant ID through [`Self::verify`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenesisProofPackage {
    /// Package format version, independent of embedded artifact versions.
    pub schema_version: u32,
    /// Complete proof data for the selected layer, without cached lower layers.
    pub proof: GenesisProofLayer,
}

impl GenesisProofPackage {
    /// Wrap an existing proof without changing its covenant-ID claim.
    pub fn new(proof: impl Into<GenesisProofLayer>) -> Self {
        Self { schema_version: GENESIS_PROOF_SCHEMA_VERSION, proof: proof.into() }
    }

    /// Parse JSON and check the package version, without verifying proof contents.
    pub fn from_json(source: &str) -> Result<Self, GenesisProofPackageError> {
        let package: Self = serde_json::from_str(source)?;
        package.check_version()?;
        Ok(package)
    }

    /// Serialize a supported package with Silverscript's pretty JSON formatter.
    pub fn to_json(&self) -> Result<String, GenesisProofPackageError> {
        self.check_version()?;
        Ok(silverscript_abi::to_pretty_json(self)?)
    }

    /// Check the package version, embedded data, and published covenant-ID claim.
    pub fn check_consistency(&self) -> Result<(), GenesisProofPackageError> {
        self.check_version()?;
        match &self.proof {
            GenesisProofLayer::Consensus(proof) => proof.check_consistency()?,
            GenesisProofLayer::Sil(proof) => proof.check_consistency()?,
            GenesisProofLayer::Argent(package) => package.check_consistency()?,
        }
        Ok(())
    }

    /// Verify the selected proof layer against an independently obtained covenant ID.
    ///
    /// This checks only the package's selected layer. Use [`Self::verify_argent`]
    /// when authored actor states and derived route context must also be checked.
    pub fn verify(&self, expected: Hash) -> Result<(), GenesisProofPackageError> {
        self.check_version()?;
        match &self.proof {
            GenesisProofLayer::Consensus(proof) => proof.verify(expected)?,
            GenesisProofLayer::Sil(proof) => proof.verify(expected)?,
            GenesisProofLayer::Argent(package) => package.verify(expected)?,
        }
        Ok(())
    }

    /// Require an Argent-layer package and verify it against an independent ID.
    ///
    /// Consensus and Silverscript packages are rejected even if they produce
    /// the expected ID.
    pub fn verify_argent(&self, expected: Hash) -> Result<(), GenesisProofPackageError> {
        self.check_version()?;
        match &self.proof {
            GenesisProofLayer::Argent(package) => package.verify(expected)?,
            GenesisProofLayer::Consensus(_) => return Err(GenesisProofPackageError::ArgentLayerRequired { found: "consensus" }),
            GenesisProofLayer::Sil(_) => return Err(GenesisProofPackageError::ArgentLayerRequired { found: "sil" }),
        }
        Ok(())
    }

    fn check_version(&self) -> Result<(), GenesisProofPackageError> {
        if self.schema_version != GENESIS_PROOF_SCHEMA_VERSION {
            return Err(GenesisProofPackageError::UnsupportedVersion {
                expected: GENESIS_PROOF_SCHEMA_VERSION,
                found: self.schema_version,
            });
        }
        Ok(())
    }
}

/// The selected layer and the data needed to check it.
///
/// Each higher layer derives and checks the lower layers. No layer proves
/// source-code correspondence or application correctness.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case", deny_unknown_fields)]
pub enum GenesisProofLayer {
    /// Check exact genesis outputs and the authorizing outpoint against the ID.
    Consensus(ConsensusGenesisProof),
    /// Also derive those outputs from contract frames and physical states.
    Sil(SilGenesisProof),
    /// Also derive physical states from authored actor states and artifact plans.
    Argent(Box<ArgentGenesisPackage>),
}

impl From<ConsensusGenesisProof> for GenesisProofLayer {
    fn from(proof: ConsensusGenesisProof) -> Self {
        Self::Consensus(proof)
    }
}

impl From<SilGenesisProof> for GenesisProofLayer {
    fn from(proof: SilGenesisProof) -> Self {
        Self::Sil(proof)
    }
}

impl From<ArgentGenesisPackage> for GenesisProofLayer {
    fn from(package: ArgentGenesisPackage) -> Self {
        Self::Argent(Box::new(package))
    }
}

/// Owned artifacts and authored states for one primary app's genesis group.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArgentGenesisPackage {
    /// Primary app artifact, retained without modification.
    pub primary: Artifact,
    /// Attached app artifacts, including the complete dependency closure.
    pub dependencies: Vec<Artifact>,
    /// Owned proof data, including the published claim and authored outputs.
    pub proof: ArgentGenesisProof,
}

impl ArgentGenesisPackage {
    /// Own the bundle's artifacts and existing proof without changing its claim.
    ///
    /// This stores the verification context; it does not verify the proof.
    pub fn new(bundle: &ArtifactBundle<'_>, proof: ArgentGenesisProof) -> Self {
        let dependencies = bundle.dependencies().cloned().collect();
        Self { primary: bundle.primary().clone(), dependencies, proof }
    }

    /// Check and borrow the owned artifacts as the proof's runtime context.
    ///
    /// Dependency relations and states are checked by the proof's consistency
    /// and verification operations.
    pub fn runtime_bundle(&self) -> Result<ArtifactBundle<'_>, ArgentGenesisProofError> {
        let mut bundle = ArtifactBundle::new(&self.primary)?;
        for artifact in &self.dependencies {
            bundle = bundle.with_artifact(artifact)?;
        }
        Ok(bundle)
    }

    /// Check the artifact context, authored states, and published covenant-ID claim.
    pub fn check_consistency(&self) -> Result<(), ArgentGenesisProofError> {
        self.proof.check_consistency(&self.runtime_bundle()?)
    }

    /// Verify the authored proof against an independently obtained covenant ID.
    pub fn verify(&self, expected: Hash) -> Result<(), ArgentGenesisProofError> {
        self.proof.verify(&self.runtime_bundle()?, expected)
    }
}

#[derive(Debug, Error)]
pub enum GenesisProofPackageError {
    #[error("unsupported genesis proof package schema version {found}; expected {expected}")]
    UnsupportedVersion { expected: u32, found: u32 },
    #[error("Argent genesis verification requires an Argent proof package, found `{found}`")]
    ArgentLayerRequired { found: &'static str },
    #[error("genesis proof package JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Consensus(#[from] GenesisProofError),
    #[error(transparent)]
    Sil(#[from] SilGenesisProofError),
    #[error(transparent)]
    Argent(#[from] ArgentGenesisProofError),
}

#[cfg(test)]
mod tests;

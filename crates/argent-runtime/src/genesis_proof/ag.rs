//! Authored actor-state genesis proofs for one Argent app.

use std::collections::BTreeMap;

use kaspa_consensus_core::{Hash, tx::TransactionOutpoint};
use silverscript_abi::ArtifactValue;
use thiserror::Error;

use super::{SilGenesisOutput, SilGenesisProof, SilGenesisProofError};
use crate::{ArtifactBundle, BuilderError, TxBuilder};

/// A genesis proof using authored states from the bundle's primary app.
///
/// Dependency artifacts supply checked templates and interfaces. They cannot
/// contribute actors to this covenant's genesis group. This proof borrows its
/// artifacts; portable package ownership and serialization are separate.
#[derive(Clone, Debug)]
pub struct ArgentGenesisProof<'a> {
    /// Primary app and its complete artifact dependency closure.
    pub bundle: ArtifactBundle<'a>,
    /// Previous outpoint of the input that authorized this genesis group.
    pub authorizing_outpoint: TransactionOutpoint,
    /// Covenant ID claimed by the proof publisher.
    pub claimed_covenant_id: Hash,
    /// Ordered authored actor states in the genesis group.
    pub outputs: Vec<ArgentGenesisOutput>,
}

impl<'a> ArgentGenesisProof<'a> {
    /// Compose a proof and derive its covenant ID from authored actor states.
    pub fn compose(
        bundle: &ArtifactBundle<'a>,
        authorizing_outpoint: TransactionOutpoint,
        outputs: Vec<ArgentGenesisOutput>,
    ) -> Result<Self, ArgentGenesisProofError> {
        let sil_outputs = materialize_outputs(bundle, &outputs)?;
        let sil_proof = SilGenesisProof::compose(vec![bundle.primary().sil_abi.clone()], authorizing_outpoint, sil_outputs)?;
        Ok(Self { bundle: bundle.clone(), authorizing_outpoint, claimed_covenant_id: sil_proof.claimed_covenant_id, outputs })
    }

    /// Derive physical state from the artifact plans and authored values.
    ///
    /// The result retains the published claim; it does not replace it with a
    /// newly calculated covenant ID.
    pub fn sil_proof(&self) -> Result<SilGenesisProof, ArgentGenesisProofError> {
        let outputs = materialize_outputs(&self.bundle, &self.outputs)?;
        Ok(SilGenesisProof {
            abis: vec![self.bundle.primary().sil_abi.clone()],
            authorizing_outpoint: self.authorizing_outpoint,
            claimed_covenant_id: self.claimed_covenant_id,
            outputs,
        })
    }

    /// Check dependencies, authored states, and the claimed covenant ID.
    pub fn check_consistency(&self) -> Result<(), ArgentGenesisProofError> {
        self.sil_proof()?.check_consistency()?;
        Ok(())
    }

    /// Verify the proof against a covenant ID from an independent source.
    pub fn verify(&self, expected: Hash) -> Result<(), ArgentGenesisProofError> {
        self.sil_proof()?.verify(expected)?;
        Ok(())
    }
}

/// One authored actor state in an Argent covenant genesis group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArgentGenesisOutput {
    /// Position of this output in the launch transaction.
    pub index: u32,
    /// KAS value, in sompi units (also called litras), committed by the covenant ID.
    pub value: u64,
    /// Actor name within the bundle's primary app.
    pub actor: String,
    /// Authored fields, including expansion preimages but not generated route fields.
    pub authored_state: BTreeMap<String, ArtifactValue>,
}

impl ArgentGenesisOutput {
    pub fn new(index: u32, value: u64, actor: impl Into<String>, authored_state: BTreeMap<String, ArtifactValue>) -> Self {
        Self { index, value, actor: actor.into(), authored_state }
    }
}

#[derive(Debug, Error)]
pub enum ArgentGenesisProofError {
    #[error("genesis artifact bundle is inconsistent: {0}")]
    Bundle(#[from] BuilderError),
    #[error("cannot materialize genesis output {output_index} for actor `{actor}`: {source}")]
    OutputState {
        output_index: u32,
        actor: String,
        #[source]
        source: Box<BuilderError>,
    },
    #[error(transparent)]
    Sil(#[from] SilGenesisProofError),
}

fn materialize_outputs(
    bundle: &ArtifactBundle<'_>,
    outputs: &[ArgentGenesisOutput],
) -> Result<Vec<SilGenesisOutput>, ArgentGenesisProofError> {
    let builder = TxBuilder::from_bundle(bundle)?;
    let artifact = bundle.primary();
    outputs
        .iter()
        .map(|output| {
            let runtime_state = builder
                .contract_in_artifact(artifact, &output.actor)
                .and_then(|contract| builder.runtime_state_values(artifact, &output.actor, contract, output.authored_state.clone()))
                .map_err(|source| ArgentGenesisProofError::OutputState {
                    output_index: output.index,
                    actor: output.actor.clone(),
                    source: Box::new(source),
                })?;
            Ok(SilGenesisOutput::new(output.index, output.value, 0, &output.actor, runtime_state))
        })
        .collect()
}

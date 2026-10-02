//! Authored actor-state genesis proofs for one Argent app.

use std::collections::BTreeMap;

use argent_runtime::{ArtifactBundle, BuilderError, ContextInput, OutputCovenant, OutputOwner, OutputState, TxBuilder, TxContext};
use kaspa_consensus_core::{Hash, tx::TransactionOutpoint};
use serde::{Deserialize, Serialize};
use silverscript_abi::ArtifactValue;
use thiserror::Error;

use super::{SilGenesisOutput, SilGenesisProof, SilGenesisProofError};

/// Initial authored actor states for one app's covenant genesis group.
///
/// This data carries no covenant-ID claim. Composition derives the ID from
/// the selected app's artifacts and these states.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArgentCovenantBootstrap {
    /// Primary app containing every actor in the genesis group.
    pub app: String,
    /// Previous outpoint of the input that authorizes this genesis group.
    pub authorizing_outpoint: TransactionOutpoint,
    /// Authored actor states, in strictly increasing transaction-output order.
    pub outputs: Vec<ArgentGenesisOutput>,
}

impl ArgentCovenantBootstrap {
    /// Export one genesis group, preserving global output indices and authored states.
    ///
    /// The group is selected by both its authorizing input and subgroup name.
    /// Selected outputs must be primary-app actors with static authored state;
    /// raw scripts and deferred state cannot supply an authored bootstrap.
    /// Unrelated outputs and callbacks are left untouched. This does not build
    /// the transaction; composition checks the exported actor states.
    pub fn from_context(
        bundle: &ArtifactBundle<'_>,
        context: &TxContext<'_>,
        authorizing_input: u16,
        subgroup: &str,
    ) -> Result<Self, ArgentGenesisProofError> {
        let input = context.inputs.get(usize::from(authorizing_input)).ok_or_else(|| {
            ArgentGenesisProofError::Context(Box::new(BuilderError::GenesisAuthorizingInputOutOfRange {
                authorizing_input,
                input_count: context.inputs.len(),
            }))
        })?;
        let authorizing_outpoint = match input {
            ContextInput::Actor(input) => input.outpoint,
            ContextInput::Ordinary(input) => input.outpoint,
        };
        let mut outputs = Vec::new();
        for (output_index, output) in context.outputs.iter().enumerate() {
            if !matches!(&output.covenant, OutputCovenant::Genesis { authorizing_input: input, subgroup: name }
                if *input == authorizing_input && name == subgroup)
            {
                continue;
            }
            let OutputOwner::Actor { actor, state } = &output.owner else {
                return Err(ArgentGenesisProofError::ExportOutput {
                    output_index,
                    reason: "raw-script output has no authored actor state".into(),
                });
            };
            if actor.app.as_deref().is_some_and(|alias| alias != bundle.primary_alias()) {
                return Err(ArgentGenesisProofError::ExportOutput {
                    output_index,
                    reason: format!("actor `{actor}` is not in primary app `{}`", bundle.primary().app),
                });
            }
            let OutputState::Static(state) = state else {
                return Err(ArgentGenesisProofError::Context(Box::new(BuilderError::GenesisOutputStateCallback {
                    output_index,
                    actor: actor.to_string(),
                })));
            };
            let index = u32::try_from(output_index)
                .map_err(|_| ArgentGenesisProofError::Context(Box::new(BuilderError::GenesisOutputIndexOverflow(output_index))))?;
            outputs.push(ArgentGenesisOutput::new(index, output.value, &actor.actor, state.clone()));
        }
        if outputs.is_empty() {
            return Err(ArgentGenesisProofError::MissingGenesisGroup { authorizing_input, subgroup: subgroup.to_string() });
        }
        Ok(Self { app: bundle.primary().app.clone(), authorizing_outpoint, outputs })
    }

    /// Check the app and compose a proof, deriving its covenant-ID claim.
    pub fn compose(self, bundle: &ArtifactBundle<'_>) -> Result<ArgentGenesisProof, ArgentGenesisProofError> {
        if self.app != bundle.primary().app {
            return Err(ArgentGenesisProofError::AppMismatch { expected: bundle.primary().app.clone(), found: self.app });
        }
        ArgentGenesisProof::compose(bundle, self.authorizing_outpoint, self.outputs)
    }
}

/// Owned genesis proof data using authored actor states from one app.
///
/// Composition, lowering, and verification receive an artifact bundle as their
/// context. Dependency artifacts supply checked templates and interfaces, but
/// cannot contribute actors to this covenant's genesis group.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArgentGenesisProof {
    /// Previous outpoint of the input that authorized this genesis group.
    pub authorizing_outpoint: TransactionOutpoint,
    /// Covenant ID claimed by the proof publisher.
    pub claimed_covenant_id: Hash,
    /// Ordered authored actor states in the genesis group.
    pub outputs: Vec<ArgentGenesisOutput>,
}

impl ArgentGenesisProof {
    /// Compose a proof and derive its covenant ID from authored actor states.
    pub fn compose(
        bundle: &ArtifactBundle<'_>,
        authorizing_outpoint: TransactionOutpoint,
        outputs: Vec<ArgentGenesisOutput>,
    ) -> Result<Self, ArgentGenesisProofError> {
        let sil_outputs = materialize_outputs(bundle, &outputs)?;
        let sil_proof = SilGenesisProof::compose(vec![bundle.primary().sil_abi.clone()], authorizing_outpoint, sil_outputs)?;
        Ok(Self { authorizing_outpoint, claimed_covenant_id: sil_proof.claimed_covenant_id, outputs })
    }

    /// Derive physical state from the artifact plans and authored values.
    ///
    /// The result retains the published claim; it does not replace it with a
    /// newly calculated covenant ID.
    pub fn sil_proof(&self, bundle: &ArtifactBundle<'_>) -> Result<SilGenesisProof, ArgentGenesisProofError> {
        let outputs = materialize_outputs(bundle, &self.outputs)?;
        Ok(SilGenesisProof {
            abis: vec![bundle.primary().sil_abi.clone()],
            authorizing_outpoint: self.authorizing_outpoint,
            claimed_covenant_id: self.claimed_covenant_id,
            outputs,
        })
    }

    /// Check dependencies, authored states, and the claimed covenant ID.
    pub fn check_consistency(&self, bundle: &ArtifactBundle<'_>) -> Result<(), ArgentGenesisProofError> {
        self.sil_proof(bundle)?.check_consistency()?;
        Ok(())
    }

    /// Verify the proof against a covenant ID from an independent source.
    pub fn verify(&self, bundle: &ArtifactBundle<'_>, expected: Hash) -> Result<(), ArgentGenesisProofError> {
        self.sil_proof(bundle)?.verify(expected)?;
        Ok(())
    }
}

/// One authored actor state in an Argent covenant genesis group.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    #[error("bootstrap app `{found}` does not match primary app `{expected}`")]
    AppMismatch { expected: String, found: String },
    #[error("genesis group `{subgroup}` authorized by input {authorizing_input} has no outputs")]
    MissingGenesisGroup { authorizing_input: u16, subgroup: String },
    #[error("cannot export genesis output {output_index}: {reason}")]
    ExportOutput { output_index: usize, reason: String },
    #[error("cannot export genesis bootstrap: {0}")]
    Context(#[source] Box<BuilderError>),
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
    outputs
        .iter()
        .map(|output| {
            let runtime_state = builder.materialize_actor_state(&output.actor, output.authored_state.clone()).map_err(|source| {
                ArgentGenesisProofError::OutputState {
                    output_index: output.index,
                    actor: output.actor.clone(),
                    source: Box::new(source),
                }
            })?;
            Ok(SilGenesisOutput::new(output.index, output.value, 0, &output.actor, runtime_state))
        })
        .collect()
}

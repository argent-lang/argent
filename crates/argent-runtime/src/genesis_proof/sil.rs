//! Silverscript contract and physical-state genesis proofs.

use std::collections::BTreeMap;

use kaspa_consensus_core::{Hash, tx::TransactionOutpoint};
use kaspa_txscript::pay_to_script_hash_script;
use silverscript_abi::{
    ArtifactValue, CodecError, SilAbiArtifact, SilAbiVerificationError, SilContractArtifact, encode_runtime_state_script,
};
use thiserror::Error;

use super::{ConsensusGenesisProof, GenesisProofError, IndexedGenesisOutput};

/// A self-contained Silverscript proof for one covenant genesis group.
///
/// ABI compilation units remain separate, in caller-supplied order. Each
/// output selects its ABI by index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SilGenesisProof {
    /// Independent Sil ABI compilation units used by the proof outputs.
    pub abis: Vec<SilAbiArtifact>,
    /// Previous outpoint of the input that authorized this genesis group.
    pub authorizing_outpoint: TransactionOutpoint,
    /// Covenant ID claimed by the proof publisher.
    pub claimed_covenant_id: Hash,
    /// Ordered physical contract states in the genesis group.
    pub outputs: Vec<SilGenesisOutput>,
}

impl SilGenesisProof {
    /// Compose a proof from independent ABI units and physical contract states.
    pub fn compose(
        abis: Vec<SilAbiArtifact>,
        authorizing_outpoint: TransactionOutpoint,
        outputs: Vec<SilGenesisOutput>,
    ) -> Result<Self, SilGenesisProofError> {
        check_abis(&abis)?;
        let indexed_outputs = materialize_outputs(&abis, &outputs)?;
        let preimage = ConsensusGenesisProof::compose(authorizing_outpoint, indexed_outputs)?;

        Ok(Self { abis, authorizing_outpoint, claimed_covenant_id: preimage.claimed_covenant_id, outputs })
    }

    /// Reconstruct the complete consensus proof from the Sil ABI and states.
    pub fn consensus_proof(&self) -> Result<ConsensusGenesisProof, SilGenesisProofError> {
        check_abis(&self.abis)?;
        Ok(ConsensusGenesisProof {
            authorizing_outpoint: self.authorizing_outpoint,
            claimed_covenant_id: self.claimed_covenant_id,
            outputs: materialize_outputs(&self.abis, &self.outputs)?,
        })
    }

    /// Check the ABI units, physical states, and claimed covenant ID.
    pub fn check_consistency(&self) -> Result<(), SilGenesisProofError> {
        self.consensus_proof()?.check_consistency()?;
        Ok(())
    }

    /// Verify the complete proof against a covenant ID from an independent source.
    pub fn verify(&self, expected: Hash) -> Result<(), SilGenesisProofError> {
        self.consensus_proof()?.verify(expected)?;
        Ok(())
    }
}

/// One physical Silverscript contract state in a covenant genesis group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SilGenesisOutput {
    /// Position of this output in the launch transaction.
    pub index: u32,
    /// KAS value, in sompi units (also called litras), committed by the covenant ID.
    pub value: u64,
    /// Index into [`SilGenesisProof::abis`].
    pub abi_index: usize,
    /// Contract name within the selected ABI.
    pub contract: String,
    /// Complete physical runtime state, including compiler-owned fields.
    pub runtime_state: BTreeMap<String, ArtifactValue>,
}

impl SilGenesisOutput {
    pub fn new(
        index: u32,
        value: u64,
        abi_index: usize,
        contract: impl Into<String>,
        runtime_state: BTreeMap<String, ArtifactValue>,
    ) -> Self {
        Self { index, value, abi_index, contract: contract.into(), runtime_state }
    }
}

#[derive(Debug, Error)]
pub enum SilGenesisProofError {
    #[error("Sil ABI at index {abi_index} is inconsistent: {source}")]
    InvalidAbi {
        abi_index: usize,
        #[source]
        source: SilAbiVerificationError,
    },
    #[error("genesis output {output_index} references unknown Sil ABI index {abi_index}")]
    UnknownAbi { output_index: u32, abi_index: usize },
    #[error("genesis output {output_index} references unknown contract `{contract}` in Sil ABI at index {abi_index}")]
    UnknownContract { output_index: u32, abi_index: usize, contract: String },
    #[error("cannot encode genesis output {output_index} state for contract `{contract}` in Sil ABI at index {abi_index}: {source}")]
    RuntimeState {
        output_index: u32,
        abi_index: usize,
        contract: String,
        #[source]
        source: CodecError,
    },
    #[error(transparent)]
    Preimage(#[from] GenesisProofError),
}

fn check_abis(abis: &[SilAbiArtifact]) -> Result<(), SilGenesisProofError> {
    for (abi_index, abi) in abis.iter().enumerate() {
        abi.check_consistency().map_err(|source| SilGenesisProofError::InvalidAbi { abi_index, source })?;
    }
    Ok(())
}

fn materialize_outputs(
    abis: &[SilAbiArtifact],
    outputs: &[SilGenesisOutput],
) -> Result<Vec<IndexedGenesisOutput>, SilGenesisProofError> {
    outputs
        .iter()
        .map(|output| {
            let abi = abis
                .get(output.abi_index)
                .ok_or(SilGenesisProofError::UnknownAbi { output_index: output.index, abi_index: output.abi_index })?;
            let contract = abi.contract(&output.contract).ok_or_else(|| SilGenesisProofError::UnknownContract {
                output_index: output.index,
                abi_index: output.abi_index,
                contract: output.contract.clone(),
            })?;
            let redeem_script = materialize_redeem_script(abi, contract, &output.runtime_state).map_err(|source| {
                SilGenesisProofError::RuntimeState {
                    output_index: output.index,
                    abi_index: output.abi_index,
                    contract: output.contract.clone(),
                    source,
                }
            })?;
            Ok(IndexedGenesisOutput::new(output.index, output.value, pay_to_script_hash_script(&redeem_script)))
        })
        .collect()
}

/// Insert physical runtime state into a checked Sil contract frame.
///
/// The ABI must have passed [`SilAbiArtifact::check_consistency`].
pub(crate) fn materialize_redeem_script(
    abi: &SilAbiArtifact,
    contract: &SilContractArtifact,
    runtime_state: &BTreeMap<String, ArtifactValue>,
) -> Result<Vec<u8>, CodecError> {
    let state_script = encode_runtime_state_script(abi, &contract.runtime_state, runtime_state)?;
    let compiled = &contract.compiled;
    let (prefix, _, suffix) =
        compiled.script_parts(&compiled.bytecode).expect("Sil ABI state span was checked before runtime-state materialization");
    Ok(prefix.iter().chain(&state_script).chain(suffix).copied().collect())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use kaspa_consensus_core::{Hash, tx::TransactionOutpoint};
    use kaspa_txscript::pay_to_script_hash_script;
    use silverscript_abi::{
        ArtifactValue, CompiledContractArtifact, FieldArtifact, RuntimeFieldArtifact, RuntimeStateArtifact, SIL_ABI_SCHEMA_VERSION,
        SilAbiArtifact, SilContractArtifact, StateSpanArtifact, StructArtifact, TypeArtifact, encode_runtime_state_script,
        template_hash,
    };

    use super::{SilGenesisOutput, SilGenesisProof, SilGenesisProofError};
    use crate::GenesisProofError;

    fn outpoint() -> TransactionOutpoint {
        TransactionOutpoint::new(Hash::from_bytes([0x31; 32]), 4)
    }

    fn struct_value(value: ArtifactValue) -> BTreeMap<String, ArtifactValue> {
        BTreeMap::from([("payload".to_string(), ArtifactValue::Object(BTreeMap::from([("value".to_string(), value)])))])
    }

    fn abi(prefix: u8, field_type: TypeArtifact, initial_value: ArtifactValue) -> SilAbiArtifact {
        let runtime_state = RuntimeStateArtifact {
            source: "State".to_string(),
            fields: vec![RuntimeFieldArtifact {
                name: "payload".to_string(),
                ty: TypeArtifact::Struct { name: "Shared".to_string() },
            }],
        };
        let mut abi = SilAbiArtifact {
            schema_version: SIL_ABI_SCHEMA_VERSION,
            compiler_version: "test".to_string(),
            structs: BTreeMap::from([(
                "Shared".to_string(),
                StructArtifact { fields: vec![FieldArtifact { name: "value".to_string(), ty: field_type }] },
            )]),
            contracts: BTreeMap::from([(
                "Counter".to_string(),
                SilContractArtifact {
                    source_path: "sil/Counter.sil".to_string(),
                    runtime_state: runtime_state.clone(),
                    entries: BTreeMap::new(),
                    cov_decl_to_abi: BTreeMap::new(),
                    delegate_entry_abi: None,
                    compiled: CompiledContractArtifact {
                        bytecode: Vec::new(),
                        template_hash: template_hash(&[], &[]),
                        state_span: StateSpanArtifact { offset: 0, len: 0 },
                    },
                },
            )]),
        };

        let state_script =
            encode_runtime_state_script(&abi, &runtime_state, &struct_value(initial_value)).expect("initial state encodes");
        let prefix = [prefix];
        let suffix = [0xcc];
        let contract = abi.contracts.get_mut("Counter").expect("contract exists");
        contract.compiled.bytecode = [prefix.as_slice(), state_script.as_slice(), suffix.as_slice()].concat();
        contract.compiled.template_hash = template_hash(&prefix, &suffix);
        contract.compiled.state_span = StateSpanArtifact { offset: prefix.len(), len: state_script.len() };
        abi
    }

    fn sil_proof() -> SilGenesisProof {
        SilGenesisProof::compose(
            vec![abi(0xaa, TypeArtifact::Byte, ArtifactValue::Byte(1)), abi(0xbb, TypeArtifact::Int, ArtifactValue::Int(2))],
            outpoint(),
            vec![
                SilGenesisOutput::new(0, 1_000, 0, "Counter", struct_value(ArtifactValue::Byte(3))),
                SilGenesisOutput::new(2, 2_000, 1, "Counter", struct_value(ArtifactValue::Int(4))),
            ],
        )
        .expect("proof composes")
    }

    #[test]
    fn independent_abis_keep_same_named_contracts_and_structs_separate() {
        let proof = sil_proof();
        assert_eq!(proof.outputs[0].abi_index, 0);
        assert_eq!(proof.outputs[1].abi_index, 1);
        let preimage = proof.consensus_proof().expect("preimage materializes");
        assert_ne!(preimage.outputs[0].script_public_key, preimage.outputs[1].script_public_key);

        let first_abi = &proof.abis[0];
        let first_contract = first_abi.contract("Counter").expect("contract exists");
        let first_state = encode_runtime_state_script(first_abi, &first_contract.runtime_state, &proof.outputs[0].runtime_state)
            .expect("state encodes");
        let expected_redeem_script = [&[0xaa][..], first_state.as_slice(), &[0xcc][..]].concat();
        assert_eq!(preimage.outputs[0].script_public_key, pay_to_script_hash_script(&expected_redeem_script));

        proof.verify(proof.claimed_covenant_id).expect("proof verifies");
    }

    #[test]
    fn proof_rejects_invalid_abi_contract_and_runtime_state() {
        let valid_abi = abi(0xaa, TypeArtifact::Byte, ArtifactValue::Byte(1));

        let mut invalid_abi = valid_abi.clone();
        invalid_abi.contracts.get_mut("Counter").expect("contract exists").compiled.template_hash = [0; 32];
        assert!(matches!(
            SilGenesisProof::compose(
                vec![invalid_abi],
                outpoint(),
                vec![SilGenesisOutput::new(0, 1_000, 0, "Counter", struct_value(ArtifactValue::Byte(1)))]
            ),
            Err(SilGenesisProofError::InvalidAbi { abi_index: 0, .. })
        ));

        let unknown_abi = SilGenesisOutput {
            index: 0,
            value: 1_000,
            abi_index: 1,
            contract: "Counter".to_string(),
            runtime_state: struct_value(ArtifactValue::Byte(1)),
        };
        assert!(matches!(
            SilGenesisProof::compose(vec![valid_abi.clone()], outpoint(), vec![unknown_abi]),
            Err(SilGenesisProofError::UnknownAbi { abi_index: 1, .. })
        ));

        assert!(matches!(
            SilGenesisProof::compose(
                vec![valid_abi.clone()],
                outpoint(),
                vec![SilGenesisOutput::new(0, 1_000, 0, "Missing", struct_value(ArtifactValue::Byte(1)))]
            ),
            Err(SilGenesisProofError::UnknownContract { ref contract, .. }) if contract == "Missing"
        ));

        assert!(matches!(
            SilGenesisProof::compose(
                vec![valid_abi],
                outpoint(),
                vec![SilGenesisOutput::new(0, 1_000, 0, "Counter", struct_value(ArtifactValue::Int(1)))]
            ),
            Err(SilGenesisProofError::RuntimeState { .. })
        ));
    }

    #[test]
    fn proof_detects_changed_physical_state_and_external_id() {
        let mut proof = sil_proof();
        proof.outputs[0].runtime_state = struct_value(ArtifactValue::Byte(9));
        assert!(matches!(
            proof.check_consistency(),
            Err(SilGenesisProofError::Preimage(GenesisProofError::ClaimedCovenantIdMismatch { .. }))
        ));

        let proof = sil_proof();
        assert!(matches!(
            proof.verify(Hash::from_bytes([0x99; 32])),
            Err(SilGenesisProofError::Preimage(GenesisProofError::ExpectedCovenantIdMismatch { .. }))
        ));
    }
}

//! Composition and verification of the consensus covenant-ID preimage.
//!
//! This lowest proof layer records only the fields committed by Kaspa's
//! covenant-ID preimage. Higher-level Silverscript and Argent proofs lower to
//! this form.

use kaspa_consensus_core::{
    Hash,
    hashing::covenant_id::covenant_id,
    tx::{ScriptPublicKey, TransactionOutpoint, TransactionOutput},
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// One output in a covenant genesis group.
///
/// `index` is the output's index in the launch transaction. The covenant
/// binding is absent because consensus excludes it from the covenant-ID
/// preimage.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexedGenesisOutput {
    /// Position of this output in the launch transaction.
    pub index: u32,
    /// KAS value, in sompi units (also called litras), committed by the covenant ID.
    pub value: u64,
    /// Exact script public key committed by the covenant ID.
    pub script_public_key: ScriptPublicKey,
}

impl IndexedGenesisOutput {
    pub fn new(index: u32, value: u64, script_public_key: ScriptPublicKey) -> Self {
        Self { index, value, script_public_key }
    }

    fn transaction_output(&self) -> TransactionOutput {
        TransactionOutput::new(self.value, self.script_public_key.clone())
    }
}

/// The exact consensus preimage of one covenant genesis group.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsensusGenesisProof {
    /// Previous outpoint of the input that authorized this genesis group.
    pub authorizing_outpoint: TransactionOutpoint,
    /// Covenant ID claimed by the proof publisher.
    pub claimed_covenant_id: Hash,
    /// Ordered outputs that form this covenant's genesis group.
    pub outputs: Vec<IndexedGenesisOutput>,
}

impl ConsensusGenesisProof {
    /// Compose a proof and derive its covenant ID from the supplied preimage.
    pub fn compose(authorizing_outpoint: TransactionOutpoint, outputs: Vec<IndexedGenesisOutput>) -> Result<Self, GenesisProofError> {
        let claimed_covenant_id = compute_covenant_id(authorizing_outpoint, &outputs)?;
        Ok(Self { authorizing_outpoint, claimed_covenant_id, outputs })
    }

    /// Compute the covenant ID from the proof's consensus preimage.
    pub fn computed_covenant_id(&self) -> Result<Hash, GenesisProofError> {
        compute_covenant_id(self.authorizing_outpoint, &self.outputs)
    }

    /// Check that the claimed covenant ID matches the proof's preimage.
    pub fn check_consistency(&self) -> Result<(), GenesisProofError> {
        let computed = self.computed_covenant_id()?;
        if computed != self.claimed_covenant_id {
            return Err(GenesisProofError::ClaimedCovenantIdMismatch { claimed: self.claimed_covenant_id, computed });
        }
        Ok(())
    }

    /// Verify the proof against a covenant ID from an independent source.
    pub fn verify(&self, expected: Hash) -> Result<(), GenesisProofError> {
        self.check_consistency()?;
        if self.claimed_covenant_id != expected {
            return Err(GenesisProofError::ExpectedCovenantIdMismatch { expected, computed: self.claimed_covenant_id });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum GenesisProofError {
    #[error("genesis proof has no outputs")]
    EmptyOutputs,
    #[error("genesis proof output indices must be strictly increasing: index {current} follows {previous}")]
    OutputIndicesNotStrictlyIncreasing { previous: u32, current: u32 },
    #[error("genesis proof claims covenant ID {claimed}, but its preimage produces {computed}")]
    ClaimedCovenantIdMismatch { claimed: Hash, computed: Hash },
    #[error("genesis proof produces covenant ID {computed}, expected {expected}")]
    ExpectedCovenantIdMismatch { expected: Hash, computed: Hash },
}

fn compute_covenant_id(
    authorizing_outpoint: TransactionOutpoint,
    outputs: &[IndexedGenesisOutput],
) -> Result<Hash, GenesisProofError> {
    check_output_order(outputs)?;

    let transaction_outputs = outputs.iter().map(IndexedGenesisOutput::transaction_output).collect::<Vec<_>>();
    Ok(covenant_id(authorizing_outpoint, outputs.iter().map(|output| output.index).zip(&transaction_outputs)))
}

fn check_output_order(outputs: &[IndexedGenesisOutput]) -> Result<(), GenesisProofError> {
    if outputs.is_empty() {
        return Err(GenesisProofError::EmptyOutputs);
    }

    for pair in outputs.windows(2) {
        let previous = pair[0].index;
        let current = pair[1].index;
        if current <= previous {
            return Err(GenesisProofError::OutputIndicesNotStrictlyIncreasing { previous, current });
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use argent_runtime::TxBuilder;
    use kaspa_consensus_core::{
        Hash,
        tx::{GenesisCovenantGroup, ScriptPublicKey, TransactionOutpoint, TransactionOutput},
    };

    use super::{ConsensusGenesisProof, GenesisProofError, IndexedGenesisOutput};

    fn outpoint(byte: u8, index: u32) -> TransactionOutpoint {
        TransactionOutpoint::new(Hash::from_bytes([byte; 32]), index)
    }

    fn output(index: u32, value: u64, version: u16, script: &[u8]) -> IndexedGenesisOutput {
        IndexedGenesisOutput::new(index, value, ScriptPublicKey::new(version, script.to_vec().into()))
    }

    fn proof() -> ConsensusGenesisProof {
        ConsensusGenesisProof::compose(outpoint(0x11, 7), vec![output(0, 1_000, 0, &[0x51]), output(2, 3_000, 0, &[0x52, 0x53])])
            .expect("valid proof")
    }

    #[test]
    fn proof_matches_transaction_genesis_population() {
        let proof = proof();
        let unrelated = TransactionOutput::new(2_000, ScriptPublicKey::new(0, vec![0x54].into()));
        let mut transaction = TxBuilder::transaction(
            vec![TxBuilder::transaction_input(proof.authorizing_outpoint, Vec::new())],
            vec![proof.outputs[0].transaction_output(), unrelated, proof.outputs[1].transaction_output()],
        );

        TxBuilder::populate_genesis_covenants(&mut transaction, &[GenesisCovenantGroup::new(0, vec![0, 2])])
            .expect("genesis covenant population succeeds");

        assert_eq!(transaction.outputs[0].covenant.expect("first output is bound").covenant_id, proof.claimed_covenant_id);
        assert_eq!(transaction.outputs[2].covenant.expect("second output is bound").covenant_id, proof.claimed_covenant_id);
        proof.verify(transaction.outputs[0].covenant.expect("first output is bound").covenant_id).expect("proof verifies");
    }

    #[test]
    fn proof_rejects_empty_repeated_and_unordered_outputs() {
        assert_eq!(ConsensusGenesisProof::compose(outpoint(0x11, 7), Vec::new()), Err(GenesisProofError::EmptyOutputs));
        assert_eq!(
            ConsensusGenesisProof::compose(outpoint(0x11, 7), vec![output(1, 1_000, 0, &[0x51]), output(1, 2_000, 0, &[0x52])]),
            Err(GenesisProofError::OutputIndicesNotStrictlyIncreasing { previous: 1, current: 1 })
        );
        assert_eq!(
            ConsensusGenesisProof::compose(outpoint(0x11, 7), vec![output(2, 1_000, 0, &[0x51]), output(1, 2_000, 0, &[0x52])]),
            Err(GenesisProofError::OutputIndicesNotStrictlyIncreasing { previous: 2, current: 1 })
        );
    }

    #[test]
    fn every_consensus_preimage_field_affects_the_claim() {
        let original = proof();

        let mut variants = Vec::new();

        let mut changed_outpoint = original.clone();
        changed_outpoint.authorizing_outpoint = outpoint(0x12, 7);
        variants.push(changed_outpoint);

        let mut changed_outpoint_index = original.clone();
        changed_outpoint_index.authorizing_outpoint = outpoint(0x11, 8);
        variants.push(changed_outpoint_index);

        let mut changed_output_count = original.clone();
        changed_output_count.outputs.pop();
        variants.push(changed_output_count);

        let mut changed_index = original.clone();
        changed_index.outputs[1].index = 3;
        variants.push(changed_index);

        let mut changed_value = original.clone();
        changed_value.outputs[0].value += 1;
        variants.push(changed_value);

        let mut changed_version = original.clone();
        changed_version.outputs[0].script_public_key = ScriptPublicKey::new(1, vec![0x51].into());
        variants.push(changed_version);

        let mut changed_script = original.clone();
        changed_script.outputs[0].script_public_key = ScriptPublicKey::new(0, vec![0x52].into());
        variants.push(changed_script);

        for changed in variants {
            assert_ne!(changed.computed_covenant_id().expect("changed preimage remains valid"), original.claimed_covenant_id);
            assert!(matches!(changed.check_consistency(), Err(GenesisProofError::ClaimedCovenantIdMismatch { .. })));
        }
    }

    #[test]
    fn proof_checks_its_claim_before_the_external_id() {
        let proof = proof();
        let expected = Hash::from_bytes([0x99; 32]);
        assert_eq!(
            proof.verify(expected),
            Err(GenesisProofError::ExpectedCovenantIdMismatch { expected, computed: proof.claimed_covenant_id })
        );

        let mut inconsistent = proof.clone();
        inconsistent.claimed_covenant_id = Hash::from_bytes([0xaa; 32]);
        assert!(matches!(inconsistent.verify(expected), Err(GenesisProofError::ClaimedCovenantIdMismatch { .. })));
    }
}

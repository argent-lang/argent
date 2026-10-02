//! Runtime-state insertion into compiled Silverscript contract frames.

use std::collections::BTreeMap;

use silverscript_abi::{ArtifactValue, CodecError, SilAbiArtifact, SilContractArtifact, encode_runtime_state_script};

/// Insert physical runtime state into a checked Silverscript contract frame.
///
/// The ABI must have passed [`SilAbiArtifact::check_consistency`], and `contract`
/// must be taken unchanged from that ABI. This encodes physical fields; it does
/// not derive Argent route context or expansion digests.
///
/// # Panics
///
/// Panics if the contract's state span is invalid, which the ABI check rejects.
pub fn materialize_redeem_script(
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

//! Public ABI encoding and runtime-state codec helpers.
//!
//! These operations share their representation with generated Sil contracts.

pub use silverscript_abi::{
    ArtifactValue, CodecError, CodecResult, decode_hex, decode_runtime_state_script, encode_contract_entry_sig_script,
    encode_entry_sig_script, encode_hex, encode_runtime_state_script,
};

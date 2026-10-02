//! Layered composition and verification of covenant genesis proofs.
//!
//! Consensus, Silverscript, and Argent proofs share the runtime's state and
//! script materialization. This crate owns proof data and portable packages;
//! compilation, file loading, and node access belong to callers.

mod ag;
mod package;
mod preimage;
mod sil;

pub use ag::{ArgentGenesisOutput, ArgentGenesisProof, ArgentGenesisProofError};
pub use package::{
    ArgentGenesisPackage, GENESIS_PROOF_SCHEMA_VERSION, GenesisProofLayer, GenesisProofPackage, GenesisProofPackageError,
};
pub use preimage::{ConsensusGenesisProof, GenesisProofError, IndexedGenesisOutput};
pub use sil::{SilGenesisOutput, SilGenesisProof, SilGenesisProofError};

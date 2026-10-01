//! Layered composition and verification of covenant genesis proofs.

mod ag;
mod preimage;
mod sil;

pub use ag::{ArgentGenesisOutput, ArgentGenesisProof, ArgentGenesisProofError};
pub use preimage::{ConsensusGenesisProof, GenesisProofError, IndexedGenesisOutput};
pub(crate) use sil::materialize_redeem_script;
pub use sil::{SilGenesisOutput, SilGenesisProof, SilGenesisProofError};

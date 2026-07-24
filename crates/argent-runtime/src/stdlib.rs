//! Runtime equivalents of Argent standard-library functions.
//!
//! These helpers let off-chain code reproduce values calculated by contracts.

/// Rust implementations of functions in Argent `std::core`.
pub mod core {
    use blake2b_simd::{KEYBYTES, Params as Blake2bParams};
    use kaspa_consensus_core::{Hash, tx::TransactionOutpoint};
    use thiserror::Error;

    /// Returns the canonical Silverscript template hash.
    ///
    /// This re-export keeps standard-library-compatible helpers in one
    /// Argent runtime namespace. The implementation remains in
    /// `silverscript-abi`, which owns the template encoding.
    pub use silverscript_abi::template_hash;

    /// An invalid domain passed to [`invocation_uid`].
    #[derive(Debug, Clone, PartialEq, Eq, Error)]
    pub enum InvocationUidError {
        #[error("invocation UID domain must not be empty")]
        EmptyDomain,
        #[error("invocation UID domain contains {length} bytes; the maximum is {KEYBYTES}")]
        DomainTooLong { length: usize },
    }

    /// Returns the UID derived by Argent `std::core::invocation_uid`.
    ///
    /// The outpoint identifies the active actor invocation. The domain must
    /// contain between 1 and 64 bytes.
    pub fn invocation_uid(outpoint: &TransactionOutpoint, domain: &[u8]) -> Result<Hash, InvocationUidError> {
        if domain.is_empty() {
            return Err(InvocationUidError::EmptyDomain);
        }
        if domain.len() > KEYBYTES {
            return Err(InvocationUidError::DomainTooLong { length: domain.len() });
        }

        let mut bytes = Vec::with_capacity(36);
        bytes.extend_from_slice(outpoint.transaction_id.as_bytes().as_slice());
        bytes.extend_from_slice(&outpoint.index.to_le_bytes());
        Ok(Hash::from_slice(Blake2bParams::new().hash_length(32).key(domain).hash(&bytes).as_bytes()))
    }

    #[cfg(test)]
    mod tests {
        use kaspa_consensus_core::{
            Hash,
            tx::{TransactionId, TransactionOutpoint},
        };

        use super::{InvocationUidError, invocation_uid};

        #[test]
        fn template_hash_is_available_through_stdlib_core() {
            assert_eq!(super::template_hash(b"prefix", b"suffix"), silverscript_abi::template_hash(b"prefix", b"suffix"));
        }

        #[test]
        fn invocation_uid_matches_the_argent_stdlib_vector() {
            // `argent::builder::tests::context_executes_and_pins_invocation_uid`
            // uses this vector to compare this helper with an executed
            // `std::core::invocation_uid` contract call.
            let outpoint = TransactionOutpoint::new(TransactionId::from_bytes([0x61; 32]), 0x0102_0304);
            let expected = Hash::from_bytes([
                0x80, 0x94, 0x3a, 0xd8, 0xa6, 0xca, 0x14, 0x3c, 0xca, 0xfa, 0x47, 0x97, 0x8a, 0x1e, 0x8f, 0x4f, 0x5d, 0x32, 0x4c,
                0x58, 0x2b, 0x61, 0x93, 0x46, 0x52, 0xf8, 0x11, 0xb4, 0x36, 0xa2, 0x71, 0x2b,
            ]);

            assert_eq!(invocation_uid(&outpoint, b"LeaguePlayerId"), Ok(expected));
        }

        #[test]
        fn invocation_uid_rejects_domains_that_the_argent_function_rejects() {
            let outpoint = TransactionOutpoint::default();

            assert_eq!(invocation_uid(&outpoint, b""), Err(InvocationUidError::EmptyDomain));
            assert_eq!(invocation_uid(&outpoint, &[0; 65]), Err(InvocationUidError::DomainTooLong { length: 65 }));
        }
    }
}

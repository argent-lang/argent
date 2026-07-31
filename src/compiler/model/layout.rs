//! Fixed-width state layout facts shared by validation and code generation.

use crate::compiler::syntax::word;
use crate::compiler::syntax::{ArrayDim, TypeRef};
use crate::error::{ArgentError, Result};

/// Return the packed byte width of a supported state field.
pub(crate) fn packed_field_len(ty: &TypeRef) -> Result<usize> {
    if ty.is_actor_type() {
        return Ok(32);
    }
    match (ty.name.as_str(), ty.array) {
        ("int", None) => Ok(8),
        ("bool", None) | ("byte", None) => Ok(1),
        ("byte", Some(ArrayDim::Fixed(len))) => Ok(len),
        ("pubkey", None) | (word::COVENANT_ID, None) => Ok(32),
        ("sig", None) => Ok(65),
        ("datasig", None) => Ok(64),
        ("bytes", None) | ("string", None) | (_, Some(_)) => Err(ArgentError::new("only fixed-width scalar fields are supported")),
        (name, None) => Err(ArgentError::new(format!("unsupported type `{name}`"))),
    }
}

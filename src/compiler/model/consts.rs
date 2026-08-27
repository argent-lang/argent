//! Resolves typed values from the compiler's top-level constant declarations.
//!
//! Semantic models use this service without depending on constant source syntax.

use std::collections::BTreeMap;

use crate::compiler::syntax::lexer::parse_int_literal;
use crate::compiler::syntax::{ConstDecl, TypeRef};

/// A failure to resolve one constant as an integer value.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum ConstIntError {
    Unknown,
    WrongType(String),
    InvalidLiteral,
}

/// Indexed access to typed compile-time constants.
pub(crate) struct ConstResolver<'a> {
    declarations: BTreeMap<&'a str, &'a ConstDecl>,
}

impl<'a> ConstResolver<'a> {
    /// Index the compiler's collected constant declarations.
    pub(crate) fn new(declarations: &[&'a ConstDecl]) -> Self {
        Self { declarations: declarations.iter().map(|ct| (ct.name.as_str(), *ct)).collect() }
    }

    /// Resolve a named `const int` initialized by an integer literal.
    pub(crate) fn resolve_int(&self, name: &str) -> std::result::Result<i64, ConstIntError> {
        let ct = self.declarations.get(name).ok_or(ConstIntError::Unknown)?;
        if ct.ty != TypeRef::new("int") {
            return Err(ConstIntError::WrongType(ct.ty.to_source()));
        }
        parse_int_literal(&ct.value).ok_or(ConstIntError::InvalidLiteral)
    }
}

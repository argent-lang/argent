//! Authored type uses and their Sil carriers from the shared token stream.

use silverscript_lang::ast::{ArrayDim as SilArrayDim, TypeBase, TypeRef as SilTypeRef};

use super::{ArgentTypeUse, NamePath, Origin, Parser, TokenKind, TypeRef, word};
use crate::error::Result;

impl<'src> Parser<'src> {
    pub(super) fn parse_type(&mut self) -> Result<(TypeRef, ArgentTypeUse)> {
        let name = self.expect_any_qualified_ident()?;
        let (sil, actor_state) = self.parse_sil_type_tail(name.clone())?;
        let legacy = if let Some(state) = &actor_state {
            if !sil.array_dims.is_empty() {
                return Err(self.error(
                    "actor_type arrays are supported only in local body declarations, not signatures, state fields, or constants",
                ));
            }
            TypeRef::actor_type(state.segments.join("::"))
        } else {
            match sil.array_dims.as_slice() {
                [] => TypeRef::new(name),
                [SilArrayDim::Dynamic] => TypeRef::dynamic_array(name),
                [SilArrayDim::Fixed(len)] => TypeRef::array(name, *len),
                [SilArrayDim::Inferred] => return Err(self.error("inferred array length is only valid in an expression")),
                [SilArrayDim::Constant(_)] => return Err(self.error("constant array length is not supported in declarations yet")),
                _ => return Err(self.error("multi-dimensional declaration types are not supported yet")),
            }
        };
        Ok((legacy, ArgentTypeUse { ty: sil, actor_state }))
    }

    pub(super) fn parse_sil_type(&mut self) -> Result<SilTypeRef> {
        let start = self.current().span.start;
        let name = self.expect_any_qualified_ident()?;
        let (ty, actor_state) = self.parse_sil_type_tail(name)?;
        self.pending_type_uses.insert((start, self.previous().span.end), ArgentTypeUse { ty: ty.clone(), actor_state });
        Ok(ty)
    }

    pub(super) fn parse_sil_type_tail(&mut self, name: String) -> Result<(SilTypeRef, Option<NamePath>)> {
        let actor_state = if name == word::ACTOR_TYPE && self.consume_symbol('<') {
            let start = self.current().span.start;
            let state = self.expect_any_qualified_ident()?;
            let end = self.previous().span.end;
            self.expect_symbol('>')?;
            Some(NamePath {
                segments: state.split("::").map(str::to_string).collect(),
                origin: Origin::Authored { source: self.file.id, start, end },
            })
        } else {
            None
        };
        let base = match name.as_str() {
            "int" => TypeBase::Int,
            "temporal" => TypeBase::Temporal,
            "bool" => TypeBase::Bool,
            "string" => TypeBase::String,
            "pubkey" => TypeBase::Pubkey,
            "sig" => TypeBase::Sig,
            "datasig" => TypeBase::Datasig,
            "byte" => TypeBase::Byte,
            _ => TypeBase::Custom(name),
        };
        let mut array_dims = Vec::new();
        while self.consume_symbol('[') {
            let dim = if self.check_symbol(']') {
                SilArrayDim::Dynamic
            } else if self.check_ident("_") {
                self.advance();
                SilArrayDim::Inferred
            } else {
                match self.current().kind.clone() {
                    TokenKind::Number(raw) => {
                        self.advance();
                        SilArrayDim::Fixed(raw.parse::<usize>().map_err(|_| self.error("invalid array length"))?)
                    }
                    TokenKind::Ident(_) => SilArrayDim::Constant(self.expect_any_qualified_ident()?),
                    _ => return Err(self.error("invalid array length")),
                }
            };
            self.expect_symbol(']')?;
            array_dims.push(dim);
        }
        Ok((SilTypeRef { base, array_dims }, actor_state))
    }
}

#[cfg(test)]
mod tests;

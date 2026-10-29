//! Contract-wide Sil AST sections assembled from completed model facts.

use std::collections::BTreeSet;

use crate::codec::decode_hex;
use crate::compiler::model::AppCompilationContext;
use crate::compiler::syntax::ActorDecl;
use crate::compiler::syntax::node::{DeclId, EntryId, RootSlot};
use crate::error::{ArgentError, Result};
use silverscript_lang::ast::visit::AstVisitorMut;
use silverscript_lang::ast::{
    ArrayDim as SilArrayDim, ConstantAst, ContractAst, Expr as SilExpr, PragmaDirectiveAst, Span as SilSpan, TypeBase as SilTypeBase,
    TypeRef as SilTypeRef,
};
use silverscript_lang::compiler::{CompileOptions, compile_contract_ast};

use super::functions::FunctionAstLowerer;
use super::names::{ImportedTemplateSpec, SilNames, current_template_length_const_name, hidden_imported_template_const_name};
use super::state_types::StateValueTypes;
use super::state_types::{audit_omitted_equivalent_state_structs, contract_state_shell, lower_bound_type};
use crate::compiler::codegen::abi::constructor_args_for_actor;
use crate::compiler::model::StaticActorId;

/// Lowers contract-wide sections from one completed actor model.
pub(in crate::compiler::codegen) struct ContractLowerer<'m, 'src> {
    pub(super) actor_id: DeclId,
    pub(super) actor: &'m ActorDecl,
    pub(super) model: &'m AppCompilationContext<'src>,
    pub(super) state_values: StateValueTypes<'m>,
}

impl<'m, 'src> ContractLowerer<'m, 'src> {
    pub(in crate::compiler::codegen) fn new(actor_id: DeclId, model: &'m AppCompilationContext<'src>) -> Result<Self> {
        let actor = model.actor_by_decl(actor_id)?;
        Ok(Self { actor_id, actor, model, state_values: StateValueTypes::new(actor_id, model)? })
    }

    /// Lower and assemble one final actor AST from the completed model.
    pub(in crate::compiler::codegen) fn lower_actor(&self) -> Result<ContractAst<'m>> {
        let (params, fields) = contract_state_shell(self.actor_id, self.model)?;
        let entries = self
            .actor
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| self.lower_entry(EntryId { actor: self.actor_id, index }, entry))
            .collect::<Result<Vec<_>>>()?;
        let digest_helpers = entries.iter().flat_map(|entry| entry.digest_helpers.iter().cloned()).collect::<BTreeSet<_>>();
        let imported_templates = self
            .model
            .contract_imported_templates(self.actor_id)
            .iter()
            .map(|reference| {
                self.model.linked_actors.get(reference).map(ImportedTemplateSpec::from_linked).ok_or_else(|| {
                    ArgentError::new(format!("planned imported template `{}::{}` has no linked actor", reference.app, reference.actor))
                })
            })
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let (omitted_authored_structs, structs) = self.state_structs()?;
        let function_lowerer = FunctionAstLowerer::new(self.model, &self.state_values);
        let mut functions = self
            .model
            .functions
            .iter()
            .map(|(owner, function)| function_lowerer.lower(*owner, None, function, None))
            .collect::<Result<Vec<_>>>()?;
        let actor_model = self
            .model
            .actor_models
            .get(&self.actor_id)
            .ok_or_else(|| ArgentError::new("selected actor has no completed actor model"))?;
        functions.extend(
            actor_model
                .functions()
                .enumerate()
                .map(|(index, function)| function_lowerer.lower(actor_model.id, Some(index), function, Some(self.actor)))
                .collect::<Result<Vec<_>>>()?,
        );
        functions.extend(self.authored_state_digest_helpers(&digest_helpers)?);
        functions.extend(self.checked_range_index_helper()?);
        functions.extend(entries.into_iter().map(|entry| entry.function));
        let span = SilSpan::default();
        let mut constants: Vec<ConstantAst<'m>> =
            self.shared_constants()?.into_iter().chain(self.imported_template_constants(&imported_templates)?).collect();
        let mut embeds_current_lengths = false;
        for index in 0..self.actor.entries.len() {
            embeds_current_lengths |= self
                .model
                .entry_template_uses(EntryId { actor: self.actor_id, index })?
                .reads
                .contains(&StaticActorId::InApp(self.actor_id));
        }
        if embeds_current_lengths {
            let span = SilSpan::default();
            for part in ["prefix", "suffix"] {
                constants.push(ConstantAst {
                    type_ref: SilTypeRef { base: SilTypeBase::Byte, array_dims: vec![SilArrayDim::Fixed(4)] },
                    name: current_template_length_const_name(&self.actor.name, part),
                    expr: SilExpr::bytes([0; 4].to_vec()),
                    span,
                    type_span: span,
                    name_span: span,
                });
            }
        }
        let mut contract = ContractAst {
            pragma: Some(PragmaDirectiveAst {
                name: "silverscript".to_string(),
                value: "^0.1.0".to_string(),
                span,
                name_span: span,
                value_span: span,
            }),
            name: self.actor.name.clone(),
            params,
            structs,
            fields,
            constants,
            functions,
            span,
            name_span: span,
        };
        audit_omitted_equivalent_state_structs(&mut contract, &omitted_authored_structs)?;
        if embeds_current_lengths {
            self.resolve_current_template_lengths(&mut contract)?;
        }
        Ok(contract)
    }

    /// Fill the fixed-width current template constants without changing the compiled cut.
    fn resolve_current_template_lengths(&self, contract: &mut ContractAst<'m>) -> Result<()> {
        let args = constructor_args_for_actor(self.actor_id, self.actor, self.model)?;
        let compiled = compile_contract_ast(contract, &args, CompileOptions::default()).map_err(|err| {
            ArgentError::new(format!("generated Silverscript for actor `{}` failed to compile: {err}", self.actor.name))
        })?;
        let prefix_len = compiled.state_layout.start;
        let suffix_len = compiled.bytecode.len() - prefix_len - compiled.state_layout.len;
        for (part, len) in [("prefix", prefix_len), ("suffix", suffix_len)] {
            let len = i32::try_from(len)
                .map_err(|_| ArgentError::new(format!("template length for actor `{}` exceeds byte[4]", self.actor.name)))?;
            let name = current_template_length_const_name(&self.actor.name, part);
            let constant = contract
                .constants
                .iter_mut()
                .find(|constant| constant.name == name)
                .ok_or_else(|| ArgentError::new(format!("missing current template length constant `{name}`")))?;
            constant.expr = SilExpr::bytes(len.to_le_bytes().to_vec());
        }
        let compiled = compile_contract_ast(contract, &args, CompileOptions::default()).map_err(|err| {
            ArgentError::new(format!("generated Silverscript for actor `{}` failed to compile: {err}", self.actor.name))
        })?;
        if compiled.state_layout.start != prefix_len
            || compiled.bytecode.len() - compiled.state_layout.start - compiled.state_layout.len != suffix_len
        {
            return Err(ArgentError::new(format!(
                "embedded template lengths changed the script layout for actor `{}`",
                self.actor.name
            )));
        }
        Ok(())
    }

    fn shared_constants(&self) -> Result<Vec<ConstantAst<'m>>> {
        self.model
            .consts
            .iter()
            .map(|(owner, ct)| {
                let owner = *owner;
                let mut expr = self.model.resolution.const_expression(owner)?;
                let mut names = SilNames::new(self.model, owner, RootSlot::ConstValue);
                names.visit_expr(&mut expr);
                self.state_values.lower_bound_expression_types(&mut expr, &names)?;
                let resolved = self
                    .model
                    .types
                    .constants
                    .get(&owner)
                    .ok_or_else(|| ArgentError::new(format!("constant `{}` has no resolved type", ct.name)))?;
                let type_ref = self
                    .state_values
                    .constant_id(owner)
                    .map(|value| self.state_values.sil_type_ref(value))
                    .unwrap_or_else(|| lower_bound_type(&ct.ty, resolved));
                let span = SilSpan::default();
                Ok(ConstantAst {
                    type_ref,
                    name: self.model.types.display_names[&owner].clone(),
                    expr,
                    span,
                    type_span: span,
                    name_span: span,
                })
            })
            .collect()
    }

    fn imported_template_constants(&self, specs: &[ImportedTemplateSpec]) -> Result<Vec<ConstantAst<'static>>> {
        specs
            .iter()
            .map(|spec| {
                let bytes =
                    decode_hex(&spec.hash_hex).map_err(|err| ArgentError::new(format!("invalid linked template hash: {err}")))?;
                if bytes.len() != 32 {
                    return Err(ArgentError::new(format!("linked template hash for `{}` is not 32 bytes", spec.actor_reference())));
                }
                let span = SilSpan::default();
                Ok(ConstantAst {
                    type_ref: SilTypeRef { base: SilTypeBase::Byte, array_dims: vec![SilArrayDim::Fixed(32)] },
                    name: hidden_imported_template_const_name(spec),
                    expr: SilExpr::bytes(bytes),
                    span,
                    type_span: span,
                    name_span: span,
                })
            })
            .collect()
    }
}

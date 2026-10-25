//! Lowers structured Argent entry bodies into generated Sil statements.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use crate::compiler::model::{
    AppCompilationContext, BoundRouteActor, CallableId, ClauseActorTypeRef, CovenantGroup, CovenantIdSource, CurrentInputGroupPolicy,
    EntryInteraction, EntryOutputPlan, InputReferenceOrigin, InteractionId, InteractionLocation, InteractionSource,
    ObservedOutputFieldWitnessSpec, ObservedTemplateSource, OutputProofRequirement, ResolvedSuccessor, SourceFieldId, SourceStateId,
    StaticActorId, TemplateSelector, TemplateWitnessSource, WitnessAbiType, WitnessPlan, observed_is_dynamic_binding,
};
use crate::compiler::resolve::{Binding, LocalId, ResolvedName};
use crate::compiler::syntax::body::RouteArity;
use crate::compiler::syntax::node::{ChildEdge, EntryId, RootSlot, SourceNodeCursor, SymbolKind};
use crate::compiler::syntax::word;
use crate::compiler::syntax::*;
use crate::error::{ArgentError, Result};
use silverscript_lang::ast::visit::{AstVisitorMut, walk_expr_mut, walk_statement_mut};
use silverscript_lang::ast::{
    ArrayDim as SilArrayDim, BinaryOp as SilBinaryOp, Expr as SilExpr, ExprKind as SilExprKind, FunctionAst,
    IndexedIntrospectionKind as SilIndexedIntrospectionKind, IntrospectionKind as SilIntrospectionKind, ParamAst, Span as SilSpan,
    StateFieldExpr as SilStateFieldExpr, Statement as SilStatement, TypeBase as SilTypeBase, TypeRef as SilTypeRef,
    UnarySuffixKind as SilUnarySuffixKind,
};

// Body lowering uses model plans and Sil-local materialization helpers.
use super::contract::ContractLowerer;
use super::expr::CoSpentLowerer;
use super::names::*;
use super::state_boundary::{
    EntryInputReferencePlan, OutputValidationContext, PlannedEntryInputReference, authored_state_payload_digest_ast,
    output_state_target, plan_actor_output_state, plan_entry_input_references, plan_output_validation, plan_selector_output_state,
};
use super::state_types::StateValueTypes;
use super::state_types::{lower_bound_type, source_type_ref};

// Genesis output validation uses version-0 P2SH scripts of fixed length.
pub(in crate::compiler::codegen) const P2SH_SPK_VERSION: [u8; 2] = [0, 0];
pub(in crate::compiler::codegen) const SPK_VERSION_LEN: usize = P2SH_SPK_VERSION.len();
pub(in crate::compiler::codegen) const P2SH_SCRIPT_LEN: usize = 35;

#[cfg(test)]
#[path = "body/range_tests.rs"]
mod range_tests;

pub(in crate::compiler::codegen) struct LoweredEntryBody<'src> {
    pub(in crate::compiler::codegen) digest_helpers: BTreeSet<SourceStateId>,
    pub(in crate::compiler::codegen) authored_statements: Vec<SilStatement<'src>>,
}

/// One entry function and its contract-level helper requirements.
pub(in crate::compiler::codegen) struct LoweredEntry<'src> {
    pub(in crate::compiler::codegen) function: FunctionAst<'src>,
    pub(in crate::compiler::codegen) digest_helpers: BTreeSet<SourceStateId>,
}

impl<'src> LoweredEntryBody<'src> {
    /// Join generated entry checks with the authored body in one entrypoint AST.
    pub(in crate::compiler::codegen) fn take_entrypoint(
        &mut self,
        name: String,
        params: Vec<ParamAst<'static>>,
        prefix: Vec<SilStatement<'static>>,
    ) -> FunctionAst<'src> {
        let body = std::mem::take(&mut self.authored_statements);
        let mut statements: Vec<SilStatement<'src>> = prefix;
        let span = SilSpan::default();
        if body.is_empty() {
            statements.push(SilStatement::Require {
                expr: SilExpr::new(
                    SilExprKind::Binary { op: SilBinaryOp::Eq, left: Box::new(SilExpr::int(1)), right: Box::new(SilExpr::int(1)) },
                    span,
                ),
                message: None,
                span,
                message_span: None,
            });
        } else {
            statements.extend(body);
        }
        FunctionAst {
            name,
            attributes: Vec::new(),
            params,
            entrypoint: true,
            return_types: Vec::new(),
            returns_tuple: false,
            body: statements,
            return_type_spans: Vec::new(),
            span,
            name_span: span,
            body_span: span,
        }
    }
}

impl<'m, 'src> ContractLowerer<'m, 'src> {
    /// Join planned entry authentication, authored operations, and output checks.
    pub(in crate::compiler::codegen) fn lower_entry(&self, entry_id: EntryId, entry: &'m EntryDecl) -> Result<LoweredEntry<'m>> {
        let input_references = plan_entry_input_references(entry_id, self.actor, entry, self.model, &self.state_values)?;
        let mut lowered_body = lower_entry_body(entry_id, self.actor, entry, self.model, &input_references, &self.state_values)?;
        let witnesses = self.model.witness_plan_by_id(entry_id)?;
        let params = self.entry_params(entry_id, entry, witnesses)?;
        let imported = ImportedTemplateSpec::from_witness_plan(witnesses, self.model)?;
        let mut prefix = self.entry_template_prelude(witnesses, &imported);
        if self.model.entry_template_uses(entry_id)?.reads.contains(&StaticActorId::InApp(self.actor_id)) {
            let span = SilSpan::default();
            for (part, name) in [
                ("prefix", hidden_witness_prefix_len_name(&self.actor.name)),
                ("suffix", hidden_witness_suffix_len_name(&self.actor.name)),
            ] {
                prefix.push(SilStatement::VariableDefinition {
                    type_ref: SilTypeRef { base: SilTypeBase::Int, array_dims: Vec::new() },
                    modifiers: Vec::new(),
                    name,
                    expr: Some(SilExpr::call(
                        "int",
                        vec![SilExpr::identifier(current_template_length_const_name(&self.actor.name, part))],
                    )),
                    span,
                    type_span: span,
                    modifier_spans: Vec::new(),
                    name_span: span,
                });
            }
        }
        prefix.extend(self.current_input_prelude(entry_id, &input_references)?);
        if !entry.observes.is_empty() {
            prefix.extend(self.observed_input_prelude(entry_id, entry, &input_references)?);
        }
        prefix.extend(self.authenticated_expansion_prelude()?);
        prefix.extend(self.current_output_prelude(entry_id)?);
        prefix.extend(self.spawn_prelude(entry_id)?);
        let function = lowered_body.take_entrypoint(entry.name.clone(), params, prefix);
        Ok(LoweredEntry { function, digest_helpers: lowered_body.digest_helpers })
    }

    /// Project ordered authored and hidden ABI roles into one entry signature.
    pub(in crate::compiler::codegen) fn entry_params(
        &self,
        entry_id: EntryId,
        entry: &EntryDecl,
        witness_specs: &WitnessPlan,
    ) -> Result<Vec<ParamAst<'static>>> {
        let mut params = Vec::new();
        for (index, param) in entry.params.iter().enumerate() {
            let resolved = self.model.types.entry_params.get(&(entry_id.actor, entry_id.index, index)).ok_or_else(|| {
                ArgentError::new(format!(
                    "entry `{}::{}` has no resolved type for parameter `{}`",
                    self.actor.name, entry.name, param.name
                ))
            })?;
            let mut type_ref = lower_bound_type(&param.ty, resolved);
            if let Some(value) = self.state_values.entry_param(entry_id, index) {
                type_ref = self.state_values.sil_type_ref(value);
            }
            params.push(ParamAst {
                type_ref,
                name: param.name.clone(),
                span: SilSpan::default(),
                type_span: SilSpan::default(),
                name_span: SilSpan::default(),
            });
        }
        for &role in &witness_specs.roles {
            let type_ref = match witness_specs.role_type(role) {
                WitnessAbiType::Bytes => SilTypeRef { base: SilTypeBase::Byte, array_dims: vec![SilArrayDim::Dynamic] },
                WitnessAbiType::Int => SilTypeRef { base: SilTypeBase::Int, array_dims: Vec::new() },
                WitnessAbiType::FixedBytes(len) => SilTypeRef { base: SilTypeBase::Byte, array_dims: vec![SilArrayDim::Fixed(len)] },
            };
            let name = witness_role_name(witness_specs, role);
            params.push(ParamAst {
                type_ref,
                name,
                span: SilSpan::default(),
                type_span: SilSpan::default(),
                name_span: SilSpan::default(),
            });
        }
        Ok(params)
    }

    /// Materialize the completed witness plan's route tables and linked templates.
    pub(in crate::compiler::codegen) fn entry_template_prelude(
        &self,
        witnesses: &WitnessPlan,
        imported: &[ImportedTemplateSpec],
    ) -> Vec<SilStatement<'static>> {
        let span = SilSpan::default();
        let mut statements = Vec::new();
        for spec in imported {
            statements.push(SilStatement::VariableDefinition {
                type_ref: SilTypeRef { base: SilTypeBase::Byte, array_dims: vec![SilArrayDim::Fixed(32)] },
                modifiers: Vec::new(),
                name: hidden_imported_template_name(spec),
                expr: Some(SilExpr::identifier(hidden_imported_template_const_name(spec))),
                span,
                type_span: span,
                modifier_spans: Vec::new(),
                name_span: span,
            });
        }
        for spec in &witnesses.families {
            let table = hidden_route_family_table_name_by_id(&spec.family_id);
            let commitment = hidden_route_family_commitment_name_by_id(&spec.family_id);
            statements.push(SilStatement::Require {
                expr: SilExpr::new(
                    SilExprKind::Binary {
                        op: SilBinaryOp::Eq,
                        left: Box::new(SilExpr::call("blake3", vec![SilExpr::call("byte[]", vec![SilExpr::identifier(table)])])),
                        right: Box::new(SilExpr::identifier(commitment)),
                    },
                    span,
                ),
                message: None,
                span,
                message_span: None,
            });
        }
        for spec in &witnesses.templates {
            let TemplateWitnessSource::FamilyTable { family_id, offset } = &spec.source else {
                continue;
            };
            let start = *offset;
            let table = hidden_route_family_table_name_by_id(family_id);
            statements.push(SilStatement::VariableDefinition {
                type_ref: SilTypeRef { base: SilTypeBase::Byte, array_dims: vec![SilArrayDim::Fixed(32)] },
                modifiers: Vec::new(),
                name: hidden_template_name(&spec.actor),
                expr: Some(SilExpr::call(
                    "byte[32]",
                    vec![SilExpr::new(
                        SilExprKind::Slice {
                            source: Box::new(SilExpr::identifier(table)),
                            start: Box::new(SilExpr::int(start as i64)),
                            end: Box::new(SilExpr::int((start + 32) as i64)),
                            span,
                        },
                        span,
                    )],
                )),
                span,
                type_span: span,
                modifier_spans: Vec::new(),
                name_span: span,
            });
        }
        statements
    }
    /// Lower current-covenant output count, index and coordinated leader checks.
    pub(in crate::compiler::codegen) fn current_output_prelude(&self, entry_id: EntryId) -> Result<Vec<SilStatement<'static>>> {
        let current = self.model.entry_model_by_id(entry_id)?.current();
        let outputs = self.model.entry_output_plan_by_id(entry_id)?;
        let span = SilSpan::default();
        let active_input = || SilExpr::new(SilExprKind::Introspection(SilIntrospectionKind::ActiveInputIndex), span);
        let comparison = |op, left: SilExpr<'static>, right: SilExpr<'static>| SilStatement::Require {
            expr: SilExpr::new(SilExprKind::Binary { op, left: Box::new(left), right: Box::new(right) }, span),
            message: None,
            span,
            message_span: None,
        };
        let mut statements = Vec::new();
        if let Some(count) = outputs.exact_current_output_count() {
            statements.push(comparison(
                SilBinaryOp::Eq,
                SilExpr::call("OpAuthOutputCount", vec![active_input()]),
                SilExpr::int(count as i64),
            ));
        }
        for interaction in current.outputs() {
            let handle = interaction.handle();
            if let Some((minimum, maximum, singleton_count)) = outputs.current_output_range(interaction.id()) {
                let name = hidden_output_count_name(handle);
                let count = SilExpr::call("OpAuthOutputCount", vec![active_input()]);
                let value = if singleton_count == 0 {
                    count
                } else {
                    SilExpr::new(
                        SilExprKind::Binary {
                            op: SilBinaryOp::Sub,
                            left: Box::new(count),
                            right: Box::new(SilExpr::int(singleton_count as i64)),
                        },
                        span,
                    )
                };
                statements.push(SilStatement::VariableDefinition {
                    type_ref: SilTypeRef { base: SilTypeBase::Int, array_dims: Vec::new() },
                    modifiers: Vec::new(),
                    name: name.clone(),
                    expr: Some(value),
                    span,
                    type_span: span,
                    modifier_spans: Vec::new(),
                    name_span: span,
                });
                statements.push(comparison(SilBinaryOp::Ge, SilExpr::identifier(name.clone()), SilExpr::int(minimum)));
                statements.push(comparison(SilBinaryOp::Le, SilExpr::identifier(name), SilExpr::int(maximum)));
                continue;
            }
            let index = match interaction.location() {
                InteractionLocation::FromStart(index) => SilExpr::int(index as i64),
                InteractionLocation::FromEnd(distance) => SilExpr::new(
                    SilExprKind::Binary {
                        op: SilBinaryOp::Sub,
                        left: Box::new(SilExpr::call("OpAuthOutputCount", vec![active_input()])),
                        right: Box::new(SilExpr::int(distance as i64)),
                    },
                    span,
                ),
                InteractionLocation::Range { .. } => unreachable!("singleton output has no range location"),
            };
            statements.push(SilStatement::VariableDefinition {
                type_ref: SilTypeRef { base: SilTypeBase::Int, array_dims: Vec::new() },
                modifiers: Vec::new(),
                name: hidden_output_idx_name(handle),
                expr: Some(SilExpr::call("OpAuthOutputIdx", vec![active_input(), index])),
                span,
                type_span: span,
                modifier_spans: Vec::new(),
                name_span: span,
            });
        }
        // The coordinated leader authorizes every current-covenant continuation.
        if outputs.coordinates_current_outputs() {
            statements.push(comparison(
                SilBinaryOp::Eq,
                SilExpr::call("OpCovOutputCount", vec![SilExpr::identifier(hidden_cov_id_name())]),
                SilExpr::call("OpAuthOutputCount", vec![active_input()]),
            ));
        }
        Ok(statements)
    }

    /// Authenticate the planned existing-covenant groups before authored statements.
    pub(in crate::compiler::codegen) fn observed_input_prelude(
        &self,
        entry_id: EntryId,
        entry: &EntryDecl,
        input_references: &EntryInputReferencePlan,
    ) -> Result<Vec<SilStatement<'static>>> {
        let entry_model = self.model.entry_model_by_id(entry_id)?;
        let inputs = self.model.input_plan_by_id(entry_id)?;
        let outputs = self.model.entry_output_plan_by_id(entry_id)?;
        let span = SilSpan::default();
        let mut statements = Vec::new();
        for group in entry_model.existing_groups() {
            let observe = group.observe().expect("existing covenant group retains its observe clause");
            let cov_id = hidden_observe_cov_id_name(&observe.name);
            let source = match inputs.observed_covenant_source(group.id())? {
                CovenantIdSource::StateField { field } => {
                    input_references.active().project_field_ast(field.field()).ok_or_else(|| {
                        ArgentError::new(format!(
                            "observed covenant `{}` has no authenticated active field `{}`",
                            observe.name,
                            field.field()
                        ))
                    })?
                }
                CovenantIdSource::EntryArgument { index } => {
                    let argument = entry.params.get(*index).ok_or_else(|| {
                        ArgentError::new(format!("observed covenant `{}` has no entry argument at index {index}", observe.name))
                    })?;
                    SilExpr::identifier(argument.name.clone())
                }
            };
            statements.push(SilStatement::VariableDefinition {
                type_ref: SilTypeRef { base: SilTypeBase::Byte, array_dims: vec![SilArrayDim::Fixed(32)] },
                modifiers: Vec::new(),
                name: cov_id.clone(),
                expr: Some(source),
                span,
                type_span: span,
                modifier_spans: Vec::new(),
                name_span: span,
            });
            for (builtin, count) in [("OpCovInputCount", group.inputs().len()), ("OpCovOutputCount", group.outputs().len())] {
                statements.push(SilStatement::Require {
                    expr: SilExpr::new(
                        SilExprKind::Binary {
                            op: SilBinaryOp::Eq,
                            left: Box::new(SilExpr::call(builtin, vec![SilExpr::identifier(cov_id.clone())])),
                            right: Box::new(SilExpr::int(count as i64)),
                        },
                        span,
                    ),
                    message: None,
                    span,
                    message_span: None,
                });
            }
            let mut materialized_open_bindings = BTreeSet::new();
            for interaction in group.outputs() {
                let InteractionSource::ObserveOutput(output) = interaction.source() else {
                    unreachable!("existing covenant outputs are observations")
                };
                if !observed_is_dynamic_binding(entry_id, observe, output, self.model)? {
                    continue;
                }
                let OutputProofRequirement::BoundObserved(id) = outputs.observed(interaction.id())? else { continue };
                if !materialized_open_bindings.insert(*id) {
                    continue;
                }
                let input = match inputs.reference(*id).map(|reference| &reference.origin) {
                    Some(InputReferenceOrigin::Observed(input)) => *input,
                    _ => return Err(ArgentError::new("dynamic observed output has no bound input")),
                };
                let witness = inputs
                    .observed(input)?
                    .observed_witness
                    .as_ref()
                    .ok_or_else(|| ArgentError::new("dynamic observed input has no planned witness descriptor"))?;
                statements.push(SilStatement::VariableDefinition {
                    type_ref: SilTypeRef { base: SilTypeBase::Byte, array_dims: vec![SilArrayDim::Fixed(32)] },
                    modifiers: Vec::new(),
                    name: output.actor.clone(),
                    expr: Some(SilExpr::identifier(hidden_observed_actor_template_name(witness))),
                    span,
                    type_span: span,
                    modifier_spans: Vec::new(),
                    name_span: span,
                });
            }
            for interaction in group.inputs() {
                let InteractionSource::ObserveInput(input) = interaction.source() else {
                    unreachable!("existing covenant inputs are observed inputs");
                };
                let index = match interaction.location() {
                    InteractionLocation::FromStart(index) => SilExpr::int(index as i64),
                    InteractionLocation::FromEnd(distance) => SilExpr::new(
                        SilExprKind::Binary {
                            op: SilBinaryOp::Sub,
                            left: Box::new(SilExpr::call("OpCovInputCount", vec![SilExpr::identifier(cov_id.clone())])),
                            right: Box::new(SilExpr::int(distance as i64)),
                        },
                        span,
                    ),
                    InteractionLocation::Range { .. } => unreachable!("observed input has no range location"),
                };
                statements.push(SilStatement::VariableDefinition {
                    type_ref: SilTypeRef { base: SilTypeBase::Int, array_dims: Vec::new() },
                    modifiers: Vec::new(),
                    name: hidden_observed_input_idx_name(&observe.name, &input.name),
                    expr: Some(SilExpr::call("OpCovInputIdx", vec![SilExpr::identifier(cov_id.clone()), index])),
                    span,
                    type_span: span,
                    modifier_spans: Vec::new(),
                    name_span: span,
                });
                statements.push(input_references.observed(interaction.id())?.read_statement());
            }
            for interaction in group.outputs() {
                let InteractionSource::ObserveOutput(output) = interaction.source() else {
                    unreachable!("existing covenant outputs are observed outputs");
                };
                let index = match interaction.location() {
                    InteractionLocation::FromStart(index) => SilExpr::int(index as i64),
                    InteractionLocation::FromEnd(distance) => SilExpr::new(
                        SilExprKind::Binary {
                            op: SilBinaryOp::Sub,
                            left: Box::new(SilExpr::call("OpCovOutputCount", vec![SilExpr::identifier(cov_id.clone())])),
                            right: Box::new(SilExpr::int(distance as i64)),
                        },
                        span,
                    ),
                    InteractionLocation::Range { .. } => unreachable!("observed output has no range location"),
                };
                statements.push(SilStatement::VariableDefinition {
                    type_ref: SilTypeRef { base: SilTypeBase::Int, array_dims: Vec::new() },
                    modifiers: Vec::new(),
                    name: hidden_observed_output_idx_name(&observe.name, &output.name),
                    expr: Some(SilExpr::call("OpCovOutputIdx", vec![SilExpr::identifier(cov_id.clone()), index])),
                    span,
                    type_span: span,
                    modifier_spans: Vec::new(),
                    name_span: span,
                });
            }
        }
        Ok(statements)
    }

    /// Lower current-covenant input authentication and group-position rules.
    pub(in crate::compiler::codegen) fn current_input_prelude(
        &self,
        entry_id: EntryId,
        input_references: &EntryInputReferencePlan,
    ) -> Result<Vec<SilStatement<'static>>> {
        let entry_model = self.model.entry_model_by_id(entry_id)?;
        let input_plan = self.model.input_plan_by_id(entry_id)?;
        let span = SilSpan::default();
        let active_input = || SilExpr::new(SilExprKind::Introspection(SilIntrospectionKind::ActiveInputIndex), span);
        let comparison = |op, left: SilExpr<'static>, right: SilExpr<'static>| SilStatement::Require {
            expr: SilExpr::new(SilExprKind::Binary { op, left: Box::new(left), right: Box::new(right) }, span),
            message: None,
            span,
            message_span: None,
        };
        let mut statements = Vec::new();
        if input_plan.current_group_policy() != CurrentInputGroupPolicy::Batchable {
            let cov_id = hidden_cov_id_name();
            statements.push(SilStatement::VariableDefinition {
                type_ref: SilTypeRef { base: SilTypeBase::Byte, array_dims: vec![SilArrayDim::Fixed(32)] },
                modifiers: Vec::new(),
                name: cov_id.clone(),
                expr: Some(SilExpr::call("OpInputCovenantId", vec![active_input()])),
                span,
                type_span: span,
                modifier_spans: Vec::new(),
                name_span: span,
            });
            match input_plan.current_group_policy() {
                CurrentInputGroupPolicy::LeaderRanged => statements.push(comparison(
                    SilBinaryOp::Eq,
                    SilExpr::call("OpCovInputIdx", vec![SilExpr::identifier(cov_id.clone()), SilExpr::int(0)]),
                    active_input(),
                )),
                CurrentInputGroupPolicy::LeaderFixed { count } => {
                    statements.push(comparison(
                        SilBinaryOp::Eq,
                        SilExpr::call("OpCovInputCount", vec![SilExpr::identifier(cov_id.clone())]),
                        SilExpr::int(count as i64),
                    ));
                    if count > 1 {
                        statements.push(comparison(
                            SilBinaryOp::Eq,
                            SilExpr::call("OpCovInputIdx", vec![SilExpr::identifier(cov_id.clone()), SilExpr::int(0)]),
                            active_input(),
                        ));
                    }
                }
                CurrentInputGroupPolicy::Delegate { minimum_count } => {
                    statements.push(comparison(
                        SilBinaryOp::Ge,
                        SilExpr::call("OpCovInputCount", vec![SilExpr::identifier(cov_id.clone())]),
                        SilExpr::int(minimum_count as i64),
                    ));
                    statements.push(comparison(
                        SilBinaryOp::Ne,
                        SilExpr::call("OpCovInputIdx", vec![SilExpr::identifier(cov_id.clone()), SilExpr::int(0)]),
                        active_input(),
                    ));
                }
                CurrentInputGroupPolicy::Batchable => unreachable!("batchable current input has no group check"),
            }
            let slot_offset = input_plan.current_group_policy().slot_offset();
            for interaction in entry_model.current().inputs() {
                if let Some(bounds) = input_plan.consumed_range(interaction.id()) {
                    statements.extend(self.ranged_current_input(interaction, bounds, &cov_id, slot_offset, input_references)?);
                    continue;
                }
                let name = hidden_input_idx_name(interaction.handle());
                let index = match interaction.location() {
                    InteractionLocation::FromStart(index) => SilExpr::int((slot_offset + index) as i64),
                    InteractionLocation::FromEnd(distance) => SilExpr::new(
                        SilExprKind::Binary {
                            op: SilBinaryOp::Sub,
                            left: Box::new(SilExpr::call("OpCovInputCount", vec![SilExpr::identifier(cov_id.clone())])),
                            right: Box::new(SilExpr::int(distance as i64)),
                        },
                        span,
                    ),
                    InteractionLocation::Range { .. } => unreachable!("singleton input has no range location"),
                };
                statements.push(SilStatement::VariableDefinition {
                    type_ref: SilTypeRef { base: SilTypeBase::Int, array_dims: Vec::new() },
                    modifiers: Vec::new(),
                    name,
                    expr: Some(SilExpr::call("OpCovInputIdx", vec![SilExpr::identifier(cov_id.clone()), index])),
                    span,
                    type_span: span,
                    modifier_spans: Vec::new(),
                    name_span: span,
                });
                // A direct read is safe only when the input plan proves the
                // singleton self-actor covenant domain. The reference owns the
                // selected authenticated read builtin.
                statements.push(input_references.consumed(interaction.id())?.read_statement());
            }
        }
        // A zero-minimum leader entry cannot replace a delegate at a consumed
        // group position, even when independent inputs may be batched.
        if input_plan.requires_optional_delegate_guard() {
            let cov_id = hidden_cov_id_name();
            statements.push(SilStatement::VariableDefinition {
                type_ref: SilTypeRef { base: SilTypeBase::Byte, array_dims: vec![SilArrayDim::Fixed(32)] },
                modifiers: Vec::new(),
                name: cov_id.clone(),
                expr: Some(SilExpr::call("OpInputCovenantId", vec![active_input()])),
                span,
                type_span: span,
                modifier_spans: Vec::new(),
                name_span: span,
            });
            statements.push(comparison(
                SilBinaryOp::Eq,
                SilExpr::call("OpCovInputIdx", vec![SilExpr::identifier(cov_id), SilExpr::int(0)]),
                active_input(),
            ));
        }
        Ok(statements)
    }

    /// Authenticate and cache each member of a ranged current-covenant consume.
    fn ranged_current_input(
        &self,
        interaction: &crate::compiler::model::EntryInteraction<'_>,
        (minimum, maximum): (i64, i64),
        cov_id: &str,
        slot_offset: usize,
        input_references: &EntryInputReferencePlan,
    ) -> Result<Vec<SilStatement<'static>>> {
        let InteractionLocation::Range { start, singleton_count } = interaction.location() else {
            unreachable!("ranged current input has a ranged location");
        };
        let span = SilSpan::default();
        let excluded = slot_offset + singleton_count;
        let count = hidden_input_count_name(interaction.handle());
        let group_count = SilExpr::call("OpCovInputCount", vec![SilExpr::identifier(cov_id)]);
        let count_value = if excluded == 0 {
            group_count
        } else {
            SilExpr::new(
                SilExprKind::Binary {
                    op: SilBinaryOp::Sub,
                    left: Box::new(group_count),
                    right: Box::new(SilExpr::int(excluded as i64)),
                },
                span,
            )
        };
        let bound = |op, value| SilStatement::Require {
            expr: SilExpr::new(
                SilExprKind::Binary { op, left: Box::new(SilExpr::identifier(count.clone())), right: Box::new(SilExpr::int(value)) },
                span,
            ),
            message: None,
            span,
            message_span: None,
        };
        let mut checks = vec![
            SilStatement::VariableDefinition {
                type_ref: SilTypeRef { base: SilTypeBase::Int, array_dims: Vec::new() },
                modifiers: Vec::new(),
                name: count.clone(),
                expr: Some(count_value),
                span,
                type_span: span,
                modifier_spans: Vec::new(),
                name_span: span,
            },
            bound(SilBinaryOp::Ge, minimum),
            bound(SilBinaryOp::Le, maximum),
        ];
        let input_reference = input_references.consumed(interaction.id())?;
        checks.extend(input_reference.range_cache_declarations(interaction.handle()));
        let position = hidden_input_position_name(interaction.handle());
        let first_cov_index = slot_offset + start;
        let cov_index = if first_cov_index == 0 {
            SilExpr::identifier(position.clone())
        } else {
            SilExpr::new(
                SilExprKind::Binary {
                    op: SilBinaryOp::Add,
                    left: Box::new(SilExpr::int(first_cov_index as i64)),
                    right: Box::new(SilExpr::identifier(position.clone())),
                },
                span,
            )
        };
        let mut body = vec![SilStatement::VariableDefinition {
            type_ref: SilTypeRef { base: SilTypeBase::Int, array_dims: Vec::new() },
            modifiers: Vec::new(),
            name: hidden_input_idx_name(interaction.handle()),
            expr: Some(SilExpr::call("OpCovInputIdx", vec![SilExpr::identifier(cov_id), cov_index])),
            span,
            type_span: span,
            modifier_spans: Vec::new(),
            name_span: span,
        }];
        body.push(input_reference.read_statement());
        body.extend(input_reference.range_cache_append(interaction.handle())?);
        checks.push(SilStatement::For {
            ident: position,
            start: SilExpr::int(0),
            end: SilExpr::identifier(count),
            max_iterations: SilExpr::int(maximum),
            body,
            span,
            ident_span: span,
            body_span: span,
        });
        Ok(checks)
    }

    /// Authenticate each declared genesis group from transaction-derived output data.
    ///
    /// Scripts cannot enumerate genesis outputs authorized by an input. The
    /// witnessed global output indices select candidates, while the active
    /// outpoint and each candidate's value and script bytes come from the
    /// transaction. The preimage commits to the declared count and indices in
    /// source order, using the version-0 P2SH script shape checked elsewhere.
    /// Consensus derives a genesis covenant ID from the complete output group
    /// ordered by global index. Matching the reconstructed ID against one
    /// selected member proves the witnessed sequence is that complete group
    /// under hash collision resistance. Strictly increasing first indices
    /// keep separate spawn clauses on distinct groups in declaration order.
    pub(in crate::compiler::codegen) fn spawn_prelude(&self, entry_id: EntryId) -> Result<Vec<SilStatement<'static>>> {
        let span = SilSpan::default();
        let active_input = || SilExpr::new(SilExprKind::Introspection(SilIntrospectionKind::ActiveInputIndex), span);
        let as_byte8 = |value| SilExpr::call("__as_cast_byte[8]", vec![value]);
        let slice4 = |source| {
            SilExpr::new(
                SilExprKind::Slice {
                    source: Box::new(source),
                    start: Box::new(SilExpr::int(0)),
                    end: Box::new(SilExpr::int(4)),
                    span,
                },
                span,
            )
        };
        let mut statements = Vec::new();
        let mut previous_first_output_idx = None;
        for group in self.model.entry_model_by_id(entry_id)?.genesis_groups() {
            let spawn = group.spawn().expect("genesis group has a spawn declaration");
            let outputs = group.outputs();
            let preimage = hidden_spawn_preimage_name(&spawn.name);
            let mut parts = vec![
                SilExpr::call("OpOutpointTxId", vec![active_input()]),
                slice4(as_byte8(SilExpr::call("OpOutpointIndex", vec![active_input()]))),
                as_byte8(SilExpr::int(outputs.len() as i64)),
            ];
            for output in outputs {
                let output_idx = hidden_spawn_output_idx_name(&spawn.name, output.handle());
                parts.push(slice4(as_byte8(SilExpr::identifier(output_idx.clone()))));
                parts.push(as_byte8(SilExpr::new(
                    SilExprKind::IndexedIntrospection {
                        kind: SilIndexedIntrospectionKind::OutputValue,
                        index: Box::new(SilExpr::identifier(output_idx.clone())),
                        field_span: span,
                    },
                    span,
                )));
                parts.push(SilExpr::bytes(P2SH_SPK_VERSION.to_vec()));
                parts.push(as_byte8(SilExpr::int(P2SH_SCRIPT_LEN as i64)));
                parts.push(SilExpr::call(
                    "OpTxOutputSpkSubstr",
                    vec![
                        SilExpr::identifier(output_idx),
                        SilExpr::int(SPK_VERSION_LEN as i64),
                        SilExpr::int((SPK_VERSION_LEN + P2SH_SCRIPT_LEN) as i64),
                    ],
                ));
            }
            let mut parts = parts.into_iter();
            let first = parts.next().expect("spawn preimage starts with its active input outpoint");
            let preimage_value = parts.fold(first, |left, right| {
                SilExpr::new(SilExprKind::Binary { op: SilBinaryOp::Add, left: Box::new(left), right: Box::new(right) }, span)
            });
            statements.push(SilStatement::VariableDefinition {
                type_ref: SilTypeRef { base: SilTypeBase::Byte, array_dims: vec![SilArrayDim::Dynamic] },
                modifiers: Vec::new(),
                name: preimage.clone(),
                expr: Some(preimage_value),
                span,
                type_span: span,
                modifier_spans: Vec::new(),
                name_span: span,
            });
            statements.push(SilStatement::VariableDefinition {
                type_ref: SilTypeRef { base: SilTypeBase::Byte, array_dims: vec![SilArrayDim::Fixed(32)] },
                modifiers: Vec::new(),
                name: spawn.covenant.clone(),
                expr: Some(SilExpr::call(
                    "blake2bWithKey",
                    vec![SilExpr::identifier(preimage), SilExpr::call("byte[]", vec![SilExpr::string("CovenantID")])],
                )),
                span,
                type_span: span,
                modifier_spans: Vec::new(),
                name_span: span,
            });
            let first_output = outputs.first().expect("spawn outputs checked during model validation");
            let first_output_idx = hidden_spawn_output_idx_name(&spawn.name, first_output.handle());
            if let Some(previous_first_output_idx) = &previous_first_output_idx {
                statements.push(SilStatement::Require {
                    expr: SilExpr::new(
                        SilExprKind::Binary {
                            op: SilBinaryOp::Lt,
                            left: Box::new(SilExpr::identifier(previous_first_output_idx)),
                            right: Box::new(SilExpr::identifier(first_output_idx.clone())),
                        },
                        span,
                    ),
                    message: None,
                    span,
                    message_span: None,
                });
            }
            // Consensus commits to the complete group. Matching its first member proves
            // the witnessed sequence covers the group under hash collision resistance.
            statements.push(SilStatement::Require {
                expr: SilExpr::new(
                    SilExprKind::Binary {
                        op: SilBinaryOp::Eq,
                        left: Box::new(SilExpr::call("OpOutputCovenantId", vec![SilExpr::identifier(first_output_idx.clone())])),
                        right: Box::new(SilExpr::identifier(spawn.covenant.clone())),
                    },
                    span,
                ),
                message: None,
                span,
                message_span: None,
            });
            previous_first_output_idx = Some(first_output_idx);
        }
        Ok(statements)
    }

    /// Generate the checked range index function when an entry needs it.
    pub(in crate::compiler::codegen) fn checked_range_index_helper(&self) -> Result<Option<FunctionAst<'static>>> {
        let mut uses_ranges = false;
        for (index, _) in self.actor.entries.iter().enumerate() {
            uses_ranges = self.model.input_plan_by_id(EntryId { actor: self.actor_id, index })?.requires_checked_range_index();
            if uses_ranges {
                break;
            }
        }
        if !uses_ranges {
            return Ok(None);
        }

        let index = hidden_range_index_arg_name();
        let count = hidden_range_count_arg_name();
        let span = SilSpan::default();
        let int_type = SilTypeRef { base: SilTypeBase::Int, array_dims: Vec::new() };
        let param =
            |name: &str| ParamAst { type_ref: int_type.clone(), name: name.to_string(), span, type_span: span, name_span: span };
        let binary = |op, left: SilExpr<'static>, right: SilExpr<'static>| {
            SilExpr::new(SilExprKind::Binary { op, left: Box::new(left), right: Box::new(right) }, span)
        };
        Ok(Some(FunctionAst {
            name: hidden_checked_range_index_name(),
            attributes: Vec::new(),
            params: vec![param(&index), param(&count)],
            entrypoint: false,
            return_types: vec![int_type],
            returns_tuple: false,
            body: vec![
                SilStatement::Require {
                    expr: binary(SilBinaryOp::Ge, SilExpr::identifier(index.as_str()), SilExpr::int(0)),
                    message: None,
                    span,
                    message_span: None,
                },
                SilStatement::Require {
                    expr: binary(SilBinaryOp::Lt, SilExpr::identifier(index.as_str()), SilExpr::identifier(count.as_str())),
                    message: None,
                    span,
                    message_span: None,
                },
                SilStatement::Return { exprs: vec![SilExpr::identifier(index)], span },
            ],
            return_type_spans: vec![span],
            span,
            name_span: span,
            body_span: span,
        }))
    }
}

pub(in crate::compiler::codegen) fn lower_entry_body<'src>(
    entry_id: EntryId,
    actor: &'src ActorDecl,
    entry: &'src EntryDecl,
    model: &AppCompilationContext<'src>,
    input_references: &EntryInputReferencePlan,
    state_values: &StateValueTypes,
) -> Result<LoweredEntryBody<'src>> {
    BodyLowerer::new(entry_id, actor, entry, model, input_references, state_values)?.lower()
}

/// Source details needed while lowering one non-current covenant output.
#[derive(Clone, Copy)]
enum CovenantOutputContext<'a> {
    Existing { observe: &'a ObserveDecl, output: &'a ObservedActorDecl },
    Genesis { spawn: &'a SpawnDecl, output: &'a SpawnOutputDecl },
}

impl<'a> CovenantOutputContext<'a> {
    fn group_name(self) -> &'a str {
        match self {
            Self::Existing { observe, .. } => &observe.name,
            Self::Genesis { spawn, .. } => &spawn.name,
        }
    }

    fn output_name(self) -> &'a str {
        match self {
            Self::Existing { output, .. } => &output.name,
            Self::Genesis { output, .. } => &output.name,
        }
    }
}

struct BodyLowerer<'a, 'm, 'p> {
    entry_id: EntryId,
    actor: &'a ActorDecl,
    entry: &'a EntryDecl,
    model: &'m AppCompilationContext<'a>,
    input_references: &'p EntryInputReferencePlan,
    active_reference: PlannedEntryInputReference,
    state_values: &'p StateValueTypes<'m>,
    bindings: BodyBindings,
    output_plan: &'m EntryOutputPlan,
    output_values: BTreeMap<InteractionId, String>,
    observed_output_fields: Vec<ObservedOutputFieldWitnessSpec>,
    // Expression lowering records contract-level helpers without making the
    // otherwise read-only lowering API mutable.
    digest_helpers: RefCell<BTreeSet<SourceStateId>>,
}

struct EntryInputStateAstLowerer<'n, 'm, 'p, 'src> {
    actor: &'src ActorDecl,
    entry_id: EntryId,
    names: &'n SilNames<'m, 'src>,
    references: &'p EntryInputReferencePlan,
    model: &'m AppCompilationContext<'src>,
    state_values: &'p StateValueTypes<'m>,
    output_plan: &'m EntryOutputPlan,
    output_values: &'m BTreeMap<InteractionId, String>,
    bindings: &'m BodyBindings,
    active_reference: &'m PlannedEntryInputReference,
    observed_output_fields: &'m [ObservedOutputFieldWitnessSpec],
    digest_helpers: &'m RefCell<BTreeSet<SourceStateId>>,
    error: Option<ArgentError>,
}

impl EntryInputStateAstLowerer<'_, '_, '_, '_> {
    fn lower_state_name(&mut self, name: &mut String, span: SilSpan<'_>) {
        match self.names.state_type_source(span) {
            Ok(Some(source)) => {
                if let Some(authored) = self.state_values.authored_sil_type(&source) {
                    *name = authored.to_string();
                } else {
                    self.error.get_or_insert_with(|| {
                        ArgentError::new(format!("bound state `{}` has no authored Sil representation", source.as_str()))
                    });
                }
            }
            Ok(None) => {
                let bound = self.names.type_name(name, span);
                if span.as_str().is_empty() || self.names.is_builtin(span) {
                    *name = bound;
                    return;
                }
                self.error.get_or_insert_with(|| ArgentError::new(format!("state type `{bound}` has no bound source identity")));
            }
            Err(err) => {
                self.error.get_or_insert(err);
            }
        }
    }

    fn lower_type(&mut self, type_ref: &mut SilTypeRef, span: SilSpan<'_>) {
        self.names.lower_type_dimensions(type_ref, span);
        if self.names.is_actor_handle_type(span) {
            type_ref.base = SilTypeBase::Byte;
            type_ref.array_dims.insert(0, silverscript_lang::ast::ArrayDim::Fixed(32));
            return;
        }
        if let SilTypeBase::Custom(name) = &mut type_ref.base {
            let bound = self.names.type_name(name, span);
            if bound == word::COVENANT_ID {
                type_ref.base = SilTypeBase::Byte;
                type_ref.array_dims = vec![silverscript_lang::ast::ArrayDim::Fixed(32)];
                return;
            }
            if matches!(self.names.type_target(span), Some(id) if id.kind() == SymbolKind::ActorEnum) {
                type_ref.base = SilTypeBase::Int;
                return;
            }
            self.lower_state_name(name, span);
        }
    }

    fn checked_range_index<'a>(&self, index: SilExpr<'a>, range: &RangeValueLocation) -> SilExpr<'a> {
        let literal = match &index.kind {
            SilExprKind::Int(value) => Some(*value),
            _ => None,
        };
        if literal.is_some_and(|value| value < range.minimum) {
            index
        } else {
            SilExpr::call(hidden_checked_range_index_name(), vec![index, SilExpr::identifier(range.count.clone())])
        }
    }
}

impl<'ast> AstVisitorMut<'ast> for EntryInputStateAstLowerer<'_, '_, '_, '_> {
    fn visit_statement(&mut self, statement: &mut SilStatement<'ast>) {
        match statement {
            SilStatement::VariableDefinition { type_ref, type_span, .. } => self.lower_type(type_ref, *type_span),
            SilStatement::TupleAssignment { left_type_ref, left_type_span, right_type_ref, right_type_span, .. } => {
                self.lower_type(left_type_ref, *left_type_span);
                self.lower_type(right_type_ref, *right_type_span);
            }
            SilStatement::FunctionCallAssign { bindings, .. } => {
                for binding in bindings {
                    self.lower_type(&mut binding.type_ref, binding.type_span);
                }
            }
            SilStatement::StateFunctionCallAssign { target_struct, target_struct_span, bindings, .. } => {
                self.lower_state_name(target_struct, *target_struct_span);
                for binding in bindings {
                    self.lower_type(&mut binding.type_ref, binding.type_span);
                }
            }
            SilStatement::StructDestructure { struct_name, struct_name_span, bindings, expr, .. } => {
                if let SilExprKind::Identifier(field_name) = &expr.kind
                    && matches!(self.names.binding(expr.span), Some(Binding::ActorField))
                    && let Ok(state) = self.model.state(&self.actor.state)
                    && let Some(expansion) = &state.expansion
                    && let Some(digest) = expansion.digests.iter().find(|digest| digest.field == *field_name)
                {
                    let example = self.model.state(&digest.state).ok().and_then(|state| state.fields.first()).map_or_else(
                        || "; project its fields directly".to_string(),
                        |field| {
                            format!(
                                "; project its fields directly, for example `{} opened_{} = {field_name}.{};`",
                                source_type_ref(&field.ty),
                                field.name,
                                field.name
                            )
                        },
                    );
                    self.error = Some(ArgentError::new(format!(
                        "active expanded state field `{field_name}` cannot be destructured as a whole value{example}"
                    )));
                    return;
                }
                self.lower_state_name(struct_name, *struct_name_span);
                for binding in bindings {
                    self.lower_type(&mut binding.type_ref, binding.type_span);
                }
            }
            _ => {}
        }
        walk_statement_mut(self, statement);
    }

    fn visit_expr(&mut self, expr: &mut SilExpr<'ast>) {
        if let SilExprKind::Call { name_span, args, .. } = &expr.kind {
            let callable = match self.names.binding(*name_span) {
                Some(Binding::Source(ResolvedName::Declaration(id))) if id.kind() == SymbolKind::Function => {
                    Some(CallableId { owner: *id, member: None })
                }
                Some(Binding::ActorHelper(member)) => Some(CallableId { owner: self.entry_id.actor, member: Some(*member) }),
                _ => None,
            };
            if let Some(signature) = callable.and_then(|id| self.state_values.signature_id(id)) {
                for (index, arg) in args.iter().enumerate() {
                    if signature.param(index).is_some()
                        && let SilExprKind::Identifier(handle) = &arg.kind
                        && self.references.reference_ast(arg).is_some()
                    {
                        self.error = Some(ArgentError::new(format!(
                            "input reference `{handle}` is not an authored state value; use `state({handle})`"
                        )));
                        return;
                    }
                }
            }
        }
        if let SilExprKind::Call { name, args, .. } = &expr.kind
            && name == word::DIGEST
            && args.len() != 1
        {
            self.error = Some(ArgentError::new("`digest(...)` requires exactly one authored state value"));
            return;
        }
        if let SilExprKind::Call { name, args, .. } = &expr.kind
            && name == word::STATE
            && args.len() != 1
        {
            self.error = Some(ArgentError::new("`state(...)` requires exactly one input reference"));
            return;
        }
        match &mut expr.kind {
            SilExprKind::Array { type_ref, type_span, .. } => self.lower_type(type_ref, *type_span),
            SilExprKind::StructLiteral { name, name_span, fields } => {
                if fields.iter().any(|field| field.name.is_empty()) {
                    self.error = Some(ArgentError::new("state constructor component requires a named field"));
                    return;
                }
                let source = match self.names.state_type_source(*name_span) {
                    Ok(source) => source,
                    Err(err) => {
                        self.error.get_or_insert(err);
                        return;
                    }
                };
                let bound = self.names.type_name(name, *name_span);
                let storage = match source.as_ref().map(|source| self.model.storage_state_by_source(source)).transpose() {
                    Ok(storage) => storage,
                    Err(err) => {
                        self.error.get_or_insert(err);
                        return;
                    }
                };
                if let Some(storage) = storage {
                    let expansion = source
                        .as_ref()
                        .and_then(|source| self.model.state_by_source(source).ok())
                        .and_then(|state| state.expansion.as_ref());
                    if let Some(expansion) = expansion {
                        for digest in &expansion.digests {
                            if fields.iter().any(|field| {
                                field.name == digest.field
                                    && matches!(&field.expr.kind, SilExprKind::StructLiteral { name, .. } if name.is_empty())
                            }) {
                                self.error = Some(ArgentError::new(format!(
                                    "state `{}` constructor slot `{}` must use `{} {{ ... }}`",
                                    bound, digest.field, digest.state
                                )));
                                return;
                            }
                        }
                    }
                    for field in &storage.fields {
                        if field.virtual_slot && !fields.iter().any(|candidate| candidate.name == field.name) {
                            let field_id = source.as_ref().map(|source| SourceFieldId::new(source.clone(), &field.name));
                            let mut matches =
                                self.observed_output_fields.iter().filter(|spec| field_id.as_ref() == Some(&spec.field_id));
                            if let (Some(spec), None) = (matches.next(), matches.next()) {
                                let span = SilSpan::default();
                                fields.push(SilStateFieldExpr {
                                    name: field.name.clone(),
                                    expr: SilExpr::identifier(hidden_observed_output_field_name(spec)),
                                    span,
                                    name_span: span,
                                });
                            }
                        }
                    }
                    fields.sort_by_key(|field| storage.fields.iter().position(|candidate| candidate.name == field.name));
                }
                self.lower_state_name(name, *name_span);
            }
            SilExprKind::New { name, name_span, .. } => {
                self.lower_state_name(name, *name_span);
            }
            _ => {}
        }
        if let SilExprKind::FieldAccess { source, .. } = &mut expr.kind
            && let SilExprKind::ArrayIndex { source: array, index } = &mut source.kind
            && let SilExprKind::Identifier(_) = &array.kind
            && (matches!(self.bindings.input_value(self.names.binding(array.span)), Some(InputValueLocation::Range { .. }))
                || self.bindings.output_range(self.names.binding(array.span)).is_some())
        {
            self.visit_expr(index);
        }
        if let SilExprKind::Call { name, args, .. } = &mut expr.kind
            && name == word::STATE
            && let [reference] = args.as_mut_slice()
            && let SilExprKind::ArrayIndex { source, index } = &mut reference.kind
            && let SilExprKind::Identifier(_) = &source.kind
            && matches!(self.bindings.input_value(self.names.binding(source.span)), Some(InputValueLocation::Range { .. }))
        {
            self.visit_expr(index);
        }
        if let SilExprKind::Call { name, args, .. } = &expr.kind
            && name == word::DIGEST
            && let [state] = args.as_slice()
            && let SilExprKind::Call { name: state_name, args: state_args, .. } = &state.kind
            && state_name == word::STATE
            && let [reference] = state_args.as_slice()
            && let Some(planned) = self.references.reference_ast(reference)
            && let Some(lowered) = planned.authored_payload_digest_ast(self.model)
        {
            *expr = lowered;
            return;
        }
        if let SilExprKind::Call { name, args, .. } = &expr.kind
            && name == word::DIGEST
            && let [value] = args.as_slice()
        {
            if let SilExprKind::FieldAccess { source, field, .. } = &value.kind
                && field == word::STATE
                && let Some(reference) = self.references.reference_ast(source)
            {
                self.error = Some(ArgentError::new(format!(
                    "input reference `{}` has no `.state` member; use `{}({})` for complete authored state or project a field directly",
                    reference.reference(),
                    word::STATE,
                    reference.reference()
                )));
                return;
            }
            let Some(planned) = self.names.digest_operand(value.span).cloned() else {
                self.error = Some(ArgentError::new(format!(
                    "`{}(...)` requires a proven authored state value, but `{}` has no known source type",
                    word::DIGEST,
                    value.span.as_str().trim()
                )));
                return;
            };
            let mut value = value.clone();
            self.visit_expr(&mut value);
            if matches!(value.kind, SilExprKind::Identifier(_)) {
                if let Ok(lowering) = self.model.state_lowering_by_id(self.entry_id.actor)
                    && let Some(lowered) = authored_state_payload_digest_ast(planned.source(), value, lowering, self.model)
                {
                    *expr = lowered;
                    return;
                }
            } else {
                self.digest_helpers.borrow_mut().insert(planned.source().clone());
                *expr = SilExpr::call(self.state_values.digest_helper_name(planned.source()), vec![value]);
                return;
            }
            self.error = Some(ArgentError::new(format!("cannot lower authored state digest for `{}`", planned.source().as_str())));
            return;
        }
        if let SilExprKind::Call { name, args, .. } = &expr.kind
            && name == word::STATE
            && let [reference] = args.as_slice()
            && let SilExprKind::ArrayIndex { source, index } = &reference.kind
            && let SilExprKind::Identifier(handle) = &source.kind
            && let Some(InputValueLocation::Range { range, .. }) = self.bindings.input_value(self.names.binding(source.span))
            && let Some(planned) = self.references.reference_ast(source)
        {
            let checked = self.checked_range_index((**index).clone(), range);
            if let Some(lowered) = planned.complete_range_item_ast(handle, checked) {
                *expr = lowered;
            } else {
                self.error = Some(planned.unavailable_authored_state());
            }
            return;
        }
        if let SilExprKind::Call { name, args, .. } = &expr.kind
            && name == word::STATE
            && let [reference] = args.as_slice()
            && let Some(planned) = self.references.reference_ast(reference)
            && let Some(lowered) = planned.complete_authored_ast()
        {
            *expr = lowered;
            return;
        }
        if let SilExprKind::Call { name, args, .. } = &expr.kind
            && name == word::STATE
            && let [reference] = args.as_slice()
            && let Some(planned) = self.references.reference_ast(reference)
        {
            self.error = Some(planned.unavailable_authored_state());
            return;
        }
        if let SilExprKind::Call { name, args, .. } = &expr.kind
            && name == word::STATE
            && let [reference] = args.as_slice()
        {
            self.error = Some(ArgentError::new(format!(
                "`state(...)` requires one visible entry input reference, but `{}` is not one",
                reference.span.as_str().trim()
            )));
            return;
        }
        if let SilExprKind::FieldAccess { source, field, .. } = &expr.kind
            && field == word::STATE
            && let Some(reference) = self.references.reference_ast(source)
        {
            self.error = Some(ArgentError::new(format!(
                "input reference `{}` has no `.state` member; use `state({})` for complete authored state",
                reference.reference(),
                reference.reference()
            )));
            return;
        }
        if let SilExprKind::FieldAccess { source, field, .. } = &expr.kind {
            let active_field = self.names.actor_field(source.span).map(crate::compiler::model::SourceFieldId::field);
            if let Some(lowered) = active_field.and_then(|name| self.active_reference.expanded_field_ast(name, field)) {
                *expr = lowered;
                return;
            }
        }
        if let SilExprKind::FieldAccess { source, field, .. } = &expr.kind
            && matches!(&source.kind, SilExprKind::Identifier(name) if name == word::SELF)
            && let Some(lowered) = self.active_reference.project_field_ast(field)
        {
            *expr = lowered;
            return;
        }
        if let SilExprKind::Identifier(_) = &expr.kind
            && let Some(field) = self.names.actor_field(expr.span)
            && let Some(lowered) = self.active_reference.project_field_ast(field.field())
        {
            *expr = lowered;
            return;
        }
        if let SilExprKind::UnarySuffix { source, kind: SilUnarySuffixKind::Length, .. } = &expr.kind
            && let SilExprKind::Identifier(_) = &source.kind
            && let Some(InputValueLocation::Range { range, .. }) = self.bindings.input_value(self.names.binding(source.span))
        {
            *expr = SilExpr::identifier(range.count.clone());
            return;
        }
        if let SilExprKind::UnarySuffix { source, kind: SilUnarySuffixKind::Length, .. } = &expr.kind
            && let SilExprKind::Identifier(_) = &source.kind
            && let Some(range) = self.bindings.output_range(self.names.binding(source.span))
        {
            *expr = SilExpr::identifier(range.count.clone());
            return;
        }
        if let SilExprKind::FieldAccess { source, field, .. } = &expr.kind
            && let SilExprKind::ArrayIndex { source: array, index } = &source.kind
            && let SilExprKind::Identifier(handle) = &array.kind
            && let Some(InputValueLocation::Range { covenant_id, range }) = self.bindings.input_value(self.names.binding(array.span))
            && let Some(reference) = self.references.reference_ast(array)
        {
            let span = SilSpan::default();
            let checked = self.checked_range_index((**index).clone(), range);
            let index_value = if range.first_index == 0 {
                checked.clone()
            } else {
                SilExpr::new(
                    SilExprKind::Binary {
                        op: SilBinaryOp::Add,
                        left: Box::new(SilExpr::int(range.first_index as i64)),
                        right: Box::new(checked.clone()),
                    },
                    span,
                )
            };
            let input_index = SilExpr::call("OpCovInputIdx", vec![SilExpr::identifier(covenant_id), index_value]);
            let lowered = match field.as_str() {
                word::VALUE => SilExpr::new(
                    SilExprKind::IndexedIntrospection {
                        kind: SilIndexedIntrospectionKind::InputValue,
                        index: Box::new(input_index),
                        field_span: span,
                    },
                    span,
                ),
                word::COVENANT_ID => SilExpr::call("OpInputCovenantId", vec![input_index]),
                field => match reference.range_field_ast(handle, checked, field) {
                    Some(value) => value,
                    None => {
                        self.error = Some(reference.unavailable_field(field));
                        return;
                    }
                },
            };
            *expr = lowered;
            return;
        }
        if let SilExprKind::FieldAccess { source, field, .. } = &expr.kind
            && field == word::VALUE
            && let SilExprKind::ArrayIndex { source: array, index } = &source.kind
            && matches!(&array.kind, SilExprKind::Identifier(_))
            && let Some(range) = self.bindings.output_range(self.names.binding(array.span))
        {
            let span = SilSpan::default();
            let checked = self.checked_range_index((**index).clone(), range);
            let offset = if range.first_index == 0 {
                checked
            } else {
                SilExpr::new(
                    SilExprKind::Binary {
                        op: SilBinaryOp::Add,
                        left: Box::new(SilExpr::int(range.first_index as i64)),
                        right: Box::new(checked),
                    },
                    span,
                )
            };
            let output_index = SilExpr::call(
                "OpAuthOutputIdx",
                vec![SilExpr::new(SilExprKind::Introspection(SilIntrospectionKind::ActiveInputIndex), span), offset],
            );
            *expr = SilExpr::new(
                SilExprKind::IndexedIntrospection {
                    kind: SilIndexedIntrospectionKind::OutputValue,
                    index: Box::new(output_index),
                    field_span: span,
                },
                span,
            );
            return;
        }
        if let SilExprKind::FieldAccess { field, .. } = &expr.kind
            && field == word::VALUE
        {
            let output_index = self.output_plan.value_use(expr.span).and_then(|interaction| self.output_values.get(&interaction));
            if let Some(output_index) = output_index {
                let span = SilSpan::default();
                *expr = SilExpr::new(
                    SilExprKind::IndexedIntrospection {
                        kind: SilIndexedIntrospectionKind::OutputValue,
                        index: Box::new(SilExpr::identifier(output_index.clone())),
                        field_span: span,
                    },
                    span,
                );
                return;
            }
        }
        if let SilExprKind::FieldAccess { source, field, .. } = &expr.kind
            && field == word::VALUE
            && let Some(planned) = self.references.reference_ast(source)
        {
            *expr = planned.native_value_ast();
            return;
        }
        if let SilExprKind::FieldAccess { source, field, .. } = &expr.kind
            && field == word::COVENANT_ID
            && let Some(planned) = self.references.reference_ast(source)
        {
            *expr = planned.covenant_id_ast();
            return;
        }
        if let SilExprKind::FieldAccess { source, field, .. } = &expr.kind
            && let Some(planned) = self.references.reference_ast(source)
            && let Some(lowered) = planned.project_field_ast(field)
        {
            *expr = lowered;
            return;
        }
        if let SilExprKind::ArrayIndex { source, .. } = &expr.kind
            && let SilExprKind::Identifier(_) = &source.kind
            && matches!(self.bindings.input_value(self.names.binding(source.span)), Some(InputValueLocation::Range { .. }))
        {
            self.error = Some(ArgentError::new(format!(
                "ranged input item `{}` is an input reference; use `state({})` for an authored state value",
                expr.span.as_str().trim(),
                expr.span.as_str().trim()
            )));
            return;
        }
        walk_expr_mut(self, expr);
    }
}

/// Generated transaction positions and resolved bounds for one ranged handle.
#[derive(Clone, Debug)]
struct RangeValueLocation {
    first_index: usize,
    count: String,
    minimum: i64,
}

#[derive(Clone, Debug)]
enum InputValueLocation {
    Singleton,
    Range { covenant_id: String, range: RangeValueLocation },
}

/// Materialized transaction locations for one entry.
struct BodyBindings {
    input_values: BTreeMap<LocalId, InputValueLocation>,
    output_ranges: BTreeMap<LocalId, RangeValueLocation>,
}

impl BodyBindings {
    fn new() -> Self {
        Self { input_values: BTreeMap::new(), output_ranges: BTreeMap::new() }
    }

    fn input_value(&self, binding: Option<&Binding>) -> Option<&InputValueLocation> {
        match binding {
            Some(Binding::Local(id)) => self.input_values.get(id),
            _ => None,
        }
    }

    fn output_range(&self, binding: Option<&Binding>) -> Option<&RangeValueLocation> {
        match binding {
            Some(Binding::Local(id)) => self.output_ranges.get(id),
            _ => None,
        }
    }

    fn attach_output_range(&mut self, id: LocalId, output_range: RangeValueLocation) {
        self.output_ranges.insert(id, output_range);
    }
}

impl<'a, 'm, 'p> BodyLowerer<'a, 'm, 'p> {
    fn new(
        entry_id: EntryId,
        actor: &'a ActorDecl,
        entry: &'a EntryDecl,
        model: &'m AppCompilationContext<'a>,
        input_references: &'p EntryInputReferencePlan,
        state_values: &'p StateValueTypes<'m>,
    ) -> Result<Self> {
        let entry_model = model.entry_model_by_id(entry_id)?;
        let input_plan = model.input_plan_by_id(entry_id)?;
        let output_plan = model.entry_output_plan_by_id(entry_id)?;
        let active_reference = input_references.active().clone();
        let mut bindings = BodyBindings::new();
        let declaration_bindings = model.resolution.bindings(entry_model.id.actor);

        for interaction in entry_model.current().inputs() {
            input_references.consumed(interaction.id())?;
            let input_value = if let Some((minimum, _)) = input_plan.consumed_range(interaction.id()) {
                let InteractionLocation::Range { start, .. } = interaction.location() else {
                    unreachable!("ranged interaction retains a ranged location");
                };
                let slot_offset = input_plan.current_group_policy().slot_offset();
                InputValueLocation::Range {
                    covenant_id: hidden_cov_id_name(),
                    range: RangeValueLocation {
                        first_index: slot_offset + start,
                        count: hidden_input_count_name(interaction.handle()),
                        minimum,
                    },
                }
            } else {
                InputValueLocation::Singleton
            };
            let crate::compiler::model::InteractionId::CurrentInput(input) = interaction.id() else {
                unreachable!("current input has a consume identity")
            };
            let id = *declaration_bindings
                .entry_consumes
                .get(&(entry_model.id.index, input))
                .ok_or_else(|| ArgentError::new("consumed input has no bound local identity"))?;
            bindings.input_values.insert(id, input_value);
        }

        let mut output_values = BTreeMap::new();
        for interaction in entry_model.current().outputs() {
            if let Some((minimum, _, _)) = output_plan.current_output_range(interaction.id()) {
                let InteractionLocation::Range { start, .. } = interaction.location() else {
                    unreachable!("ranged output retains a ranged location");
                };
                let crate::compiler::model::InteractionId::CurrentOutput(output) = interaction.id() else {
                    unreachable!("current output has an emit identity")
                };
                let id = *declaration_bindings
                    .entry_emits
                    .get(&(entry_model.id.index, output))
                    .ok_or_else(|| ArgentError::new("ranged output has no bound local identity"))?;
                bindings.attach_output_range(
                    id,
                    RangeValueLocation { first_index: start, count: hidden_output_count_name(interaction.handle()), minimum },
                );
            } else {
                let output_index = hidden_output_idx_name(interaction.handle());
                output_values.insert(interaction.id(), output_index);
            }
        }
        // This entry creates spawned outputs, so it owns their value policy.
        // Observed outputs remain the responsibility of their emitting contracts.
        for group in entry_model.genesis_groups() {
            let spawn = group.spawn().expect("genesis group retains its spawn declaration");
            for interaction in group.outputs() {
                let output_index = hidden_spawn_output_idx_name(&spawn.name, interaction.handle());
                output_values.insert(interaction.id(), output_index);
            }
        }

        let observed_output_fields = model.witness_plan_by_id(entry_id)?.observed_output_fields.clone();

        Ok(Self {
            entry_id,
            actor,
            entry,
            model,
            input_references,
            active_reference,
            state_values,
            bindings,
            output_plan,
            output_values,
            observed_output_fields,
            digest_helpers: RefCell::new(BTreeSet::new()),
        })
    }

    fn lower(self) -> Result<LoweredEntryBody<'a>> {
        let entry_id = self.entry_id;
        let owner = entry_id.actor;
        let mut names = SilNames::new(self.model, owner, RootSlot::Entry(entry_id.index));
        let retained_body = self.model.resolution.entry_body(entry_id)?;
        let body_cursor = SourceNodeCursor::new(entry_id.actor, RootSlot::Entry(entry_id.index)).child(ChildEdge::Body);
        let mut authored_statements = self
            .direct_authored_statements(retained_body, &body_cursor, true, &mut BTreeSet::new())
            .ok_or_else(|| self.error("retained entry body has no direct Sil AST lowering"))?;
        {
            let statements = &mut authored_statements;
            let storage = self.model.storage_state_for_actor(owner)?;
            let actor_fields = storage.fields.iter().map(|field| field.name.clone()).collect();
            CoSpentLowerer::new(&names, &actor_fields)
                .lower_entry_statements(statements)
                .map_err(|err| self.error(err.to_string()))?;
            let mut input_states = EntryInputStateAstLowerer {
                actor: self.actor,
                entry_id,
                names: &names,
                references: self.input_references,
                model: self.model,
                state_values: self.state_values,
                output_plan: self.output_plan,
                output_values: &self.output_values,
                bindings: &self.bindings,
                active_reference: &self.active_reference,
                observed_output_fields: &self.observed_output_fields,
                digest_helpers: &self.digest_helpers,
                error: None,
            };
            for statement in statements.iter_mut() {
                input_states.visit_statement(statement);
            }
            if let Some(error) = input_states.error {
                return Err(self.error(error.to_string()));
            }
            for statement in statements {
                names.visit_statement(statement);
            }
        }
        Ok(LoweredEntryBody { digest_helpers: self.digest_helpers.into_inner(), authored_statements })
    }

    fn direct_authored_statements(
        &self,
        statements: &[AuthoredEntryStatement<'a>],
        cursor: &SourceNodeCursor,
        indexed: bool,
        materialized_selectors: &mut BTreeSet<String>,
    ) -> Option<Vec<SilStatement<'a>>> {
        let mut result = Vec::with_capacity(statements.len());
        for (index, statement) in statements.iter().enumerate() {
            let statement_cursor = if indexed { cursor.child(ChildEdge::Statement(index)) } else { cursor.clone() };
            if let AuthoredEntryStatement::ForeignBecome { routes, .. } = statement {
                let entry_model = self.model.entry_model_by_id(self.entry_id).ok()?;
                let group_site = self.model.resolution.nodes().find(&statement_cursor.child(ChildEdge::ForeignGroup).address)?;
                let group_id = entry_model.foreign_group(group_site)?;
                let covenant_group = entry_model.group(group_id)?;
                for route in routes {
                    let (route_group, output_id) = entry_model.route_output(route.id)?;
                    if route_group != group_id {
                        return None;
                    }
                    let output = covenant_group.outputs().iter().find(|output| output.id() == output_id)?;
                    let context = Self::covenant_output_context(covenant_group, output);
                    result.extend(self.covenant_output_route_ast(context, output, route)?);
                }
                continue;
            }
            if let AuthoredEntryStatement::Become { routes, .. } = statement {
                let entry_model = self.model.entry_model_by_id(self.entry_id).ok()?;
                let owner = entry_model.id.actor;
                let route_names = SilNames::new(self.model, owner, RootSlot::Entry(entry_model.id.index));
                for route in routes {
                    let resolved = entry_model.route(route.id)?;
                    let span = SilSpan::default();
                    if let ResolvedSuccessor::Constructed { arity: RouteArity::Many, .. } = &resolved.successor {
                        let targets = self.model.route_target_ids_by_id(self.entry_id, resolved).ok()?;
                        let [_] = targets.as_slice() else { return None };
                        let AuthoredSuccessor::Constructed { state, many: true, .. } = &route.successor else {
                            return None;
                        };
                        let SilExprKind::Identifier(_) = &state.kind else {
                            return None;
                        };
                        let (group_id, output_id) = entry_model.route_output(resolved.id)?;
                        if group_id != crate::compiler::model::CovenantGroupId::Current {
                            return None;
                        }
                        let output = entry_model.current().outputs().iter().find(|output| output.id() == output_id)?;
                        let (_, maximum, _) = self.output_plan.current_output_range(output.id())?;
                        let InteractionLocation::Range { start, .. } = output.location() else {
                            return None;
                        };
                        let target_id = self.model.route_static_target_id(resolved).ok()?;
                        let target = plan_actor_output_state(self.entry_id.actor, &target_id, self.model).ok()?;
                        let count = hidden_output_count_name(&resolved.output);
                        result.push(SilStatement::Require {
                            expr: SilExpr::new(
                                SilExprKind::Binary {
                                    op: SilBinaryOp::Eq,
                                    left: Box::new(SilExpr::new(
                                        SilExprKind::UnarySuffix {
                                            source: Box::new((**state).clone()),
                                            kind: SilUnarySuffixKind::Length,
                                            span,
                                        },
                                        span,
                                    )),
                                    right: Box::new(SilExpr::identifier(count.clone())),
                                },
                                span,
                            ),
                            message: None,
                            span,
                            message_span: None,
                        });
                        let position = hidden_output_position_name(&resolved.output);
                        let source_name = hidden_output_temporary_name(route.id, &resolved.output, OutputTemporaryRole::Source);
                        let item = SilExpr::new(
                            SilExprKind::ArrayIndex {
                                source: Box::new((**state).clone()),
                                index: Box::new(SilExpr::identifier(position.clone())),
                            },
                            span,
                        );
                        let mut loop_body = vec![SilStatement::VariableDefinition {
                            type_ref: SilTypeRef {
                                base: SilTypeBase::Custom(target.authored_sil_type().to_string()),
                                array_dims: Vec::new(),
                            },
                            modifiers: Vec::new(),
                            name: source_name.clone(),
                            expr: Some(item),
                            span,
                            type_span: span,
                            modifier_spans: Vec::new(),
                            name_span: span,
                        }];
                        let lowering = self.model.state_lowering_by_id(self.entry_id.actor).ok()?;
                        let physical_ast =
                            target.materialize_authored_ast(SilExpr::identifier(source_name.clone()), lowering, self.model)?;
                        let output_index = if start == 0 {
                            SilExpr::identifier(position.clone())
                        } else {
                            SilExpr::new(
                                SilExprKind::Binary {
                                    op: SilBinaryOp::Add,
                                    left: Box::new(SilExpr::int(start as i64)),
                                    right: Box::new(SilExpr::identifier(position.clone())),
                                },
                                span,
                            )
                        };
                        let output_index = SilExpr::call(
                            "OpAuthOutputIdx",
                            vec![SilExpr::new(SilExprKind::Introspection(SilIntrospectionKind::ActiveInputIndex), span), output_index],
                        );
                        let validation = plan_output_validation(
                            self.entry_id,
                            self.entry,
                            OutputValidationContext::Actor { target: target_id },
                            hidden_output_temporary_name(route.id, &resolved.output, OutputTemporaryRole::Physical),
                            self.model,
                        )
                        .ok()?;
                        loop_body.extend(validation.direct_ast_statements(&target, output_index, physical_ast)?);
                        result.push(SilStatement::For {
                            ident: position,
                            start: SilExpr::int(0),
                            end: SilExpr::identifier(count),
                            max_iterations: SilExpr::int(maximum),
                            body: loop_body,
                            span,
                            ident_span: span,
                            body_span: span,
                        });
                        continue;
                    }
                    if let ResolvedSuccessor::Constructed { arity: RouteArity::One, bound: Some(bound), .. } = &resolved.successor {
                        let AuthoredSuccessor::Constructed { state, many: false, .. } = &route.successor else {
                            return None;
                        };
                        let selector = match bound.actor_target {
                            BoundRouteActor::Local(local) => {
                                self.model.entry_model_by_id(self.entry_id).ok()?.selector_for_local(local)
                            }
                            BoundRouteActor::Selector(_) => None,
                            BoundRouteActor::Fixed(_) | BoundRouteActor::Linked(_) => None,
                            BoundRouteActor::Expression(_) => return None,
                        };
                        let target_id = selector.is_none().then(|| self.model.route_static_target_id(resolved)).transpose().ok()?;
                        let target = match selector {
                            Some(selector) => plan_selector_output_state(self.entry_id.actor, selector, self.model).ok()?,
                            None => plan_actor_output_state(self.entry_id.actor, target_id.as_ref()?, self.model).ok()?,
                        };
                        let mut value = (**state).clone();
                        if let SilExprKind::Identifier(field) = &value.kind
                            && matches!(route_names.binding(value.span), Some(Binding::ActorField))
                            && let Some(projected) = self.active_reference.project_field_ast(field)
                        {
                            value = projected;
                        }
                        // Retain authored identifier spans for the later bound-name lowering pass.
                        let source = match &mut value.kind {
                            SilExprKind::Identifier(_) => value,
                            _ => {
                                if let SilExprKind::StructLiteral { name, .. } = &mut value.kind {
                                    *name = target.authored_sil_type().to_string();
                                }
                                let source_name =
                                    hidden_output_temporary_name(route.id, &resolved.output, OutputTemporaryRole::Source);
                                result.push(SilStatement::VariableDefinition {
                                    type_ref: SilTypeRef {
                                        base: SilTypeBase::Custom(target.authored_sil_type().to_string()),
                                        array_dims: Vec::new(),
                                    },
                                    modifiers: Vec::new(),
                                    name: source_name.clone(),
                                    expr: Some(value),
                                    span,
                                    type_span: span,
                                    modifier_spans: Vec::new(),
                                    name_span: span,
                                });
                                SilExpr::identifier(source_name)
                            }
                        };
                        let lowering = self.model.state_lowering_by_id(self.entry_id.actor).ok()?;
                        let physical_ast = target.materialize_authored_ast(source, lowering, self.model)?;
                        let context = match selector {
                            Some(selector) => OutputValidationContext::Selector {
                                selector,
                                template: hidden_template_selector_template_name(&selector.name),
                            },
                            None => OutputValidationContext::Actor { target: target_id? },
                        };
                        let validation = plan_output_validation(
                            self.entry_id,
                            self.entry,
                            context,
                            hidden_output_temporary_name(route.id, &resolved.output, OutputTemporaryRole::Physical),
                            self.model,
                        )
                        .ok()?;
                        let mut validation_statements = validation
                            .direct_ast_statements(
                                &target,
                                SilExpr::identifier(hidden_output_idx_name(&resolved.output)),
                                physical_ast,
                            )?
                            .into_iter();
                        // Stabilize the physical state before selector checks, preserving the existing template bytecode.
                        if !target.is_unwrapped_authored_value() {
                            result.push(validation_statements.next()?);
                        }
                        if let Some(selector) = selector
                            && materialized_selectors.insert(selector.name.clone())
                        {
                            let index = if let Some(fixed_index) = selector.fixed_index {
                                SilExpr::int(fixed_index as i64)
                            } else {
                                SilExpr::identifier(route_names.local_name(selector.binding?)?)
                            };
                            result.extend(self.selector_template_ast(selector, index)?);
                        }
                        result.extend(validation_statements);
                        continue;
                    }
                    if !matches!(resolved.successor, ResolvedSuccessor::ExactSelf) {
                        return None;
                    }
                    let active = SilExpr::new(SilExprKind::Introspection(SilIntrospectionKind::ActiveInputIndex), span);
                    let output_script = SilExpr::new(
                        SilExprKind::IndexedIntrospection {
                            kind: SilIndexedIntrospectionKind::OutputScriptPubKey,
                            index: Box::new(SilExpr::identifier(hidden_output_idx_name(&resolved.output))),
                            field_span: span,
                        },
                        span,
                    );
                    let input_script = SilExpr::new(
                        SilExprKind::IndexedIntrospection {
                            kind: SilIndexedIntrospectionKind::InputScriptPubKey,
                            index: Box::new(active),
                            field_span: span,
                        },
                        span,
                    );
                    result.push(SilStatement::Require {
                        expr: SilExpr::new(
                            SilExprKind::Binary { op: SilBinaryOp::Eq, left: Box::new(output_script), right: Box::new(input_script) },
                            span,
                        ),
                        message: None,
                        span,
                        message_span: None,
                    });
                }
                continue;
            }
            result.push(match statement {
                AuthoredEntryStatement::Sil(statement) => (**statement).clone(),
                AuthoredEntryStatement::Block { statements, span } => {
                    let mut nested_selectors = materialized_selectors.clone();
                    SilStatement::Block {
                        body: self.direct_authored_statements(statements, &statement_cursor, true, &mut nested_selectors)?,
                        span: *span,
                    }
                }
                AuthoredEntryStatement::If { condition, then_branch, else_branch, span, .. } => {
                    let AuthoredEntryStatement::Block { statements: then_statements, span: then_span } = then_branch.as_ref() else {
                        return None;
                    };
                    let then_branch = self.direct_authored_statements(
                        then_statements,
                        &statement_cursor.child(ChildEdge::ThenBranch),
                        true,
                        &mut materialized_selectors.clone(),
                    )?;
                    let (else_branch, else_span) = match else_branch.as_deref() {
                        Some(AuthoredEntryStatement::Block { statements, span }) => (
                            Some(self.direct_authored_statements(
                                statements,
                                &statement_cursor.child(ChildEdge::ElseBranch),
                                true,
                                &mut materialized_selectors.clone(),
                            )?),
                            Some(*span),
                        ),
                        Some(other @ AuthoredEntryStatement::If { span, .. }) => (
                            Some(self.direct_authored_statements(
                                std::slice::from_ref(other),
                                &statement_cursor.child(ChildEdge::ElseBranch),
                                false,
                                &mut materialized_selectors.clone(),
                            )?),
                            Some(*span),
                        ),
                        None => (None, None),
                        Some(_) => return None,
                    };
                    SilStatement::If {
                        condition: condition.clone(),
                        then_branch,
                        else_branch,
                        span: *span,
                        then_span: *then_span,
                        else_span,
                    }
                }
                AuthoredEntryStatement::Become { .. } => unreachable!("current routes are handled above"),
                AuthoredEntryStatement::ForeignBecome { .. } => return None,
            });
        }
        Self::discard_unrestricted_markers(&mut result);
        Some(result)
    }

    /// `unrestricted(value)` records a compile-time output policy; it is not a Sil call.
    fn discard_unrestricted_markers(statements: &mut Vec<SilStatement<'a>>) {
        statements.retain(|statement| !matches!(statement, SilStatement::FunctionCall { name, .. } if name == word::UNRESTRICTED));
        for statement in statements {
            match statement {
                SilStatement::Block { body, .. } | SilStatement::For { body, .. } => Self::discard_unrestricted_markers(body),
                SilStatement::If { then_branch, else_branch, .. } => {
                    Self::discard_unrestricted_markers(then_branch);
                    if let Some(else_branch) = else_branch {
                        Self::discard_unrestricted_markers(else_branch);
                    }
                }
                _ => {}
            }
        }
    }

    fn selector_template_ast<'i>(&self, selector: &TemplateSelector, selector_expr: SilExpr<'i>) -> Option<Vec<SilStatement<'i>>> {
        let spec =
            self.model.witness_plan_by_id(self.entry_id).ok()?.selectors.iter().find(|spec| Some(spec.binding) == selector.binding)?;
        let span = SilSpan::default();
        let index = hidden_template_selector_index_name(&selector.name);
        let index_expr = || SilExpr::identifier(index.clone());
        let binary = |op, left: SilExpr<'i>, right: SilExpr<'i>| {
            SilExpr::new(SilExprKind::Binary { op, left: Box::new(left), right: Box::new(right) }, span)
        };
        let table = SilExpr::identifier(hidden_route_family_table_name_by_id(&spec.family_id));
        let start = binary(SilBinaryOp::Mul, index_expr(), SilExpr::int(32));
        let end = binary(SilBinaryOp::Add, binary(SilBinaryOp::Mul, index_expr(), SilExpr::int(32)), SilExpr::int(32));
        let template =
            SilExpr::new(SilExprKind::Slice { source: Box::new(table), start: Box::new(start), end: Box::new(end), span }, span);
        let template = SilExpr::call("byte[32]", vec![template]);
        let variable = |name: String, type_ref: SilTypeRef, expr| SilStatement::VariableDefinition {
            type_ref,
            modifiers: Vec::new(),
            name,
            expr: Some(expr),
            span,
            type_span: span,
            modifier_spans: Vec::new(),
            name_span: span,
        };
        let require = |expr| SilStatement::Require { expr, message: None, span, message_span: None };
        Some(vec![
            variable(index.clone(), SilTypeRef { base: SilTypeBase::Int, array_dims: Vec::new() }, selector_expr),
            require(binary(SilBinaryOp::Ge, index_expr(), SilExpr::int(0))),
            require(binary(SilBinaryOp::Lt, index_expr(), SilExpr::int(selector.variant_actor_ids().ok()?.len() as i64))),
            variable(
                hidden_template_selector_template_name(&selector.name),
                SilTypeRef { base: SilTypeBase::Byte, array_dims: vec![silverscript_lang::ast::ArrayDim::Fixed(32)] },
                template,
            ),
        ])
    }

    fn covenant_output_route_ast(
        &self,
        context: CovenantOutputContext<'a>,
        output: &EntryInteraction<'a>,
        route: &AuthoredEntryRoute<'a>,
    ) -> Option<Vec<SilStatement<'a>>> {
        if self.output_plan.current_output_range(output.id()).is_some() {
            return None;
        }
        let interaction_id = output.id();
        let AuthoredSuccessor::Constructed { state, many: false, .. } = &route.successor else {
            return None;
        };
        let outputs = self.model.entry_output_plan_by_id(self.entry_id).ok()?;
        let target_id = match context {
            CovenantOutputContext::Existing { .. } => outputs.observed_target(interaction_id).ok()?,
            CovenantOutputContext::Genesis { .. } => outputs.spawned_target(interaction_id).ok()?,
        };
        let target_plan = self.model.output_plan_by_id(self.entry_id.actor).ok()?.target(target_id).ok()?;
        let target = output_state_target(self.entry_id.actor, target_plan, self.model).ok()?;
        let span = SilSpan::default();
        let mut statements = Vec::new();
        let mut value = (**state).clone();
        let entry_model = self.model.entry_model_by_id(self.entry_id).ok()?;
        let owner = entry_model.id.actor;
        let route_names = SilNames::new(self.model, owner, RootSlot::Entry(entry_model.id.index));
        if let SilExprKind::Identifier(field) = &value.kind
            && matches!(route_names.binding(value.span), Some(Binding::ActorField))
            && let Some(projected) = self.active_reference.project_field_ast(field)
        {
            value = projected;
        }
        let source = match &mut value.kind {
            SilExprKind::Identifier(_) => value,
            _ => {
                if let SilExprKind::StructLiteral { name, .. } = &mut value.kind {
                    *name = target.authored_sil_type().to_string();
                }
                let source_name = hidden_output_temporary_name(route.id, context.output_name(), OutputTemporaryRole::Source);
                statements.push(SilStatement::VariableDefinition {
                    type_ref: SilTypeRef { base: SilTypeBase::Custom(target.authored_sil_type().to_string()), array_dims: Vec::new() },
                    modifiers: Vec::new(),
                    name: source_name.clone(),
                    expr: Some(value),
                    span,
                    type_span: span,
                    modifier_spans: Vec::new(),
                    name_span: span,
                });
                SilExpr::identifier(source_name)
            }
        };
        let lowering = self.model.state_lowering_by_id(self.entry_id.actor).ok()?;
        let physical_ast = target.materialize_authored_ast(source, lowering, self.model)?;
        let observed_spec = match context {
            CovenantOutputContext::Existing { .. } => {
                Some(self.model.entry_output_plan_by_id(self.entry_id).ok()?.observed_witness(interaction_id).ok()?)
            }
            CovenantOutputContext::Genesis { .. } => None,
        };
        let (template, template_ast) = match context {
            CovenantOutputContext::Existing { output, .. } => {
                let spec = observed_spec?;
                match &spec.template_source {
                    ObservedTemplateSource::FixedInApp(id) => {
                        let name = hidden_template_name(&self.model.types.display_names[id]);
                        (name.clone(), SilExpr::identifier(name))
                    }
                    ObservedTemplateSource::FixedLinked(id) => {
                        let linked = self.model.linked_actors.get(id)?;
                        let name = hidden_imported_template_name(&ImportedTemplateSpec::from_linked(linked));
                        (name.clone(), SilExpr::identifier(name))
                    }
                    ObservedTemplateSource::DynamicBinding => (output.actor.clone(), SilExpr::identifier(output.actor.clone())),
                    ObservedTemplateSource::ActorTypeValue => {
                        let source = spec.source.as_ref()?;
                        let name = match source {
                            ClauseActorTypeRef::StateField { field, .. } => field.field(),
                            ClauseActorTypeRef::EntryArgument { name, .. } => name,
                        };
                        (name.to_string(), self.actor_type_value_ast(source)?)
                    }
                    ObservedTemplateSource::Witness => {
                        let name = hidden_observed_actor_template_name(spec);
                        (name.clone(), SilExpr::identifier(name))
                    }
                }
            }
            CovenantOutputContext::Genesis { .. } => match outputs.spawned_actor(interaction_id).ok()? {
                Some(StaticActorId::InApp(id)) => {
                    let name = hidden_template_name(&self.model.types.display_names[id]);
                    (name.clone(), SilExpr::identifier(name))
                }
                Some(StaticActorId::Linked(id)) => {
                    let actor = self.model.linked_actors.get(id)?;
                    let name = hidden_imported_template_name(&ImportedTemplateSpec::from_linked(actor));
                    (name.clone(), SilExpr::identifier(name))
                }
                None => {
                    let CovenantOutputContext::Genesis { .. } = context else { unreachable!() };
                    let source =
                        self.model.witness_plan_by_id(self.entry_id).ok()?.spawn_output(interaction_id).ok()?.source.as_ref()?;
                    let name = match source {
                        ClauseActorTypeRef::StateField { field, .. } => field.field(),
                        ClauseActorTypeRef::EntryArgument { name, .. } => name,
                    };
                    (name.to_string(), self.actor_type_value_ast(source)?)
                }
            },
        };
        let output_index = match context {
            CovenantOutputContext::Existing { .. } => hidden_observed_output_idx_name(context.group_name(), context.output_name()),
            CovenantOutputContext::Genesis { .. } => hidden_spawn_output_idx_name(context.group_name(), context.output_name()),
        };
        let validation_context = match context {
            CovenantOutputContext::Existing { .. } => {
                OutputValidationContext::Observed { id: interaction_id, template, template_ast: Some(Box::new(template_ast)) }
            }
            CovenantOutputContext::Genesis { .. } => {
                OutputValidationContext::Spawned { id: interaction_id, template, template_ast: Some(Box::new(template_ast)) }
            }
        };
        let validation = plan_output_validation(
            self.entry_id,
            self.entry,
            validation_context,
            hidden_output_temporary_name(route.id, context.output_name(), OutputTemporaryRole::Physical),
            self.model,
        )
        .ok()?;
        statements.extend(validation.direct_ast_statements(&target, SilExpr::identifier(output_index), physical_ast)?);
        Some(statements)
    }

    fn actor_type_value_ast(&self, source: &ClauseActorTypeRef) -> Option<SilExpr<'static>> {
        match source {
            ClauseActorTypeRef::StateField { field, .. } => self.active_reference.project_field_ast(field.field()),
            ClauseActorTypeRef::EntryArgument { name, .. } => Some(SilExpr::identifier(name.clone())),
        }
    }

    fn covenant_output_context(group: &CovenantGroup<'a>, interaction: &EntryInteraction<'a>) -> CovenantOutputContext<'a> {
        match (group.observe(), group.spawn(), interaction.source()) {
            (Some(observe), None, InteractionSource::ObserveOutput(output)) => CovenantOutputContext::Existing { observe, output },
            (None, Some(spawn), InteractionSource::SpawnOutput(output)) => CovenantOutputContext::Genesis { spawn, output },
            _ => unreachable!("external covenant output retains its matching source clause"),
        }
    }
}

impl BodyLowerer<'_, '_, '_> {
    fn error(&self, message: impl Into<String>) -> ArgentError {
        ArgentError::new(format!("{} in `{}::{}`", message.into(), self.actor.name, self.entry.name))
    }
}

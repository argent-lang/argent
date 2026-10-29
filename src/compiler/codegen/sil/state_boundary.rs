//! Typed boundary between authored state and physical contract state.
//!
//! Plans authenticated input projection, successor materialization, and output
//! template proof without exposing compiler-owned fields to authored values.

use std::collections::{BTreeMap, BTreeSet};

use crate::compiler::model::{
    AppCompilationContext, ClauseActorTypeRef, ContractStateLowering, EntryInputReferenceId, GeneratedFieldId, GeneratedFieldSource,
    InputAuthentication, InputFieldAvailability, InputReferenceOrigin, InputReferenceRequirement, InteractionId, InteractionSource,
    ObservedTemplateSource, OutputProofRequirement, OutputTargetPlan, PhysicalFieldId, PhysicalStateLayout, PhysicalTargetId,
    ResolvedTypeBase, SilStateType, SourceFieldId, SourceStateId, SourceStorageRelation, StaticActorId, TargetPhysicalPlan,
    TemplateSelector, TemplateWitnessForm, packed_field_len,
};
use crate::compiler::syntax::node::{DeclId, EntryId};
use crate::compiler::syntax::{ActorDecl, ArrayDim, EntryDecl, TypeRef, word};
use crate::error::{ArgentError, Result};
use silverscript_lang::ast::{
    ArrayDim as SilArrayDim, Expr as SilExpr, ExprKind as SilExprKind, FunctionAst,
    IndexedIntrospectionKind as SilIndexedIntrospectionKind, IntrospectionKind as SilIntrospectionKind, ParamAst, Span as SilSpan,
    StateFieldExpr as SilStateFieldExpr, Statement as SilStatement, TypeBase as SilTypeBase, TypeRef as SilTypeRef,
    UnarySuffixKind as SilUnarySuffixKind,
};

use super::contract::ContractLowerer;
use super::names::*;
use super::state_types::StateValueTypes;

#[cfg(test)]
mod tests;

impl ContractLowerer<'_, '_> {
    /// Authenticate expanded active-state preimages before authored entry operations.
    pub(in crate::compiler::codegen) fn authenticated_expansion_prelude(&self) -> Result<Vec<SilStatement<'static>>> {
        let specs = self.model.state_expansion_witnesses_by_id(self.actor_id)?;
        let span = SilSpan::default();
        let unpack = |ty: &TypeRef, packed: &str, offset: usize, end: usize| -> Result<SilExpr<'static>> {
            if matches!((ty.name.as_str(), ty.array), ("byte", None)) {
                return Ok(SilExpr::new(
                    SilExprKind::ArrayIndex {
                        source: Box::new(SilExpr::identifier(packed)),
                        index: Box::new(SilExpr::int(offset as i64)),
                    },
                    span,
                ));
            }
            let slice = SilExpr::new(
                SilExprKind::Slice {
                    source: Box::new(SilExpr::identifier(packed)),
                    start: Box::new(SilExpr::int(offset as i64)),
                    end: Box::new(SilExpr::int(end as i64)),
                    span,
                },
                span,
            );
            if ty.is_actor_type() {
                return Ok(SilExpr::call("byte[32]", vec![slice]));
            }
            let slice_expr = format!("{packed}.slice({offset}, {end})");
            match (ty.name.as_str(), ty.array) {
                ("int", None) => Ok(SilExpr::call("OpBin2Num", vec![slice])),
                ("temporal", None) => Ok(SilExpr::call("temporal", vec![SilExpr::call("OpBin2Num", vec![slice])])),
                ("bool", None) => Ok(SilExpr::new(
                    SilExprKind::Binary {
                        op: silverscript_lang::ast::BinaryOp::Ne,
                        left: Box::new(SilExpr::call("OpBin2Num", vec![slice])),
                        right: Box::new(SilExpr::int(0)),
                    },
                    span,
                )),
                ("byte", Some(ArrayDim::Fixed(len))) => Ok(SilExpr::call(format!("byte[{len}]"), vec![slice])),
                ("pubkey", None) | (word::COVENANT_ID, None) => Ok(SilExpr::call("byte[32]", vec![slice])),
                ("sig", None) => Ok(SilExpr::call("byte[65]", vec![slice])),
                ("datasig", None) => Ok(SilExpr::call("byte[64]", vec![slice])),
                ("bytes", None) | ("string", None) | (_, Some(_)) => {
                    Err(ArgentError::new(format!("cannot unpack unsupported variable or array field from `{slice_expr}`")))
                }
                (name, None) => Err(ArgentError::new(format!("cannot unpack unsupported type `{name}` from `{slice_expr}`"))),
            }
        };
        let mut statements = Vec::new();
        for spec in specs {
            let hidden = hidden_state_expansion_preimage_name(spec);
            statements.push(SilStatement::Require {
                expr: SilExpr::new(
                    SilExprKind::Binary {
                        op: silverscript_lang::ast::BinaryOp::Eq,
                        left: Box::new(SilExpr::call(
                            "blake3",
                            vec![SilExpr::call("byte[]", vec![SilExpr::identifier(hidden.clone())])],
                        )),
                        right: Box::new(SilExpr::identifier(spec.field.clone())),
                    },
                    span,
                ),
                message: None,
                span,
                message_span: None,
            });
            let mut offset = 0usize;
            for field in &self.model.state_by_source(&spec.memory_source)?.fields {
                let len = packed_field_len(&field.ty)?;
                let end = offset + len;
                statements.push(SilStatement::VariableDefinition {
                    type_ref: super::state_types::lower_planned_type(&field.ty, false),
                    modifiers: Vec::new(),
                    name: hidden_state_expansion_field_name(spec, &field.name),
                    expr: Some(unpack(&field.ty, &hidden, offset, end)?),
                    span,
                    type_span: span,
                    modifier_spans: Vec::new(),
                    name_span: span,
                });
                offset = end;
            }
        }
        Ok(statements)
    }

    /// Emit the digest helpers required by lowered entry bodies.
    pub(in crate::compiler::codegen) fn authored_state_digest_helpers(
        &self,
        sources: &BTreeSet<SourceStateId>,
    ) -> Result<Vec<FunctionAst<'static>>> {
        let lowering = self.model.state_lowering_by_id(self.actor_id)?;
        sources
            .iter()
            .map(|source| {
                let sil_type = self.state_values.authored_sil_type(source).ok_or_else(|| {
                    ArgentError::new(format!("state `{}` has no contract-local authored representation", source.as_str()))
                })?;
                let name = self.state_values.digest_helper_name(source);
                let value = format!("{name}_value");
                let span = SilSpan::default();
                let digest_ast = authored_state_payload_digest_ast(source, SilExpr::identifier(value.clone()), lowering, self.model)
                    .ok_or_else(|| ArgentError::new(format!("state `{}` has no direct digest AST", source.as_str())))?;
                Ok(FunctionAst {
                    name,
                    attributes: Vec::new(),
                    params: vec![ParamAst {
                        type_ref: SilTypeRef { base: SilTypeBase::Custom(sil_type.to_string()), array_dims: Vec::new() },
                        name: value,
                        span,
                        type_span: span,
                        name_span: span,
                    }],
                    entrypoint: false,
                    return_types: vec![SilTypeRef { base: SilTypeBase::Byte, array_dims: vec![SilArrayDim::Fixed(32)] }],
                    returns_tuple: false,
                    body: vec![SilStatement::Return { exprs: vec![digest_ast], span }],
                    return_type_spans: vec![span],
                    span,
                    name_span: span,
                    body_span: span,
                })
            })
            .collect()
    }
}

/// Output layout, rendered type, and compiler-owned field sources selected as one plan.
pub(in crate::compiler::codegen) struct OutputStateTarget {
    sil_type: String,
    authored_sil_type: String,
    named_physical_layout: Option<PhysicalStateLayout>,
    physical: TargetPhysicalPlan,
    generated_field_asts: BTreeMap<GeneratedFieldId, SilExpr<'static>>,
}

impl OutputStateTarget {
    pub(in crate::compiler::codegen) fn physical_type(&self) -> &str {
        &self.sil_type
    }

    pub(in crate::compiler::codegen) fn named_physical_layout(&self) -> Option<(&str, &PhysicalStateLayout)> {
        self.named_physical_layout.as_ref().map(|layout| (self.sil_type.as_str(), layout))
    }

    pub(in crate::compiler::codegen) fn authored_sil_type(&self) -> &str {
        &self.authored_sil_type
    }

    pub(in crate::compiler::codegen) fn is_unwrapped_authored_value(&self) -> bool {
        self.physical.source_to_storage().is_identity()
            && self.physical.storage_to_physical().is_identity()
            && self.sil_type == self.authored_sil_type
    }

    pub(in crate::compiler::codegen) fn materialize_authored_ast<'ast>(
        &self,
        authored: SilExpr<'ast>,
        lowering: &ContractStateLowering,
        model: &AppCompilationContext<'_>,
    ) -> Option<SilExpr<'ast>> {
        if self.is_unwrapped_authored_value() {
            return Some(authored);
        }
        let span = SilSpan::default();
        let mut physical_fields = BTreeMap::new();
        for field in self.physical.source_to_storage().fields() {
            let physical = self.physical.storage_to_physical().physical_field(field.storage())?.clone();
            let source_value = SilExpr::new(
                SilExprKind::FieldAccess {
                    source: Box::new(authored.clone()),
                    field: field.source().field().to_string(),
                    field_span: span,
                },
                span,
            );
            let value = match field.expanded_state() {
                Some(expanded) => authored_state_payload_digest_ast(expanded, source_value, lowering, model)?,
                None => source_value,
            };
            if physical_fields.insert(physical, value).is_some() {
                return None;
            }
        }
        for (id, value) in &self.generated_field_asts {
            if physical_fields.insert(PhysicalFieldId::Generated(id.clone()), value.clone()).is_some() {
                return None;
            }
        }
        let fields = self
            .physical
            .physical()
            .fields()
            .iter()
            .map(|field| {
                Some(SilStateFieldExpr {
                    name: field.sil_name().to_string(),
                    expr: physical_fields.remove(field.id())?,
                    span,
                    name_span: span,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        if !physical_fields.is_empty() {
            return None;
        }
        Some(SilExpr::new(SilExprKind::StructLiteral { name: self.sil_type.clone(), fields, name_span: span }, span))
    }
}

pub(in crate::compiler::codegen) fn plan_actor_output_state(
    actor: DeclId,
    target_actor: &StaticActorId,
    model: &AppCompilationContext<'_>,
) -> Result<OutputStateTarget> {
    let target = model.output_plan_by_id(actor)?.actor(target_actor)?;
    output_state_target(actor, target, model)
}

pub(in crate::compiler::codegen) fn plan_selector_output_state(
    actor: DeclId,
    selector: &TemplateSelector,
    model: &AppCompilationContext<'_>,
) -> Result<OutputStateTarget> {
    let target = model.output_plan_by_id(actor)?.selector(selector)?;
    output_state_target(actor, target, model)
}

pub(in crate::compiler::codegen) fn output_state_target(
    source_actor: DeclId,
    plan: &OutputTargetPlan,
    model: &AppCompilationContext<'_>,
) -> Result<OutputStateTarget> {
    let lowering = model.state_lowering_by_id(source_actor)?;
    let physical = plan.physical.clone();
    let authored_sil_type = lowering
        .source_representation(physical.source())
        .ok_or_else(|| ArgentError::new("output target source has no authored representation plan"))
        .and_then(|representation| render_sil_state_type(representation.sil_type()))?;
    let named_physical_layout = match &plan.sil_type {
        SilStateType::State | SilStateType::Source(_) => None,
        SilStateType::StoragePhysical(_) | SilStateType::TargetPhysical(_) => Some(
            lowering
                .target(&plan.canonical_target)
                .ok_or_else(|| ArgentError::new("output type owner has no physical target layout"))?
                .physical()
                .clone(),
        ),
    };
    let generated_field_asts = plan_generated_fields(plan, model)?;
    Ok(OutputStateTarget {
        sil_type: render_sil_state_type(&plan.sil_type)?,
        authored_sil_type,
        named_physical_layout,
        physical,
        generated_field_asts,
    })
}

fn plan_generated_fields(
    plan: &OutputTargetPlan,
    model: &AppCompilationContext<'_>,
) -> Result<BTreeMap<GeneratedFieldId, SilExpr<'static>>> {
    let fields = plan
        .generated_fields
        .iter()
        .map(|(id, source)| {
            let field = plan
                .physical
                .physical()
                .field(&PhysicalFieldId::Generated(id.clone()))
                .ok_or_else(|| ArgentError::new("generated output field is missing from its physical layout"))?;
            let span = SilSpan::default();
            let concat = |parts: Vec<SilExpr<'static>>| {
                parts
                    .into_iter()
                    .reduce(|left, right| {
                        SilExpr::new(
                            SilExprKind::Binary {
                                op: silverscript_lang::ast::BinaryOp::Add,
                                left: Box::new(left),
                                right: Box::new(right),
                            },
                            span,
                        )
                    })
                    .unwrap_or_else(|| SilExpr::bytes(Vec::new()))
            };
            let ast = match source {
                GeneratedFieldSource::Carried | GeneratedFieldSource::TemplateField => SilExpr::identifier(field.sil_name()),
                GeneratedFieldSource::TableFromTemplates(actors) => {
                    let ast = concat(
                        actors
                            .iter()
                            .map(|actor| {
                                model
                                    .app_actors
                                    .name(*actor)
                                    .map(|name| SilExpr::identifier(hidden_template_name(name)))
                                    .ok_or_else(|| ArgentError::new("output route table references an unknown selected actor"))
                            })
                            .collect::<Result<Vec<_>>>()?,
                    );
                    SilExpr::call(field.sil_type(), vec![ast])
                }
                GeneratedFieldSource::DigestFromTable(family) => {
                    let table = hidden_route_family_table_name_by_id(family);
                    SilExpr::call("blake3", vec![SilExpr::call("byte[]", vec![SilExpr::identifier(table)])])
                }
                GeneratedFieldSource::DigestFromTemplates(actors) => {
                    let ast = concat(
                        actors
                            .iter()
                            .map(|actor| {
                                model
                                    .app_actors
                                    .name(*actor)
                                    .map(|name| SilExpr::identifier(hidden_template_name(name)))
                                    .ok_or_else(|| ArgentError::new("output digest references an unknown selected actor"))
                            })
                            .collect::<Result<Vec<_>>>()?,
                    );
                    SilExpr::call("blake3", vec![SilExpr::call("byte[]", vec![ast])])
                }
            };
            Ok((id.clone(), ast))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    Ok(fields)
}

#[derive(Clone)]
enum InputTemplateProof {
    CovenantDomain,
    Template { prefix_len: InputTemplateLength, suffix_len: InputTemplateLength, template_ast: Box<SilExpr<'static>> },
}

#[derive(Clone)]
enum InputTemplateLength {
    IntWitness(String),
    BytesWitness(String),
}

impl InputTemplateLength {
    fn expr(&self) -> SilExpr<'static> {
        match self {
            Self::IntWitness(name) => SilExpr::identifier(name.clone()),
            Self::BytesWitness(name) => {
                let span = SilSpan::default();
                SilExpr::new(
                    SilExprKind::UnarySuffix {
                        source: Box::new(SilExpr::identifier(name.clone())),
                        kind: SilUnarySuffixKind::Length,
                        span,
                    },
                    span,
                )
            }
        }
    }
}

#[derive(Clone)]
struct AuthenticatedPhysicalInput {
    expr: String,
    sil_type: String,
    input_index: InputIndexExpr,
    proof: InputTemplateProof,
}

impl AuthenticatedPhysicalInput {
    fn read_statement(&self) -> SilStatement<'static> {
        let mut args = vec![self.input_index.ast()];
        let builtin = match &self.proof {
            InputTemplateProof::CovenantDomain => "readInputState",
            InputTemplateProof::Template { prefix_len, suffix_len, template_ast, .. } => {
                args.extend([prefix_len.expr(), suffix_len.expr(), template_ast.as_ref().clone()]);
                "readInputStateWithTemplate"
            }
        };
        let span = SilSpan::default();
        SilStatement::VariableDefinition {
            type_ref: SilTypeRef { base: SilTypeBase::Custom(self.sil_type.clone()), array_dims: Vec::new() },
            modifiers: Vec::new(),
            name: self.expr.clone(),
            expr: Some(SilExpr::call(builtin, args)),
            span,
            type_span: span,
            modifier_spans: Vec::new(),
            name_span: span,
        }
    }
}

fn packed_field_ast<'a>(ty: &TypeRef, value: SilExpr<'a>) -> Option<SilExpr<'a>> {
    let cast = |name: &str, value| SilExpr::call(name, vec![value]);
    if ty.is_actor_type() {
        return Some(cast("byte[]", value));
    }
    match (ty.name.as_str(), ty.array) {
        ("int", None) => Some(cast("__as_cast_byte[8]", value)),
        ("temporal", None) => Some(cast("__as_cast_byte[8]", cast("int", value))),
        ("bool", None) => Some(cast("__as_cast_byte[1]", cast("__as_cast_int", value))),
        ("byte", None | Some(crate::compiler::syntax::ArrayDim::Fixed(_)))
        | ("pubkey" | "sig" | "datasig" | word::COVENANT_ID, None) => Some(cast("byte[]", value)),
        _ => None,
    }
}

pub(in crate::compiler::codegen) fn authored_state_payload_digest_ast<'a>(
    state: &SourceStateId,
    value: SilExpr<'a>,
    lowering: &ContractStateLowering,
    model: &AppCompilationContext<'_>,
) -> Option<SilExpr<'a>> {
    let relation = lowering.source_representation(state)?.source_to_storage();
    let storage = model.storage_state_by_source(state).ok()?;
    let span = SilSpan::default();
    let mut parts = Vec::with_capacity(relation.fields().len());
    for field in relation.fields() {
        let storage_field = storage.fields.iter().find(|candidate| candidate.name == field.storage().field())?;
        let source_field = SilExpr::new(
            SilExprKind::FieldAccess { source: Box::new(value.clone()), field: field.source().field().to_string(), field_span: span },
            span,
        );
        let stored = match field.expanded_state() {
            Some(expanded) => authored_state_payload_digest_ast(expanded, source_field, lowering, model)?,
            None => source_field,
        };
        parts.push(packed_field_ast(&storage_field.ty, stored)?);
    }
    let bytes = parts
        .into_iter()
        .reduce(|left, right| {
            SilExpr::new(
                SilExprKind::Binary { op: silverscript_lang::ast::BinaryOp::Add, left: Box::new(left), right: Box::new(right) },
                span,
            )
        })
        .unwrap_or_else(|| SilExpr::bytes(Vec::new()));
    Some(SilExpr::call("blake3", vec![SilExpr::call("byte[]", vec![bytes])]))
}

#[derive(Clone)]
struct PlannedSourceField {
    name: String,
    sil_type_ref: SilTypeRef,
    ast_value: Option<SilExpr<'static>>,
    trusted_storage: Option<String>,
}

/// Opaque source-level access derived from validated input provenance.
#[derive(Clone)]
struct SourceStateAccess {
    source: SourceStateId,
    source_to_storage: SourceStorageRelation,
    authored_sil_type: String,
    complete: Option<String>,
    fields: Vec<PlannedSourceField>,
    target: PhysicalTargetId,
}

enum OutputTemplateProof {
    Current,
    BoundInput { input_index_ast: SilExpr<'static>, prefix_len: String, suffix_len: String },
    Witnessed { prefix: String, suffix: String },
}

/// Physical output and template proof planned as independent validation inputs.
pub(in crate::compiler::codegen) struct PlannedOutputValidation {
    state_binding: String,
    proof: OutputTemplateProof,
    template_ast: SilExpr<'static>,
}

impl PlannedOutputValidation {
    pub(in crate::compiler::codegen) fn direct_ast_statements<'ast>(
        self,
        target: &OutputStateTarget,
        output_index: SilExpr<'ast>,
        state: SilExpr<'ast>,
    ) -> Option<Vec<SilStatement<'ast>>> {
        let span = SilSpan::default();
        let mut statements = Vec::new();
        let state = if !target.is_unwrapped_authored_value() {
            statements.push(SilStatement::VariableDefinition {
                type_ref: SilTypeRef { base: SilTypeBase::Custom(target.physical_type().to_string()), array_dims: Vec::new() },
                modifiers: Vec::new(),
                name: self.state_binding.clone(),
                expr: Some(state),
                span,
                type_span: span,
                modifier_spans: Vec::new(),
                name_span: span,
            });
            SilExpr::identifier(self.state_binding)
        } else {
            state
        };
        let mut args = vec![output_index, state];
        let builtin = match self.proof {
            OutputTemplateProof::Current => "validateOutputState",
            OutputTemplateProof::BoundInput { input_index_ast, prefix_len, suffix_len, .. } => {
                args.push(input_index_ast);
                args.extend([prefix_len, suffix_len].map(SilExpr::identifier));
                args.push(self.template_ast);
                "validateOutputStateWithInputTemplate"
            }
            OutputTemplateProof::Witnessed { prefix, suffix, .. } => {
                args.extend([prefix, suffix].map(SilExpr::identifier));
                args.push(self.template_ast);
                "validateOutputStateWithTemplate"
            }
        };
        statements.push(SilStatement::FunctionCall { name: builtin.to_string(), args, span, name_span: span });
        Some(statements)
    }
}

/// Resolved route context from which output authentication is planned.
pub(in crate::compiler::codegen) enum OutputValidationContext<'a> {
    Actor { target: StaticActorId },
    Selector { selector: &'a TemplateSelector, template: String },
    Observed { id: InteractionId, template: String, template_ast: Option<Box<SilExpr<'static>>> },
    Spawned { id: InteractionId, template: String, template_ast: Option<Box<SilExpr<'static>>> },
}

pub(in crate::compiler::codegen) fn plan_output_validation(
    entry_id: EntryId,
    entry: &EntryDecl,
    context: OutputValidationContext<'_>,
    state_binding: impl Into<String>,
    model: &AppCompilationContext<'_>,
) -> Result<PlannedOutputValidation> {
    let outputs = model.entry_output_plan_by_id(entry_id)?;
    let requirement = match &context {
        OutputValidationContext::Actor { target } => outputs.actor(target)?,
        OutputValidationContext::Selector { selector, .. } => outputs.selector(selector)?,
        OutputValidationContext::Observed { id, .. } => outputs.observed(*id)?,
        OutputValidationContext::Spawned { id, .. } => outputs.spawned(*id)?,
    };
    let template = match &context {
        OutputValidationContext::Actor { target } => hidden_template_name(&model.static_actor_reference(target)?),
        OutputValidationContext::Selector { template, .. }
        | OutputValidationContext::Observed { template, .. }
        | OutputValidationContext::Spawned { template, .. } => template.clone(),
    };
    let template_ast = match &context {
        OutputValidationContext::Observed { template_ast: Some(expr), .. }
        | OutputValidationContext::Spawned { template_ast: Some(expr), .. } => expr.as_ref().clone(),
        _ => SilExpr::identifier(template.clone()),
    };
    let proof = match requirement {
        OutputProofRequirement::Current => OutputTemplateProof::Current,
        OutputProofRequirement::BoundInput { input, target } => {
            let target = model.static_actor_reference(target)?;
            let reference = model
                .input_plan_by_id(entry_id)?
                .reference(*input)
                .ok_or_else(|| ArgentError::new("output proof references an unknown authenticated input"))?;
            let input_index_ast = match reference.origin {
                InputReferenceOrigin::Consumed(_) => {
                    let path = reference.origin.source_path(entry)?;
                    let [name] = path.as_slice() else { return Err(ArgentError::new("consumed output proof has no input handle")) };
                    match reference.location {
                        Some(crate::compiler::model::InteractionLocation::Range { .. }) => {
                            let position = reference
                                .ranged_proof_input_position
                                .ok_or_else(|| ArgentError::new("ranged consumed output proof has no planned input position"))?;
                            SilExpr::call(
                                "OpCovInputIdx",
                                vec![SilExpr::identifier(hidden_cov_id_name()), SilExpr::int(position as i64)],
                            )
                        }
                        Some(_) => {
                            let index = hidden_input_idx_name(name);
                            SilExpr::identifier(index)
                        }
                        None => return Err(ArgentError::new("consumed output proof has no input location")),
                    }
                }
                InputReferenceOrigin::Observed(_) => {
                    let path = reference.origin.source_path(entry)?;
                    let [observe, _, handle] = path.as_slice() else {
                        return Err(ArgentError::new("observed output proof has no input handle"));
                    };
                    let index = hidden_observed_input_idx_name(observe, handle);
                    SilExpr::identifier(index)
                }
                InputReferenceOrigin::Active => return Err(ArgentError::new("active input cannot prove a foreign output template")),
            };
            OutputTemplateProof::BoundInput {
                input_index_ast,
                prefix_len: hidden_witness_prefix_len_name(&target),
                suffix_len: hidden_witness_suffix_len_name(&target),
            }
        }
        OutputProofRequirement::WitnessedActor(target) => {
            let target = model.static_actor_reference(target)?;
            OutputTemplateProof::Witnessed { prefix: hidden_witness_prefix_name(&target), suffix: hidden_witness_suffix_name(&target) }
        }
        OutputProofRequirement::Selector(id) => {
            let OutputValidationContext::Selector { selector, .. } = &context else {
                return Err(ArgentError::new("selector output proof has no selector context"));
            };
            if selector.binding != Some(*id) {
                return Err(ArgentError::new("selector output proof references the wrong binding"));
            }
            OutputTemplateProof::Witnessed {
                prefix: hidden_template_selector_prefix_name(&selector.name),
                suffix: hidden_template_selector_suffix_name(&selector.name),
            }
        }
        OutputProofRequirement::BoundObserved(input) => {
            let reference = model
                .input_plan_by_id(entry_id)?
                .reference(*input)
                .ok_or_else(|| ArgentError::new("observed output proof references an unknown authenticated input"))?;
            let InputReferenceOrigin::Observed(_) = reference.origin else {
                return Err(ArgentError::new("observed output proof references the wrong input"));
            };
            let path = reference.origin.source_path(entry)?;
            let [observe, _, handle] = path.as_slice() else {
                return Err(ArgentError::new("observed output proof has no input handle"));
            };
            let input_spec = reference
                .observed_witness
                .as_ref()
                .ok_or_else(|| ArgentError::new("observed output proof has no planned observed input witness"))?;
            OutputTemplateProof::BoundInput {
                input_index_ast: SilExpr::identifier(hidden_observed_input_idx_name(observe, handle)),
                prefix_len: hidden_observed_actor_prefix_len_name(input_spec),
                suffix_len: hidden_observed_actor_suffix_len_name(input_spec),
            }
        }
        OutputProofRequirement::ObservedWitness => {
            let OutputValidationContext::Observed { id, .. } = &context else {
                return Err(ArgentError::new("observed output proof has no observed witness"));
            };
            let witness = outputs.observed_witness(*id)?;
            OutputTemplateProof::Witnessed {
                prefix: hidden_observed_actor_prefix_name(witness),
                suffix: hidden_observed_actor_suffix_name(witness),
            }
        }
        OutputProofRequirement::SpawnWitness => {
            let OutputValidationContext::Spawned { id, .. } = &context else {
                return Err(ArgentError::new("spawn output proof has no spawn witness"));
            };
            let witness = model.witness_plan_by_id(entry_id)?.spawn_output(*id)?;
            OutputTemplateProof::Witnessed {
                prefix: hidden_spawn_actor_prefix_name(witness),
                suffix: hidden_spawn_actor_suffix_name(witness),
            }
        }
    };
    Ok(PlannedOutputValidation { state_binding: state_binding.into(), proof, template_ast })
}

struct InputReferenceSpec {
    id: EntryInputReferenceId,
    reference: String,
    physical_expr: String,
    input_index: InputIndexExpr,
    direct_authored_state: bool,
}

#[derive(Clone)]
struct InputIndexExpr {
    expr: SilExpr<'static>,
}

impl InputIndexExpr {
    fn ast(&self) -> SilExpr<'static> {
        self.expr.clone()
    }
}

/// One input reference whose physical provenance remains private to the boundary.
#[derive(Clone)]
pub(in crate::compiler::codegen) struct PlannedEntryInputReference {
    id: EntryInputReferenceId,
    reference: String,
    input_index: InputIndexExpr,
    access: SourceStateAccess,
    physical: Option<AuthenticatedPhysicalInput>,
}

impl PlannedEntryInputReference {
    pub(in crate::compiler::codegen) fn reference(&self) -> &str {
        &self.reference
    }

    pub(in crate::compiler::codegen) fn physical_target(&self) -> &PhysicalTargetId {
        &self.access.target
    }

    pub(in crate::compiler::codegen) fn read_statement(&self) -> SilStatement<'static> {
        self.physical.as_ref().expect("only authenticated external references are emitted as reads").read_statement()
    }

    pub(in crate::compiler::codegen) fn native_value_ast(&self) -> SilExpr<'static> {
        let span = SilSpan::default();
        let index = self.input_index.ast();
        SilExpr::new(
            SilExprKind::IndexedIntrospection {
                kind: SilIndexedIntrospectionKind::InputValue,
                index: Box::new(index),
                field_span: span,
            },
            span,
        )
    }

    pub(in crate::compiler::codegen) fn covenant_id_ast(&self) -> SilExpr<'static> {
        SilExpr::call("OpInputCovenantId", vec![self.input_index.ast()])
    }

    pub(in crate::compiler::codegen) fn project_field_ast(&self, field_name: &str) -> Option<SilExpr<'static>> {
        self.access.fields.iter().find(|field| field.name == field_name)?.ast_value.clone()
    }

    pub(in crate::compiler::codegen) fn unavailable_authored_state(&self) -> ArgentError {
        if let Some(field) = self.access.fields.iter().find(|field| field.ast_value.is_none()) {
            ArgentError::new(format!(
                "expanded input state `{}` from target `{:?}` cannot be materialized without a validated preimage for field `{}`",
                self.access.source.as_str(),
                self.access.target,
                field.name
            ))
        } else {
            ArgentError::new(format!("cannot lower authored state for `{}`", self.reference))
        }
    }

    pub(in crate::compiler::codegen) fn unavailable_field(&self, field_name: &str) -> ArgentError {
        if self.access.fields.iter().any(|field| field.name == field_name) {
            ArgentError::new(format!(
                "expanded input field `{field_name}` cannot be projected from authenticated physical state without its validated preimage"
            ))
        } else {
            ArgentError::new(format!("state `{}` has no field `{field_name}`", self.access.source.as_str()))
        }
    }

    pub(in crate::compiler::codegen) fn expanded_field_ast(&self, field_name: &str, component_name: &str) -> Option<SilExpr<'static>> {
        let source = self.access.fields.iter().find(|field| field.name == field_name && field.trusted_storage.is_some())?;
        let SilExprKind::StructLiteral { fields, .. } = &source.ast_value.as_ref()?.kind else { return None };
        fields.iter().find(|field| field.name == component_name).map(|field| field.expr.clone())
    }

    pub(in crate::compiler::codegen) fn range_field_ast<'i>(
        &self,
        handle: &str,
        index: SilExpr<'i>,
        field_name: &str,
    ) -> Option<SilExpr<'i>> {
        self.access.fields.iter().find(|field| field.name == field_name && field.ast_value.is_some())?;
        let span = SilSpan::default();
        if self.has_complete_range_cache() {
            let item = SilExpr::new(
                SilExprKind::ArrayIndex {
                    source: Box::new(SilExpr::identifier(hidden_consumed_input_authored_cache_name(handle))),
                    index: Box::new(index),
                },
                span,
            );
            Some(SilExpr::new(
                SilExprKind::FieldAccess { source: Box::new(item), field: field_name.to_string(), field_span: span },
                span,
            ))
        } else {
            Some(SilExpr::new(
                SilExprKind::ArrayIndex {
                    source: Box::new(SilExpr::identifier(hidden_consumed_input_field_cache_name(handle, field_name))),
                    index: Box::new(index),
                },
                span,
            ))
        }
    }

    pub(in crate::compiler::codegen) fn complete_authored_ast(&self) -> Option<SilExpr<'static>> {
        if let Some(complete) = &self.access.complete {
            return crate::compiler::naming::is_identifier(complete).then(|| SilExpr::identifier(complete.clone()));
        }
        let span = SilSpan::default();
        let fields = self
            .access
            .fields
            .iter()
            .map(|field| Some(SilStateFieldExpr { name: field.name.clone(), expr: field.ast_value.clone()?, span, name_span: span }))
            .collect::<Option<Vec<_>>>()?;
        Some(SilExpr::new(SilExprKind::StructLiteral { name: self.access.authored_sil_type.clone(), fields, name_span: span }, span))
    }

    pub(in crate::compiler::codegen) fn complete_range_item_ast<'i>(&self, handle: &str, index: SilExpr<'i>) -> Option<SilExpr<'i>> {
        self.has_complete_range_cache().then(|| {
            SilExpr::new(
                SilExprKind::ArrayIndex {
                    source: Box::new(SilExpr::identifier(hidden_consumed_input_authored_cache_name(handle))),
                    index: Box::new(index),
                },
                SilSpan::default(),
            )
        })
    }

    pub(in crate::compiler::codegen) fn authored_payload_digest_ast(
        &self,
        model: &AppCompilationContext<'_>,
    ) -> Option<SilExpr<'static>> {
        let storage = model.storage_state_by_source(&self.access.source).ok()?;
        let mut parts = Vec::new();
        for field in self.access.source_to_storage.fields() {
            let stored = storage.fields.iter().find(|candidate| candidate.name == field.storage().field())?;
            let source = self.access.fields.iter().find(|candidate| candidate.name == field.source().field())?;
            let value = match field.expanded_state() {
                Some(_) => SilExpr::identifier(
                    source.trusted_storage.as_ref().filter(|value| crate::compiler::naming::is_identifier(value))?.clone(),
                ),
                None => source.ast_value.clone()?,
            };
            parts.push(packed_field_ast(&stored.ty, value)?);
        }
        let span = SilSpan::default();
        let bytes = parts
            .into_iter()
            .reduce(|left, right| {
                SilExpr::new(
                    SilExprKind::Binary { op: silverscript_lang::ast::BinaryOp::Add, left: Box::new(left), right: Box::new(right) },
                    span,
                )
            })
            .unwrap_or_else(|| SilExpr::bytes(Vec::new()));
        Some(SilExpr::call("blake3", vec![SilExpr::call("byte[]", vec![bytes])]))
    }

    fn has_complete_range_cache(&self) -> bool {
        self.access.fields.iter().all(|field| field.ast_value.is_some())
    }

    pub(in crate::compiler::codegen) fn range_cache_declarations(&self, handle: &str) -> Vec<SilStatement<'static>> {
        let span = SilSpan::default();
        if self.has_complete_range_cache() {
            return vec![SilStatement::VariableDefinition {
                type_ref: SilTypeRef {
                    base: SilTypeBase::Custom(self.access.authored_sil_type.clone()),
                    array_dims: vec![SilArrayDim::Dynamic],
                },
                modifiers: Vec::new(),
                name: hidden_consumed_input_authored_cache_name(handle),
                expr: None,
                span,
                type_span: span,
                modifier_spans: Vec::new(),
                name_span: span,
            }];
        }
        self.access
            .fields
            .iter()
            .filter(|field| field.ast_value.is_some())
            .map(|field| {
                let mut type_ref = field.sil_type_ref.clone();
                type_ref.array_dims.push(SilArrayDim::Dynamic);
                SilStatement::VariableDefinition {
                    type_ref,
                    modifiers: Vec::new(),
                    name: hidden_consumed_input_field_cache_name(handle, &field.name),
                    expr: None,
                    span,
                    type_span: span,
                    modifier_spans: Vec::new(),
                    name_span: span,
                }
            })
            .collect()
    }

    pub(in crate::compiler::codegen) fn range_cache_append(&self, handle: &str) -> Result<Vec<SilStatement<'static>>> {
        let span = SilSpan::default();
        if self.has_complete_range_cache() {
            let cache = hidden_consumed_input_authored_cache_name(handle);
            let value = if let Some(complete) = &self.access.complete {
                SilExpr::identifier(complete.clone())
            } else {
                let fields = self
                    .access
                    .fields
                    .iter()
                    .map(|field| {
                        Ok(SilStateFieldExpr {
                            name: field.name.clone(),
                            expr: field.ast_value.clone().ok_or_else(|| {
                                ArgentError::new(format!("authenticated range field `{}` has no AST projection", field.name))
                            })?,
                            span,
                            name_span: span,
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                SilExpr::new(SilExprKind::StructLiteral { name: self.access.authored_sil_type.clone(), fields, name_span: span }, span)
            };
            return Ok(vec![SilStatement::Assign {
                name: cache.clone(),
                expr: SilExpr::new(
                    SilExprKind::Append { source: Box::new(SilExpr::identifier(cache)), args: vec![value], span },
                    span,
                ),
                span,
                name_span: span,
            }]);
        }
        self.access
            .fields
            .iter()
            .filter(|field| field.ast_value.is_some())
            .map(|field| {
                let cache = hidden_consumed_input_field_cache_name(handle, &field.name);
                let value = field
                    .ast_value
                    .clone()
                    .ok_or_else(|| ArgentError::new(format!("authenticated range field `{}` has no AST projection", field.name)))?;
                Ok(SilStatement::Assign {
                    name: cache.clone(),
                    expr: SilExpr::new(
                        SilExprKind::Append { source: Box::new(SilExpr::identifier(cache)), args: vec![value], span },
                        span,
                    ),
                    span,
                    name_span: span,
                })
            })
            .collect()
    }
}

/// All compiler-planned input references for one emitted entry.
pub(in crate::compiler::codegen) struct EntryInputReferencePlan {
    references: Vec<PlannedEntryInputReference>,
    active: EntryInputReferenceId,
    consumed: BTreeMap<InteractionId, EntryInputReferenceId>,
    observed: BTreeMap<InteractionId, EntryInputReferenceId>,
    reference_uses: BTreeMap<(usize, usize), EntryInputReferenceId>,
}

impl EntryInputReferencePlan {
    pub(in crate::compiler::codegen) fn reference_ast(&self, expr: &SilExpr<'_>) -> Option<&PlannedEntryInputReference> {
        if !matches!(expr.kind, SilExprKind::Identifier(_) | SilExprKind::FieldAccess { .. }) {
            return None;
        }
        self.reference_uses.get(&(expr.span.start(), expr.span.end())).and_then(|id| self.reference(*id))
    }

    fn reference(&self, id: EntryInputReferenceId) -> Option<&PlannedEntryInputReference> {
        self.references.get(id.0).filter(|reference| reference.id == id)
    }

    pub(in crate::compiler::codegen) fn active(&self) -> &PlannedEntryInputReference {
        self.reference(self.active).expect("entry input reference plan retains its active input")
    }

    pub(in crate::compiler::codegen) fn consumed(&self, interaction: InteractionId) -> Result<&PlannedEntryInputReference> {
        self.consumed
            .get(&interaction)
            .and_then(|id| self.reference(*id))
            .ok_or_else(|| ArgentError::new(format!("missing consumed input reference `{interaction:?}`")))
    }

    pub(in crate::compiler::codegen) fn observed(&self, interaction: InteractionId) -> Result<&PlannedEntryInputReference> {
        self.observed
            .get(&interaction)
            .and_then(|id| self.reference(*id))
            .ok_or_else(|| ArgentError::new(format!("missing observed input reference `{interaction:?}`")))
    }
}

pub(in crate::compiler::codegen) fn plan_entry_input_references(
    entry_id: EntryId,
    actor: &ActorDecl,
    entry: &EntryDecl,
    model: &AppCompilationContext<'_>,
    state_values: &StateValueTypes,
) -> Result<EntryInputReferencePlan> {
    let actor_id = entry_id.actor;
    let lowering = model.state_lowering_by_id(actor_id)?;
    let semantic = model.input_plan_by_id(entry_id)?;
    if semantic.active().origin != InputReferenceOrigin::Active {
        return Err(ArgentError::new("active input materialization has the wrong semantic origin"));
    }
    let active = active_input_reference(actor_id, actor, semantic.active(), model, lowering, state_values)?;
    if active.physical_target() != &semantic.active().target {
        return Err(ArgentError::new("active input materialization differs from its semantic target"));
    }
    let mut references = vec![active];
    let mut consumed = BTreeMap::new();
    for interaction in model.entry_model_by_id(entry_id)?.current().inputs() {
        let InteractionSource::Consume(consume) = interaction.source() else { unreachable!("current input has consume source") };
        let requirement = semantic.consumed(interaction.id())?;
        if requirement.origin != InputReferenceOrigin::Consumed(interaction.id()) {
            return Err(ArgentError::new("consumed input materialization differs from its semantic origin"));
        }
        let target =
            lowering.target(&requirement.target).ok_or_else(|| ArgentError::new("input requirement has no physical target"))?;
        let proof = if requirement.authentication == InputAuthentication::CovenantDomain {
            InputTemplateProof::CovenantDomain
        } else {
            let target_id = interaction
                .target()
                .single_static_actor()
                .ok_or_else(|| ArgentError::new("consumed input has no bound actor identity"))?;
            let witness = model.witness_plan_by_id(entry_id)?.template(target_id).ok_or_else(|| {
                ArgentError::new(format!(
                    "entry `{}::{}` has no template witness plan for consumed actor `{}`",
                    actor.name, entry.name, consume.actor
                ))
            })?;
            let (prefix_len, suffix_len) = if witness.form == TemplateWitnessForm::Bytes {
                (
                    InputTemplateLength::BytesWitness(hidden_witness_prefix_name(&witness.actor)),
                    InputTemplateLength::BytesWitness(hidden_witness_suffix_name(&witness.actor)),
                )
            } else {
                (
                    InputTemplateLength::IntWitness(hidden_witness_prefix_len_name(&witness.actor)),
                    InputTemplateLength::IntWitness(hidden_witness_suffix_len_name(&witness.actor)),
                )
            };
            let template = hidden_template_name(&witness.actor);
            InputTemplateProof::Template { prefix_len, suffix_len, template_ast: Box::new(SilExpr::identifier(template.clone())) }
        };
        if requirement.id.0 != references.len() {
            return Err(ArgentError::new("consumed input materialization differs from the planned reference order"));
        }
        let id = requirement.id;
        references.push(input_reference(
            InputReferenceSpec {
                id,
                reference: consume.name.clone(),
                physical_expr: hidden_consumed_input_state_name(&consume.name),
                input_index: InputIndexExpr { expr: SilExpr::identifier(hidden_input_idx_name(&consume.name)) },
                direct_authored_state: requirement.direct_authored_state,
            },
            proof,
            target,
            lowering,
            state_values,
            model,
            &requirement.fields,
        )?);
        consumed.insert(interaction.id(), id);
    }

    let mut observed = BTreeMap::new();
    for group in model.entry_model_by_id(entry_id)?.existing_groups() {
        let observe = group.observe().expect("existing group has observe declaration");
        for interaction in group.inputs() {
            let InteractionSource::ObserveInput(input) = interaction.source() else {
                unreachable!("observed input has its source declaration")
            };
            let reference = format!("{}.inputs.{}", observe.name, input.name);
            let physical_expr = hidden_observed_input_state_name(&observe.name, &input.name);
            let requirement = semantic.observed(interaction.id())?;
            if requirement.origin != InputReferenceOrigin::Observed(interaction.id()) {
                return Err(ArgentError::new("observed input materialization differs from its semantic origin"));
            }
            let target = lowering
                .target(&requirement.target)
                .ok_or_else(|| ArgentError::new("observed input requirement has no physical target"))?;
            let proof = if requirement.authentication == InputAuthentication::CovenantDomain {
                InputTemplateProof::CovenantDomain
            } else {
                let spec = requirement
                    .observed_witness
                    .as_ref()
                    .ok_or_else(|| ArgentError::new("observed input requirement has no witness descriptor"))?;
                let target_reference = match &spec.template_source {
                    ObservedTemplateSource::FixedInApp(id) => Some(model.types.display_names[id].clone()),
                    ObservedTemplateSource::FixedLinked(id) => Some(format!("{}::{}", id.app, id.actor)),
                    _ => None,
                };
                let template_ast = match &spec.template_source {
                    ObservedTemplateSource::FixedInApp(id) => {
                        SilExpr::identifier(hidden_template_name(&model.types.display_names[id]))
                    }
                    ObservedTemplateSource::FixedLinked(id) => {
                        SilExpr::identifier(hidden_imported_template_name(&ImportedTemplateSpec::from_linked(
                            model
                                .linked_actors
                                .get(id)
                                .ok_or_else(|| ArgentError::new("observed input has no planned fixed template target"))?,
                        )))
                    }
                    ObservedTemplateSource::DynamicBinding => SilExpr::identifier(input.actor.clone()),
                    ObservedTemplateSource::ActorTypeValue => match spec.source.as_ref() {
                        Some(ClauseActorTypeRef::StateField { field, .. }) => {
                            references[0].project_field_ast(field.field()).ok_or_else(|| {
                                ArgentError::new(format!("observed actor type has no authenticated active field `{}`", field.field()))
                            })?
                        }
                        Some(ClauseActorTypeRef::EntryArgument { name, .. }) => SilExpr::identifier(name.clone()),
                        None => return Err(ArgentError::new("observed actor type has no planned value source")),
                    },
                    ObservedTemplateSource::Witness => SilExpr::identifier(hidden_observed_actor_template_name(spec)),
                };
                InputTemplateProof::Template {
                    prefix_len: InputTemplateLength::IntWitness(
                        target_reference
                            .as_deref()
                            .map_or_else(|| hidden_observed_actor_prefix_len_name(spec), hidden_witness_prefix_len_name),
                    ),
                    suffix_len: InputTemplateLength::IntWitness(
                        target_reference
                            .as_deref()
                            .map_or_else(|| hidden_observed_actor_suffix_len_name(spec), hidden_witness_suffix_len_name),
                    ),
                    template_ast: Box::new(template_ast),
                }
            };
            if requirement.id.0 != references.len() {
                return Err(ArgentError::new("observed input materialization differs from the planned reference order"));
            }
            let id = requirement.id;
            references.push(input_reference(
                InputReferenceSpec {
                    id,
                    reference,
                    physical_expr,
                    input_index: InputIndexExpr {
                        expr: SilExpr::identifier(hidden_observed_input_idx_name(&observe.name, &input.name)),
                    },
                    direct_authored_state: requirement.direct_authored_state,
                },
                proof,
                target,
                lowering,
                state_values,
                model,
                &requirement.fields,
            )?);
            observed.insert(interaction.id(), id);
        }
    }
    Ok(EntryInputReferencePlan {
        references,
        active: EntryInputReferenceId(0),
        consumed,
        observed,
        reference_uses: semantic.reference_uses.clone(),
    })
}

fn active_input_reference(
    actor_id: DeclId,
    actor: &ActorDecl,
    requirement: &InputReferenceRequirement,
    model: &AppCompilationContext<'_>,
    lowering: &ContractStateLowering,
    state_values: &StateValueTypes,
) -> Result<PlannedEntryInputReference> {
    let target = lowering
        .target(&requirement.target)
        .ok_or_else(|| ArgentError::new(format!("actor `{}` has no active input reference target", actor.name)))?;
    let authored_sil_type = lowering
        .source_representation(target.source())
        .ok_or_else(|| ArgentError::new("active input source has no authored representation plan"))
        .and_then(|representation| render_sil_state_type(representation.sil_type()))?;
    let expansion_specs = model.state_expansion_witnesses_by_id(actor_id)?;
    let fields = target
        .source_fields()?
        .into_iter()
        .map(|field| {
            let name = field.source().field().to_string();
            let sil_type_ref = source_field_sil_type(field.source(), state_values, model)?;
            let availability = requirement.fields.get(field.source());
            let (ast_value, trusted_storage) = match availability {
                Some(InputFieldAvailability::Direct) if field.is_identity() => (Some(SilExpr::identifier(name.clone())), None),
                Some(InputFieldAvailability::CheckedPreimage) if !field.is_identity() => {
                    let spec = expansion_specs
                        .iter()
                        .find(|spec| spec.field_id == *field.source())
                        .ok_or_else(|| ArgentError::new(format!("active expanded field `{name}` has no validated opening plan")))?;
                    let sil_type = lowering
                        .source_representation(&spec.memory_source)
                        .ok_or_else(|| {
                            ArgentError::new(format!("expanded state `{}` has no authored representation plan", spec.memory_state))
                        })
                        .and_then(|representation| render_sil_state_type(representation.sil_type()))?;
                    let fields = model
                        .state_by_source(&spec.memory_source)?
                        .fields
                        .iter()
                        .map(|field| (field.name.clone(), hidden_state_expansion_field_name(spec, &field.name)))
                        .collect::<Vec<_>>();
                    let span = SilSpan::default();
                    let ast_fields = fields
                        .iter()
                        .map(|(name, binding)| SilStateFieldExpr {
                            name: name.clone(),
                            expr: SilExpr::identifier(binding.clone()),
                            span,
                            name_span: span,
                        })
                        .collect();
                    let ast_value =
                        SilExpr::new(SilExprKind::StructLiteral { name: sil_type.clone(), fields: ast_fields, name_span: span }, span);
                    (Some(ast_value), Some(name.clone()))
                }
                Some(InputFieldAvailability::Unavailable) => (None, None),
                Some(_) | None => {
                    return Err(ArgentError::new(format!("active input field `{name}` differs from its completed availability plan")));
                }
            };
            Ok(PlannedSourceField { name, sil_type_ref, ast_value, trusted_storage })
        })
        .collect::<Result<Vec<_>>>()?;
    let access = SourceStateAccess {
        source: target.source().clone(),
        source_to_storage: target.source_to_storage().clone(),
        authored_sil_type,
        complete: None,
        fields,
        target: target.id().clone(),
    };
    Ok(PlannedEntryInputReference {
        id: EntryInputReferenceId(0),
        reference: word::SELF.to_string(),
        input_index: InputIndexExpr {
            expr: SilExpr::new(SilExprKind::Introspection(SilIntrospectionKind::ActiveInputIndex), SilSpan::default()),
        },
        access,
        physical: None,
    })
}

fn input_reference(
    spec: InputReferenceSpec,
    proof: InputTemplateProof,
    target: &TargetPhysicalPlan,
    lowering: &ContractStateLowering,
    state_values: &StateValueTypes,
    model: &AppCompilationContext<'_>,
    field_availability: &BTreeMap<SourceFieldId, InputFieldAvailability>,
) -> Result<PlannedEntryInputReference> {
    let InputReferenceSpec { id, reference, physical_expr, input_index, direct_authored_state } = spec;
    let fields = target.source_fields()?;
    let physical_sil_type = render_sil_state_type(target.sil_type())?;
    let authored_sil_type = lowering
        .source_representation(target.source())
        .ok_or_else(|| ArgentError::new("input target source has no authored representation plan"))
        .and_then(|representation| render_sil_state_type(representation.sil_type()))?;
    let physical = AuthenticatedPhysicalInput {
        expr: physical_expr.clone(),
        sil_type: physical_sil_type.clone(),
        input_index: input_index.clone(),
        proof,
    };
    if direct_authored_state && physical_sil_type != authored_sil_type {
        return Err(ArgentError::new("direct authored input type differs from its completed model plan"));
    }
    let planned_fields = fields
        .iter()
        .map(|field| {
            let name = field.source().field().to_string();
            let sil_type_ref = source_field_sil_type(field.source(), state_values, model)?;
            let availability = field_availability
                .get(field.source())
                .ok_or_else(|| ArgentError::new(format!("input field `{name}` has no completed availability plan")))?;
            let direct = match availability {
                InputFieldAvailability::Direct if field.is_identity() => true,
                InputFieldAvailability::Unavailable => false,
                InputFieldAvailability::Direct | InputFieldAvailability::CheckedPreimage => {
                    return Err(ArgentError::new(format!("input field `{name}` differs from its completed availability plan")));
                }
            };
            Ok(PlannedSourceField {
                name,
                sil_type_ref,
                ast_value: direct.then(|| {
                    SilExpr::new(
                        SilExprKind::FieldAccess {
                            source: Box::new(SilExpr::identifier(physical_expr.clone())),
                            field: field.sil_name().to_string(),
                            field_span: SilSpan::default(),
                        },
                        SilSpan::default(),
                    )
                }),
                trusted_storage: None,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let access = SourceStateAccess {
        source: target.source().clone(),
        source_to_storage: target.source_to_storage().clone(),
        authored_sil_type,
        complete: direct_authored_state.then(|| physical_expr.clone()),
        fields: planned_fields,
        target: target.id().clone(),
    };
    Ok(PlannedEntryInputReference { id, reference, input_index, access, physical: Some(physical) })
}

fn source_field_sil_type(
    field: &SourceFieldId,
    state_values: &StateValueTypes,
    model: &AppCompilationContext<'_>,
) -> Result<SilTypeRef> {
    let source = field.state();
    let field_name = field.field();
    let storage_source = model.storage_source_id(source);
    let storage_state = model.state_by_source(storage_source)?;
    let (field_index, storage_field) = storage_state
        .fields
        .iter()
        .enumerate()
        .find(|(_, field)| field.name == field_name)
        .ok_or_else(|| ArgentError::new(format!("state `{}` has no source field `{field_name}`", source.as_str())))?;
    let type_ref = if let Some(value) = state_values.field_value(field) {
        state_values.sil_type_ref(value)
    } else if let Some(storage_id) = model.state_decl_id_by_source(storage_source) {
        let resolved =
            model.types.state_fields.get(&(storage_id, field_index)).ok_or_else(|| {
                ArgentError::new(format!("state `{}` field `{field_name}` has no resolved type", storage_state.name))
            })?;
        if matches!(resolved.base, ResolvedTypeBase::State(_)) {
            return Err(ArgentError::new(format!(
                "state `{}` field `{field_name}` has no completed state-value plan",
                source.as_str()
            )));
        }
        super::state_types::lower_bound_type(&storage_field.ty, resolved)
    } else if model.linked_field_sources.contains_key(&SourceFieldId::new(storage_source.clone(), field_name)) {
        return Err(ArgentError::new(format!("state `{}` field `{field_name}` has no completed state-value plan", source.as_str())));
    } else {
        super::state_types::lower_planned_type(&storage_field.ty, false)
    };
    Ok(type_ref)
}

pub(in crate::compiler::codegen) fn render_sil_state_type(ty: &SilStateType) -> Result<String> {
    Ok(match ty {
        SilStateType::State => "State".to_string(),
        SilStateType::Source(source) => source.as_str().to_string(),
        SilStateType::StoragePhysical(source) => hidden_storage_state_type_name(source.as_str()),
        SilStateType::TargetPhysical(PhysicalTargetId::Actor(actor)) => hidden_actor_state_type_name(actor.actor()),
        SilStateType::TargetPhysical(PhysicalTargetId::OpenState(state)) => hidden_storage_state_type_name(state.as_str()),
        SilStateType::TargetPhysical(PhysicalTargetId::ActorDomain { .. }) => {
            return Err(ArgentError::new("actor-domain physical types are not valid input read targets"));
        }
    })
}

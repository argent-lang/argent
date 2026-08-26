//! Typed boundary between authored state and physical contract state.
//!
//! Plans authenticated input projection, successor materialization, and output
//! template proof without exposing compiler-owned fields to authored values.

use std::collections::BTreeMap;

use crate::compiler::model::{
    CompilerRouteTransition, ContractStateLowering, GeneratedFieldId, Model, OutputPhysicalTypePlan, PhysicalFieldId,
    PhysicalStateLayout, PhysicalTargetId, SilStateType, SourceStateId, SourceStorageRelation, StaticActorTarget, TargetPhysicalPlan,
    TemplateSelector,
};
use crate::compiler::syntax::{ActorDecl, EntryDecl, ObserveDecl, ObservedActorDecl, word};
use crate::error::{ArgentError, Result};

use super::super::emitter::*;
use super::token_refs::count_qualified_ref;

#[cfg(test)]
mod tests;

/// Output layout, rendered type, and compiler-owned field sources selected as one plan.
pub(in crate::compiler::codegen) struct OutputStateTarget {
    #[cfg(test)]
    canonical_target: PhysicalTargetId,
    sil_type: String,
    authored_sil_type: String,
    named_physical_layout: Option<PhysicalStateLayout>,
    physical: TargetPhysicalPlan,
    generated_fields: BTreeMap<GeneratedFieldId, String>,
}

impl OutputStateTarget {
    pub(in crate::compiler::codegen) fn physical_type(&self) -> &str {
        &self.sil_type
    }

    pub(in crate::compiler::codegen) fn named_physical_layout(&self) -> Option<(&str, &PhysicalStateLayout)> {
        self.named_physical_layout.as_ref().map(|layout| (self.sil_type.as_str(), layout))
    }

    pub(in crate::compiler::codegen) fn source_identity(&self) -> &str {
        self.physical.source().as_str()
    }

    pub(in crate::compiler::codegen) fn authored_sil_type(&self) -> &str {
        &self.authored_sil_type
    }

    pub(in crate::compiler::codegen) fn require_authored_value(
        &self,
        lower: impl FnOnce(&str) -> Result<String>,
    ) -> Result<AuthoredStateExpr> {
        Ok(AuthoredStateExpr {
            source: self.physical.source().clone(),
            sil_type: self.authored_sil_type.clone(),
            sil: lower(&self.authored_sil_type)?,
        })
    }

    #[cfg(test)]
    pub(in crate::compiler::codegen) fn authored_value(&self, sil: impl Into<String>) -> AuthoredStateExpr {
        AuthoredStateExpr { source: self.physical.source().clone(), sil_type: self.authored_sil_type.clone(), sil: sil.into() }
    }

    #[cfg(test)]
    pub(in crate::compiler::codegen) fn target(&self) -> &PhysicalTargetId {
        self.physical.id()
    }

    #[cfg(test)]
    pub(in crate::compiler::codegen) fn canonical_target(&self) -> &PhysicalTargetId {
        &self.canonical_target
    }
}

pub(in crate::compiler::codegen) fn plan_actor_output_state(
    actor: &ActorDecl,
    target_actor: &str,
    model: &Model<'_>,
) -> Result<OutputStateTarget> {
    let lowering = model.state_lowering(&actor.name)?;
    let target = lowering
        .output_type_for_actor(target_actor)
        .ok_or_else(|| ArgentError::new(format!("actor `{}` has no output state target plan for `{target_actor}`", actor.name)))?;
    let transition = output_transition_for_actor(actor, target_actor, model)?;
    output_state_target(actor, target, lowering, &transition, model)
}

pub(in crate::compiler::codegen) fn plan_static_actor_output_state(
    actor: &ActorDecl,
    target_actor: StaticActorTarget<'_>,
    model: &Model<'_>,
) -> Result<OutputStateTarget> {
    let lowering = model.state_lowering(&actor.name)?;
    let (target, transition) = match target_actor {
        StaticActorTarget::InApp(target) => {
            (lowering.output_type_for_actor(&target.name), output_transition_for_actor(actor, &target.name, model)?)
        }
        StaticActorTarget::CrossApp(target) => {
            (lowering.output_type_for_compiled_actor(&target.app, &target.actor), CompilerRouteTransition::default())
        }
    };
    let target = target.ok_or_else(|| {
        ArgentError::new(format!("actor `{}` has no output state target plan for `{}`", actor.name, target_actor.artifact_reference()))
    })?;
    output_state_target(actor, target, lowering, &transition, model)
}

pub(in crate::compiler::codegen) fn plan_open_output_state(
    actor: &ActorDecl,
    state: &str,
    model: &Model<'_>,
) -> Result<OutputStateTarget> {
    let lowering = model.state_lowering(&actor.name)?;
    let target = lowering
        .output_type_for_open_state(&SourceStateId::new(state))
        .ok_or_else(|| ArgentError::new(format!("actor `{}` has no open output state target plan for `{state}`", actor.name)))?;
    output_state_target(actor, target, lowering, &CompilerRouteTransition::default(), model)
}

pub(in crate::compiler::codegen) fn plan_selector_output_state(
    actor: &ActorDecl,
    selector: &TemplateSelector,
    model: &Model<'_>,
) -> Result<OutputStateTarget> {
    let lowering = model.state_lowering(&actor.name)?;
    let target = lowering.output_type_for_actor_domain(&SourceStateId::new(&selector.state), &selector.variants).ok_or_else(|| {
        ArgentError::new(format!("actor `{}` has no output state target plan for selector `{}`", actor.name, selector.name))
    })?;
    let mut transitions = selector.variants.iter().map(|variant| output_transition_for_actor(actor, variant, model));
    let transition = transitions
        .next()
        .transpose()?
        .ok_or_else(|| ArgentError::new(format!("actor selector `{}` has no variants", selector.name)))?;
    for candidate in transitions {
        if candidate? != transition {
            return Err(ArgentError::new(format!("actor selector `{}` variants do not share one route transition", selector.name)));
        }
    }
    output_state_target(actor, target, lowering, &transition, model)
}

fn output_transition_for_actor(source_actor: &ActorDecl, target_actor: &str, model: &Model<'_>) -> Result<CompilerRouteTransition> {
    if target_actor == source_actor.name || model.linked_actor(target_actor).is_some() {
        return Ok(CompilerRouteTransition::default());
    }
    model.route_transition(&source_actor.name, target_actor).cloned().ok_or_else(|| {
        ArgentError::new(format!("entry model has no route transition from `{}` to in-app target `{target_actor}`", source_actor.name))
    })
}

fn output_state_target(
    source_actor: &ActorDecl,
    plan: &OutputPhysicalTypePlan,
    lowering: &ContractStateLowering,
    transition: &CompilerRouteTransition,
    model: &Model<'_>,
) -> Result<OutputStateTarget> {
    let physical =
        lowering.target(plan.target()).ok_or_else(|| ArgentError::new("output target has no physical layout plan"))?.clone();
    let authored_sil_type = lowering
        .source_representation(physical.source())
        .ok_or_else(|| ArgentError::new("output target source has no authored representation plan"))
        .and_then(|representation| render_sil_state_type(representation.sil_type()))?;
    let named_physical_layout = match plan.sil_type() {
        SilStateType::State | SilStateType::Source(_) => None,
        SilStateType::StoragePhysical(_) | SilStateType::TargetPhysical(_) => Some(
            lowering
                .target(plan.canonical_target())
                .ok_or_else(|| ArgentError::new("output type owner has no physical target layout"))?
                .physical()
                .clone(),
        ),
    };
    let generated_fields = plan_generated_fields(source_actor, &physical, transition, model)?;
    Ok(OutputStateTarget {
        #[cfg(test)]
        canonical_target: plan.canonical_target().clone(),
        sil_type: render_sil_state_type(plan.sil_type())?,
        authored_sil_type,
        named_physical_layout,
        physical,
        generated_fields,
    })
}

fn plan_generated_fields(
    source_actor: &ActorDecl,
    target: &TargetPhysicalPlan,
    transition: &CompilerRouteTransition,
    model: &Model<'_>,
) -> Result<BTreeMap<GeneratedFieldId, String>> {
    let mut families_to_pack = transition.families_to_pack.clone();
    let generated = target
        .storage_to_physical()
        .generated_fields()
        .iter()
        .map(|id| {
            let field = target
                .physical()
                .field(&PhysicalFieldId::Generated(id.clone()))
                .ok_or_else(|| ArgentError::new("generated output field is missing from its physical layout"))?;
            let expr = match id {
                GeneratedFieldId::Template(_) | GeneratedFieldId::RouteFamilyTable { .. } => field.sil_name().to_string(),
                GeneratedFieldId::RouteFamilyDigest { family, .. } => {
                    let Some(index) = families_to_pack.iter().position(|candidate| candidate == family) else {
                        return Ok((id.clone(), field.sil_name().to_string()));
                    };
                    families_to_pack.remove(index);
                    let family = model
                        .route_family(family)
                        .ok_or_else(|| ArgentError::new(format!("output transition references unknown route family `{family}`")))?;
                    if model.route_family_for_actor(&source_actor.name).is_some_and(|source_family| source_family.id == family.id) {
                        format!("blake3(byte[]({}))", hidden_route_family_table_name(family))
                    } else {
                        let preimage =
                            family.table_actors().iter().map(|actor| hidden_template_name(actor)).collect::<Vec<_>>().join(" + ");
                        format!("blake3(byte[]({preimage}))")
                    }
                }
            };
            Ok((id.clone(), expr))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    if let Some(family) = families_to_pack.first() {
        return Err(ArgentError::new(format!("output transition packs route family `{family}` without a generated target field")));
    }
    Ok(generated)
}

#[derive(Clone)]
enum InputTemplateProof {
    CovenantDomain,
    Template { prefix_len: String, suffix_len: String, template: String },
}

#[derive(Clone)]
struct AuthenticatedPhysicalInput {
    expr: String,
    sil_type: String,
    input_index: String,
    proof: InputTemplateProof,
}

impl AuthenticatedPhysicalInput {
    fn emit_read(&self, out: &mut String, indent: usize) {
        let mut args = vec![self.input_index.clone()];
        let builtin = match &self.proof {
            InputTemplateProof::CovenantDomain => "readInputState",
            InputTemplateProof::Template { prefix_len, suffix_len, template } => {
                args.extend([prefix_len.clone(), suffix_len.clone(), template.clone()]);
                "readInputStateWithTemplate"
            }
        };
        push_generated_call(out, indent, &format!("{} {} = ", self.sil_type, self.expr), builtin, &args);
    }
}

#[derive(Clone)]
enum PlannedSourceExpr {
    Value(String),
    Struct { sil_type: String, fields: Vec<(String, String)> },
}

struct PlannedSourceStorageExpr {
    authored: PlannedSourceExpr,
    trusted_storage: Option<String>,
}

impl PlannedSourceExpr {
    fn render(&self, indent: usize) -> String {
        match self {
            Self::Value(expr) => expr.clone(),
            Self::Struct { sil_type, fields } => {
                let field_indent = " ".repeat(indent + 4);
                let close_indent = " ".repeat(indent);
                let mut out = format!("{sil_type} {{\n");
                if !fields.is_empty() {
                    out.push_str(&format!("{field_indent}// :: user declared fields\n"));
                }
                for (field, expr) in fields {
                    out.push_str(&format!("{field_indent}{field}: {expr},\n"));
                }
                out.push_str(&close_indent);
                out.push('}');
                out
            }
        }
    }

    fn payload_digest_in_contract(
        &self,
        state: &SourceStateId,
        lowering: &ContractStateLowering,
        model: &Model<'_>,
    ) -> Result<String> {
        let relation = lowering
            .source_representation(state)
            .ok_or_else(|| ArgentError::new(format!("state `{}` has no source-to-storage plan", state.as_str())))?
            .source_to_storage();
        source_storage_payload_digest(state, relation, lowering, model, |field| {
            Ok(PlannedSourceStorageExpr { authored: self.project_field(field)?, trusted_storage: None })
        })
    }

    fn project_field(&self, field_name: &str) -> Result<Self> {
        match self {
            Self::Value(expr) => Ok(Self::Value(format!("{expr}.{field_name}"))),
            Self::Struct { fields, .. } => fields
                .iter()
                .find_map(|(name, value)| (name == field_name).then(|| Self::Value(value.clone())))
                .ok_or_else(|| ArgentError::new(format!("validated state value is missing field `{field_name}`"))),
        }
    }
}

/// Pack one authored value through its typed storage relation, then hash it.
fn source_storage_payload_digest(
    state: &SourceStateId,
    relation: &SourceStorageRelation,
    lowering: &ContractStateLowering,
    model: &Model<'_>,
    mut source_field: impl FnMut(&str) -> Result<PlannedSourceStorageExpr>,
) -> Result<String> {
    let storage = model.storage_state(state.as_str())?;
    let mut parts = Vec::with_capacity(relation.fields().len());
    for field in relation.fields() {
        let storage_field = storage
            .fields
            .iter()
            .find(|candidate| candidate.name == field.storage().field())
            .ok_or_else(|| ArgentError::new("planned source field has no storage field"))?;
        let PlannedSourceStorageExpr { authored, trusted_storage } = source_field(field.source().field())?;
        let stored = match (field.expanded_state(), trusted_storage) {
            (Some(_), Some(storage)) => storage,
            (Some(expanded), None) => authored.payload_digest_in_contract(expanded, lowering, model)?,
            (None, _) => authored.render(0),
        };
        parts.push(packed_field_expr(&storage_field.ty, &stored)?);
    }
    let bytes = if parts.is_empty() { "0x".to_string() } else { parts.join(" + ") };
    Ok(format!("blake3(byte[]({bytes}))"))
}

/// Hash a named or previously stabilized authored expression.
pub(in crate::compiler::codegen) fn authored_state_payload_digest_expr(
    state: &SourceStateId,
    value_expr: &str,
    lowering: &ContractStateLowering,
    model: &Model<'_>,
) -> Result<String> {
    PlannedSourceExpr::Value(value_expr.to_string()).payload_digest_in_contract(state, lowering, model)
}

#[derive(Clone)]
struct PlannedSourceField {
    name: String,
    value: Option<PlannedSourceExpr>,
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

/// A complete expression in one nominal authored state representation.
#[derive(Debug)]
pub(in crate::compiler::codegen) struct AuthoredStateExpr {
    source: SourceStateId,
    sil_type: String,
    sil: String,
}

impl AuthoredStateExpr {
    pub(in crate::compiler::codegen) fn source(&self) -> &SourceStateId {
        &self.source
    }

    pub(in crate::compiler::codegen) fn into_sil(self) -> String {
        self.sil
    }

    pub(in crate::compiler::codegen) fn sil(&self) -> &str {
        &self.sil
    }

    pub(in crate::compiler::codegen) fn rebound(mut self, sil: impl Into<String>) -> Self {
        self.sil = sil.into();
        self
    }
}

/// A complete physical value accepted by an output-state builtin.
pub(in crate::compiler::codegen) struct PhysicalStateExpr {
    sil_type: String,
    sil: String,
    materialized: bool,
}

impl PhysicalStateExpr {
    fn into_argument(self, out: &mut String, indent: usize, binding: impl AsRef<str>) -> String {
        if !self.materialized {
            return self.sil;
        }
        let binding = binding.as_ref();
        push_indent(out, indent);
        out.push_str(&format!("{} {binding} = {};\n", self.sil_type, self.sil));
        binding.to_string()
    }
}

enum OutputTemplateProof {
    Current,
    BoundInput { input_index: String, prefix_len: String, suffix_len: String, template: String },
    Witnessed { prefix: String, suffix: String, template: String },
}

/// Physical output and template proof planned as independent validation inputs.
pub(in crate::compiler::codegen) struct PlannedOutputValidation {
    output_index: String,
    physical: PhysicalStateExpr,
    state_binding: String,
    proof: OutputTemplateProof,
}

/// Output validation whose physical value has already been stabilized.
pub(in crate::compiler::codegen) struct StabilizedOutputValidation {
    output_index: String,
    state_argument: String,
    proof: OutputTemplateProof,
}

impl PlannedOutputValidation {
    pub(in crate::compiler::codegen) fn stabilize(self, out: &mut String, indent: usize) -> StabilizedOutputValidation {
        StabilizedOutputValidation {
            output_index: self.output_index,
            state_argument: self.physical.into_argument(out, indent, self.state_binding),
            proof: self.proof,
        }
    }
}

impl StabilizedOutputValidation {
    pub(in crate::compiler::codegen) fn emit(self, out: &mut String, indent: usize) {
        let mut args = vec![self.output_index, self.state_argument];
        let builtin = match self.proof {
            OutputTemplateProof::Current => "validateOutputState",
            OutputTemplateProof::BoundInput { input_index, prefix_len, suffix_len, template } => {
                args.extend([input_index, prefix_len, suffix_len, template]);
                "validateOutputStateWithInputTemplate"
            }
            OutputTemplateProof::Witnessed { prefix, suffix, template } => {
                args.extend([prefix, suffix, template]);
                "validateOutputStateWithTemplate"
            }
        };
        push_generated_call(out, indent, "", builtin, &args);
    }
}

/// Lower one complete authored value through storage into its target layout.
pub(in crate::compiler::codegen) fn materialize_output_state(
    target: &OutputStateTarget,
    authored: AuthoredStateExpr,
    lowering: &ContractStateLowering,
    model: &Model<'_>,
    indent: usize,
) -> Result<PhysicalStateExpr> {
    if authored.source != *target.physical.source() {
        return Err(ArgentError::new(format!(
            "authored state `{}` cannot initialize physical target `{}`",
            authored.source.as_str(),
            target.physical.source().as_str()
        )));
    }
    if target.physical.source_to_storage().is_identity()
        && target.physical.storage_to_physical().is_identity()
        && target.sil_type == authored.sil_type
    {
        return Ok(PhysicalStateExpr { sil_type: target.sil_type.clone(), sil: authored.sil, materialized: false });
    }

    let mut physical_fields = BTreeMap::new();
    for field in target.physical.source_to_storage().fields() {
        let physical = target
            .physical
            .storage_to_physical()
            .physical_field(field.storage())
            .ok_or_else(|| ArgentError::new("authored output field has no physical target mapping"))?
            .clone();
        let source_expr = format!("{}.{}", authored.sil, field.source().field());
        let storage_expr = match field.expanded_state() {
            Some(expanded) => authored_state_payload_digest_expr(expanded, &source_expr, lowering, model)?,
            None => source_expr,
        };
        if physical_fields.insert(physical, storage_expr).is_some() {
            return Err(ArgentError::new("authored output fields map to the same physical target field"));
        }
    }
    for (id, expr) in &target.generated_fields {
        if physical_fields.insert(PhysicalFieldId::Generated(id.clone()), expr.clone()).is_some() {
            return Err(ArgentError::new("generated output field overlaps an authored storage field"));
        }
    }

    let field_indent = " ".repeat(indent + 4);
    let close_indent = " ".repeat(indent);
    let mut out = format!("{} {{\n", target.sil_type);
    let mut emitted_generated_header = false;
    let mut emitted_storage_header = false;
    for field in target.physical.physical().fields() {
        match field.id() {
            PhysicalFieldId::Generated(_) if !emitted_generated_header => {
                out.push_str(&format!("{field_indent}// :: generated fields\n"));
                emitted_generated_header = true;
            }
            PhysicalFieldId::Storage(_) if !emitted_storage_header => {
                out.push_str(&format!("{field_indent}// :: user declared fields\n"));
                emitted_storage_header = true;
            }
            PhysicalFieldId::Generated(_) | PhysicalFieldId::Storage(_) => {}
        }
        let expr = physical_fields
            .remove(field.id())
            .ok_or_else(|| ArgentError::new(format!("physical output field `{}` has no materialization source", field.sil_name())))?;
        out.push_str(&format!("{field_indent}{}: {expr},\n", field.sil_name()));
    }
    if !physical_fields.is_empty() {
        return Err(ArgentError::new("output materialization contains fields outside its physical target layout"));
    }
    out.push_str(&close_indent);
    out.push('}');
    Ok(PhysicalStateExpr { sil_type: target.sil_type.clone(), sil: out, materialized: true })
}

/// Resolved route context from which output authentication is planned.
pub(in crate::compiler::codegen) enum OutputValidationContext<'a, 'm> {
    Actor {
        target: &'a str,
    },
    Selector {
        selector: &'a str,
        template: String,
    },
    Observed {
        observe: &'a ObserveDecl,
        output: &'a ObservedActorDecl,
        static_target: Option<StaticActorTarget<'m>>,
        witness: &'a ObservedActorWitnessSpec,
        template: String,
    },
    Spawned {
        static_target: Option<StaticActorTarget<'m>>,
        witness: &'a SpawnActorWitnessSpec,
        template: String,
    },
}

pub(in crate::compiler::codegen) fn plan_output_validation(
    actor: &ActorDecl,
    entry: &EntryDecl,
    context: OutputValidationContext<'_, '_>,
    output_index: impl Into<String>,
    physical: PhysicalStateExpr,
    state_binding: impl Into<String>,
    model: &Model<'_>,
) -> Result<PlannedOutputValidation> {
    let proof = match context {
        OutputValidationContext::Actor { target } => {
            if target == actor.name {
                OutputTemplateProof::Current
            } else if let Some(input_index) = template_input_index_for_actor(actor, entry, target, model)? {
                OutputTemplateProof::BoundInput {
                    input_index,
                    prefix_len: hidden_witness_prefix_len_name(target),
                    suffix_len: hidden_witness_suffix_len_name(target),
                    template: hidden_template_name(target),
                }
            } else {
                OutputTemplateProof::Witnessed {
                    prefix: hidden_witness_prefix_name(target),
                    suffix: hidden_witness_suffix_name(target),
                    template: hidden_template_name(target),
                }
            }
        }
        OutputValidationContext::Selector { selector, template } => OutputTemplateProof::Witnessed {
            prefix: hidden_template_selector_prefix_name(selector),
            suffix: hidden_template_selector_suffix_name(selector),
            template,
        },
        OutputValidationContext::Observed { observe, output, static_target, witness, template } => {
            if let Some(proof) = fixed_output_template_proof(actor, entry, static_target, &template, model)? {
                proof
            } else if observed_reuses_input_template(observe, output) {
                let input = first_observed_input_for_actor(observe, &output.actor)
                    .expect("input-template reuse requires a matching observed input");
                let input_spec = observed_input_spec(actor, entry, observe, input, model)?;
                OutputTemplateProof::BoundInput {
                    input_index: hidden_observed_input_idx_name(&observe.name, &input.name),
                    prefix_len: hidden_observed_actor_prefix_len_name(&input_spec),
                    suffix_len: hidden_observed_actor_suffix_len_name(&input_spec),
                    template,
                }
            } else {
                OutputTemplateProof::Witnessed {
                    prefix: hidden_observed_actor_prefix_name(witness),
                    suffix: hidden_observed_actor_suffix_name(witness),
                    template,
                }
            }
        }
        OutputValidationContext::Spawned { static_target, witness, template } => {
            fixed_output_template_proof(actor, entry, static_target, &template, model)?.unwrap_or_else(|| {
                OutputTemplateProof::Witnessed {
                    prefix: hidden_spawn_actor_prefix_name(witness),
                    suffix: hidden_spawn_actor_suffix_name(witness),
                    template,
                }
            })
        }
    };
    Ok(planned_output_validation(output_index, physical, state_binding, proof))
}

fn fixed_output_template_proof(
    actor: &ActorDecl,
    entry: &EntryDecl,
    target: Option<StaticActorTarget<'_>>,
    template: &str,
    model: &Model<'_>,
) -> Result<Option<OutputTemplateProof>> {
    let Some(target) = target else {
        return Ok(None);
    };
    if target.in_app_actor().is_some_and(|target| target.name == actor.name) {
        return Ok(Some(OutputTemplateProof::Current));
    }
    let Some(input_index) = template_input_index_for_target(actor, entry, target, model)? else {
        let target_reference = target.artifact_reference();
        return Ok(Some(OutputTemplateProof::Witnessed {
            prefix: hidden_witness_prefix_name(&target_reference),
            suffix: hidden_witness_suffix_name(&target_reference),
            template: template.to_string(),
        }));
    };
    let target_reference = target.artifact_reference();
    Ok(Some(OutputTemplateProof::BoundInput {
        input_index,
        prefix_len: hidden_witness_prefix_len_name(&target_reference),
        suffix_len: hidden_witness_suffix_len_name(&target_reference),
        template: template.to_string(),
    }))
}

fn planned_output_validation(
    output_index: impl Into<String>,
    physical: PhysicalStateExpr,
    state_binding: impl Into<String>,
    proof: OutputTemplateProof,
) -> PlannedOutputValidation {
    PlannedOutputValidation { output_index: output_index.into(), physical, state_binding: state_binding.into(), proof }
}

pub(in crate::compiler::codegen) fn preserve_exact_self(out: &mut String, indent: usize, output_index: &str) {
    push_generated_binary_require(
        out,
        indent,
        &format!("tx.outputs[{output_index}].scriptPubKey"),
        "==",
        "tx.inputs[this.activeInputIndex].scriptPubKey",
    );
}

impl SourceStateAccess {
    fn source_identity(&self) -> &str {
        self.source.as_str()
    }

    #[cfg(test)]
    fn authored_sil_type(&self) -> &str {
        &self.authored_sil_type
    }

    #[cfg(test)]
    fn is_complete(&self) -> bool {
        self.complete.is_some()
    }

    fn projected_replacements(&self, source_ref: &str, indent: usize) -> Result<Vec<(String, String)>> {
        self.fields
            .iter()
            .filter_map(|field| field.value.as_ref().map(|_| field))
            .map(|field| Ok((format!("{source_ref}.{}", field.name), self.project_field(&field.name, indent)?)))
            .collect()
    }

    fn project_field(&self, field_name: &str, indent: usize) -> Result<String> {
        Ok(self.planned_field(field_name)?.render(indent))
    }

    fn planned_field(&self, field_name: &str) -> Result<&PlannedSourceExpr> {
        let field = self
            .fields
            .iter()
            .find(|field| field.name == field_name)
            .ok_or_else(|| ArgentError::new(format!("state `{}` has no field `{field_name}`", self.source.as_str())))?;
        field.value.as_ref().ok_or_else(|| {
            ArgentError::new(format!(
                "expanded input field `{field_name}` cannot be projected from authenticated physical state without its validated preimage"
            ))
        })
    }

    fn planned_storage_field(&self, field_name: &str) -> Result<PlannedSourceStorageExpr> {
        let authored = self.planned_field(field_name)?.clone();
        let trusted_storage =
            self.fields.iter().find(|field| field.name == field_name).and_then(|field| field.trusted_storage.clone());
        Ok(PlannedSourceStorageExpr { authored, trusted_storage })
    }

    fn reject_unavailable_field_refs(&self, source_ref: &str, input: &str) -> Result<()> {
        let tokens = crate::compiler::syntax::lexer::lex(input)?;
        for field in self.fields.iter().filter(|field| field.value.is_none()) {
            let reference = format!("{source_ref}.{}", field.name);
            if count_qualified_ref(&tokens, &reference) > 0 {
                return Err(ArgentError::new(format!(
                    "expanded input field `{}` cannot be projected from authenticated physical state without its validated preimage",
                    field.name
                )));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::compiler::codegen) struct EntryInputReferenceId(usize);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::compiler::codegen) struct EntryInputScopeId(usize);

#[derive(Clone, Copy)]
enum EntryInputReferenceKind {
    Active,
    Consumed,
    Observed,
}

struct InputReferenceSpec {
    id: EntryInputReferenceId,
    scope: EntryInputScopeId,
    kind: EntryInputReferenceKind,
    reference: String,
    lexical_root: String,
    physical_expr: String,
    input_index: String,
}

#[derive(Clone)]
struct InputIndexExpr(String);

impl InputIndexExpr {
    fn as_str(&self) -> &str {
        &self.0
    }
}

/// One input reference whose physical provenance remains private to the boundary.
#[derive(Clone)]
pub(in crate::compiler::codegen) struct PlannedEntryInputReference {
    id: EntryInputReferenceId,
    scope: EntryInputScopeId,
    kind: EntryInputReferenceKind,
    reference: String,
    lexical_root: String,
    input_index: InputIndexExpr,
    lowered_sil_type: String,
    access: SourceStateAccess,
    physical: Option<AuthenticatedPhysicalInput>,
    additional_replacements: Vec<(String, String)>,
}

impl PlannedEntryInputReference {
    pub(in crate::compiler::codegen) fn scope(&self) -> EntryInputScopeId {
        self.scope
    }

    pub(in crate::compiler::codegen) fn lexical_root(&self) -> &str {
        &self.lexical_root
    }

    pub(in crate::compiler::codegen) fn reference(&self) -> &str {
        &self.reference
    }

    pub(in crate::compiler::codegen) fn is_active(&self) -> bool {
        matches!(self.kind, EntryInputReferenceKind::Active)
    }

    #[cfg(test)]
    fn authored_sil_type(&self) -> &str {
        self.access.authored_sil_type()
    }

    #[cfg(test)]
    fn is_direct_authored(&self) -> bool {
        self.access.is_complete()
    }

    pub(in crate::compiler::codegen) fn source_identity(&self) -> &str {
        self.access.source_identity()
    }

    pub(in crate::compiler::codegen) fn physical_type(&self) -> &str {
        &self.lowered_sil_type
    }

    pub(in crate::compiler::codegen) fn physical_target(&self) -> &PhysicalTargetId {
        &self.access.target
    }

    pub(in crate::compiler::codegen) fn emit_read(&self, out: &mut String, indent: usize) {
        self.physical.as_ref().expect("only authenticated external references are emitted as reads").emit_read(out, indent);
    }

    pub(in crate::compiler::codegen) fn uses_covenant_domain_proof(&self) -> bool {
        self.physical.as_ref().is_some_and(|physical| matches!(physical.proof, InputTemplateProof::CovenantDomain))
    }

    pub(in crate::compiler::codegen) fn native_value(&self) -> String {
        format!("tx.inputs[{}].value", self.input_index.as_str())
    }

    pub(in crate::compiler::codegen) fn covenant_id(&self) -> String {
        format!("OpInputCovenantId({})", self.input_index.as_str())
    }

    pub(in crate::compiler::codegen) fn project_field(&self, field_name: &str, indent: usize) -> Result<String> {
        self.access.project_field(field_name, indent)
    }

    pub(in crate::compiler::codegen) fn complete_authored_state(&self, indent: usize) -> Result<AuthoredStateExpr> {
        if let Some(sil) = &self.access.complete {
            return Ok(AuthoredStateExpr {
                source: self.access.source.clone(),
                sil_type: self.access.authored_sil_type.clone(),
                sil: sil.clone(),
            });
        }
        if let Some(field) = self.access.fields.iter().find(|field| field.value.is_none()) {
            return Err(ArgentError::new(format!(
                "expanded input state `{}` from target `{:?}` cannot be materialized without a validated preimage for field `{}`",
                self.access.source.as_str(),
                self.access.target,
                field.name
            )));
        }
        let field_indent = " ".repeat(indent + 4);
        let close_indent = " ".repeat(indent);
        let mut out = format!("{} {{\n", self.access.authored_sil_type);
        if !self.access.fields.is_empty() {
            out.push_str(&format!("{field_indent}// :: user declared fields\n"));
        }
        for field in &self.access.fields {
            let value = self.project_field(&field.name, indent + 4)?;
            out.push_str(&format!("{field_indent}{}: {value},\n", field.name));
        }
        out.push_str(&close_indent);
        out.push('}');
        Ok(AuthoredStateExpr { source: self.access.source.clone(), sil_type: self.access.authored_sil_type.clone(), sil: out })
    }

    pub(in crate::compiler::codegen) fn authored_payload_digest(
        &self,
        lowering: &ContractStateLowering,
        model: &Model<'_>,
    ) -> Result<String> {
        // Validate complete reconstruction even though the digest can project
        // stable fields directly from the authenticated input.
        self.complete_authored_state(0)?;
        source_storage_payload_digest(&self.access.source, &self.access.source_to_storage, lowering, model, |field| {
            self.access.planned_storage_field(field)
        })
    }

    pub(in crate::compiler::codegen) fn operation_replacements(&self, indent: usize) -> Result<Vec<(String, String)>> {
        let mut replacements = self.access.projected_replacements(&self.reference, indent)?;
        replacements.extend(self.additional_replacements.clone());
        replacements.push((format!("{}.{}", self.reference, word::VALUE), self.native_value()));
        replacements.push((format!("{}.{}", self.reference, word::COVENANT_ID), self.covenant_id()));
        Ok(replacements)
    }

    pub(in crate::compiler::codegen) fn reject_unavailable_field_refs(&self, input: &str) -> Result<()> {
        let legacy = format!("{}.{}", self.reference, word::STATE);
        let tokens = crate::compiler::syntax::lexer::lex(input)?;
        if count_qualified_ref(&tokens, &legacy) > 0 {
            return Err(ArgentError::new(format!(
                "input reference `{}` has no `.state` member; use `{}({})` for complete authored state or project a field directly",
                self.reference,
                word::STATE,
                self.reference
            )));
        }
        self.access.reject_unavailable_field_refs(&self.reference, input)
    }
}

/// All compiler-planned input references for one emitted entry.
pub(in crate::compiler::codegen) struct EntryInputReferencePlan {
    references: Vec<PlannedEntryInputReference>,
    active: EntryInputReferenceId,
    consumed: BTreeMap<String, EntryInputReferenceId>,
    observed: BTreeMap<(String, String), EntryInputReferenceId>,
}

/// External input references available at one entry-lowering phase.
#[derive(Clone, Copy)]
pub(in crate::compiler::codegen) enum EntryInputReferenceView<'a> {
    None,
    Complete(&'a EntryInputReferencePlan),
}

impl<'a> EntryInputReferenceView<'a> {
    pub(in crate::compiler::codegen) fn active(self, actor: &ActorDecl, model: &Model<'_>) -> Result<PlannedEntryInputReference> {
        match self {
            Self::None => active_input_reference(actor, model, model.state_lowering(&actor.name)?),
            Self::Complete(plan) => Ok(plan.active().clone()),
        }
    }

    pub(in crate::compiler::codegen) fn consumed(self, name: &str) -> Result<Option<&'a PlannedEntryInputReference>> {
        match self {
            Self::None => Ok(None),
            Self::Complete(plan) => plan.consumed(name).map(Some),
        }
    }

    pub(in crate::compiler::codegen) fn observed(self, observe: &str, handle: &str) -> Result<Option<&'a PlannedEntryInputReference>> {
        match self {
            Self::Complete(plan) => plan.observed(observe, handle).map(Some),
            Self::None => Ok(None),
        }
    }

    pub(in crate::compiler::codegen) fn external_references(self) -> &'a [PlannedEntryInputReference] {
        match self {
            Self::None => &[],
            Self::Complete(plan) => plan.external_references(),
        }
    }

    pub(in crate::compiler::codegen) fn reference(self, expr: &str) -> Option<&'a PlannedEntryInputReference> {
        match self {
            Self::None => None,
            Self::Complete(plan) => plan.references().iter().find(|reference| reference.reference == expr),
        }
    }
}

impl EntryInputReferencePlan {
    fn reference(&self, id: EntryInputReferenceId) -> Option<&PlannedEntryInputReference> {
        self.references.get(id.0).filter(|reference| reference.id == id)
    }

    pub(in crate::compiler::codegen) fn active(&self) -> &PlannedEntryInputReference {
        self.reference(self.active).expect("entry input reference plan retains its active input")
    }

    pub(in crate::compiler::codegen) fn consumed(&self, name: &str) -> Result<&PlannedEntryInputReference> {
        self.consumed
            .get(name)
            .and_then(|id| self.reference(*id))
            .ok_or_else(|| ArgentError::new(format!("missing consumed input reference `{name}`")))
    }

    pub(in crate::compiler::codegen) fn observed(&self, observe: &str, handle: &str) -> Result<&PlannedEntryInputReference> {
        self.observed
            .get(&(observe.to_string(), handle.to_string()))
            .and_then(|id| self.reference(*id))
            .ok_or_else(|| ArgentError::new(format!("missing observed input reference `{observe}.{handle}`")))
    }

    pub(in crate::compiler::codegen) fn external_references(&self) -> &[PlannedEntryInputReference] {
        &self.references[1..]
    }

    fn references(&self) -> &[PlannedEntryInputReference] {
        &self.references
    }
}

pub(in crate::compiler::codegen) fn plan_entry_input_references(
    actor: &ActorDecl,
    entry: &EntryDecl,
    model: &Model<'_>,
) -> Result<EntryInputReferencePlan> {
    let lowering = model.state_lowering(&actor.name)?;
    let active = active_input_reference(actor, model, lowering)?;
    let mut references = vec![active];
    let mut consumed = BTreeMap::new();
    let mut next_scope = 1usize;
    for consume in &entry.consumes {
        let target = lowering.target_for_actor(&consume.actor).ok_or_else(|| {
            ArgentError::new(format!("actor `{}` has no input state target plan for `{}`", actor.name, consume.actor))
        })?;
        let proof = if model.app_actors.is_singleton_actor_self_target(&actor.name, &consume.actor) {
            InputTemplateProof::CovenantDomain
        } else {
            InputTemplateProof::Template {
                prefix_len: hidden_witness_prefix_len_name(&consume.actor),
                suffix_len: hidden_witness_suffix_len_name(&consume.actor),
                template: hidden_template_name(&consume.actor),
            }
        };
        let id = EntryInputReferenceId(references.len());
        let scope = EntryInputScopeId(next_scope);
        next_scope += 1;
        references.push(input_reference(
            InputReferenceSpec {
                id,
                scope,
                kind: EntryInputReferenceKind::Consumed,
                reference: consume.name.clone(),
                lexical_root: consume.name.clone(),
                physical_expr: hidden_consumed_input_state_name(&consume.name),
                input_index: hidden_input_idx_name(&consume.name),
            },
            proof,
            target,
            lowering,
        )?);
        consumed.insert(consume.name.clone(), id);
    }

    let mut observed = BTreeMap::new();
    for observe in &entry.observes {
        let scope = EntryInputScopeId(next_scope);
        next_scope += 1;
        for input in &observe.inputs {
            let reference = format!("{}.inputs.{}", observe.name, input.name);
            let physical_expr = hidden_observed_input_state_name(&observe.name, &input.name);
            let input_index = hidden_observed_input_idx_name(&observe.name, &input.name);
            let open_state = crate::compiler::model::observed_open_state_for_decl(actor, entry, observe, input, model)?;
            let target = match open_state {
                Some(state) => lowering.open_state_target(&SourceStateId::new(state)).ok_or_else(|| {
                    ArgentError::new(format!("actor `{}` has no open input state target plan for `{}`", actor.name, input.actor))
                })?,
                None => lowering.target_for_actor(&input.actor).ok_or_else(|| {
                    ArgentError::new(format!("actor `{}` has no observed input state target plan for `{}`", actor.name, input.actor))
                })?,
            };
            let static_target = static_observed_actor_target(actor, entry, observe, input, model)?;
            let in_app_target = static_target.and_then(|target| target.in_app_actor());
            let proof =
                if in_app_target.is_some_and(|target| model.app_actors.is_singleton_actor_self_target(&actor.name, &target.name)) {
                    InputTemplateProof::CovenantDomain
                } else {
                    let spec = observed_input_spec(actor, entry, observe, input, model)?;
                    let target_reference = static_target.map(|target| target.artifact_reference());
                    InputTemplateProof::Template {
                        prefix_len: target_reference
                            .as_deref()
                            .map_or_else(|| hidden_observed_actor_prefix_len_name(&spec), hidden_witness_prefix_len_name),
                        suffix_len: target_reference
                            .as_deref()
                            .map_or_else(|| hidden_observed_actor_suffix_len_name(&spec), hidden_witness_suffix_len_name),
                        template: observed_actor_template_expr_for_entry(actor, entry, model, observe, input, &spec)?,
                    }
                };
            let id = EntryInputReferenceId(references.len());
            references.push(input_reference(
                InputReferenceSpec {
                    id,
                    scope,
                    kind: EntryInputReferenceKind::Observed,
                    reference,
                    lexical_root: observe.name.clone(),
                    physical_expr,
                    input_index,
                },
                proof,
                target,
                lowering,
            )?);
            observed.insert((observe.name.clone(), input.name.clone()), id);
        }
    }
    Ok(EntryInputReferencePlan { references, active: EntryInputReferenceId(0), consumed, observed })
}

fn active_input_reference(
    actor: &ActorDecl,
    model: &Model<'_>,
    lowering: &ContractStateLowering,
) -> Result<PlannedEntryInputReference> {
    let target = lowering
        .target_for_actor(&actor.name)
        .ok_or_else(|| ArgentError::new(format!("actor `{}` has no active input reference target", actor.name)))?;
    let authored_sil_type = lowering
        .source_representation(target.source())
        .ok_or_else(|| ArgentError::new("active input source has no authored representation plan"))
        .and_then(|representation| render_sil_state_type(representation.sil_type()))?;
    let expansion_specs = state_expansion_witness_specs_for_actor(actor, model);
    let fields = target
        .source_fields()?
        .into_iter()
        .map(|field| {
            let name = field.source().field().to_string();
            let (value, trusted_storage) = if field.is_identity() {
                (PlannedSourceExpr::Value(name.clone()), None)
            } else {
                let spec = expansion_specs
                    .iter()
                    .find(|spec| spec.state == actor.state && spec.field == name)
                    .ok_or_else(|| ArgentError::new(format!("active expanded field `{name}` has no validated opening plan")))?;
                let memory_source = SourceStateId::new(&spec.memory_state);
                let sil_type = lowering
                    .source_representation(&memory_source)
                    .ok_or_else(|| {
                        ArgentError::new(format!("expanded state `{}` has no authored representation plan", spec.memory_state))
                    })
                    .and_then(|representation| render_sil_state_type(representation.sil_type()))?;
                let fields = model
                    .state(&spec.memory_state)?
                    .fields
                    .iter()
                    .map(|field| (field.name.clone(), hidden_state_expansion_field_name(spec, &field.name)))
                    .collect();
                (PlannedSourceExpr::Struct { sil_type, fields }, Some(name.clone()))
            };
            Ok(PlannedSourceField { name, value: Some(value), trusted_storage })
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
    let input_index = "this.activeInputIndex".to_string();
    let mut additional_replacements = Vec::new();
    for spec in &expansion_specs {
        for field in &model.state(&spec.memory_state)?.fields {
            let local = hidden_state_expansion_field_name(spec, &field.name);
            additional_replacements.push((format!("{}.{}.{}", word::SELF, spec.field, field.name), local.clone()));
            additional_replacements.push((format!("{}.{}", spec.field, field.name), local));
        }
    }
    Ok(PlannedEntryInputReference {
        id: EntryInputReferenceId(0),
        scope: EntryInputScopeId(0),
        kind: EntryInputReferenceKind::Active,
        reference: word::SELF.to_string(),
        lexical_root: word::SELF.to_string(),
        input_index: InputIndexExpr(input_index),
        lowered_sil_type: "State".to_string(),
        access,
        physical: None,
        additional_replacements,
    })
}

fn input_reference(
    spec: InputReferenceSpec,
    proof: InputTemplateProof,
    target: &TargetPhysicalPlan,
    lowering: &ContractStateLowering,
) -> Result<PlannedEntryInputReference> {
    let InputReferenceSpec { id, scope, kind, reference, lexical_root, physical_expr, input_index } = spec;
    let fields = target.source_fields()?;
    if fields.iter().any(|field| !matches!(field.physical(), PhysicalFieldId::Storage(_))) {
        return Err(ArgentError::new("authored input fields cannot map to compiler-generated route fields"));
    }
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
    let direct_authored = target.source_to_storage().is_identity()
        && target.storage_to_physical().is_identity()
        && physical_sil_type == authored_sil_type;
    let planned_fields = fields
        .iter()
        .map(|field| PlannedSourceField {
            name: field.source().field().to_string(),
            value: field.is_identity().then(|| PlannedSourceExpr::Value(format!("{physical_expr}.{}", field.sil_name()))),
            trusted_storage: None,
        })
        .collect::<Vec<_>>();
    let access = SourceStateAccess {
        source: target.source().clone(),
        source_to_storage: target.source_to_storage().clone(),
        authored_sil_type,
        complete: direct_authored.then(|| physical_expr.clone()),
        fields: planned_fields,
        target: target.id().clone(),
    };
    Ok(PlannedEntryInputReference {
        id,
        scope,
        kind,
        reference,
        lexical_root,
        input_index: InputIndexExpr(input_index),
        lowered_sil_type: physical_sil_type,
        access,
        physical: Some(physical),
        additional_replacements: Vec::new(),
    })
}

fn render_sil_state_type(ty: &SilStateType) -> Result<String> {
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

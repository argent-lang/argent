//! Typed boundary between authenticated physical inputs and authored state.

use std::collections::BTreeMap;

use crate::compiler::model::{
    CompilerRouteTransition, ContractStateLowering, GeneratedFieldId, Model, OutputPhysicalTypePlan, PhysicalFieldId,
    PhysicalStateLayout, PhysicalTargetId, SilStateType, SourcePhysicalField, SourceStateId, StaticActorTarget, TargetPhysicalPlan,
    TemplateSelector,
};
use crate::compiler::syntax::{ActorDecl, EntryDecl, ObserveDecl, ObservedActorDecl};
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

    pub(in crate::compiler::codegen) fn source_type(&self) -> &str {
        self.physical.source().as_str()
    }

    pub(in crate::compiler::codegen) fn require_authored_value(
        &self,
        lower: impl FnOnce(&str) -> Result<String>,
    ) -> Result<AuthoredStateExpr> {
        Ok(AuthoredStateExpr { source: self.physical.source().clone(), sil: lower(self.source_type())? })
    }

    #[cfg(test)]
    pub(in crate::compiler::codegen) fn authored_value(&self, sil: impl Into<String>) -> AuthoredStateExpr {
        AuthoredStateExpr { source: self.physical.source().clone(), sil: sil.into() }
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
pub(in crate::compiler::codegen) struct AuthenticatedPhysicalInput {
    expr: String,
    sil_type: String,
    target: PhysicalTargetId,
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
pub(in crate::compiler::codegen) struct ProjectedSourceAccess {
    source: SourceStateId,
    physical: AuthenticatedPhysicalInput,
    fields: Vec<SourcePhysicalField>,
}

/// Source-level access granted by one authenticated physical input.
#[derive(Clone)]
pub(in crate::compiler::codegen) enum SourceStateAccess {
    /// The physical read already has the nominal authored source type.
    Authored { source: SourceStateId, physical: AuthenticatedPhysicalInput },
    /// User fields are reached through typed source-to-physical projections.
    Projected(ProjectedSourceAccess),
}

/// A complete expression in one nominal authored state representation.
#[derive(Debug)]
pub(in crate::compiler::codegen) struct AuthoredStateExpr {
    source: SourceStateId,
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
        && target.sil_type == authored.source.as_str()
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
            Some(expanded) => state_payload_digest_expr(expanded.as_str(), &source_expr, model)?,
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
    pub(in crate::compiler::codegen) fn source_type(&self) -> &str {
        match self {
            Self::Authored { source, .. } => source.as_str(),
            Self::Projected(access) => access.source.as_str(),
        }
    }

    pub(in crate::compiler::codegen) fn physical_type(&self) -> &str {
        self.physical().sil_type.as_str()
    }

    #[cfg(test)]
    pub(in crate::compiler::codegen) fn physical_expr(&self) -> &str {
        self.physical().expr.as_str()
    }

    fn physical(&self) -> &AuthenticatedPhysicalInput {
        match self {
            Self::Authored { physical, .. } | Self::Projected(ProjectedSourceAccess { physical, .. }) => physical,
        }
    }

    fn reference_replacements(&self, source_ref: &str) -> Vec<(String, String)> {
        match self {
            Self::Authored { physical, .. } => vec![(source_ref.to_string(), physical.expr.clone())],
            Self::Projected(access) => access
                .fields
                .iter()
                .filter(|field| field.is_identity())
                .map(|field| {
                    (format!("{source_ref}.{}", field.source().field()), format!("{}.{}", access.physical.expr, field.sil_name()))
                })
                .collect(),
        }
    }

    fn reject_unavailable_field_refs(&self, source_ref: &str, input: &str) -> Result<()> {
        let Self::Projected(access) = self else {
            return Ok(());
        };
        let tokens = crate::compiler::syntax::lexer::lex(input)?;
        for field in access.fields.iter().filter(|field| !field.is_identity()) {
            let reference = format!("{source_ref}.{}", field.source().field());
            if count_qualified_ref(&tokens, &reference) > 0 {
                return Err(ArgentError::new(format!(
                    "expanded input field `{}` cannot be projected from authenticated physical state without its validated preimage",
                    field.source().field()
                )));
            }
        }
        Ok(())
    }

    /// Produce a complete named source value, never a physical `State` alias.
    pub(in crate::compiler::codegen) fn require_authored_value(&self, indent: usize) -> Result<AuthoredStateExpr> {
        let (source, sil) = match self {
            Self::Authored { source, physical } => (source.clone(), physical.expr.clone()),
            Self::Projected(access) => {
                if let Some(field) = access.fields.iter().find(|field| !field.is_identity()) {
                    return Err(ArgentError::new(format!(
                        "expanded input state `{}` from target `{:?}` cannot be materialized without a validated preimage for field `{}`",
                        access.source.as_str(),
                        access.physical.target,
                        field.source().field()
                    )));
                }
                let field_indent = " ".repeat(indent + 4);
                let close_indent = " ".repeat(indent);
                let mut out = format!("{} {{\n", access.source.as_str());
                if !access.fields.is_empty() {
                    out.push_str(&format!("{field_indent}// :: user declared fields\n"));
                }
                for field in &access.fields {
                    out.push_str(&format!(
                        "{field_indent}{}: {}.{},\n",
                        field.source().field(),
                        access.physical.expr,
                        field.sil_name()
                    ));
                }
                out.push_str(&close_indent);
                out.push('}');
                (access.source.clone(), out)
            }
        };
        Ok(AuthoredStateExpr { source, sil })
    }
}

/// One consumed or observed state binding shared by emission and body lowering.
pub(in crate::compiler::codegen) struct InputStateBinding {
    source_ref: String,
    access: SourceStateAccess,
}

impl InputStateBinding {
    pub(in crate::compiler::codegen) fn source_ref(&self) -> &str {
        &self.source_ref
    }

    pub(in crate::compiler::codegen) fn access(&self) -> &SourceStateAccess {
        &self.access
    }

    pub(in crate::compiler::codegen) fn physical_type(&self) -> &str {
        self.access.physical_type()
    }

    pub(in crate::compiler::codegen) fn physical_target(&self) -> &PhysicalTargetId {
        &self.access.physical().target
    }

    pub(in crate::compiler::codegen) fn emit_read(&self, out: &mut String, indent: usize) {
        self.access.physical().emit_read(out, indent);
    }

    pub(in crate::compiler::codegen) fn uses_covenant_domain_proof(&self) -> bool {
        matches!(self.access.physical().proof, InputTemplateProof::CovenantDomain)
    }
}

/// Ordered input bindings for one emitted entry.
pub(in crate::compiler::codegen) struct EntryInputStatePlan {
    consumed: BTreeMap<String, InputStateBinding>,
    observed: BTreeMap<(String, String), InputStateBinding>,
}

impl EntryInputStatePlan {
    pub(in crate::compiler::codegen) fn consumed(&self, name: &str) -> Result<&InputStateBinding> {
        self.consumed.get(name).ok_or_else(|| ArgentError::new(format!("missing consumed input state binding `{name}`")))
    }

    pub(in crate::compiler::codegen) fn observed(&self, observe: &str, handle: &str) -> Result<&InputStateBinding> {
        self.observed
            .get(&(observe.to_string(), handle.to_string()))
            .ok_or_else(|| ArgentError::new(format!("missing observed input state binding `{observe}.{handle}`")))
    }

    pub(in crate::compiler::codegen) fn bindings(&self) -> impl Iterator<Item = &InputStateBinding> {
        self.consumed.values().chain(self.observed.values())
    }

    pub(in crate::compiler::codegen) fn reference_replacements(&self) -> Vec<(String, String)> {
        self.consumed
            .values()
            .chain(self.observed.values())
            .flat_map(|binding| binding.access.reference_replacements(&binding.source_ref))
            .collect()
    }

    pub(in crate::compiler::codegen) fn reject_unavailable_field_refs(&self, input: &str) -> Result<()> {
        for binding in self.consumed.values().chain(self.observed.values()) {
            binding.access.reject_unavailable_field_refs(&binding.source_ref, input)?;
        }
        Ok(())
    }
}

pub(in crate::compiler::codegen) fn plan_entry_input_states(
    actor: &ActorDecl,
    entry: &EntryDecl,
    model: &Model<'_>,
) -> Result<EntryInputStatePlan> {
    let lowering = model.state_lowering(&actor.name)?;
    let mut consumed = BTreeMap::new();
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
        consumed.insert(
            consume.name.clone(),
            input_binding(consume.name.clone(), consume.name.clone(), hidden_input_idx_name(&consume.name), proof, target)?,
        );
    }

    let mut observed = BTreeMap::new();
    for observe in &entry.observes {
        for input in &observe.inputs {
            let source_ref = format!("{}.inputs.{}.state", observe.name, input.name);
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
            observed.insert(
                (observe.name.clone(), input.name.clone()),
                input_binding(source_ref, physical_expr, input_index, proof, target)?,
            );
        }
    }
    Ok(EntryInputStatePlan { consumed, observed })
}

fn input_binding(
    source_ref: String,
    physical_expr: String,
    input_index: String,
    proof: InputTemplateProof,
    target: &TargetPhysicalPlan,
) -> Result<InputStateBinding> {
    let fields = target.source_fields()?;
    if fields.iter().any(|field| !matches!(field.physical(), PhysicalFieldId::Storage(_))) {
        return Err(ArgentError::new("authored input fields cannot map to compiler-generated route fields"));
    }
    let physical = AuthenticatedPhysicalInput {
        expr: physical_expr,
        sil_type: render_sil_state_type(target.sil_type())?,
        target: target.id().clone(),
        input_index,
        proof,
    };
    let source = target.source().clone();
    let named_authored = target.source_to_storage().is_identity()
        && target.storage_to_physical().is_identity()
        && matches!(target.sil_type(), SilStateType::Source(candidate) if candidate == &source);
    let access = if named_authored {
        SourceStateAccess::Authored { source, physical }
    } else {
        SourceStateAccess::Projected(ProjectedSourceAccess { source, physical, fields })
    };
    Ok(InputStateBinding { source_ref, access })
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

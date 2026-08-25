//! Typed boundary between authenticated physical inputs and authored state.

use std::collections::BTreeMap;

use crate::compiler::model::{
    ContractStateLowering, Model, OutputPhysicalTypePlan, PhysicalFieldId, PhysicalStateLayout, PhysicalTargetId, SilStateType,
    SourcePhysicalField, SourceStateId, StaticActorTarget, TargetPhysicalPlan, TemplateSelector,
};
use crate::compiler::syntax::{ActorDecl, EntryDecl};
use crate::error::{ArgentError, Result};

use super::super::emitter::*;
use super::token_refs::count_qualified_ref;

#[cfg(test)]
mod tests;

/// Rendered output type and optional named layout selected from one physical target plan.
pub(in crate::compiler::codegen) struct OutputStateTarget {
    #[cfg(test)]
    target: PhysicalTargetId,
    #[cfg(test)]
    canonical_target: PhysicalTargetId,
    sil_type: String,
    named_physical_layout: Option<PhysicalStateLayout>,
}

impl OutputStateTarget {
    pub(in crate::compiler::codegen) fn physical_type(&self) -> &str {
        &self.sil_type
    }

    pub(in crate::compiler::codegen) fn named_physical_layout(&self) -> Option<(&str, &PhysicalStateLayout)> {
        self.named_physical_layout.as_ref().map(|layout| (self.sil_type.as_str(), layout))
    }

    #[cfg(test)]
    pub(in crate::compiler::codegen) fn target(&self) -> &PhysicalTargetId {
        &self.target
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
    output_state_target(target, lowering)
}

pub(in crate::compiler::codegen) fn plan_static_actor_output_state(
    actor: &ActorDecl,
    target_actor: StaticActorTarget<'_>,
    model: &Model<'_>,
) -> Result<OutputStateTarget> {
    let lowering = model.state_lowering(&actor.name)?;
    let target = match target_actor {
        StaticActorTarget::InApp(target) => lowering.output_type_for_actor(&target.name),
        StaticActorTarget::CrossApp(target) => lowering.output_type_for_compiled_actor(&target.app, &target.actor),
    }
    .ok_or_else(|| {
        ArgentError::new(format!("actor `{}` has no output state target plan for `{}`", actor.name, target_actor.artifact_reference()))
    })?;
    output_state_target(target, lowering)
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
    output_state_target(target, lowering)
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
    output_state_target(target, lowering)
}

fn output_state_target(plan: &OutputPhysicalTypePlan, lowering: &ContractStateLowering) -> Result<OutputStateTarget> {
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
    Ok(OutputStateTarget {
        #[cfg(test)]
        target: plan.target().clone(),
        #[cfg(test)]
        canonical_target: plan.canonical_target().clone(),
        sil_type: render_sil_state_type(plan.sil_type())?,
        named_physical_layout,
    })
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

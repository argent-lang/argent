//! Physical output targets and the sources of compiler-owned fields.

use std::collections::{BTreeMap, BTreeSet};

use silverscript_lang::ast::visit::{AstVisitorMut, walk_expr_mut, walk_statement_mut};
use silverscript_lang::ast::{Expr, ExprKind, Statement, UnaryOp};

use crate::compiler::resolve::{Binding, LocalId};
use crate::compiler::syntax::node::{DeclId, EntryId, RootSlot};
use crate::compiler::syntax::source::Origin;
use crate::compiler::syntax::word;
use crate::compiler::syntax::{ActorDecl, EntryDecl, EntryKind};
use crate::error::{ArgentError, Result};

use super::{
    AppCompilationContext, CompilerRouteTransition, EntryInputReferenceId, GeneratedFieldId, InputAuthentication, InteractionId,
    InteractionLocation, InteractionSource, ObservedActorSide, ObservedActorWitnessSpec, OutputPhysicalTypePlan, PhysicalFieldId,
    PhysicalTargetId, SilStateType, SourceStateId, StaticActorId, StaticActorTarget, TargetPhysicalPlan, TemplateSelector,
    observed_open_state_for_decl,
};

#[cfg(test)]
mod tests;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum GeneratedFieldSource {
    Carried,
    TemplateField,
    TableFromTemplates(Vec<DeclId>),
    DigestFromTable(String),
    DigestFromTemplates(Vec<DeclId>),
}

#[derive(Clone, Debug)]
pub(crate) struct OutputTargetPlan {
    pub(crate) target: PhysicalTargetId,
    pub(crate) canonical_target: PhysicalTargetId,
    pub(crate) sil_type: SilStateType,
    pub(crate) physical: TargetPhysicalPlan,
    pub(crate) generated_fields: BTreeMap<GeneratedFieldId, GeneratedFieldSource>,
}

#[derive(Debug)]
pub(crate) struct ActorOutputPlan {
    targets: BTreeMap<PhysicalTargetId, OutputTargetPlan>,
    actors: BTreeMap<StaticActorId, PhysicalTargetId>,
    open_states: BTreeMap<SourceStateId, PhysicalTargetId>,
    selectors: BTreeMap<LocalId, PhysicalTargetId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum OutputProofRequirement {
    Current,
    BoundInput { input: EntryInputReferenceId, target: StaticActorId },
    WitnessedActor(StaticActorId),
    Selector(LocalId),
    BoundObserved(EntryInputReferenceId),
    ObservedWitness,
    SpawnWitness,
}

#[derive(Debug)]
pub(crate) struct EntryOutputPlan {
    value_uses: BTreeMap<(usize, usize), InteractionId>,
    exact_current_output_count: Option<usize>,
    current_output_ranges: BTreeMap<InteractionId, (i64, i64, usize)>,
    coordinates_current_outputs: bool,
    actors: BTreeMap<StaticActorId, OutputProofRequirement>,
    selectors: BTreeMap<LocalId, OutputProofRequirement>,
    observed: BTreeMap<InteractionId, OutputProofRequirement>,
    observed_targets: BTreeMap<InteractionId, PhysicalTargetId>,
    observed_witnesses: BTreeMap<InteractionId, ObservedActorWitnessSpec>,
    spawned: BTreeMap<InteractionId, OutputProofRequirement>,
    spawned_targets: BTreeMap<InteractionId, PhysicalTargetId>,
    spawned_actors: BTreeMap<InteractionId, Option<StaticActorId>>,
}

impl EntryOutputPlan {
    /// Bind each output's template proof to an authenticated input or witness source.
    pub(crate) fn new(entry_id: EntryId, actor: &ActorDecl, entry: &EntryDecl, model: &AppCompilationContext<'_>) -> Result<Self> {
        let value_uses = Self::validate_value_uses(entry_id, actor, entry, model)?;
        let input_plan = model.input_plan_by_id(entry_id)?;
        let entry_model = model.entry_model_by_id(entry_id)?;
        let mut current_output_ranges = BTreeMap::new();
        for interaction in entry_model.current().outputs() {
            let Some((minimum, maximum)) = interaction.cardinality().range_bounds() else { continue };
            let InteractionLocation::Range { singleton_count, .. } = interaction.location() else {
                return Err(ArgentError::new("ranged current output has no planned range location"));
            };
            current_output_ranges.insert(interaction.id(), (minimum, maximum, singleton_count));
        }
        let exact_current_output_count = current_output_ranges.is_empty().then_some(entry_model.current().outputs().len());
        let coordinates_current_outputs = entry.kind == EntryKind::Leader && !entry.consumes.is_empty();
        let bindings = model.resolution.bindings(entry_model.id.actor);
        let lowering = model.state_lowering_by_id(entry_model.id.actor)?;
        let mut actors = BTreeMap::new();
        let source = StaticActorId::InApp(entry_model.id.actor);
        for identity in model.static_actor_ids() {
            let proof = if identity == source {
                OutputProofRequirement::Current
            } else if let Some(physical) = lowering.target_for_actor(&identity) {
                if let Some(input) = input_plan.first_authenticated_template(physical.id()) {
                    OutputProofRequirement::BoundInput { input, target: identity.clone() }
                } else {
                    OutputProofRequirement::WitnessedActor(identity.clone())
                }
            } else {
                OutputProofRequirement::WitnessedActor(identity.clone())
            };
            actors.insert(identity, proof);
        }
        let selectors = entry_model
            .template_selectors()
            .values()
            .map(|selector| {
                let id = selector.binding.ok_or_else(|| {
                    ArgentError::new(format!(
                        "entry `{}::{}` has an unbound output selector `{}`",
                        actor.name, entry.name, selector.name
                    ))
                })?;
                Ok((id, OutputProofRequirement::Selector(id)))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        let mut observed = BTreeMap::new();
        let mut observed_targets = BTreeMap::new();
        let mut observed_witnesses = BTreeMap::new();
        for group in entry_model.existing_groups() {
            let observe = group.observe().expect("existing group has observe declaration");
            for interaction in group.outputs() {
                let InteractionSource::ObserveOutput(output) = interaction.source() else {
                    unreachable!("existing outputs are observations")
                };
                let key = interaction.id();
                let open = observed_open_state_for_decl(entry_id, actor, entry, observe, output, model)?;
                let static_target = if open.is_none() { model.resolve_static_actor_target(interaction.target()) } else { None };
                if interaction.location().is_range() && static_target.is_none() {
                    return Err(ArgentError::new(format!(
                        "entry `{}::{}` observed output range `{}.outputs.{}` must use one fixed actor target",
                        actor.name, entry.name, observe.name, output.name
                    )));
                }
                let target = if let Some(target) = static_target {
                    model.output_plan_by_id(entry_model.id.actor)?.actor(&target.id())?
                } else {
                    let state = open.as_ref().ok_or_else(|| ArgentError::new("observed output has no state target"))?;
                    model.output_plan_by_id(entry_model.id.actor)?.open(state)?
                };
                observed_targets.insert(key, target.target.clone());
                observed_witnesses.insert(
                    key,
                    ObservedActorWitnessSpec::for_decl(entry_id, actor, entry, observe, ObservedActorSide::Output, output, model)?,
                );
                let static_proof = static_target.and_then(|target| actors.get(&target.id()).cloned());
                let proof = if let Some(proof) = static_proof {
                    proof
                } else {
                    let output_site = key.actor_target_site(entry_model.id, model.resolution)?;
                    let output_source = bindings.local_actor_targets.get(&output_site);
                    let mut bound_input = None;
                    for candidate in group.inputs() {
                        let input_site = candidate.id().actor_target_site(entry_model.id, model.resolution)?;
                        let matches = match (output_source, bindings.local_actor_targets.get(&input_site)) {
                            (Some(output), Some(input)) => output == input,
                            (None, None) => {
                                interaction.target().single_static_actor().is_some()
                                    && interaction.target().single_static_actor() == candidate.target().single_static_actor()
                            }
                            _ => false,
                        };
                        if matches {
                            bound_input = Some(candidate);
                            break;
                        }
                    }
                    if let Some(input) = bound_input {
                        let reference = input_plan.observed(input.id())?;
                        if reference.authentication == InputAuthentication::Template {
                            OutputProofRequirement::BoundObserved(reference.id)
                        } else {
                            OutputProofRequirement::ObservedWitness
                        }
                    } else {
                        OutputProofRequirement::ObservedWitness
                    }
                };
                observed.insert(key, proof);
            }
        }
        let mut spawned = BTreeMap::new();
        let mut spawned_targets = BTreeMap::new();
        let mut spawned_actors = BTreeMap::new();
        for group in entry_model.genesis_groups() {
            for interaction in group.outputs() {
                let InteractionSource::SpawnOutput(output) = interaction.source() else { unreachable!("genesis outputs are spawns") };
                let static_target = model.resolve_static_actor_target(interaction.target());
                spawned_actors.insert(interaction.id(), static_target.map(StaticActorTarget::id));
                let target = if let Some(target) = static_target {
                    model.output_plan_by_id(entry_model.id.actor)?.actor(&target.id())?
                } else {
                    let state = super::spawn_target_state(
                        entry_id,
                        interaction.id(),
                        interaction.target(),
                        &output.actor,
                        actor,
                        entry,
                        model,
                    )?
                    .ok_or_else(|| ArgentError::new("spawned output has no state target"))?;
                    model.output_plan_by_id(entry_model.id.actor)?.open(&state)?
                };
                spawned_targets.insert(interaction.id(), target.target.clone());
                let proof =
                    static_target.and_then(|target| actors.get(&target.id()).cloned()).unwrap_or(OutputProofRequirement::SpawnWitness);
                spawned.insert(interaction.id(), proof);
            }
        }
        Ok(Self {
            value_uses,
            exact_current_output_count,
            current_output_ranges,
            coordinates_current_outputs,
            actors,
            selectors,
            observed,
            observed_targets,
            observed_witnesses,
            spawned,
            spawned_targets,
            spawned_actors,
        })
    }

    pub(crate) fn exact_current_output_count(&self) -> Option<usize> {
        self.exact_current_output_count
    }

    pub(crate) fn current_output_range(&self, id: InteractionId) -> Option<(i64, i64, usize)> {
        self.current_output_ranges.get(&id).copied()
    }

    pub(crate) fn coordinates_current_outputs(&self) -> bool {
        self.coordinates_current_outputs
    }

    /// Enforce output-value policy from authored expression nodes before emission.
    fn validate_value_uses(
        entry_id: EntryId,
        actor: &ActorDecl,
        entry: &EntryDecl,
        model: &AppCompilationContext<'_>,
    ) -> Result<BTreeMap<(usize, usize), InteractionId>> {
        let entry_model = model.entry_model_by_id(entry_id)?;
        let body = model.resolution.entry_body(entry_model.id)?;
        let bindings = model.resolution.bindings(entry_model.id.actor);
        type RequiredOutputValue = ((LocalId, Vec<String>), String, InteractionId);
        let mut required = Vec::<RequiredOutputValue>::new();
        let mut range_maxima = BTreeMap::new();
        for (index, output) in entry_model.current().outputs().iter().enumerate() {
            let root = *bindings
                .entry_emits
                .get(&(entry_model.id.index, index))
                .ok_or_else(|| ArgentError::new(format!("entry `{}::{}` has an unbound emitted output", actor.name, entry.name)))?;
            if let Some((_, maximum)) = output.cardinality().range_bounds() {
                if maximum > 0 {
                    range_maxima.insert(root, (output.handle().to_string(), maximum));
                    required.push((
                        (root, vec!["[]".to_string(), word::VALUE.to_string()]),
                        format!("{}[i].{}", output.handle(), word::VALUE),
                        output.id(),
                    ));
                }
            } else {
                required.push(((root, vec![word::VALUE.to_string()]), format!("{}.{}", output.handle(), word::VALUE), output.id()));
            }
        }
        for (spawn_index, spawn) in entry.spawns.iter().enumerate() {
            let root = *bindings
                .entry_spawns
                .get(&(entry_model.id.index, spawn_index))
                .ok_or_else(|| ArgentError::new(format!("entry `{}::{}` has an unbound spawn group", actor.name, entry.name)))?;
            for (output_index, output) in spawn.outputs.iter().enumerate() {
                required.push((
                    (root, vec![word::OUTPUTS.to_string(), output.name.clone(), word::VALUE.to_string()]),
                    format!("{}.{}.{}.{}", spawn.name, word::OUTPUTS, output.name, word::VALUE),
                    InteractionId::SpawnedOutput { spawn: spawn_index, output: output_index },
                ));
            }
        }
        if required.is_empty() {
            return Ok(BTreeMap::new());
        }
        required.sort_by(|left, right| left.1.cmp(&right.1));
        let references = bindings
            .sites
            .iter()
            .filter_map(|(site, binding)| {
                let node = model.resolution.nodes().node(*site);
                if node.address.owner != entry_model.id.actor || node.address.root != RootSlot::Entry(entry_model.id.index) {
                    return None;
                }
                match (binding, node.origin) {
                    (Binding::Local(id), Origin::Authored { start, end, .. }) => Some(((start, end), *id)),
                    _ => None,
                }
            })
            .collect::<BTreeMap<_, _>>();

        struct OutputValueUses<'a> {
            required: &'a [RequiredOutputValue],
            range_maxima: &'a BTreeMap<LocalId, (String, i64)>,
            references: &'a BTreeMap<(usize, usize), LocalId>,
            paths: BTreeSet<(LocalId, Vec<String>)>,
            value_uses: BTreeMap<(usize, usize), InteractionId>,
            invalid_unrestricted: Option<String>,
            invalid_range_index: Option<(String, i64, i64)>,
        }

        impl OutputValueUses<'_> {
            fn path(&self, expr: &Expr<'_>) -> Option<(LocalId, Vec<String>)> {
                let mut segments = Vec::new();
                let mut current = expr;
                loop {
                    match &current.kind {
                        ExprKind::FieldAccess { source, field, .. } => {
                            segments.push(field.clone());
                            current = source;
                        }
                        ExprKind::ArrayIndex { source, .. } => {
                            segments.push("[]".to_string());
                            current = source;
                        }
                        ExprKind::Identifier(_) => {
                            segments.reverse();
                            let id = self.references.get(&(current.span.start(), current.span.end()))?;
                            return Some((*id, segments));
                        }
                        _ => return None,
                    }
                }
            }
        }

        impl<'src> AstVisitorMut<'src> for OutputValueUses<'_> {
            fn visit_statement(&mut self, statement: &mut Statement<'src>) {
                if let Statement::FunctionCall { name, args, .. } = statement
                    && name == word::UNRESTRICTED
                    && (args.len() != 1
                        || self.path(&args[0]).is_none_or(|path| !self.required.iter().any(|(required, _, _)| *required == path)))
                {
                    self.invalid_unrestricted = Some(args.iter().map(|arg| arg.span.as_str().trim()).collect::<Vec<_>>().join(", "));
                }
                walk_statement_mut(self, statement);
            }

            fn visit_expr(&mut self, expr: &mut Expr<'src>) {
                if matches!(&expr.kind, ExprKind::FieldAccess { field, .. } if field == word::VALUE)
                    && let Some(path) = self.path(expr)
                {
                    if let Some((_, _, interaction)) = self.required.iter().find(|(required, _, _)| *required == path) {
                        self.value_uses.insert((expr.span.start(), expr.span.end()), *interaction);
                    }
                    if let ExprKind::FieldAccess { source, .. } = &expr.kind
                        && let ExprKind::ArrayIndex { source, index } = &source.kind
                        && let Some((root, _)) = self.path(source)
                        && let Some((handle, maximum)) = self.range_maxima.get(&root)
                        && let Some(value) = (match &index.kind {
                            ExprKind::Int(value) => Some(*value),
                            ExprKind::Unary { op: UnaryOp::Neg, expr } => match &expr.kind {
                                ExprKind::Int(value) => value.checked_neg(),
                                _ => None,
                            },
                            _ => None,
                        })
                        && (value < 0 || value >= *maximum)
                    {
                        self.invalid_range_index = Some((handle.clone(), value, *maximum));
                    }
                    self.paths.insert(path);
                }
                walk_expr_mut(self, expr);
            }
        }

        let mut used = OutputValueUses {
            required: &required,
            range_maxima: &range_maxima,
            references: &references,
            paths: BTreeSet::new(),
            value_uses: BTreeMap::new(),
            invalid_unrestricted: None,
            invalid_range_index: None,
        };
        for statement in body {
            statement.visit_with(&mut used);
        }
        if let Some(value) = used.invalid_unrestricted {
            return Err(ArgentError::new(format!(
                "`{}(...)` expects exactly one current emit or spawn output value; `{value}` is not one in `{}::{}`",
                word::UNRESTRICTED,
                actor.name,
                entry.name,
            )));
        }
        if let Some((handle, index, maximum)) = used.invalid_range_index {
            return Err(ArgentError::new(format!(
                "range `{handle}` index `{index}` is outside its declared positions `0..{maximum}` in `{}::{}`",
                actor.name, entry.name
            )));
        }
        let missing = required
            .iter()
            .filter(|(path, _, _)| !used.paths.contains(path))
            .map(|(_, display, _)| display.as_str())
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            let values = missing.iter().map(|value| format!("`{value}`")).collect::<Vec<_>>().join(", ");
            let declarations =
                missing.iter().map(|value| format!("`{}({value});`", word::UNRESTRICTED)).collect::<Vec<_>>().join(", ");
            let noun = if missing.len() == 1 { "output value" } else { "output values" };
            return Err(ArgentError::new(format!(
                "entry `{}::{}` must reference {noun} {values}; if intentionally unrestricted, add {declarations}",
                actor.name, entry.name,
            )));
        }
        Ok(used.value_uses)
    }

    /// Resolve an authored output-value expression to its completed interaction.
    pub(crate) fn value_use(&self, span: silverscript_lang::ast::Span<'_>) -> Option<InteractionId> {
        self.value_uses.get(&(span.start(), span.end())).copied()
    }

    pub(crate) fn actor(&self, target: &StaticActorId) -> Result<&OutputProofRequirement> {
        self.actors.get(target).ok_or_else(|| ArgentError::new(format!("missing output proof for actor `{target:?}`")))
    }

    pub(crate) fn selector(&self, selector: &TemplateSelector) -> Result<&OutputProofRequirement> {
        let id = selector.binding.ok_or_else(|| ArgentError::new(format!("unbound output selector `{}`", selector.name)))?;
        self.selectors.get(&id).ok_or_else(|| ArgentError::new(format!("missing output proof for selector `{}`", selector.name)))
    }

    pub(crate) fn observed(&self, id: InteractionId) -> Result<&OutputProofRequirement> {
        self.observed.get(&id).ok_or_else(|| ArgentError::new(format!("missing output proof for observed output `{id:?}`")))
    }

    pub(crate) fn observed_witness(&self, id: InteractionId) -> Result<&ObservedActorWitnessSpec> {
        self.observed_witnesses
            .get(&id)
            .ok_or_else(|| ArgentError::new(format!("missing observed output witness descriptor for `{id:?}`")))
    }

    pub(crate) fn observed_target(&self, id: InteractionId) -> Result<&PhysicalTargetId> {
        self.observed_targets.get(&id).ok_or_else(|| ArgentError::new(format!("missing output target for observed output `{id:?}`")))
    }

    pub(crate) fn spawned(&self, id: InteractionId) -> Result<&OutputProofRequirement> {
        self.spawned.get(&id).ok_or_else(|| ArgentError::new(format!("missing output proof for spawned output `{id:?}`")))
    }

    pub(crate) fn spawned_target(&self, id: InteractionId) -> Result<&PhysicalTargetId> {
        self.spawned_targets.get(&id).ok_or_else(|| ArgentError::new(format!("missing output target for spawned output `{id:?}`")))
    }

    pub(crate) fn spawned_actor(&self, id: InteractionId) -> Result<Option<&StaticActorId>> {
        self.spawned_actors
            .get(&id)
            .map(Option::as_ref)
            .ok_or_else(|| ArgentError::new(format!("missing actor target for spawned output `{id:?}`")))
    }
}

impl ActorOutputPlan {
    /// Complete every physical target available to an actor's entries.
    pub(crate) fn new(source_id: DeclId, actor: &ActorDecl, model: &AppCompilationContext<'_>) -> Result<Self> {
        let lowering = model.state_lowering_by_id(source_id)?;
        let mut targets = BTreeMap::new();
        let mut actors = BTreeMap::new();
        let mut open_states = BTreeMap::new();
        let mut selectors = BTreeMap::new();
        for identity in model.static_actor_ids() {
            if matches!(&identity, StaticActorId::InApp(id) if *id != source_id && !model.route_transitions.contains_key(&(source_id, *id)))
            {
                continue;
            }
            let (output, transition) = match &identity {
                StaticActorId::InApp(_) => {
                    (lowering.output_type_for_actor(&identity), model.output_transition_for_actor(source_id, actor, &identity)?)
                }
                StaticActorId::Linked(_) => (lowering.output_type_for_actor(&identity), CompilerRouteTransition::default()),
            };
            let output = match output {
                Some(output) => output,
                None => {
                    return Err(ArgentError::new(format!(
                        "actor `{}` has no output state target plan for `{}`",
                        actor.name,
                        model.static_actor_reference(&identity)?
                    )));
                }
            };
            let plan = Self::target_plan(source_id, output, &transition, model)?;
            actors.insert(identity, plan.target.clone());
            targets.insert(plan.target.clone(), plan);
        }
        for id in model.state_sources() {
            if let Some(output) = lowering.output_type_for_open_state(id) {
                let plan = Self::target_plan(source_id, output, &CompilerRouteTransition::default(), model)?;
                open_states.insert(id.clone(), plan.target.clone());
                targets.insert(plan.target.clone(), plan);
            }
        }
        for (index, _entry) in actor.entries.iter().enumerate() {
            for selector in model.entry_model_by_id(EntryId { actor: source_id, index })?.template_selectors().values() {
                let output = lowering
                    .output_type_for_actor_domain(&selector.source_state(model)?, selector.variant_actor_ids()?)
                    .ok_or_else(|| {
                        ArgentError::new(format!(
                            "actor `{}` has no output state target plan for selector `{}`",
                            actor.name, selector.name
                        ))
                    })?;
                let mut variants =
                    selector.variant_actor_ids()?.iter().map(|variant| model.output_transition_for_actor(source_id, actor, variant));
                let transition = variants
                    .next()
                    .transpose()?
                    .ok_or_else(|| ArgentError::new(format!("actor selector `{}` has no variants", selector.name)))?;
                for candidate in variants {
                    if candidate? != transition {
                        return Err(ArgentError::new(format!(
                            "actor selector `{}` variants do not share one route transition",
                            selector.name
                        )));
                    }
                }
                let plan = Self::target_plan(source_id, output, &transition, model)?;
                let binding = selector
                    .binding
                    .ok_or_else(|| ArgentError::new(format!("actor selector `{}` has no bound identity", selector.name)))?;
                selectors.insert(binding, plan.target.clone());
                targets.insert(plan.target.clone(), plan);
            }
        }
        Ok(Self { targets, actors, open_states, selectors })
    }

    fn target_plan(
        source_id: DeclId,
        output: &OutputPhysicalTypePlan,
        transition: &CompilerRouteTransition,
        model: &AppCompilationContext<'_>,
    ) -> Result<OutputTargetPlan> {
        let lowering = model.state_lowering_by_id(source_id)?;
        let physical =
            lowering.target(output.target()).ok_or_else(|| ArgentError::new("output target has no physical layout plan"))?;
        let source_generated = lowering
            .active()
            .physical()
            .fields()
            .iter()
            .filter_map(|field| match field.id() {
                PhysicalFieldId::Generated(id) => Some(id.clone()),
                PhysicalFieldId::Storage(_) => None,
            })
            .collect::<BTreeSet<_>>();
        let mut families_to_pack = transition.families_to_pack.clone();
        let mut generated_fields = BTreeMap::new();
        for id in physical.storage_to_physical().generated_fields() {
            if physical.physical().field(&PhysicalFieldId::Generated(id.clone())).is_none() {
                return Err(ArgentError::new("generated output field is missing from its physical layout"));
            }
            let source = match id {
                GeneratedFieldId::Template(_) => GeneratedFieldSource::TemplateField,
                GeneratedFieldId::RouteFamilyTable { family, actors, .. }
                    if !source_generated.contains(id)
                        && actors.iter().all(|actor| source_generated.contains(&GeneratedFieldId::Template(actor.clone()))) =>
                {
                    let route_family = model
                        .route_family(family)
                        .ok_or_else(|| ArgentError::new(format!("output route table references unknown route family `{family}`")))?;
                    if route_family.table_actor_ids.len() != actors.len()
                        || actors
                            .iter()
                            .zip(&route_family.table_actor_ids)
                            .any(|(actor, id)| actor.identity() != &StaticActorId::InApp(*id))
                    {
                        return Err(ArgentError::new("output route table differs from its bound actor order"));
                    }
                    GeneratedFieldSource::TableFromTemplates(route_family.table_actor_ids.clone())
                }
                GeneratedFieldId::RouteFamilyTable { .. } => GeneratedFieldSource::Carried,
                GeneratedFieldId::RouteFamilyDigest { family, .. } => {
                    if let Some(index) = families_to_pack.iter().position(|candidate| candidate == family) {
                        families_to_pack.remove(index);
                        let family = model.route_family(family).ok_or_else(|| {
                            ArgentError::new(format!("output transition references unknown route family `{family}`"))
                        })?;
                        if model.route_family_for_actor_id(source_id).is_some_and(|source| source.id == family.id) {
                            GeneratedFieldSource::DigestFromTable(family.id.clone())
                        } else {
                            GeneratedFieldSource::DigestFromTemplates(family.table_actor_ids.clone())
                        }
                    } else {
                        GeneratedFieldSource::Carried
                    }
                }
            };
            generated_fields.insert(id.clone(), source);
        }
        if let Some(family) = families_to_pack.first() {
            return Err(ArgentError::new(format!("output transition packs route family `{family}` without a generated target field")));
        }
        Ok(OutputTargetPlan {
            target: output.target().clone(),
            canonical_target: output.canonical_target().clone(),
            sil_type: output.sil_type().clone(),
            physical: physical.clone(),
            generated_fields,
        })
    }

    pub(crate) fn target(&self, id: &PhysicalTargetId) -> Result<&OutputTargetPlan> {
        self.targets.get(id).ok_or_else(|| ArgentError::new("missing completed output target plan"))
    }

    pub(crate) fn actor(&self, actor: &StaticActorId) -> Result<&OutputTargetPlan> {
        self.actors
            .get(actor)
            .ok_or_else(|| ArgentError::new(format!("actor `{actor:?}` has no output state target plan")))
            .and_then(|target| self.target(target))
    }

    pub(crate) fn open(&self, state: &SourceStateId) -> Result<&OutputTargetPlan> {
        self.open_states
            .get(state)
            .ok_or_else(|| ArgentError::new(format!("state `{}` has no open output target plan", state.as_str())))
            .and_then(|target| self.target(target))
    }

    pub(crate) fn selector(&self, selector: &TemplateSelector) -> Result<&OutputTargetPlan> {
        let id = selector.binding.ok_or_else(|| ArgentError::new(format!("unbound output selector `{}`", selector.name)))?;
        self.selectors
            .get(&id)
            .ok_or_else(|| ArgentError::new(format!("selector `{}` has no output target plan", selector.name)))
            .and_then(|target| self.target(target))
    }
}

impl AppCompilationContext<'_> {
    fn output_transition_for_actor(
        &self,
        source_id: DeclId,
        source: &ActorDecl,
        target: &StaticActorId,
    ) -> Result<CompilerRouteTransition> {
        let StaticActorId::InApp(target_id) = target else {
            return Ok(CompilerRouteTransition::default());
        };
        if *target_id == source_id {
            return Ok(CompilerRouteTransition::default());
        }
        self.route_transitions.get(&(source_id, *target_id)).cloned().ok_or_else(|| {
            ArgentError::new(format!(
                "entry model has no route transition from `{}` to in-app target `{}`",
                source.name,
                self.app_actors.name(*target_id).unwrap_or("<unknown>")
            ))
        })
    }
}

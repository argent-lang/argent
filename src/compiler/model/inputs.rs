//! Authentication and field availability for entry input references.

use std::collections::{BTreeMap, BTreeSet};

use silverscript_lang::ast::visit::{AstVisitorMut, walk_expr_mut};
use silverscript_lang::ast::{Expr, ExprKind, UnaryOp};

use crate::compiler::resolve::{Binding, LocalId};
use crate::compiler::syntax::node::{EntryId, RootSlot};
use crate::compiler::syntax::source::Origin;
use crate::compiler::syntax::{ActorDecl, EntryDecl, EntryKind};
use crate::error::{ArgentError, Result};

use super::{
    AppCompilationContext, CovenantGroupId, CovenantIdSource, InteractionId, InteractionLocation, InteractionSource,
    ObservedActorSide, ObservedActorWitnessSpec, PhysicalFieldId, PhysicalTargetId, SilStateType, SourceFieldId, StaticActorId,
    TargetPhysicalPlan, observed_open_state_for_decl, resolve_observe_covenant_id_source,
};

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct EntryInputReferenceId(pub(crate) usize);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InputAuthentication {
    CovenantDomain,
    Template,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InputFieldAvailability {
    Direct,
    CheckedPreimage,
    Unavailable,
}

/// Current covenant input checks chosen before Sil materialization.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CurrentInputGroupPolicy {
    Batchable,
    LeaderRanged,
    LeaderFixed { count: usize },
    Delegate { minimum_count: usize },
}

impl CurrentInputGroupPolicy {
    pub(crate) fn slot_offset(self) -> usize {
        usize::from(!matches!(self, Self::Delegate { .. }))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InputReferenceOrigin {
    Active,
    Consumed(InteractionId),
    Observed(InteractionId),
}

impl InputReferenceOrigin {
    /// Resolve an input identity to its authored reference path for diagnostics and AST matching.
    pub(crate) fn source_path<'a>(&self, entry: &'a EntryDecl) -> Result<Vec<&'a str>> {
        match self {
            Self::Active => Ok(vec!["self"]),
            Self::Consumed(InteractionId::CurrentInput(index)) => entry
                .consumes
                .get(*index)
                .map(|input| vec![input.name.as_str()])
                .ok_or_else(|| ArgentError::new("consumed input reference has no source declaration")),
            Self::Observed(InteractionId::ObservedInput { observe, input }) => entry
                .observes
                .get(*observe)
                .and_then(|group| group.inputs.get(*input).map(|item| vec![group.name.as_str(), "inputs", item.name.as_str()]))
                .ok_or_else(|| ArgentError::new("observed input reference has no source declaration")),
            Self::Consumed(_) | Self::Observed(_) => Err(ArgentError::new("input reference has the wrong interaction kind")),
        }
    }
}

#[derive(Debug)]
pub(crate) struct InputReferenceRequirement {
    pub(crate) id: EntryInputReferenceId,
    pub(crate) origin: InputReferenceOrigin,
    pub(crate) root_binding: Option<LocalId>,
    pub(crate) target: PhysicalTargetId,
    pub(crate) authentication: InputAuthentication,
    pub(crate) location: Option<InteractionLocation>,
    pub(crate) ranged_proof_input_position: Option<usize>,
    pub(crate) guaranteed: bool,
    pub(crate) fields: BTreeMap<SourceFieldId, InputFieldAvailability>,
    pub(crate) direct_authored_state: bool,
    pub(crate) observed_witness: Option<ObservedActorWitnessSpec>,
}

#[derive(Debug)]
pub(crate) struct EntryInputPlan {
    references: Vec<InputReferenceRequirement>,
    consumed: BTreeMap<InteractionId, EntryInputReferenceId>,
    observed: BTreeMap<InteractionId, EntryInputReferenceId>,
    observed_covenant_sources: BTreeMap<CovenantGroupId, CovenantIdSource>,
    consumed_ranges: BTreeMap<InteractionId, (i64, i64)>,
    checked_range_index_required: bool,
    current_group_policy: CurrentInputGroupPolicy,
    requires_optional_delegate_guard: bool,
    pub(crate) reference_uses: BTreeMap<(usize, usize), EntryInputReferenceId>,
}

impl EntryInputPlan {
    /// Select physical input targets and authentication before text lowering.
    pub(crate) fn new(entry_id: EntryId, actor: &ActorDecl, entry: &EntryDecl, model: &AppCompilationContext<'_>) -> Result<Self> {
        let lowering = model.state_lowering_by_id(entry_id.actor)?;
        let direct_authored_state = |target: &TargetPhysicalPlan| -> Result<bool> {
            let authored = lowering
                .source_representation(target.source())
                .ok_or_else(|| ArgentError::new("input target source has no authored representation plan"))?;
            let same_type = target.sil_type() == authored.sil_type()
                || matches!(
                    (target.sil_type(), authored.sil_type()),
                    (SilStateType::StoragePhysical(left), SilStateType::TargetPhysical(PhysicalTargetId::OpenState(right)))
                        | (SilStateType::TargetPhysical(PhysicalTargetId::OpenState(left)), SilStateType::StoragePhysical(right))
                        if left == right
                );
            Ok(target.source_to_storage().is_identity() && target.storage_to_physical().is_identity() && same_type)
        };
        let active = lowering
            .target_for_actor(&StaticActorId::InApp(entry_id.actor))
            .ok_or_else(|| ArgentError::new(format!("actor `{}` has no active input reference target", actor.name)))?;
        let active_state = model.source_state_id_by_decl(model.types.actor_states[&entry_id.actor])?;
        let expanded = model
            .state_by_source(&active_state)?
            .expansion
            .as_ref()
            .map(|expansion| expansion.digests.iter().map(|digest| digest.field.as_str()).collect::<BTreeSet<_>>())
            .unwrap_or_default();
        let mut references = vec![InputReferenceRequirement {
            id: EntryInputReferenceId(0),
            origin: InputReferenceOrigin::Active,
            root_binding: None,
            target: active.id().clone(),
            authentication: InputAuthentication::CovenantDomain,
            location: None,
            ranged_proof_input_position: None,
            guaranteed: true,
            fields: active
                .source_fields()?
                .into_iter()
                .map(|field| {
                    let availability = if field.is_identity() {
                        InputFieldAvailability::Direct
                    } else if expanded.contains(field.source().field()) {
                        InputFieldAvailability::CheckedPreimage
                    } else {
                        InputFieldAvailability::Unavailable
                    };
                    (field.source().clone(), availability)
                })
                .collect(),
            direct_authored_state: false,
            observed_witness: None,
        }];
        let mut consumed = BTreeMap::new();
        let mut consumed_ranges = BTreeMap::new();
        let entry_model = model.entry_model_by_id(entry_id)?;
        let checked_range_index_required = entry_model
            .groups()
            .flat_map(|group| group.inputs())
            .chain(entry_model.current().outputs())
            .any(|interaction| interaction.cardinality().is_range());
        let allows_covenant_batching =
            entry.kind == EntryKind::Leader && entry.consumes.is_empty() && !model.is_leader_actor(entry_model.id.actor);
        let current_group_policy = if allows_covenant_batching {
            CurrentInputGroupPolicy::Batchable
        } else {
            match entry.kind {
                EntryKind::Leader if entry_model.current().inputs().iter().any(|input| input.cardinality().is_range()) => {
                    CurrentInputGroupPolicy::LeaderRanged
                }
                EntryKind::Leader => CurrentInputGroupPolicy::LeaderFixed { count: entry_model.current().inputs().len() + 1 },
                EntryKind::Delegate => CurrentInputGroupPolicy::Delegate { minimum_count: entry_model.current().inputs().len() + 1 },
            }
        };
        let requires_optional_delegate_guard = allows_covenant_batching
            && actor.entries.iter().any(|candidate| candidate.kind == EntryKind::Delegate)
            && entry_model
                .current()
                .outputs()
                .iter()
                .all(|interaction| interaction.cardinality().range_bounds().is_some_and(|(minimum, _)| minimum == 0));
        let bindings = model.resolution.bindings(entry_model.id.actor);
        let mut unsupported_range = None;
        for interaction in entry_model.current().inputs() {
            if interaction.location().is_range() && entry.kind == EntryKind::Delegate {
                unsupported_range.get_or_insert_with(|| {
                    format!("delegate `{}::{}` cannot use range `{}` in `consumes` yet", actor.name, entry.name, interaction.handle())
                });
            }
        }
        for interaction in entry_model.current().outputs() {
            if interaction.location().is_range() && interaction.target().single_static_actor().is_none() {
                unsupported_range.get_or_insert_with(|| {
                    format!(
                        "entry `{}::{}` range output `{}` must use one fixed actor target in this compiler version",
                        actor.name,
                        entry.name,
                        interaction.handle()
                    )
                });
            }
        }
        for interaction in entry_model.genesis_groups().flat_map(|group| group.outputs()) {
            if interaction.location().is_range() {
                unsupported_range.get_or_insert_with(|| {
                    format!(
                        "entry `{}::{}` declares range `{}`, but range code generation is not implemented yet",
                        actor.name,
                        entry.name,
                        interaction.handle()
                    )
                });
            }
        }
        if let Some(message) = unsupported_range {
            return Err(ArgentError::new(message));
        }
        for (consume_index, interaction) in entry_model.current().inputs().iter().enumerate() {
            let target_id = interaction
                .target()
                .single_static_actor()
                .ok_or_else(|| ArgentError::new(format!("entry `{}::{}` has a non-static consumed actor", actor.name, entry.name)))?;
            let target_name = model.static_actor_reference(target_id)?;
            let target = lowering.target_for_actor(target_id).ok_or_else(|| {
                ArgentError::new(format!("actor `{}` has no input state target plan for `{target_name}`", actor.name))
            })?;
            let source_fields = target.source_fields()?;
            if source_fields.iter().any(|field| !matches!(field.physical(), PhysicalFieldId::Storage(_))) {
                return Err(ArgentError::new("authored input fields cannot map to compiler-generated route fields"));
            }
            let id = EntryInputReferenceId(references.len());
            if let Some(bounds) = interaction.cardinality().range_bounds() {
                consumed_ranges.insert(interaction.id(), bounds);
            }
            references.push(InputReferenceRequirement {
                id,
                origin: InputReferenceOrigin::Consumed(interaction.id()),
                root_binding: Some(
                    *bindings
                        .entry_consumes
                        .get(&(entry_model.id.index, consume_index))
                        .ok_or_else(|| ArgentError::new("consumed input requirement has no bound handle"))?,
                ),
                target: target.id().clone(),
                authentication: if matches!(target_id, StaticActorId::InApp(id) if model.app_actors.is_singleton_actor_self_target(entry_model.id.actor, *id)) {
                    InputAuthentication::CovenantDomain
                } else {
                    InputAuthentication::Template
                },
                location: Some(interaction.location()),
                ranged_proof_input_position: match interaction.location() {
                    InteractionLocation::Range { start, .. } => Some(start + usize::from(entry.kind == EntryKind::Leader)),
                    _ => None,
                },
                guaranteed: interaction.cardinality().range_bounds().is_none_or(|(minimum, _)| minimum > 0),
                fields: source_fields
                    .into_iter()
                    .map(|field| {
                        (
                            field.source().clone(),
                            if field.is_identity() { InputFieldAvailability::Direct } else { InputFieldAvailability::Unavailable },
                        )
                    })
                    .collect(),
                direct_authored_state: direct_authored_state(target)?,
                observed_witness: None,
            });
            consumed.insert(interaction.id(), id);
        }
        let mut observed = BTreeMap::new();
        let mut observed_covenant_sources = BTreeMap::new();
        for (observe_index, group) in entry_model.existing_groups().enumerate() {
            let observe = group.observe().expect("existing group has an observe declaration");
            observed_covenant_sources.insert(group.id(), resolve_observe_covenant_id_source(entry_id, actor, entry, model, observe)?);
            for interaction in group.inputs() {
                let InteractionSource::ObserveInput(input) = interaction.source() else {
                    unreachable!("observed input has its source declaration")
                };
                let open_state = observed_open_state_for_decl(entry_id, actor, entry, observe, input, model)?;
                let static_target = if open_state.is_none() { model.resolve_static_actor_target(interaction.target()) } else { None };
                if let Some((minimum, _)) = interaction.cardinality().range_bounds() {
                    let path = format!("{}.inputs.{}", observe.name, input.name);
                    if minimum == 0 {
                        return Err(ArgentError::new(format!(
                            "entry `{}::{}` observed input range `{path}` must have a minimum of at least one",
                            actor.name, entry.name
                        )));
                    }
                    if static_target.is_none() {
                        return Err(ArgentError::new(format!(
                            "entry `{}::{}` observed input range `{path}` must use one fixed actor target",
                            actor.name, entry.name
                        )));
                    }
                }
                let target = if let Some(ref state) = open_state {
                    lowering.open_state_target(state)
                } else {
                    static_target.and_then(|target| lowering.target_for_actor(&target.id()))
                }
                .ok_or_else(|| {
                    ArgentError::new(format!("actor `{}` has no observed input state target plan for `{}`", actor.name, input.actor))
                })?;
                let source_fields = target.source_fields()?;
                if source_fields.iter().any(|field| !matches!(field.physical(), PhysicalFieldId::Storage(_))) {
                    return Err(ArgentError::new("authored input fields cannot map to compiler-generated route fields"));
                }
                let id = EntryInputReferenceId(references.len());
                references.push(InputReferenceRequirement {
                    id,
                    origin: InputReferenceOrigin::Observed(interaction.id()),
                    root_binding: Some(
                        *bindings
                            .entry_observes
                            .get(&(entry_model.id.index, observe_index))
                            .ok_or_else(|| ArgentError::new("observed input requirement has no bound covenant root"))?,
                    ),
                    target: target.id().clone(),
                    authentication: if matches!(static_target, Some(super::StaticActorTarget::InApp(id)) if model.app_actors.is_singleton_actor_self_target(entry_model.id.actor, id)) {
                        InputAuthentication::CovenantDomain
                    } else {
                        InputAuthentication::Template
                    },
                    location: Some(interaction.location()),
                    ranged_proof_input_position: match interaction.location() {
                        InteractionLocation::Range { start, .. } => Some(start),
                        _ => None,
                    },
                    guaranteed: true,
                    fields: source_fields
                        .into_iter()
                        .map(|field| {
                            (
                                field.source().clone(),
                                if field.is_identity() { InputFieldAvailability::Direct } else { InputFieldAvailability::Unavailable },
                            )
                        })
                        .collect(),
                    direct_authored_state: direct_authored_state(target)?,
                    observed_witness: Some(ObservedActorWitnessSpec::for_decl(
                        entry_id,
                        actor,
                        entry,
                        observe,
                        ObservedActorSide::Input,
                        input,
                        model,
                    )?),
                });
                observed.insert(interaction.id(), id);
            }
        }
        let unavailable_bound = references
            .iter()
            .map(|reference| {
                let suffix = reference.origin.source_path(entry)?.into_iter().skip(1).map(str::to_string).collect::<Vec<_>>();
                Ok(reference
                    .fields
                    .iter()
                    .filter(|(_, availability)| **availability == InputFieldAvailability::Unavailable)
                    .map(move |(field, _)| {
                        let mut path = suffix.clone();
                        path.push(field.field().to_string());
                        (reference.root_binding, path, field.field().to_string())
                    })
                    .collect::<Vec<_>>())
            })
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        let input_paths = references
            .iter()
            .map(|reference| {
                let suffix = reference.origin.source_path(entry)?.into_iter().skip(1).map(str::to_string).collect::<Vec<_>>();
                Ok(((reference.root_binding, suffix), reference.id))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        let mut range_maxima = BTreeMap::new();
        for interaction in entry_model.groups().flat_map(|group| group.inputs()) {
            if let Some((_, maximum)) = interaction.cardinality().range_bounds() {
                let id = consumed
                    .get(&interaction.id())
                    .or_else(|| observed.get(&interaction.id()))
                    .ok_or_else(|| ArgentError::new("ranged input requirement has no planned reference"))?;
                let handle = references[id.0].origin.source_path(entry)?.join(".");
                range_maxima.insert(*id, (handle, maximum));
            }
        }

        let mut reference_uses = BTreeMap::new();
        let unavailable_field = {
            let body = model.resolution.entry_body(entry_model.id)?;
            // Inspect bound field-access nodes, including nested entry scopes.
            struct FieldUseVisitor<'a> {
                unavailable: &'a [(Option<LocalId>, Vec<String>, String)],
                references: &'a BTreeMap<(usize, usize), Option<LocalId>>,
                input_paths: &'a BTreeMap<(Option<LocalId>, Vec<String>), EntryInputReferenceId>,
                reference_uses: &'a mut BTreeMap<(usize, usize), EntryInputReferenceId>,
                range_maxima: &'a BTreeMap<EntryInputReferenceId, (String, i64)>,
                found: Option<String>,
                invalid_range_index: Option<(String, i64, i64)>,
            }

            impl FieldUseVisitor<'_> {
                fn input_path(&self, expr: &Expr<'_>) -> Option<(Option<LocalId>, Vec<String>)> {
                    let mut segments = Vec::new();
                    let mut current = expr;
                    while let ExprKind::FieldAccess { source, field, .. } = &current.kind {
                        segments.push(field.clone());
                        current = source;
                    }
                    let ExprKind::Identifier(root_name) = &current.kind else { return None };
                    let root = *self.references.get(&(current.span.start(), current.span.end()))?;
                    if root.is_none() && root_name != "self" {
                        return None;
                    }
                    segments.reverse();
                    Some((root, segments))
                }
            }

            impl<'src> AstVisitorMut<'src> for FieldUseVisitor<'_> {
                fn visit_expr(&mut self, expr: &mut Expr<'src>) {
                    if let ExprKind::ArrayIndex { source, index } = &expr.kind
                        && let Some(path) = self.input_path(source)
                        && let Some(id) = self.input_paths.get(&path)
                        && let Some((handle, maximum)) = self.range_maxima.get(id)
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
                    if let Some((root, segments)) = self.input_path(expr) {
                        if let Some(id) = self.input_paths.get(&(root, segments.clone())) {
                            self.reference_uses.insert((expr.span.start(), expr.span.end()), *id);
                        }
                        if let Some((_, _, field)) =
                            self.unavailable.iter().find(|(binding, path, _)| *binding == root && *path == segments)
                        {
                            self.found = Some(field.clone());
                        }
                    }
                    walk_expr_mut(self, expr);
                }
            }

            let bound_references = bindings
                .sites
                .iter()
                .filter_map(|(site, binding)| {
                    let node = model.resolution.nodes().node(*site);
                    if node.address.owner != entry_model.id.actor || node.address.root != RootSlot::Entry(entry_model.id.index) {
                        return None;
                    }
                    match (binding, node.origin) {
                        (Binding::Local(id), Origin::Authored { start, end, .. }) => Some(((start, end), Some(*id))),
                        (Binding::RuntimeRoot, Origin::Authored { start, end, .. }) => Some(((start, end), None)),
                        _ => None,
                    }
                })
                .collect::<BTreeMap<_, _>>();
            let mut visitor = FieldUseVisitor {
                unavailable: &unavailable_bound,
                references: &bound_references,
                input_paths: &input_paths,
                reference_uses: &mut reference_uses,
                range_maxima: &range_maxima,
                found: None,
                invalid_range_index: None,
            };
            for statement in body {
                statement.visit_with(&mut visitor);
            }
            if let Some((handle, index, maximum)) = visitor.invalid_range_index {
                return Err(ArgentError::new(format!(
                    "range `{handle}` index `{index}` is outside its declared positions `0..{maximum}`"
                )));
            }
            visitor.found
        };
        if let Some(field) = unavailable_field {
            return Err(ArgentError::new(format!(
                "expanded input field `{field}` cannot be projected from authenticated physical state without its validated preimage"
            )));
        }
        Ok(Self {
            references,
            consumed,
            observed,
            observed_covenant_sources,
            consumed_ranges,
            checked_range_index_required,
            current_group_policy,
            requires_optional_delegate_guard,
            reference_uses,
        })
    }

    pub(crate) fn requires_checked_range_index(&self) -> bool {
        self.checked_range_index_required
    }

    pub(crate) fn current_group_policy(&self) -> CurrentInputGroupPolicy {
        self.current_group_policy
    }

    pub(crate) fn consumed_range(&self, id: InteractionId) -> Option<(i64, i64)> {
        self.consumed_ranges.get(&id).copied()
    }

    pub(crate) fn requires_optional_delegate_guard(&self) -> bool {
        self.requires_optional_delegate_guard
    }

    pub(crate) fn active(&self) -> &InputReferenceRequirement {
        &self.references[0]
    }

    pub(crate) fn external_targets(&self) -> impl Iterator<Item = &PhysicalTargetId> {
        self.references.iter().skip(1).map(|reference| &reference.target)
    }

    pub(crate) fn consumed(&self, interaction: InteractionId) -> Result<&InputReferenceRequirement> {
        self.consumed
            .get(&interaction)
            .and_then(|id| self.references.get(id.0))
            .ok_or_else(|| ArgentError::new(format!("missing consumed input reference `{interaction:?}`")))
    }

    pub(crate) fn observed(&self, interaction: InteractionId) -> Result<&InputReferenceRequirement> {
        self.observed
            .get(&interaction)
            .and_then(|id| self.references.get(id.0))
            .ok_or_else(|| ArgentError::new(format!("missing observed input reference `{interaction:?}`")))
    }

    pub(crate) fn observed_covenant_source(&self, group: CovenantGroupId) -> Result<&CovenantIdSource> {
        self.observed_covenant_sources
            .get(&group)
            .ok_or_else(|| ArgentError::new(format!("missing observed covenant id source `{group:?}`")))
    }

    pub(crate) fn first_authenticated_template(&self, target: &PhysicalTargetId) -> Option<EntryInputReferenceId> {
        self.references
            .iter()
            .skip(1)
            .find(|reference| {
                reference.guaranteed && reference.authentication == InputAuthentication::Template && &reference.target == target
            })
            .map(|reference| reference.id)
    }

    pub(crate) fn reference(&self, id: EntryInputReferenceId) -> Option<&InputReferenceRequirement> {
        self.references.get(id.0).filter(|reference| reference.id == id)
    }
}

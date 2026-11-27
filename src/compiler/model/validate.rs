//! Validates a fully constructed compiler model before code generation.

use std::collections::{BTreeMap, BTreeSet};

use silverscript_lang::ast::Statement;
use silverscript_lang::ast::visit::{AstVisitorMut, walk_statement_mut};

use crate::compiler::naming::to_snake;
use crate::compiler::resolve::{ActorSourceBinding, Binding, ClauseReference, ResolvedDeclaration, ResolvedModules, ResolvedName};
use crate::compiler::syntax::body::RouteArity;
use crate::compiler::syntax::lexer::{
    RESERVED_GENERATED_MODULE_NAME_PREFIX, RESERVED_GENERATED_PREFIX, RESERVED_GENERATED_TYPE_PREFIX,
};
use crate::compiler::syntax::node::{ChildEdge, EntryId, RootSlot, SourceNodeCursor, SourceOperation, SymbolKind};
use crate::compiler::syntax::source::Origin;
use crate::compiler::syntax::word;
use crate::compiler::syntax::{
    ActorDecl, ArrayDim, AuthoredEntryStatement, AuthoredSuccessor, Cardinality, EmitSpec, EntryDecl, EntryKind, ObserveDecl,
    ObservedActorDecl,
};
use crate::error::{ArgentError, Result};

use super::types::{BoundRouteActor, ResolvedTypeBase};
use super::{
    ActorTarget, AppCompilationContext, CovenantGroupId, EntryModel, InteractionId, InteractionSource, ResolvedRoute,
    ResolvedSuccessor, StaticActorId, observed_open_bindings, observed_open_state_for_decl, packed_field_len,
    resolve_observe_covenant_id_source, spawn_target_state,
};

#[cfg(test)]
mod tests;

#[derive(Debug)]
enum ReservedEntryNameRole {
    CurrentActor,
    ConsumeHandle,
    EmitHandle,
    ObserveRoot,
    ObservedOutputLabel { observe: String },
    SpawnRoot,
    SpawnedOutputLabel { spawn: String },
    CovenantBinding { spawn: String },
    OpenActorBinding { observe: String },
    EntryParameter,
}

impl ReservedEntryNameRole {
    fn description(&self) -> String {
        match self {
            Self::CurrentActor => "current actor context".to_string(),
            Self::ConsumeHandle => "consume handle".to_string(),
            Self::EmitHandle => "emit handle".to_string(),
            Self::ObserveRoot => "observe root".to_string(),
            Self::ObservedOutputLabel { observe } => format!("observe `{observe}` output label"),
            Self::SpawnRoot => "spawn root".to_string(),
            Self::SpawnedOutputLabel { spawn } => format!("spawn `{spawn}` output label"),
            Self::CovenantBinding { spawn } => format!("spawn `{spawn}` covenant binding"),
            Self::OpenActorBinding { observe } => format!("observe `{observe}` open-actor binding"),
            Self::EntryParameter => "entry parameter".to_string(),
        }
    }
}

#[derive(Debug, Default)]
struct ReservedEntryNames {
    body_bindings: BTreeMap<String, ReservedEntryNameRole>,
}

impl ReservedEntryNames {
    /// Validate the entry namespace and retain its role descriptions for body binding checks.
    fn for_entry(actor: &ActorDecl, entry: &EntryDecl) -> Result<Self> {
        let mut names = Self::default();
        names.reserve_unique(actor, entry, word::SELF, ReservedEntryNameRole::CurrentActor)?;
        for consume in &entry.consumes {
            names.reserve_unique(actor, entry, &consume.name, ReservedEntryNameRole::ConsumeHandle)?;
        }
        if let EmitSpec::Outputs(outputs) = &entry.emits {
            for output in outputs {
                names.reserve_unique(actor, entry, &output.name, ReservedEntryNameRole::EmitHandle)?;
            }
        }
        for observe in &entry.observes {
            names.reserve_unique(actor, entry, &observe.name, ReservedEntryNameRole::ObserveRoot)?;
            for output in &observe.outputs {
                names.reserve_output_label(
                    actor,
                    entry,
                    &output.name,
                    ReservedEntryNameRole::ObservedOutputLabel { observe: observe.name.clone() },
                )?;
            }
            for binding in observed_open_bindings(observe).into_keys() {
                names.reserve_unique(
                    actor,
                    entry,
                    binding,
                    ReservedEntryNameRole::OpenActorBinding { observe: observe.name.clone() },
                )?;
            }
        }
        for spawn in &entry.spawns {
            names.reserve_unique(actor, entry, &spawn.name, ReservedEntryNameRole::SpawnRoot)?;
            names.reserve_unique(
                actor,
                entry,
                &spawn.covenant,
                ReservedEntryNameRole::CovenantBinding { spawn: spawn.name.clone() },
            )?;
            for output in &spawn.outputs {
                names.reserve_output_label(
                    actor,
                    entry,
                    &output.name,
                    ReservedEntryNameRole::SpawnedOutputLabel { spawn: spawn.name.clone() },
                )?;
            }
        }
        for param in &entry.params {
            names.reserve_unique(actor, entry, &param.name, ReservedEntryNameRole::EntryParameter)?;
        }
        Ok(names)
    }

    fn reserve_unique(&mut self, actor: &ActorDecl, entry: &EntryDecl, name: &str, role: ReservedEntryNameRole) -> Result<()> {
        if let Some(previous) = self.body_bindings.get(name) {
            return Err(Self::collision(actor, entry, name, &role, previous));
        }
        self.body_bindings.insert(name.to_string(), role);
        Ok(())
    }

    fn reserve_output_label(&mut self, actor: &ActorDecl, entry: &EntryDecl, name: &str, role: ReservedEntryNameRole) -> Result<()> {
        let Some(previous) = self.body_bindings.get(name) else {
            self.body_bindings.insert(name.to_string(), role);
            return Ok(());
        };
        if matches!(previous, ReservedEntryNameRole::ObservedOutputLabel { .. } | ReservedEntryNameRole::SpawnedOutputLabel { .. }) {
            return Ok(());
        }
        Err(Self::collision(actor, entry, name, &role, previous))
    }

    fn collision(
        actor: &ActorDecl,
        entry: &EntryDecl,
        name: &str,
        role: &ReservedEntryNameRole,
        previous: &ReservedEntryNameRole,
    ) -> ArgentError {
        if matches!(role, ReservedEntryNameRole::EntryParameter) {
            return ArgentError::new(format!(
                "entry parameter `{name}` collides with {} of the same name in `{}::{}`",
                previous.description(),
                actor.name,
                entry.name
            ));
        }
        ArgentError::new(format!(
            "entry `{}::{}` {} `{name}` collides with {} of the same name",
            actor.name,
            entry.name,
            role.description(),
            previous.description(),
        ))
    }
}

impl ResolvedModules<'_> {
    /// Reject authored physical state constructors and helper-only input-state misuse.
    pub(super) fn validate_authored_state_operations(
        &self,
        declaration_names: &BTreeMap<crate::compiler::syntax::node::DeclId, String>,
        actor_ids: &[crate::compiler::syntax::node::DeclId],
    ) -> Result<()> {
        let resolution = self;
        for owner in declaration_names.keys().copied() {
            if owner.kind() != SymbolKind::Function && (!actor_ids.contains(&owner) || owner.kind() != SymbolKind::Actor) {
                continue;
            }
            for (site, binding) in &resolution.bindings(owner).sites {
                if !matches!(binding, Binding::Builtin) {
                    continue;
                }
                let node = resolution.nodes().node(*site);
                let Some(operation) = node.operation else { continue };
                if let (ResolvedDeclaration::Actor(actor), RootSlot::Entry(index), SourceOperation::PhysicalStateConstructor) =
                    (resolution.declaration(owner), node.address.root, operation)
                {
                    let entry = actor
                        .entries
                        .get(index)
                        .ok_or_else(|| ArgentError::at(resolution.declaration_path(owner), "invalid entry state-constructor owner"))?;
                    return Err(ArgentError::new(format!(
                        "physical `State` is compiler-owned and cannot be constructed in Argent source in `{}::{}`",
                        actor.name, entry.name
                    )));
                }
                let (name, context) = match (resolution.declaration(owner), node.address.root) {
                    (ResolvedDeclaration::Function(function), RootSlot::Declaration) => (function.name.as_str(), "global".to_string()),
                    (ResolvedDeclaration::Actor(actor), RootSlot::ActorFunction(index)) => {
                        let function = actor.functions.get(index).ok_or_else(|| {
                            ArgentError::at(resolution.declaration_path(owner), "invalid actor-function operation owner")
                        })?;
                        (function.name.as_str(), format!("actor `{}`", actor.name))
                    }
                    _ => continue,
                };
                let message = match operation {
                    SourceOperation::InputStateCall => format!(
                        "`{}(...)` input-state reconstruction is only available in entry bodies, not {context} function `{name}`",
                        word::STATE
                    ),
                    SourceOperation::PhysicalStateConstructor => {
                        format!("physical `State` is compiler-owned and cannot be constructed in Argent {context} function `{name}`")
                    }
                };
                return Err(ArgentError::new(message));
            }
        }
        Ok(())
    }

    /// An actor helper cannot capture an entry-specific expansion preimage.
    pub(super) fn validate_actor_captures(&self, actor_ids: &[crate::compiler::syntax::node::DeclId]) -> Result<()> {
        let resolution = self;
        for actor_id in actor_ids {
            let ResolvedDeclaration::Actor(actor) = resolution.declaration(*actor_id) else {
                unreachable!("selected actor ID resolves to an actor");
            };
            let Some(ResolvedName::Declaration(state_id)) = resolution.bindings(*actor_id).names.get(&actor.state) else {
                return Err(ArgentError::new(format!("actor `{}` has no bound source state", actor.name)));
            };
            let ResolvedDeclaration::State(state) = resolution.declaration(*state_id) else {
                unreachable!("bound actor state resolves to a state");
            };
            let Some(expansion) = &state.expansion else {
                continue;
            };
            let expanded_fields = expansion.digests.iter().map(|digest| digest.field.as_str()).collect::<BTreeSet<_>>();
            for (site, binding) in &resolution.bindings(*actor_id).sites {
                if !matches!(binding, Binding::ActorField) {
                    continue;
                }
                let node = resolution.nodes().node(*site);
                let RootSlot::ActorFunction(index) = node.address.root else {
                    continue;
                };
                let Origin::Authored { start, end, .. } = node.origin else {
                    return Err(ArgentError::at(resolution.declaration_path(*actor_id), "generated actor-field capture site"));
                };
                let source = resolution.source_text(site.module);
                let name = source
                    .get(start..end)
                    .ok_or_else(|| ArgentError::at(resolution.declaration_path(*actor_id), "invalid actor-field capture site"))?;
                if expanded_fields.contains(name) {
                    let function = actor.functions.get(index).ok_or_else(|| {
                        ArgentError::at(resolution.declaration_path(*actor_id), "invalid actor-function capture owner")
                    })?;
                    return Err(ArgentError::at_source(
                        resolution.declaration_path(*actor_id),
                        source,
                        start,
                        format!(
                            "actor function `{}::{}` cannot capture expanded field `{name}`; pass an authored state value as a parameter",
                            actor.name, function.name
                        ),
                    ));
                }
            }
        }
        Ok(())
    }
}

impl AppCompilationContext<'_> {
    pub(super) fn validate_pre_layout(&self) -> Result<()> {
        self.validate_reserved_identifiers()?;
        self.validate_state_expansions()?;
        self.validate_reserved_self_members()?;
        self.validate_generated_actor_suffixes()?;
        self.validate_function_namespaces()?;
        self.validate_route_plan_coverage()?;

        for (actor, entry) in self.entries_in_app_order() {
            self.validate_entry(entry.id, actor, entry.source())?;
        }
        Ok(())
    }

    /// Reject a model with missing ID-backed actor or physical layout facts.
    pub(super) fn validate_complete(&self) -> Result<()> {
        for (actor_id, actor) in self.app_actors.iter_with_ids() {
            let actor_model = &self.actor_models[&actor_id];
            let values = self.actor_value_plan_by_id(actor_id)?;
            self.output_plan_by_id(actor_id)?;
            self.state_expansion_witnesses_by_id(actor_id)?;
            if self.types.actor_states.get(&actor_model.id) != Some(&actor_model.state) {
                return Err(ArgentError::new(format!("actor `{actor}` has inconsistent source state identity")));
            }
            for (index, entry) in actor_model.entries().enumerate() {
                if entry.id != (EntryId { actor: actor_model.id, index }) {
                    return Err(ArgentError::new(format!("actor `{actor}` has inconsistent entry identity")));
                }
                for interaction in entry.groups().flat_map(|group| group.inputs().iter().chain(group.outputs())) {
                    if let super::ActorTarget::UnresolvedStatic(names) = interaction.target() {
                        return Err(ArgentError::new(format!(
                            "entry `{actor}::{}` has an unresolved static actor target `{}`",
                            entry.source().name,
                            names.join(" | ")
                        )));
                    }
                }
                self.input_plan_by_id(entry.id)?;
                self.entry_output_plan_by_id(entry.id)?;
                self.witness_plan_by_id(entry.id)?;
                for route in &entry.source().routes {
                    if entry.route(route.id).is_none() {
                        return Err(ArgentError::new(format!("entry `{actor}::{}` has an unplanned route", entry.source().name)));
                    }
                    if matches!(route.successor, crate::compiler::syntax::body::EntrySuccessor::Constructed { .. })
                        && !matches!(
                            &entry.route(route.id).expect("route coverage checked above").successor,
                            ResolvedSuccessor::Constructed { bound: Some(value), .. }
                                if Some(value) == self.types.route_values.get(&(entry.id, route.id))
                        )
                    {
                        return Err(ArgentError::new(format!(
                            "entry `{actor}::{}` has an unbound constructed successor",
                            entry.source().name
                        )));
                    }
                    if let ResolvedSuccessor::Constructed { actor: display, bound: Some(bound), .. } =
                        &entry.route(route.id).expect("route coverage checked above").successor
                    {
                        let expected = match bound.actor_target {
                            super::types::BoundRouteActor::Fixed(id) => Some(self.types.display_names[&id].as_str()),
                            super::types::BoundRouteActor::Linked(member) => {
                                Some(self.types.app_member_display_names[&member].as_str())
                            }
                            super::types::BoundRouteActor::Selector(_)
                            | super::types::BoundRouteActor::Local(_)
                            | super::types::BoundRouteActor::Expression(_) => None,
                        };
                        let display_name = display.display(entry.source(), entry.id, route.id, self)?;
                        if expected.is_some_and(|expected| expected != display_name) {
                            return Err(ArgentError::new(format!(
                                "entry `{actor}::{}` has a route target that differs from its bound source identity",
                                entry.source().name
                            )));
                        }
                    }
                }
            }
            let lowering = self.state_lowering_by_id(actor_id)?;
            if !values.required_sources.contains(&self.source_state_id_by_decl(actor_model.state)?) {
                return Err(ArgentError::new(format!("actor `{actor}` value plan omits its source state")));
            }
            for (name, id, signature) in self
                .functions
                .iter()
                .map(|(owner, function)| {
                    let id = super::types::CallableId { owner: *owner, member: None };
                    (function.name.as_str(), id, self.types.callables.get(&id))
                })
                .chain(actor_model.functions().enumerate().map(|(index, function)| {
                    let id = super::types::CallableId { owner: actor_model.id, member: Some(index) };
                    (function.name.as_str(), id, self.types.callables.get(&id))
                }))
            {
                let Some(signature) = signature else {
                    return Err(ArgentError::new(format!("actor `{actor}` has no resolved signature for `{name}`")));
                };
                let Some(value_signature) = values.signature_ids.get(&id) else {
                    return Err(ArgentError::new(format!("actor `{actor}` has no value signature for `{name}`")));
                };
                if value_signature.params.len() != signature.params.len() {
                    return Err(ArgentError::new(format!("actor `{actor}` has incomplete value signature for `{name}`")));
                }
            }
            for (id, constant) in &self.consts {
                if matches!(self.types.constants[id].base, ResolvedTypeBase::State(_)) && !values.constant_ids.contains_key(id) {
                    return Err(ArgentError::new(format!("actor `{actor}` has no state-value plan for constant `{}`", constant.name)));
                }
            }
            if lowering.target_for_actor(&super::StaticActorId::InApp(actor_id)).is_none() {
                return Err(ArgentError::new(format!("actor `{actor}` has no physical self target")));
            }
            for target_id in self.static_actor_ids() {
                if lowering.target_for_actor(&target_id).is_none() {
                    let target = self.static_actor_reference(&target_id)?;
                    return Err(ArgentError::new(format!("actor `{actor}` has no physical target for `{target}`")));
                }
            }
            for source in &values.required_sources {
                if lowering.source_representation(source).is_none() {
                    return Err(ArgentError::new(format!(
                        "actor `{actor}` has no source representation for required state `{}`",
                        source.as_str()
                    )));
                }
            }
            for source in self.state_sources() {
                if lowering.source_representation(source).is_none() {
                    return Err(ArgentError::new(format!("actor `{actor}` has no source representation for `{}`", source.as_str())));
                }
            }
            for (source, representation) in lowering.source_representations() {
                match representation.sil_type() {
                    super::SilStateType::State => {}
                    super::SilStateType::Source(planned) if planned == source => {}
                    super::SilStateType::Source(planned) => {
                        return Err(ArgentError::new(format!(
                            "source state `{}` uses unrelated authored SIL type `{}` in actor `{actor}`",
                            source.as_str(),
                            planned.as_str()
                        )));
                    }
                    super::SilStateType::StoragePhysical(_) | super::SilStateType::TargetPhysical(_) => {
                        return Err(ArgentError::new(format!(
                            "source state `{}` uses a physical SIL type at an authored state-value boundary in actor `{actor}`",
                            source.as_str()
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    fn validate_function_namespaces(&self) -> Result<()> {
        let global_functions = self.functions.iter().map(|(id, _)| self.types.display_names[id].as_str()).collect::<BTreeSet<_>>();
        for actor_model in self.actor_models.values() {
            for function in actor_model.functions() {
                if global_functions.contains(function.name.as_str()) {
                    return Err(ArgentError::new(format!(
                        "actor `{}` function `{}` conflicts with a global function of the same name",
                        actor_model.source().name,
                        function.name
                    )));
                }
            }
        }
        Ok(())
    }

    fn validate_reserved_self_members(&self) -> Result<()> {
        for (actor_id, _) in self.app_actors.iter_with_ids() {
            let actor = self.actor_by_decl(actor_id)?;
            let state = self.storage_state_for_actor(actor_id)?;
            if let Some(field) = state.fields.iter().find(|field| word::RESERVED_SELF_MEMBERS.contains(&field.name.as_str())) {
                return Err(ArgentError::new(format!(
                    "actor `{}` owned state `{}` exposes field `{}` as `self.{}`; this actor member name is reserved",
                    actor.name, actor.state, field.name, field.name
                )));
            }
        }
        Ok(())
    }

    fn validate_route_plan_coverage(&self) -> Result<()> {
        let planned_actors = self.route_leaves_by_actor.keys().cloned().collect::<BTreeSet<_>>();
        let selected_actors = self.app_actors.ids.iter().copied().collect::<BTreeSet<_>>();
        if planned_actors != selected_actors {
            return Err(ArgentError::new(format!(
                "route planner actor coverage differs from the selected app; expected {:?}, found {:?}",
                selected_actors, planned_actors
            )));
        }

        let family_ids = self.route_families.iter().map(|family| family.id.as_str()).collect::<BTreeSet<_>>();
        for family in &self.route_families {
            if !selected_actors.contains(&family.rep_id)
                || !family.actor_ids.contains(&family.rep_id)
                || family
                    .actor_ids
                    .iter()
                    .any(|actor| !selected_actors.contains(actor) || self.types.actor_states.get(actor) != Some(&family.state_id))
            {
                return Err(ArgentError::new(format!("route family `{}` has actors outside its selected state domain", family.id)));
            }
        }
        for ((source, target), transition) in &self.route_transitions {
            if !selected_actors.contains(source) || !selected_actors.contains(target) {
                return Err(ArgentError::new(format!(
                    "route transition `{}` -> `{}` falls outside the selected app",
                    self.types.display_names[source], self.types.display_names[target]
                )));
            }
            for family_id in transition.families_to_open.iter().chain(&transition.families_to_pack) {
                if !family_ids.contains(family_id.as_str()) {
                    return Err(ArgentError::new(format!(
                        "route transition `{}` -> `{}` references unknown family `{family_id}`",
                        self.types.display_names[source], self.types.display_names[target]
                    )));
                }
            }
        }
        Ok(())
    }

    fn validate_state_expansions(&self) -> Result<()> {
        for state in self.states.values() {
            for field in &state.fields {
                if field.virtual_slot
                    && (field.ty.name != "byte" || field.ty.array != Some(ArrayDim::Fixed(32)) || field.ty.actor_state.is_some())
                {
                    return Err(ArgentError::new(format!(
                        "state `{}` field `{}` is virtual, but virtual slots must be byte[32]",
                        state.name, field.name
                    )));
                }
            }
        }

        for (source, owner) in &self.state_decl_ids_by_source {
            let state = self.state_by_source(source)?;
            let Some(expansion) = &state.expansion else {
                continue;
            };
            if !state.fields.is_empty() {
                return Err(ArgentError::new(format!(
                    "state `{}` expands `{}` and cannot declare ordinary fields",
                    state.name, expansion.base
                )));
            }
            if expansion.digests.is_empty() {
                return Err(ArgentError::new(format!(
                    "state `{}` expands `{}` but declares no digest expansions",
                    state.name, expansion.base
                )));
            }
            let base = self
                .bound_state_use(*owner, RootSlot::StateBase)
                .and_then(|id| self.source_state_id_by_decl(id))
                .and_then(|source| self.state_by_source(&source))
                .map_err(|_| ArgentError::new(format!("state `{}` expands unknown base state `{}`", state.name, expansion.base)))?;
            if base.expansion.is_some() {
                return Err(ArgentError::new(format!(
                    "state `{}` expands `{}`, but expanded states cannot currently be used as bases",
                    state.name, expansion.base
                )));
            }
            let mut seen = BTreeSet::new();
            for (index, digest) in expansion.digests.iter().enumerate() {
                if !seen.insert(digest.field.as_str()) {
                    return Err(ArgentError::new(format!(
                        "state `{}` binds virtual slot `{}` more than once",
                        state.name, digest.field
                    )));
                }
                let field = base.fields.iter().find(|field| field.name == digest.field).ok_or_else(|| {
                    ArgentError::new(format!(
                        "state `{}` expands `{}` field `{}`, but `{}` has no such field",
                        state.name, expansion.base, digest.field, expansion.base
                    ))
                })?;
                if !field.virtual_slot
                    || field.ty.name != "byte"
                    || field.ty.array != Some(ArrayDim::Fixed(32))
                    || field.ty.actor_state.is_some()
                {
                    return Err(ArgentError::new(format!(
                        "state `{}` binds `{}` slot `{}`, but expanded slots must be virtual",
                        state.name, expansion.base, digest.field
                    )));
                }
                let memory_state = self
                    .bound_state_use(*owner, RootSlot::DigestState(index))
                    .and_then(|id| self.source_state_id_by_decl(id))
                    .and_then(|source| self.state_by_source(&source))
                    .map_err(|_| {
                        ArgentError::new(format!(
                            "state `{}` expands `{}` field `{}` as unknown memory state `{}`",
                            state.name, expansion.base, digest.field, digest.state
                        ))
                    })?;
                if memory_state.fields.is_empty() {
                    return Err(ArgentError::new(format!(
                        "state `{}` expands `{}` field `{}` as `{}`, but memory states must have at least one field",
                        state.name, expansion.base, digest.field, digest.state
                    )));
                }
                for memory_field in &memory_state.fields {
                    packed_field_len(&memory_field.ty).map_err(|err| {
                        ArgentError::new(format!(
                            "state `{}` slot `{}` as `{}` field `{}` cannot be packed: {err}",
                            state.name, digest.field, digest.state, memory_field.name
                        ))
                    })?;
                }
            }
        }
        Ok(())
    }

    fn validate_reserved_identifiers(&self) -> Result<()> {
        reject_reserved_identifier(word::APP, &self.app_name)?;
        for (_, ct) in &self.consts {
            reject_reserved_identifier("constant", &ct.name)?;
        }
        for (_, function) in &self.functions {
            reject_reserved_function_identifier(&function.name)?;
            for param in &function.params {
                reject_reserved_identifier(&format!("function `{}` parameter", function.name), &param.name)?;
            }
        }
        for state in self.states.values() {
            reject_reserved_identifier(word::STATE, &state.name)?;
            for field in &state.fields {
                reject_reserved_identifier(&format!("state `{}` field", state.name), &field.name)?;
            }
            if let Some(expansion) = &state.expansion {
                for digest in &expansion.digests {
                    reject_reserved_identifier(&format!("state `{}` expanded digest field", state.name), &digest.field)?;
                }
            }
        }
        for actor_enum in self.actor_enums.values() {
            reject_reserved_identifier("actor enum", &actor_enum.name)?;
        }
        for actor in self.actors_by_name.values() {
            reject_reserved_identifier(word::ACTOR, &actor.name)?;
            for function in &actor.functions {
                reject_reserved_function_identifier(&function.name)?;
                for param in &function.params {
                    reject_reserved_identifier(&format!("actor function `{}::{}` parameter", actor.name, function.name), &param.name)?;
                }
            }
            for entry in &actor.entries {
                reject_reserved_identifier(&format!("entry `{}::{}`", actor.name, entry.name), &entry.name)?;
                for param in &entry.params {
                    reject_reserved_identifier(&format!("entry `{}::{}` parameter", actor.name, entry.name), &param.name)?;
                }
                for consume in &entry.consumes {
                    reject_reserved_identifier(&format!("entry `{}::{}` consume handle", actor.name, entry.name), &consume.name)?;
                }
                for observe in &entry.observes {
                    reject_reserved_identifier(&format!("entry `{}::{}` observe handle", actor.name, entry.name), &observe.name)?;
                    for observed in &observe.inputs {
                        reject_reserved_identifier(
                            &format!("entry `{}::{}` observe `{}` input handle", actor.name, entry.name, observe.name),
                            &observed.name,
                        )?;
                    }
                    for observed in &observe.outputs {
                        reject_reserved_identifier(
                            &format!("entry `{}::{}` observe `{}` output handle", actor.name, entry.name, observe.name),
                            &observed.name,
                        )?;
                    }
                }
                for spawn in &entry.spawns {
                    reject_reserved_identifier(&format!("entry `{}::{}` spawn handle", actor.name, entry.name), &spawn.name)?;
                    reject_reserved_identifier(
                        &format!("entry `{}::{}` spawn covenant binding", actor.name, entry.name),
                        &spawn.covenant,
                    )?;
                    for output in &spawn.outputs {
                        reject_reserved_identifier(
                            &format!("entry `{}::{}` spawn `{}` output handle", actor.name, entry.name, spawn.name),
                            &output.name,
                        )?;
                    }
                }
                if let EmitSpec::Outputs(outputs) = &entry.emits {
                    for output in outputs {
                        reject_reserved_identifier(&format!("entry `{}::{}` output handle", actor.name, entry.name), &output.name)?;
                    }
                }
                for route in &entry.routes {
                    reject_reserved_identifier(&format!("entry `{}::{}` route output handle", actor.name, entry.name), &route.output)?;
                }
            }
        }
        for owner in self.functions.iter().map(|(id, _)| *id).chain(self.actor_models.keys().copied()) {
            for (local, name) in &self.resolution.bindings(owner).local_names {
                let context = match (self.resolution.declaration(owner), local.callable) {
                    (ResolvedDeclaration::Function(function), RootSlot::Declaration) => {
                        format!("function `{}` binding", function.name)
                    }
                    (ResolvedDeclaration::Actor(actor), RootSlot::ActorFunction(index)) => {
                        format!("actor function `{}::{}` binding", actor.name, actor.functions[index].name)
                    }
                    _ => continue,
                };
                reject_reserved_identifier(&context, name)?;
            }
        }
        Ok(())
    }

    fn validate_generated_actor_suffixes(&self) -> Result<()> {
        let mut seen = BTreeMap::new();
        for actor in self.app_actors.iter() {
            let suffix = to_snake(actor);
            if let Some(previous) = seen.insert(suffix.clone(), actor.as_str()) {
                return Err(ArgentError::new(format!(
                    "template actors `{previous}` and `{actor}` both map to generated suffix `{suffix}`"
                )));
            }
        }
        Ok(())
    }

    fn validate_entry(&self, entry_id: EntryId, actor: &ActorDecl, entry: &EntryDecl) -> Result<()> {
        for param in &entry.params {
            if param.ty.name == "State" {
                return Err(ArgentError::new(format!(
                    "entry `{}::{}` parameter `{}` uses compiler-owned physical type `{}`; entry parameters must use an Argent-authored state type",
                    actor.name,
                    entry.name,
                    param.name,
                    param.ty.to_source()
                )));
            }
            if param.ty.is_actor_type() && self.static_actor_target(&param.name).is_some() {
                return Err(ArgentError::new(format!(
                    "entry `{}::{}` actor_type parameter `{}` shadows an actor reference with the same name; rename the parameter",
                    actor.name, entry.name, param.name
                )));
            }
        }
        self.validate_observes(entry_id, actor, entry)?;
        self.validate_spawns(entry_id, actor, entry)?;

        let names = ReservedEntryNames::for_entry(actor, entry)?;

        {
            let body = self.resolution.entry_body(entry_id)?;
            struct ReservedBindingVisitor<'a> {
                reserved: &'a BTreeMap<String, ReservedEntryNameRole>,
                collision: Option<(String, String)>,
            }

            impl<'src> AstVisitorMut<'src> for ReservedBindingVisitor<'_> {
                fn visit_statement(&mut self, statement: &mut Statement<'src>) {
                    let bindings: Vec<&str> = match statement {
                        Statement::VariableDefinition { name, .. } => vec![name],
                        Statement::For { ident, .. } => vec![ident],
                        Statement::TupleAssignment { left_name, right_name, .. } => vec![left_name, right_name],
                        Statement::FunctionCallAssign { bindings, .. } => {
                            bindings.iter().map(|binding| binding.name.as_str()).collect()
                        }
                        Statement::StateFunctionCallAssign { bindings, .. } | Statement::StructDestructure { bindings, .. } => {
                            bindings.iter().map(|binding| binding.name.as_str()).collect()
                        }
                        _ => Vec::new(),
                    };
                    for name in bindings {
                        if let Some(role) = self.reserved.get(name) {
                            self.collision.get_or_insert_with(|| (name.to_string(), role.description()));
                        }
                    }
                    walk_statement_mut(self, statement);
                }
            }

            let mut visitor = ReservedBindingVisitor { reserved: &names.body_bindings, collision: None };
            for statement in body {
                statement.visit_with(&mut visitor);
            }
            if let Some((name, role)) = visitor.collision {
                return Err(ArgentError::new(format!(
                    "entry binding `{name}` collides with {role} of the same name in `{}::{}`",
                    actor.name, entry.name
                )));
            }
        }

        self.validate_foreign_route_coverage(entry_id, entry)?;

        if entry.kind == EntryKind::Delegate && entry.consumes.is_empty() {
            return Err(ArgentError::new(format!(
                "delegate `{}::{}` must declare its leader as the first `consumes` actor",
                actor.name, entry.name
            )));
        }

        let entry_model = self.entry_model_by_id(entry_id)?;
        for (index, consume) in entry.consumes.iter().enumerate() {
            let target = entry_model
                .current()
                .inputs()
                .get(index)
                .and_then(|interaction| interaction.target().single_static_actor())
                .ok_or_else(|| {
                    ArgentError::new(format!("entry `{}::{}` consumes unknown actor `{}`", actor.name, entry.name, consume.actor))
                })?;
            self.require_selected_actor_target(
                target,
                format!("entry `{}::{}` consumes unknown actor `{}`", actor.name, entry.name, consume.actor),
            )?;
        }

        match &entry.emits {
            EmitSpec::None => {}
            EmitSpec::Outputs(outputs) => {
                let mut names = BTreeSet::new();
                let mut auth_indices = BTreeSet::new();
                for (index, output) in outputs.iter().enumerate() {
                    if !names.insert(output.name.clone()) {
                        return Err(ArgentError::new(format!(
                            "entry `{}::{}` declares output `{}` more than once",
                            actor.name, entry.name, output.name
                        )));
                    }
                    if output.auth_index >= outputs.len() {
                        return Err(ArgentError::new(format!(
                            "entry `{}::{}` output `{}` uses auth[{}], but only {} outputs are emitted",
                            actor.name,
                            entry.name,
                            output.name,
                            output.auth_index,
                            outputs.len()
                        )));
                    }
                    if !auth_indices.insert(output.auth_index) {
                        return Err(ArgentError::new(format!(
                            "entry `{}::{}` maps multiple outputs to auth[{}]",
                            actor.name, entry.name, output.auth_index
                        )));
                    }
                    let interaction = entry_model
                        .current()
                        .outputs()
                        .get(index)
                        .ok_or_else(|| ArgentError::new("current output has no normalized interaction"))?;
                    let ActorTarget::Static(targets) = interaction.target() else {
                        return Err(ArgentError::new(format!(
                            "entry `{}::{}` output `{}` has no bound actor targets",
                            actor.name, entry.name, output.name
                        )));
                    };
                    for target in targets {
                        let label = self.static_actor_reference(target)?;
                        self.require_selected_actor_target(
                            target,
                            format!("entry `{}::{}` output `{}` emits unknown actor `{label}`", actor.name, entry.name, output.name),
                        )?;
                    }
                }
            }
        }

        if entry.kind == EntryKind::Delegate && !entry.routes.is_empty() {
            return Err(ArgentError::new(format!(
                "delegate `{}::{}` cannot use `become`; delegates verify coordinated transitions but emit no outputs",
                actor.name, entry.name
            )));
        }

        for route in entry_model.routes() {
            if let ResolvedSuccessor::Constructed { bound: Some(bound), .. } = &route.successor
                && matches!(bound.actor_target, BoundRouteActor::Selector(_))
            {
                return Err(ArgentError::new(format!(
                    "entry `{}::{}` must assign an indexed actor enum choice to a local actor handle before `become`",
                    actor.name, entry.name
                )));
            }
            if matches!(&route.successor, ResolvedSuccessor::Constructed { .. }) {
                for target in self.route_target_ids_by_id(entry_model.id, route)? {
                    let label = self.static_actor_reference(&target)?;
                    self.require_selected_actor_target(
                        &target,
                        format!("entry `{}::{}` routes to unknown actor `{label}`", actor.name, entry.name),
                    )?;
                }
            }
            self.validate_route_allowed(entry_id, actor, entry, route)?;
        }
        self.validate_route_coverage(actor, entry, entry_model)?;
        Ok(())
    }

    fn validate_spawns(&self, entry_id: EntryId, actor: &ActorDecl, entry: &EntryDecl) -> Result<()> {
        if entry.kind == EntryKind::Delegate && !entry.spawns.is_empty() {
            return Err(ArgentError::new(format!("delegate `{}::{}` cannot spawn covenant outputs", actor.name, entry.name)));
        }

        let observe_names = entry.observes.iter().map(|observe| observe.name.as_str()).collect::<BTreeSet<_>>();
        let mut source_names = self
            .storage_state_for_actor(entry_id.actor)?
            .fields
            .iter()
            .map(|field| field.name.as_str())
            .chain(entry.params.iter().map(|param| param.name.as_str()))
            .chain(entry.consumes.iter().map(|consume| consume.name.as_str()))
            .collect::<BTreeSet<_>>();
        for observe in &entry.observes {
            source_names.extend(observed_open_bindings(observe).into_keys());
        }

        let mut names = BTreeSet::new();
        let mut covenant_bindings = BTreeSet::new();
        for group in self.entry_model_by_id(entry_id)?.genesis_groups() {
            let spawn = group.spawn().expect("genesis covenant group retains its spawn declaration");
            if !names.insert(spawn.name.as_str()) {
                return Err(ArgentError::new(format!(
                    "entry `{}::{}` declares spawn `{}` more than once",
                    actor.name, entry.name, spawn.name
                )));
            }
            if observe_names.contains(spawn.name.as_str()) {
                return Err(ArgentError::new(format!(
                    "entry `{}::{}` uses `{}` as both an observe and a spawn",
                    actor.name, entry.name, spawn.name
                )));
            }
            if !covenant_bindings.insert(spawn.covenant.as_str()) {
                return Err(ArgentError::new(format!(
                    "entry `{}::{}` uses spawn covenant binding `{}` more than once",
                    actor.name, entry.name, spawn.covenant
                )));
            }
            if !source_names.insert(spawn.covenant.as_str()) {
                return Err(ArgentError::new(format!(
                    "entry `{}::{}` spawn covenant binding `{}` collides with a source value",
                    actor.name, entry.name, spawn.covenant
                )));
            }
            if spawn.outputs.is_empty() {
                return Err(ArgentError::new(format!(
                    "entry `{}::{}` spawn `{}` must declare at least one output",
                    actor.name, entry.name, spawn.name
                )));
            }

            let mut output_names = BTreeSet::new();
            for interaction in group.outputs() {
                let InteractionSource::SpawnOutput(output) = interaction.source() else {
                    unreachable!("genesis covenant outputs are spawn outputs");
                };
                if !output_names.insert(output.name.as_str()) {
                    return Err(ArgentError::new(format!(
                        "entry `{}::{}` spawn `{}` declares output `{}` more than once",
                        actor.name, entry.name, spawn.name, output.name
                    )));
                }
                if spawn_target_state(entry_id, interaction.id(), interaction.target(), &output.actor, actor, entry, self)?.is_none() {
                    return Err(ArgentError::new(format!(
                        "entry `{}::{}` spawn `{}.{}` target `{}` must be an actor_type value or a selected-app or linked actor",
                        actor.name, entry.name, spawn.name, output.name, output.actor
                    )));
                }
            }
        }
        Ok(())
    }

    /// Check foreign covenant routes before generated Sil chooses witnesses or temporary names.
    fn validate_foreign_route_coverage(&self, entry_id: EntryId, entry: &EntryDecl) -> Result<()> {
        let entry_model = self.entry_model_by_id(entry_id)?;
        let body = self.resolution.entry_body(entry_model.id)?;
        let body_cursor = SourceNodeCursor::new(entry_model.id.actor, RootSlot::Entry(entry_model.id.index)).child(ChildEdge::Body);
        let mut pending = body
            .iter()
            .enumerate()
            .rev()
            .map(|(index, statement)| (statement, body_cursor.child(ChildEdge::Statement(index)), true))
            .collect::<Vec<_>>();
        let mut validated_spawns = BTreeSet::new();
        while let Some((statement, cursor, unconditional)) = pending.pop() {
            match statement {
                AuthoredEntryStatement::Block { statements, .. } => {
                    pending.extend(
                        statements
                            .iter()
                            .enumerate()
                            .rev()
                            .map(|(index, statement)| (statement, cursor.child(ChildEdge::Statement(index)), unconditional)),
                    );
                }
                AuthoredEntryStatement::If { then_branch, else_branch, .. } => {
                    if let Some(else_branch) = else_branch {
                        pending.push((else_branch.as_ref(), cursor.child(ChildEdge::ElseBranch), false));
                    }
                    pending.push((then_branch.as_ref(), cursor.child(ChildEdge::ThenBranch), false));
                }
                AuthoredEntryStatement::ForeignBecome { group, routes, .. } => {
                    let name = group.segments.first().ok_or_else(|| ArgentError::new("foreign output route has no group name"))?;
                    let site = self
                        .resolution
                        .nodes()
                        .find(&cursor.child(ChildEdge::ForeignGroup).address)
                        .ok_or_else(|| ArgentError::new("foreign output route has no indexed group"))?;
                    let group_id = entry_model
                        .foreign_group(site)
                        .ok_or_else(|| ArgentError::new(format!("unknown observe or spawn `{name}`")))?;
                    let covenant =
                        entry_model.group(group_id).ok_or_else(|| ArgentError::new("foreign route has no covenant group"))?;
                    let label = if covenant.spawn().is_some() { "spawn" } else { "observe" };
                    if covenant.spawn().is_some() {
                        if !unconditional {
                            return Err(ArgentError::new(format!("spawn `{name}` output validation must be unconditional")));
                        }
                        if !validated_spawns.insert(group_id) {
                            return Err(ArgentError::new(format!("spawn `{name}` outputs are validated more than once")));
                        }
                    }
                    let mut seen = BTreeSet::new();
                    for route in routes {
                        let handle = route
                            .output
                            .segments
                            .first()
                            .ok_or_else(|| ArgentError::new(format!("{label} `{name}` has an output route without a handle")))?;
                        let (route_group, output_id) = entry_model
                            .route_output(route.id)
                            .ok_or_else(|| ArgentError::new(format!("{label} `{name}` has no output `{handle}`")))?;
                        if route_group != group_id {
                            return Err(ArgentError::new("foreign route output belongs to another covenant group"));
                        }
                        let output = covenant
                            .outputs()
                            .iter()
                            .find(|output| output.id() == output_id)
                            .ok_or_else(|| ArgentError::new(format!("{label} `{name}` has no output `{handle}`")))?;
                        if !seen.insert(output_id) {
                            return Err(ArgentError::new(format!("{label} `{name}` validates output `{handle}` more than once")));
                        }
                        let AuthoredSuccessor::Constructed { actor: target, many, .. } = &route.successor else {
                            return Err(ArgentError::new(format!(
                                "cannot use exact successor `self` for observe or spawn `{name}` outputs"
                            )));
                        };
                        if !output.cardinality().is_range() && *many {
                            return Err(ArgentError::new(format!(
                                "{label} `{name}` singleton output `{handle}` must use scalar become syntax `{handle} <- Actor(state)`"
                            )));
                        }
                        if covenant.observe().is_some() && output.cardinality().is_range() && !*many {
                            return Err(ArgentError::new(format!(
                                "observe `{name}` range output `{handle}` must use bulk become syntax `{handle} <- Actor[](states)`"
                            )));
                        }
                        let expected = match output.source() {
                            InteractionSource::ObserveOutput(output) => &output.actor,
                            InteractionSource::SpawnOutput(output) => &output.actor,
                            _ => unreachable!("foreign covenant outputs retain observed or spawned declarations"),
                        };
                        let output_site = output.id().actor_target_site(entry_model.id, self.resolution)?;
                        let (target_site, _) = self
                            .resolution
                            .nodes()
                            .route_sites(entry_model.id, route.id)
                            .ok_or_else(|| ArgentError::new("foreign route has no indexed actor target"))?;
                        let bindings = self.resolution.bindings(entry_model.id.actor);
                        let bound_target = bindings.sites.get(&target_site);
                        let expected_static = bindings.actor_targets.get(&output_site);
                        let expected_source = bindings.local_actor_targets.get(&output_site);
                        let bound_reference = bindings.route_actor_references.get(&target_site).and_then(Option::as_ref);
                        let same_target = match (expected_static, expected_source, bound_target, bound_reference) {
                            (Some(expected), None, Some(Binding::Source(actual)), _) => expected == actual,
                            (Some(ResolvedName::Declaration(expected)), None, Some(Binding::EnumVariant { actor, .. }), _) => {
                                expected == actor
                            }
                            (None, Some(ActorSourceBinding::Local(expected)), Some(Binding::Local(actual)), _) => expected == actual,
                            (
                                None,
                                Some(ActorSourceBinding::StateField(expected)),
                                _,
                                Some(ClauseReference::StateField { index: Some(actual), .. }),
                            ) => expected == actual,
                            (
                                None,
                                Some(ActorSourceBinding::StateField(expected)),
                                Some(Binding::ActorField),
                                Some(ClauseReference::BareStateField { index: actual, .. }),
                            ) => expected == actual,
                            _ => false,
                        };
                        let target_name = match bound_target {
                            Some(Binding::Source(ResolvedName::Declaration(id))) if id.kind() == SymbolKind::Actor => {
                                self.types.display_names[id].as_str()
                            }
                            Some(Binding::Source(ResolvedName::AppMember(member))) => {
                                self.types.app_member_display_names[member].as_str()
                            }
                            Some(Binding::EnumVariant { actor, .. }) => self.types.display_names[actor].as_str(),
                            _ => target.span.as_str().trim(),
                        };
                        if !same_target {
                            return Err(ArgentError::new(format!(
                                "{label} `{name}` output `{handle}` expects `{expected}`, but route uses `{}`",
                                target_name
                            )));
                        }
                    }
                    for output in covenant.outputs() {
                        if !seen.contains(&output.id()) {
                            return Err(ArgentError::new(format!("{label} `{name}` does not validate output `{}`", output.handle())));
                        }
                    }
                }
                AuthoredEntryStatement::Become { .. } | AuthoredEntryStatement::Sil(_) => {}
            }
        }
        for (index, spawn) in entry.spawns.iter().enumerate() {
            if !validated_spawns.contains(&CovenantGroupId::Genesis(index)) {
                return Err(ArgentError::new(format!(
                    "spawn `{}` must be validated with `require {}.outputs become`",
                    spawn.name, spawn.name
                )));
            }
        }
        Ok(())
    }

    fn validate_observes(&self, entry_id: EntryId, actor: &ActorDecl, entry: &EntryDecl) -> Result<()> {
        let mut observe_names = BTreeSet::new();
        for observe in &entry.observes {
            if !observe_names.insert(observe.name.as_str()) {
                return Err(ArgentError::new(format!(
                    "entry `{}::{}` declares observe `{}` more than once",
                    actor.name, entry.name, observe.name
                )));
            }
            if observe.covenant_expr.trim().is_empty() {
                return Err(ArgentError::new(format!(
                    "entry `{}::{}` observe `{}` has an empty covenant expression",
                    actor.name, entry.name, observe.name
                )));
            }
            resolve_observe_covenant_id_source(entry_id, actor, entry, self, observe)?;
            self.validate_observed_open_bindings(entry_id, actor, entry, observe)?;
            self.validate_observed_actor_types(entry_id, actor, entry, observe, "input", &observe.inputs)?;
            self.validate_observed_actor_types(entry_id, actor, entry, observe, "output", &observe.outputs)?;
        }
        Ok(())
    }

    fn validate_observed_open_bindings(
        &self,
        entry_id: EntryId,
        actor: &ActorDecl,
        entry: &EntryDecl,
        observe: &ObserveDecl,
    ) -> Result<()> {
        let entry_model = self.entry_model_by_id(entry_id)?;
        let observe_index = entry
            .observes
            .iter()
            .position(|candidate| std::ptr::eq(candidate, observe))
            .ok_or_else(|| ArgentError::new("observed open binding has no entry clause"))?;
        let clause_bindings = self.resolution.bindings(entry_model.id.actor);
        let mut bindings = BTreeMap::new();
        let mut source_names = self
            .storage_state_for_actor(entry_model.id.actor)?
            .fields
            .iter()
            .map(|field| field.name.as_str())
            .collect::<BTreeSet<_>>();
        source_names.extend(entry.params.iter().map(|param| param.name.as_str()));
        source_names.extend(entry.consumes.iter().map(|consume| consume.name.as_str()));
        for (input_index, input) in observe.inputs.iter().enumerate() {
            let Some(state) = input.open_state.as_deref() else {
                continue;
            };
            reject_reserved_identifier(
                &format!("entry `{}::{}` observe `{}` open actor binding", actor.name, entry.name, observe.name),
                &input.actor,
            )?;
            if source_names.contains(input.actor.as_str()) {
                return Err(ArgentError::new(format!(
                    "entry `{}::{}` observe `{}` open observed actor binding `{}` collides with a source value",
                    actor.name, entry.name, observe.name, input.actor
                )));
            }
            let input_site = InteractionId::ObservedInput { observe: observe_index, input: input_index }
                .actor_target_site(entry_model.id, self.resolution)?;
            let bound_state = clause_bindings
                .open_state_targets
                .get(&input_site)
                .ok_or_else(|| ArgentError::new("open observed actor has no bound state"))?;
            let state_id = self.source_state_id_by_decl(*bound_state)?;
            self.state_by_source(&state_id)?;
            if let Some(previous_state) = bindings.insert(input.actor.as_str(), state_id) {
                return Err(ArgentError::new(format!(
                    "entry `{}::{}` observe `{}` declares open observed actor binding `{}` for both `{}` and `{state}`",
                    actor.name,
                    entry.name,
                    observe.name,
                    input.actor,
                    previous_state.as_str()
                )));
            }
            let source = clause_bindings
                .local_actor_targets
                .get(&input_site)
                .ok_or_else(|| ArgentError::new("open observed actor has no bound source"))?;
            let mut used_by_output = false;
            for output_index in 0..observe.outputs.len() {
                let output_site = InteractionId::ObservedOutput { observe: observe_index, output: output_index }
                    .actor_target_site(entry_model.id, self.resolution)?;
                if clause_bindings.local_actor_targets.get(&output_site) == Some(source) {
                    used_by_output = true;
                    break;
                }
            }
            if !used_by_output {
                return Err(ArgentError::new(format!(
                    "entry `{}::{}` observe `{}` open observed actor binding `{}` must be used by an output",
                    actor.name, entry.name, observe.name, input.actor
                )));
            }
        }
        Ok(())
    }

    fn validate_observed_actor_types(
        &self,
        entry_id: EntryId,
        actor: &ActorDecl,
        entry: &EntryDecl,
        observe: &ObserveDecl,
        section: &str,
        observed_actors: &[ObservedActorDecl],
    ) -> Result<()> {
        let mut names = BTreeSet::new();
        for observed in observed_actors {
            if !names.insert(observed.name.as_str()) {
                return Err(ArgentError::new(format!(
                    "entry `{}::{}` observe `{}` declares {section} `{}` more than once",
                    actor.name, entry.name, observe.name, observed.name
                )));
            }
            if let Some(state) = observed_open_state_for_decl(entry_id, actor, entry, observe, observed, self)? {
                self.state_by_source(&state).map_err(|_| {
                    ArgentError::new(format!(
                        "entry `{}::{}` observe `{}` {section} `{}` references unknown state `{}`",
                        actor.name,
                        entry.name,
                        observe.name,
                        observed.name,
                        state.as_str()
                    ))
                })?;
                continue;
            }
            let target = self.static_observed_actor_target(entry_id, actor, entry, observe, observed)?;
            let Some(target) = target else {
                return Err(ArgentError::new(format!(
                    "entry `{}::{}` observe `{}` {section} `{}` references actor `{}` outside selected app `{}`; foreign actors must be imported through their app",
                    actor.name,
                    entry.name,
                    observe.name,
                    observed.name,
                    observed.actor.rsplit("::").next().unwrap_or(&observed.actor),
                    self.app_name
                )));
            };
            self.static_actor_source_state(&target.id()).map_err(|_| {
                ArgentError::new(format!(
                    "entry `{}::{}` observe `{}` {section} `{}` references unknown actor `{}`",
                    actor.name, entry.name, observe.name, observed.name, observed.actor
                ))
            })?;
        }
        Ok(())
    }

    fn require_selected_actor_target(&self, target: &StaticActorId, message: String) -> Result<()> {
        if !matches!(target, StaticActorId::InApp(id) if self.app_actors.name(*id).is_some()) {
            return Err(ArgentError::new(message));
        }
        self.static_actor_source_state(target)
            .and_then(|source| self.storage_state_by_source(&source).map(|_| ()))
            .map_err(|_| ArgentError::new(message))
    }

    fn validate_route_allowed(&self, entry_id: EntryId, actor: &ActorDecl, entry: &EntryDecl, route: &ResolvedRoute) -> Result<()> {
        let target_label = match &route.successor {
            ResolvedSuccessor::ExactSelf => word::SELF.to_string(),
            ResolvedSuccessor::Constructed { actor: target, .. } => target.display(entry, entry_id, route.id, self)?,
        };
        match &entry.emits {
            EmitSpec::None => Err(ArgentError::new(format!(
                "entry `{}::{}` has a `become` route to `{}`, but declares `emits none`",
                actor.name, entry.name, target_label
            ))),
            EmitSpec::Outputs(_) => {
                let entry_model = self.entry_model_by_id(entry_id)?;
                let output_id = entry_model
                    .route_output(route.id)
                    .and_then(|(group, output)| (group == CovenantGroupId::Current).then_some(output));
                let interaction = output_id.and_then(|id| entry_model.current().outputs().iter().find(|output| output.id() == id));
                let interaction = interaction.ok_or_else(|| {
                    ArgentError::new(format!(
                        "entry `{}::{}` routes through unknown output `{}`",
                        actor.name, entry.name, route.output
                    ))
                })?;
                let InteractionSource::CurrentOutput(output) = interaction.source() else {
                    return Err(ArgentError::new("current route has no current output declaration"));
                };
                let arity = match &route.successor {
                    ResolvedSuccessor::ExactSelf => RouteArity::One,
                    ResolvedSuccessor::Constructed { arity, .. } => *arity,
                };
                match (matches!(output.cardinality, Cardinality::Range { .. }), arity) {
                    (true, RouteArity::One) => {
                        return Err(ArgentError::new(format!(
                            "entry `{}::{}` range output `{}` must use bulk become syntax `{} <- Actor[](states)`",
                            actor.name, entry.name, output.name, output.name
                        )));
                    }
                    (false, RouteArity::Many) => {
                        return Err(ArgentError::new(format!(
                            "entry `{}::{}` singleton output `{}` must use scalar become syntax `{} <- Actor(state)`",
                            actor.name, entry.name, output.name, output.name
                        )));
                    }
                    _ => {}
                }
                let allowed = interaction.target().static_actors().cloned().collect::<BTreeSet<_>>();
                let targets = self.route_target_ids_by_id(entry_model.id, route)?;
                if targets.iter().all(|target| allowed.iter().any(|allowed| allowed == target)) {
                    Ok(())
                } else {
                    Err(ArgentError::new(format!(
                        "entry `{}::{}` routes output `{}` to `{}`, but that output allows only {}",
                        actor.name,
                        entry.name,
                        output.name,
                        target_label,
                        output.actors.join(" | ")
                    )))
                }
            }
        }
    }

    fn validate_route_coverage(&self, actor: &ActorDecl, entry: &EntryDecl, entry_model: &EntryModel<'_>) -> Result<()> {
        match &entry.emits {
            EmitSpec::None => Ok(()),
            EmitSpec::Outputs(_) => self.validate_named_output_coverage(actor, entry, entry_model),
        }
    }

    fn validate_named_output_coverage(&self, actor: &ActorDecl, entry: &EntryDecl, entry_model: &EntryModel<'_>) -> Result<()> {
        let outputs = entry_model.current().outputs();
        if outputs.is_empty() {
            return Ok(());
        }
        if entry.terminal_route_sets.is_empty() {
            return Err(ArgentError::new(format!(
                "entry `{}::{}` declares {} emit outputs but has no terminal `become` route",
                actor.name,
                entry.name,
                outputs.len()
            )));
        }

        for (path_idx, routes) in entry.terminal_route_sets.iter().enumerate() {
            let mut seen = BTreeSet::new();
            for route_id in routes {
                let route = entry_model.route(*route_id).expect("terminal syntax route has a resolved route");
                let Some((CovenantGroupId::Current, output)) = entry_model.route_output(*route_id) else {
                    continue;
                };
                if !seen.insert(output) {
                    return Err(ArgentError::new(format!(
                        "entry `{}::{}` terminal path {} validates output `{}` more than once",
                        actor.name, entry.name, path_idx, route.output
                    )));
                }
            }

            for output in outputs {
                if !seen.contains(&output.id()) {
                    return Err(ArgentError::new(format!(
                        "entry `{}::{}` terminal path {} does not validate output `{}`",
                        actor.name,
                        entry.name,
                        path_idx,
                        output.handle()
                    )));
                }
            }
        }
        Ok(())
    }
}

fn reject_reserved_function_identifier(name: &str) -> Result<()> {
    if name == word::CO_SPENT {
        return Err(ArgentError::new(format!("function identifier `{}` is reserved for covenant co-spend checks", word::CO_SPENT)));
    }
    if name == word::DIGEST {
        return Err(ArgentError::new(format!("function identifier `{}` is reserved for authored state digests", word::DIGEST)));
    }
    if name == word::STATE {
        return Err(ArgentError::new(format!(
            "function identifier `{}` is reserved for authored input-state reconstruction",
            word::STATE
        )));
    }
    if name == word::UNRESTRICTED {
        return Err(ArgentError::new(format!(
            "function identifier `{}` is reserved for output-value declarations",
            word::UNRESTRICTED
        )));
    }
    reject_reserved_identifier("function", name)
}

fn reject_reserved_identifier(context: &str, name: &str) -> Result<()> {
    // Only compatibility module names may enter the model in a generated namespace.
    // The source lexer still rejects every authored use of these namespaces.
    if !name.starts_with(RESERVED_GENERATED_MODULE_NAME_PREFIX) {
        let generated_prefix =
            [RESERVED_GENERATED_PREFIX, RESERVED_GENERATED_TYPE_PREFIX].into_iter().find(|prefix| name.starts_with(prefix));
        if let Some(generated_prefix) = generated_prefix {
            return Err(ArgentError::new(format!(
                "{context} identifier `{name}` uses reserved generated namespace `{generated_prefix}`"
            )));
        }
    }
    if name == word::SELF {
        return Err(ArgentError::new(format!("{context} identifier `{}` is reserved for the current actor context", word::SELF)));
    }
    if name == "State" {
        return Err(ArgentError::new(format!("{context} identifier `State` is reserved for generated Silverscript state")));
    }
    Ok(())
}

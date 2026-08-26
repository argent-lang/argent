//! Validates a fully constructed compiler model before code generation.

use std::collections::{BTreeMap, BTreeSet};

use crate::compiler::naming::to_snake;
use crate::compiler::syntax::lexer::{RESERVED_GENERATED_PREFIX, RESERVED_GENERATED_TYPE_PREFIX};
use crate::compiler::syntax::word;
use crate::compiler::syntax::{ActorDecl, ArrayDim, EmitOutput, EmitSpec, EntryDecl, EntryKind, ObserveDecl, ObservedActorDecl};
use crate::error::{ArgentError, Result};

use super::{
    EntryModel, InteractionSource, Model, ResolvedRoute, ResolvedSuccessor, observed_open_bindings, observed_open_state_for_decl,
    packed_field_len, resolve_observe_covenant_id_source, spawn_target_state,
};

impl Model<'_> {
    pub(super) fn validate(&self) -> Result<()> {
        self.validate_reserved_identifiers()?;
        self.validate_state_expansions()?;
        self.validate_reserved_self_members()?;
        self.validate_generated_actor_suffixes()?;
        self.validate_function_namespaces()?;
        self.validate_route_plan_coverage()?;

        for actor in &self.actors {
            for entry in &actor.entries {
                self.validate_entry(actor, entry)?;
            }
        }
        Ok(())
    }

    fn validate_function_namespaces(&self) -> Result<()> {
        let global_functions = self.functions.iter().map(|function| function.name.as_str()).collect::<BTreeSet<_>>();
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
        for actor in &self.actors {
            let state = self.storage_state(&actor.state)?;
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
        if &planned_actors != self.app_actors.members() {
            return Err(ArgentError::new(format!(
                "route planner actor coverage differs from the selected app; expected {:?}, found {:?}",
                self.app_actors.members(),
                planned_actors
            )));
        }

        let family_ids = self.route_families.iter().map(|family| family.id.as_str()).collect::<BTreeSet<_>>();
        for ((source, target), transition) in &self.route_transitions {
            if !self.app_actors.contains(source) || !self.app_actors.contains(target) {
                return Err(ArgentError::new(format!("route transition `{source}` -> `{target}` falls outside the selected app")));
            }
            for family_id in transition.families_to_open.iter().chain(&transition.families_to_pack) {
                if !family_ids.contains(family_id.as_str()) {
                    return Err(ArgentError::new(format!(
                        "route transition `{source}` -> `{target}` references unknown family `{family_id}`"
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

        for state in self.states.values() {
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
                .state(&expansion.base)
                .map_err(|_| ArgentError::new(format!("state `{}` expands unknown base state `{}`", state.name, expansion.base)))?;
            if base.expansion.is_some() {
                return Err(ArgentError::new(format!(
                    "state `{}` expands `{}`, but expanded states cannot currently be used as bases",
                    state.name, expansion.base
                )));
            }
            let mut seen = BTreeSet::new();
            for digest in &expansion.digests {
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
                let memory_state = self.state(&digest.state).map_err(|_| {
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
        for ct in &self.consts {
            reject_reserved_identifier("constant", &ct.name)?;
        }
        for function in &self.functions {
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

    fn validate_entry(&self, actor: &ActorDecl, entry: &EntryDecl) -> Result<()> {
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
        self.validate_observes(actor, entry)?;
        self.validate_spawns(actor, entry)?;

        if entry.kind == EntryKind::Delegate && entry.consumes.is_empty() {
            return Err(ArgentError::new(format!(
                "delegate `{}::{}` must declare its leader as the first `consumes` actor",
                actor.name, entry.name
            )));
        }

        for consume in &entry.consumes {
            self.require_template_actor(
                &consume.actor,
                format!("entry `{}::{}` consumes unknown actor `{}`", actor.name, entry.name, consume.actor),
            )?;
        }

        match &entry.emits {
            EmitSpec::None => {}
            EmitSpec::Outputs(outputs) => {
                let mut names = BTreeSet::new();
                let mut auth_indices = BTreeSet::new();
                for output in outputs {
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
                    for target in self.expand_actor_refs(&output.actors) {
                        self.require_template_actor(
                            &target,
                            format!("entry `{}::{}` output `{}` emits unknown actor `{target}`", actor.name, entry.name, output.name),
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

        let entry_model = self.entry_model(actor, entry)?;
        for route in entry_model.routes() {
            if let ResolvedSuccessor::Constructed { actor: target_expr, state } = &route.successor {
                if state.trim().is_empty() {
                    return Err(ArgentError::new(format!(
                        "entry `{}::{}` has an empty `become` state for actor `{target_expr}`",
                        actor.name, entry.name
                    )));
                }
                for target in self.route_targets(actor, entry, route)? {
                    self.require_template_actor(
                        &target,
                        format!("entry `{}::{}` routes to unknown actor `{target}`", actor.name, entry.name),
                    )?;
                    self.actor_state(&target)?;
                }
            }
            self.validate_route_allowed(actor, entry, route)?;
        }
        self.validate_route_coverage(actor, entry, entry_model)?;
        Ok(())
    }

    fn validate_spawns(&self, actor: &ActorDecl, entry: &EntryDecl) -> Result<()> {
        if entry.kind == EntryKind::Delegate && !entry.spawns.is_empty() {
            return Err(ArgentError::new(format!("delegate `{}::{}` cannot spawn covenant outputs", actor.name, entry.name)));
        }

        let observe_names = entry.observes.iter().map(|observe| observe.name.as_str()).collect::<BTreeSet<_>>();
        let mut source_names = self
            .storage_state(&actor.state)?
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
        for group in self.entry_model(actor, entry)?.genesis_groups() {
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
                if spawn_target_state(interaction.target(), &output.actor, actor, entry, self)?.is_none() {
                    return Err(ArgentError::new(format!(
                        "entry `{}::{}` spawn `{}.{}` target `{}` must be an actor_type value or a selected-app or linked actor",
                        actor.name, entry.name, spawn.name, output.name, output.actor
                    )));
                }
            }
        }
        Ok(())
    }

    fn validate_observes(&self, actor: &ActorDecl, entry: &EntryDecl) -> Result<()> {
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
            resolve_observe_covenant_id_source(actor, entry, self, observe)?;
            self.validate_observed_open_bindings(actor, entry, observe)?;
            self.validate_observed_actor_types(actor, entry, observe, "input", &observe.inputs)?;
            self.validate_observed_actor_types(actor, entry, observe, "output", &observe.outputs)?;
        }
        Ok(())
    }

    fn validate_observed_open_bindings(&self, actor: &ActorDecl, entry: &EntryDecl, observe: &ObserveDecl) -> Result<()> {
        let mut bindings = BTreeMap::new();
        let mut source_names =
            self.storage_state(&actor.state)?.fields.iter().map(|field| field.name.as_str()).collect::<BTreeSet<_>>();
        source_names.extend(entry.params.iter().map(|param| param.name.as_str()));
        source_names.extend(entry.consumes.iter().map(|consume| consume.name.as_str()));
        for input in &observe.inputs {
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
            self.state(state)?;
            if let Some(previous_state) = bindings.insert(input.actor.as_str(), state) {
                return Err(ArgentError::new(format!(
                    "entry `{}::{}` observe `{}` declares open observed actor binding `{}` for both `{previous_state}` and `{state}`",
                    actor.name, entry.name, observe.name, input.actor
                )));
            }
            if !observe.outputs.iter().any(|output| output.actor == input.actor) {
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
            if let Some(state) = observed_open_state_for_decl(actor, entry, observe, observed, self)? {
                self.state(&state).map_err(|_| {
                    ArgentError::new(format!(
                        "entry `{}::{}` observe `{}` {section} `{}` references unknown state `{state}`",
                        actor.name, entry.name, observe.name, observed.name
                    ))
                })?;
                continue;
            }
            if self.linked_actor(&observed.actor).is_none() && !self.app_actors.contains(&observed.actor) {
                return Err(ArgentError::new(format!(
                    "entry `{}::{}` observe `{}` {section} `{}` references actor `{}` outside selected app `{}`; foreign actors must be imported through their app",
                    actor.name, entry.name, observe.name, observed.name, observed.actor, self.app_name
                )));
            }
            self.actor_state(&observed.actor).map_err(|_| {
                ArgentError::new(format!(
                    "entry `{}::{}` observe `{}` {section} `{}` references unknown actor `{}`",
                    actor.name, entry.name, observe.name, observed.name, observed.actor
                ))
            })?;
        }
        Ok(())
    }

    fn require_template_actor(&self, actor: &str, message: String) -> Result<()> {
        if !self.app_actors.contains(actor) {
            return Err(ArgentError::new(message));
        }
        self.actor_state(actor)?;
        Ok(())
    }

    fn validate_route_allowed(&self, actor: &ActorDecl, entry: &EntryDecl, route: &ResolvedRoute) -> Result<()> {
        let target_label = match &route.successor {
            ResolvedSuccessor::ExactSelf => word::SELF,
            ResolvedSuccessor::Constructed { actor, .. } => actor,
        };
        match &entry.emits {
            EmitSpec::None => Err(ArgentError::new(format!(
                "entry `{}::{}` has a `become` route to `{}`, but declares `emits none`",
                actor.name, entry.name, target_label
            ))),
            EmitSpec::Outputs(outputs) => {
                let output = outputs.iter().find(|output| output.name == route.output).ok_or_else(|| {
                    ArgentError::new(format!(
                        "entry `{}::{}` routes through unknown output `{}`",
                        actor.name, entry.name, route.output
                    ))
                })?;
                let allowed = self.expand_actor_refs(&output.actors);
                let targets = self.route_targets(actor, entry, route)?;
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
            EmitSpec::Outputs(outputs) => self.validate_named_output_coverage(actor, entry, entry_model, outputs),
        }
    }

    fn validate_named_output_coverage(
        &self,
        actor: &ActorDecl,
        entry: &EntryDecl,
        entry_model: &EntryModel<'_>,
        outputs: &[EmitOutput],
    ) -> Result<()> {
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

        let declared = outputs.iter().map(|output| output.name.as_str()).collect::<BTreeSet<_>>();
        for (path_idx, routes) in entry.terminal_route_sets.iter().enumerate() {
            let mut seen = BTreeSet::new();
            for route_id in routes {
                let route = entry_model.route(*route_id).expect("terminal syntax route has a resolved route");
                let output = route.output.as_str();
                if !declared.contains(output) {
                    continue;
                }
                if !seen.insert(output) {
                    return Err(ArgentError::new(format!(
                        "entry `{}::{}` terminal path {} validates output `{}` more than once",
                        actor.name, entry.name, path_idx, output
                    )));
                }
            }

            for output in outputs {
                if !seen.contains(output.name.as_str()) {
                    return Err(ArgentError::new(format!(
                        "entry `{}::{}` terminal path {} does not validate output `{}`",
                        actor.name, entry.name, path_idx, output.name
                    )));
                }
            }
        }
        Ok(())
    }
}

fn reject_reserved_function_identifier(name: &str) -> Result<()> {
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
    let generated_prefix =
        [RESERVED_GENERATED_PREFIX, RESERVED_GENERATED_TYPE_PREFIX].into_iter().find(|prefix| name.starts_with(prefix));
    if let Some(generated_prefix) = generated_prefix {
        return Err(ArgentError::new(format!("{context} identifier `{name}` uses reserved generated namespace `{generated_prefix}`")));
    }
    if name == word::SELF {
        return Err(ArgentError::new(format!("{context} identifier `{}` is reserved for the current actor context", word::SELF)));
    }
    if name == "State" {
        return Err(ArgentError::new(format!("{context} identifier `State` is reserved for generated Silverscript state")));
    }
    Ok(())
}

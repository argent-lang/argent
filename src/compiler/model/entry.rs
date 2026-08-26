//! Source-backed entry interactions grouped by covenant context.

use std::collections::{BTreeMap, BTreeSet};

use crate::compiler::naming::{is_identifier, to_snake};
use crate::compiler::syntax::body::{EntryStatement, EntrySuccessor};
use crate::compiler::syntax::lexer::{Token, TokenKind, lex};
use crate::compiler::syntax::word;
use crate::compiler::syntax::{
    ActorDecl, ConsumeDecl, EmitOutput, EmitSpec, EntryDecl, ObserveDecl, ObservedActorDecl, RouteId, SpawnDecl, SpawnOutputDecl,
    TypeRef,
};
use crate::error::{ArgentError, Result};

use super::{ActorEnumInfo, AppActors, Model};

#[cfg(test)]
mod tests;

/// The normalized interactions and selector-expanded routes for one entry.
#[derive(Debug)]
pub(crate) struct EntryModel<'a> {
    source: &'a EntryDecl,
    groups: Vec<CovenantGroup<'a>>,
    template_selectors: BTreeMap<String, TemplateSelector>,
    routes: Vec<ResolvedRoute>,
    route_indexes: BTreeMap<RouteId, usize>,
}

/// One semantically resolved current-covenant successor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedRoute {
    pub(crate) id: RouteId,
    pub(crate) output: String,
    pub(crate) successor: ResolvedSuccessor,
}

/// The state-preservation intent of one resolved successor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResolvedSuccessor {
    ExactSelf,
    Constructed { actor: String, state: String },
}

impl<'a> EntryModel<'a> {
    /// Build an entry model from its source actor and declaration.
    pub(crate) fn build(actor: &'a ActorDecl, source: &'a EntryDecl, actor_enums: &BTreeMap<String, ActorEnumInfo>) -> Result<Self> {
        Self::new(actor, source, actor_enums, template_selectors_for_entry(actor, source, actor_enums)?)
    }

    fn new(
        actor: &'a ActorDecl,
        source: &'a EntryDecl,
        actor_enums: &BTreeMap<String, ActorEnumInfo>,
        template_selectors: BTreeMap<String, TemplateSelector>,
    ) -> Result<Self> {
        reject_external_exact_successors(actor, source, source.body.statements())?;
        let routes = resolve_current_routes(actor, source, actor_enums)?;
        let route_indexes = routes.iter().enumerate().map(|(index, route)| (route.id, index)).collect();
        let current_inputs = source
            .consumes
            .iter()
            .enumerate()
            .map(|(index, consume)| EntryInteraction {
                source: InteractionSource::Consume(consume),
                handle: &consume.name,
                index,
                target: ActorTarget::static_actor(&consume.actor),
            })
            .collect();
        let current_outputs = match &source.emits {
            EmitSpec::None => Vec::new(),
            EmitSpec::Outputs(outputs) => outputs
                .iter()
                .map(|output| EntryInteraction {
                    source: InteractionSource::CurrentOutput(output),
                    handle: &output.name,
                    index: output.auth_index,
                    target: ActorTarget::domain(&output.actors, actor_enums),
                })
                .collect(),
        };
        let mut groups = vec![CovenantGroup { covenant: CovenantContext::Current, inputs: current_inputs, outputs: current_outputs }];
        groups.extend(source.observes.iter().map(|observe| {
            CovenantGroup {
                covenant: CovenantContext::Existing(observe),
                inputs: observe
                    .inputs
                    .iter()
                    .enumerate()
                    .map(|(index, input)| EntryInteraction {
                        source: InteractionSource::ObserveInput(input),
                        handle: &input.name,
                        index,
                        target: ActorTarget::observed(source, observe, input),
                    })
                    .collect(),
                outputs: observe
                    .outputs
                    .iter()
                    .enumerate()
                    .map(|(index, output)| EntryInteraction {
                        source: InteractionSource::ObserveOutput(output),
                        handle: &output.name,
                        index,
                        target: ActorTarget::observed(source, observe, output),
                    })
                    .collect(),
            }
        }));
        groups.extend(source.spawns.iter().map(|spawn| {
            CovenantGroup {
                covenant: CovenantContext::Genesis(spawn),
                inputs: Vec::new(),
                outputs: spawn
                    .outputs
                    .iter()
                    .map(|output| EntryInteraction {
                        source: InteractionSource::SpawnOutput(output),
                        handle: &output.name,
                        index: output.group_index,
                        target: ActorTarget::source_or_static(source, &output.actor),
                    })
                    .collect(),
            }
        }));
        Ok(Self { source, groups, template_selectors, routes, route_indexes })
    }

    /// Return the source entry declaration.
    pub(crate) fn source(&self) -> &'a EntryDecl {
        self.source
    }

    /// Return the interaction group governed by the current covenant.
    pub(crate) fn current(&self) -> &CovenantGroup<'a> {
        self.groups.first().expect("entry model always contains its current covenant")
    }

    /// Iterate all covenant groups in current, existing, then genesis order.
    pub(crate) fn groups(&self) -> impl Iterator<Item = &CovenantGroup<'a>> {
        self.groups.iter()
    }

    /// Iterate existing-covenant groups in `observes` declaration order.
    pub(crate) fn existing_groups(&self) -> impl Iterator<Item = &CovenantGroup<'a>> {
        self.groups.iter().filter(|group| matches!(group.covenant, CovenantContext::Existing(_)))
    }

    /// Iterate genesis groups in `spawns` declaration order.
    pub(crate) fn genesis_groups(&self) -> impl Iterator<Item = &CovenantGroup<'a>> {
        self.groups.iter().filter(|group| matches!(group.covenant, CovenantContext::Genesis(_)))
    }

    /// Return the actor-enum selectors visible to this entry.
    pub(crate) fn template_selectors(&self) -> &BTreeMap<String, TemplateSelector> {
        &self.template_selectors
    }

    /// Return resolved routes in terminal source order.
    pub(crate) fn routes(&self) -> &[ResolvedRoute] {
        &self.routes
    }

    /// Resolve a syntax route through its stable source identity.
    pub(crate) fn route(&self, id: RouteId) -> Option<&ResolvedRoute> {
        self.route_indexes.get(&id).and_then(|index| self.routes.get(*index))
    }

    /// Expand body-derived constructed routes to their concrete artifact targets.
    pub(crate) fn expanded_routes(&self) -> Vec<ResolvedRoute> {
        expand_routes(self.routes.iter(), &self.template_selectors)
    }

    /// Collect concrete app actors whose templates this entry reads or writes.
    ///
    /// Current outputs follow the body-selected routes; existing and genesis
    /// outputs are exhaustive in their covenant groups.
    pub(crate) fn actor_template_uses(&self, source_actor: &str, app_actors: &AppActors) -> ActorTemplateUses {
        let mut uses = ActorTemplateUses::default();

        for group in self.groups() {
            for interaction in group.inputs() {
                for target in interaction.target().static_actors() {
                    if app_actors.contains(target) && !app_actors.is_singleton_actor_self_target(source_actor, target) {
                        uses.reads.insert(target.to_string());
                    }
                }
            }
        }

        // Current declarations define allowed output domains; body routes identify
        // concrete template writes, while selector routes use selector witnesses.
        for route in &self.routes {
            let ResolvedSuccessor::Constructed { actor, .. } = &route.successor else {
                continue;
            };
            if !self.template_selectors.contains_key(actor) && app_actors.contains(actor) && actor != source_actor {
                uses.writes.insert(actor.clone());
            }
        }
        for group in self.existing_groups().chain(self.genesis_groups()) {
            for interaction in group.outputs() {
                for target in interaction.target().static_actors() {
                    if app_actors.contains(target) && target != source_actor {
                        uses.writes.insert(target.to_string());
                    }
                }
            }
        }

        uses
    }
}

fn resolve_current_routes(
    actor: &ActorDecl,
    entry: &EntryDecl,
    actor_enums: &BTreeMap<String, ActorEnumInfo>,
) -> Result<Vec<ResolvedRoute>> {
    entry
        .routes
        .iter()
        .map(|route| {
            if matches!(route.successor, EntrySuccessor::ExactSelf { .. }) {
                require_exact_self_output(actor, entry, actor_enums, &route.output)?;
            }
            let successor = match route.successor {
                EntrySuccessor::ExactSelf { .. } => ResolvedSuccessor::ExactSelf,
                EntrySuccessor::Constructed { actor: target, state } => ResolvedSuccessor::Constructed {
                    actor: entry.body.span_text(target).trim().to_string(),
                    state: entry.body.span_text(state).trim().to_string(),
                },
            };
            Ok(ResolvedRoute { id: route.id, output: route.output.clone(), successor })
        })
        .collect()
}

fn require_exact_self_output(
    actor: &ActorDecl,
    entry: &EntryDecl,
    actor_enums: &BTreeMap<String, ActorEnumInfo>,
    output_name: &str,
) -> Result<()> {
    let EmitSpec::Outputs(outputs) = &entry.emits else {
        return Err(ArgentError::new(format!(
            "entry `{}::{}` cannot use exact successor `self` because it declares `emits none`",
            actor.name, entry.name
        )));
    };
    let output = outputs.iter().find(|output| output.name == output_name).ok_or_else(|| {
        ArgentError::new(format!("entry `{}::{}` routes through unknown output `{output_name}`", actor.name, entry.name))
    })?;
    if output_permits_actor(output, &actor.name, actor_enums) {
        return Ok(());
    }
    Err(ArgentError::new(format!(
        "entry `{}::{}` cannot preserve exact self through output `{output_name}` because it allows only {}",
        actor.name,
        entry.name,
        output.actors.join(" | ")
    )))
}

fn output_permits_actor(output: &EmitOutput, actor: &str, actor_enums: &BTreeMap<String, ActorEnumInfo>) -> bool {
    output.actors.iter().any(|candidate| {
        candidate == actor
            || actor_enums.get(candidate).is_some_and(|actor_enum| actor_enum.variants.iter().any(|variant| variant == actor))
    })
}

fn reject_external_exact_successors(actor: &ActorDecl, entry: &EntryDecl, statements: &[EntryStatement]) -> Result<()> {
    for statement in statements {
        match statement {
            EntryStatement::If { then_branch, else_branch, .. } => {
                reject_external_exact_successors(actor, entry, std::slice::from_ref(then_branch.as_ref()))?;
                if let Some(else_branch) = else_branch {
                    reject_external_exact_successors(actor, entry, std::slice::from_ref(else_branch.as_ref()))?;
                }
            }
            EntryStatement::For { body, .. } => {
                reject_external_exact_successors(actor, entry, std::slice::from_ref(body.as_ref()))?;
            }
            EntryStatement::Block { statements, .. } => reject_external_exact_successors(actor, entry, statements)?,
            EntryStatement::ValidateOutputsBecome { group, routes, .. }
                if routes.iter().any(|route| matches!(route.successor, EntrySuccessor::ExactSelf { .. })) =>
            {
                return Err(ArgentError::new(format!(
                    "entry `{}::{}` cannot use exact successor `self` for observe or spawn `{group}` outputs",
                    actor.name, entry.name
                )));
            }
            EntryStatement::Become { .. }
            | EntryStatement::ValidateOutputsBecome { .. }
            | EntryStatement::Local { .. }
            | EntryStatement::Plain { .. } => {}
        }
    }
    Ok(())
}

/// Actor template capabilities used while lowering one entry.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ActorTemplateUses {
    pub(crate) reads: BTreeSet<String>,
    pub(crate) writes: BTreeSet<String>,
}

/// Ordered interactions governed by one covenant ID.
///
/// The current covenant and each observe or spawn clause form separate groups.
#[derive(Debug)]
pub(crate) struct CovenantGroup<'a> {
    covenant: CovenantContext<'a>,
    inputs: Vec<EntryInteraction<'a>>,
    outputs: Vec<EntryInteraction<'a>>,
}

impl<'a> CovenantGroup<'a> {
    /// Return the authored clause name, or `None` for the current covenant.
    pub(crate) fn name(&self) -> Option<&'a str> {
        match self.covenant {
            CovenantContext::Current => None,
            CovenantContext::Existing(observe) => Some(&observe.name),
            CovenantContext::Genesis(spawn) => Some(&spawn.name),
        }
    }

    /// Return the group's ordered input interactions.
    pub(crate) fn inputs(&self) -> &[EntryInteraction<'a>] {
        &self.inputs
    }

    /// Return the group's ordered output interactions.
    pub(crate) fn outputs(&self) -> &[EntryInteraction<'a>] {
        &self.outputs
    }

    /// Return the source `observes` clause for an existing covenant.
    pub(crate) fn observe(&self) -> Option<&'a ObserveDecl> {
        match self.covenant {
            CovenantContext::Existing(observe) => Some(observe),
            CovenantContext::Current | CovenantContext::Genesis(_) => None,
        }
    }

    /// Return the source `spawns` clause for a genesis covenant.
    pub(crate) fn spawn(&self) -> Option<&'a SpawnDecl> {
        match self.covenant {
            CovenantContext::Genesis(spawn) => Some(spawn),
            CovenantContext::Current | CovenantContext::Existing(_) => None,
        }
    }
}

/// The covenant instance described by an interaction group.
#[derive(Clone, Copy, Debug)]
pub(crate) enum CovenantContext<'a> {
    /// The covenant executing the entry.
    Current,
    /// An existing covenant selected by an `observes` clause.
    Existing(&'a ObserveDecl),
    /// A new covenant created by a `spawns` clause.
    Genesis(&'a SpawnDecl),
}

/// One normalized interaction retaining its source and target domain.
#[derive(Debug)]
pub(crate) struct EntryInteraction<'a> {
    source: InteractionSource<'a>,
    handle: &'a str,
    index: usize,
    target: ActorTarget,
}

impl<'a> EntryInteraction<'a> {
    /// Return the exact source node that declared this interaction.
    pub(crate) fn source(&self) -> InteractionSource<'a> {
        self.source
    }

    /// Return the declared interaction handle.
    pub(crate) fn handle(&self) -> &'a str {
        self.handle
    }

    /// Return the interaction's index within its covenant side.
    pub(crate) fn index(&self) -> usize {
        self.index
    }

    /// Return the compiler-known target candidates.
    pub(crate) fn target(&self) -> &ActorTarget {
        &self.target
    }
}

/// The exact source declaration represented by an entry interaction.
#[derive(Clone, Copy, Debug)]
pub(crate) enum InteractionSource<'a> {
    /// A current-covenant input from `consumes`.
    Consume(&'a ConsumeDecl),
    /// A current-covenant output.
    CurrentOutput(&'a EmitOutput),
    /// An input from an `observes` clause.
    ObserveInput(&'a ObservedActorDecl),
    /// An output from an `observes` clause.
    ObserveOutput(&'a ObservedActorDecl),
    /// An output from a `spawns` clause.
    SpawnOutput(&'a SpawnOutputDecl),
}

/// A source-selected target or compiler-known static actor domain.
#[derive(Debug)]
pub(crate) enum ActorTarget {
    /// A runtime actor-type value or open observed binding.
    Source(String),
    /// One fixed actor or an expanded actor-enum domain.
    Static(Vec<String>),
}

impl ActorTarget {
    fn static_actor(actor: &str) -> Self {
        Self::Static(vec![actor.to_string()])
    }

    fn source_or_static(entry: &EntryDecl, expression: &str) -> Self {
        let static_target = is_actor_reference(expression)
            && !entry.params.iter().any(|param| param.name == expression)
            && !expression.strip_prefix("self.").is_some_and(is_identifier);
        if static_target { Self::Static(vec![expression.to_string()]) } else { Self::Source(expression.to_string()) }
    }

    fn observed(entry: &EntryDecl, observe: &ObserveDecl, observed: &ObservedActorDecl) -> Self {
        let open_binding = observed.open_state.is_some()
            || observe.inputs.iter().any(|input| input.actor == observed.actor && input.open_state.is_some());
        if open_binding { Self::Source(observed.actor.clone()) } else { Self::source_or_static(entry, &observed.actor) }
    }

    fn domain(actors: &[String], actor_enums: &BTreeMap<String, ActorEnumInfo>) -> Self {
        Self::Static(
            actors
                .iter()
                .flat_map(|actor| actor_enums.get(actor).map_or_else(|| vec![actor.clone()], |actor_enum| actor_enum.variants.clone()))
                .collect(),
        )
    }

    /// Iterate candidate actor names or unresolved target expressions.
    pub(crate) fn actors(&self) -> impl Iterator<Item = &str> {
        match self {
            Self::Source(actor) => std::slice::from_ref(actor),
            Self::Static(actors) => actors.as_slice(),
        }
        .iter()
        .map(String::as_str)
    }

    /// Iterate only compiler-known static actor candidates.
    pub(crate) fn static_actors(&self) -> impl Iterator<Item = &str> {
        match self {
            Self::Source(_) => [].as_slice(),
            Self::Static(actors) => actors.as_slice(),
        }
        .iter()
        .map(String::as_str)
    }

    /// Return the sole static actor, if this target is a singleton domain.
    pub(crate) fn single_static_actor(&self) -> Option<&str> {
        match self {
            Self::Static(actors) if actors.len() == 1 => Some(actors[0].as_str()),
            Self::Source(_) | Self::Static(_) => None,
        }
    }

    /// Return whether a source value selects this target.
    pub(crate) fn is_source(&self) -> bool {
        matches!(self, Self::Source(_))
    }
}

/// A simple entry-clause reference resolved without lowering its expression.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum ClauseReference {
    StateField(String),
    Bare(String),
}

/// An actor-type source and the state selected by its declared type.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum ClauseActorTypeRef {
    StateField { field: String, state: String },
    EntryArgument { name: String, state: String },
}

impl ClauseActorTypeRef {
    /// Return the actor state declared by this source value.
    pub(crate) fn state(&self) -> &str {
        match self {
            Self::StateField { state, .. } | Self::EntryArgument { state, .. } => state,
        }
    }
}

/// A source value supplying an observed covenant ID.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CovenantIdSource {
    StateField { field: String },
    EntryArgument { index: usize },
}

fn clause_reference(expr: &str) -> Result<Option<ClauseReference>> {
    let tokens = lex(expr).map_err(|err| ArgentError::new(format!("failed to lex clause reference `{expr}`: {}", err.message)))?;
    match tokens.as_slice() {
        [
            Token { kind: TokenKind::Ident(self_name), .. },
            Token { kind: TokenKind::Symbol('.'), .. },
            Token { kind: TokenKind::Ident(field), .. },
            Token { kind: TokenKind::Eof, .. },
        ] if self_name == word::SELF => Ok(Some(ClauseReference::StateField(field.clone()))),
        [Token { kind: TokenKind::Ident(name), .. }, Token { kind: TokenKind::Eof, .. }] => {
            Ok(Some(ClauseReference::Bare(name.clone())))
        }
        _ => Ok(None),
    }
}

pub(crate) fn clause_actor_type_ref(
    expr: &str,
    actor: &ActorDecl,
    entry: &EntryDecl,
    model: &Model<'_>,
) -> Result<Option<ClauseActorTypeRef>> {
    let state = model.storage_state(&actor.state)?;
    let (source, ty) = match clause_reference(expr)? {
        Some(ClauseReference::StateField(field_name)) => {
            let field = state.fields.iter().find(|field| field.name == field_name).ok_or_else(|| {
                ArgentError::new(format!(
                    "entry `{}::{}` references unknown state field `{}.{field_name}`",
                    actor.name,
                    entry.name,
                    word::SELF
                ))
            })?;
            (ClauseReference::StateField(field_name), &field.ty)
        }
        Some(ClauseReference::Bare(name)) => {
            if let Some(param) = entry.params.iter().find(|param| param.name == name) {
                (ClauseReference::Bare(name), &param.ty)
            } else if state.fields.iter().any(|field| field.name == name) {
                return Err(ArgentError::new(format!(
                    "entry `{}::{}` state field `{name}` must be referenced as `{}.{name}` in entry clauses",
                    actor.name,
                    entry.name,
                    word::SELF
                )));
            } else {
                return Ok(None);
            }
        }
        None => return Ok(None),
    };

    let Some(actor_state) = ty.actor_state.as_ref() else {
        return Err(ArgentError::new(format!(
            "entry `{}::{}` clause reference `{}` has type `{}`; expected `{}<State>`",
            actor.name,
            entry.name,
            expr.trim(),
            ty.to_source(),
            word::ACTOR_TYPE
        )));
    };
    model.state(actor_state)?;
    Ok(Some(match source {
        ClauseReference::StateField(field) => ClauseActorTypeRef::StateField { field, state: actor_state.clone() },
        ClauseReference::Bare(name) => ClauseActorTypeRef::EntryArgument { name, state: actor_state.clone() },
    }))
}

pub(crate) fn source_actor_type_state_for_expr(
    expr: &str,
    actor: &ActorDecl,
    entry: &EntryDecl,
    model: &Model<'_>,
) -> Result<Option<String>> {
    Ok(clause_actor_type_ref(expr, actor, entry, model)?.map(|source| source.state().to_string()))
}

// Spawn targets may be an explicitly dynamic actor_type value or any fixed
// actor resolved by the selected app. Linked templates remain imported
// capabilities and do not enter the selected app's route graph.
pub(crate) fn spawn_target_state(
    target: &ActorTarget,
    expr: &str,
    actor: &ActorDecl,
    entry: &EntryDecl,
    model: &Model<'_>,
) -> Result<Option<String>> {
    if let Some(target) = model.resolve_static_actor_target(target) {
        return Ok(Some(target.state().to_string()));
    }
    source_actor_type_state_for_expr(expr, actor, entry, model)
}

pub(crate) fn observed_open_bindings(observe: &ObserveDecl) -> BTreeMap<&str, &str> {
    observe.inputs.iter().filter_map(|input| input.open_state.as_deref().map(|state| (input.actor.as_str(), state))).collect()
}

pub(crate) fn observed_dynamic_binding_state<'a>(observe: &'a ObserveDecl, observed: &'a ObservedActorDecl) -> Option<&'a str> {
    observed.open_state.as_deref().or_else(|| observed_open_bindings(observe).get(observed.actor.as_str()).copied())
}

pub(crate) fn observed_open_state_for_decl(
    actor: &ActorDecl,
    entry: &EntryDecl,
    observe: &ObserveDecl,
    observed: &ObservedActorDecl,
    model: &Model<'_>,
) -> Result<Option<String>> {
    if let Some(state) = observed_dynamic_binding_state(observe, observed) {
        model.state(state)?;
        return Ok(Some(state.to_string()));
    }
    source_actor_type_state_for_expr(&observed.actor, actor, entry, model)
}

pub(crate) fn observed_is_dynamic_binding(observe: &ObserveDecl, observed: &ObservedActorDecl) -> bool {
    observed_dynamic_binding_state(observe, observed).is_some()
}

pub(crate) fn resolve_observe_covenant_id_source(
    actor: &ActorDecl,
    entry: &EntryDecl,
    model: &Model<'_>,
    observe: &ObserveDecl,
) -> Result<CovenantIdSource> {
    match clause_reference(&observe.covenant_expr)? {
        Some(ClauseReference::StateField(field_name)) => {
            let field = model.storage_state(&actor.state)?.fields.iter().find(|field| field.name == field_name).ok_or_else(|| {
                ArgentError::new(format!(
                    "entry `{}::{}` observe `{}` references unknown state field `{}.{field_name}`",
                    actor.name,
                    entry.name,
                    observe.name,
                    word::SELF
                ))
            })?;
            require_covenant_id_source_type(actor, entry, observe, &format!("{}.{field_name}", word::SELF), &field.ty)?;
            Ok(CovenantIdSource::StateField { field: field_name })
        }
        Some(ClauseReference::Bare(argument_name)) => {
            if let Some((index, param)) = entry.params.iter().enumerate().find(|(_, param)| param.name == argument_name) {
                require_covenant_id_source_type(actor, entry, observe, &argument_name, &param.ty)?;
                return Ok(CovenantIdSource::EntryArgument { index });
            }
            if model.storage_state(&actor.state)?.fields.iter().any(|field| field.name == argument_name) {
                return Err(ArgentError::new(format!(
                    "entry `{}::{}` observe `{}` state field `{argument_name}` must be referenced as `{}.{argument_name}`",
                    actor.name,
                    entry.name,
                    observe.name,
                    word::SELF
                )));
            }
            Err(unsupported_observe_covenant_id_source(actor, entry, observe))
        }
        None => Err(unsupported_observe_covenant_id_source(actor, entry, observe)),
    }
}

fn require_covenant_id_source_type(
    actor: &ActorDecl,
    entry: &EntryDecl,
    observe: &ObserveDecl,
    source: &str,
    ty: &TypeRef,
) -> Result<()> {
    if ty.name == word::COVENANT_ID && ty.array.is_none() && ty.actor_state.is_none() {
        return Ok(());
    }
    Err(ArgentError::new(format!(
        "entry `{}::{}` observe `{}` covenant id source `{source}` has type `{}`; expected `{}`",
        actor.name,
        entry.name,
        observe.name,
        ty.to_source(),
        word::COVENANT_ID
    )))
}

fn unsupported_observe_covenant_id_source(actor: &ActorDecl, entry: &EntryDecl, observe: &ObserveDecl) -> ArgentError {
    ArgentError::new(format!(
        "entry `{}::{}` observe `{}` covenant id source must be a `{}.<field>` state field or entry argument of type `{}`",
        actor.name,
        entry.name,
        observe.name,
        word::SELF,
        word::COVENANT_ID
    ))
}

fn is_actor_reference(value: &str) -> bool {
    is_identifier(value) || value.split_once("::").is_some_and(|(app, actor)| is_identifier(app) && is_identifier(actor))
}

/// An actor-enum value selecting a template within one state domain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TemplateSelector {
    pub(crate) name: String,
    pub(crate) actor_enum: String,
    pub(crate) state: String,
    pub(crate) variants: Vec<String>,
    pub(crate) selector_expr: String,
    pub(crate) fixed_actor: Option<String>,
}

impl TemplateSelector {
    /// Return the fixed actor, or the complete selector domain when dynamic.
    pub(crate) fn route_actors(&self) -> Vec<String> {
        self.fixed_actor.as_ref().map_or_else(|| self.variants.clone(), |actor| vec![actor.clone()])
    }
}

/// Source context shared while validating template selectors.
struct TemplateSelectorContext<'a> {
    actor: &'a ActorDecl,
    entry: &'a EntryDecl,
    actor_enums: &'a BTreeMap<String, ActorEnumInfo>,
}

/// One selector construction request and its optional type constraints.
struct TemplateSelectorRequest<'a> {
    name: &'a str,
    actor_enum_name: &'a str,
    selector_expr: &'a str,
    fixed_actor: Option<&'a str>,
    expected_state: Option<&'a str>,
    expected_actor_enum: Option<&'a str>,
}

/// Collect actor-enum selectors declared by entry parameters and body locals.
fn template_selectors_for_entry(
    actor: &ActorDecl,
    entry: &EntryDecl,
    actor_enums: &BTreeMap<String, ActorEnumInfo>,
) -> Result<BTreeMap<String, TemplateSelector>> {
    let ctx = TemplateSelectorContext { actor, entry, actor_enums };
    let mut selectors = BTreeMap::new();
    for param in &entry.params {
        if param.ty.array.is_some() || !actor_enums.contains_key(&param.ty.name) {
            continue;
        }
        let selector = template_selector_from_actor_enum_value(
            &ctx,
            TemplateSelectorRequest {
                name: &param.name,
                actor_enum_name: &param.ty.name,
                selector_expr: &param.name,
                fixed_actor: None,
                expected_state: None,
                expected_actor_enum: Some(&param.ty.name),
            },
        )?;
        insert_template_selector(actor, entry, &mut selectors, selector)?;
    }

    // Route planning needs every possible selector domain. Lexical visibility
    // remains the responsibility of body lowering at each route use.
    for declaration in entry.body.local_declarations() {
        let binding = &declaration.binding;
        let expr = entry.body.span_text(declaration.initializer).trim();
        if let Some(state) = binding.actor_type_state.as_deref() {
            let selector = template_selector_from_initializer(&ctx, &binding.name, Some(state), None, expr)?;
            insert_template_selector(actor, entry, &mut selectors, selector)?;
            continue;
        }

        if actor_enums.contains_key(&binding.source_type) {
            let mut selector = template_selector_from_initializer(&ctx, &binding.name, None, Some(&binding.source_type), expr)?;
            selector.selector_expr = binding.name.clone();
            insert_template_selector(actor, entry, &mut selectors, selector)?;
        }
    }
    Ok(selectors)
}

fn insert_template_selector(
    actor: &ActorDecl,
    entry: &EntryDecl,
    selectors: &mut BTreeMap<String, TemplateSelector>,
    selector: TemplateSelector,
) -> Result<()> {
    let name = selector.name.clone();
    if selectors.insert(name.clone(), selector).is_some() {
        return Err(ArgentError::new(format!("entry `{}::{}` declares actor handle `{name}` more than once", actor.name, entry.name)));
    }
    Ok(())
}

fn template_selector_from_initializer(
    ctx: &TemplateSelectorContext<'_>,
    name: &str,
    expected_state: Option<&str>,
    expected_actor_enum: Option<&str>,
    expr: &str,
) -> Result<TemplateSelector> {
    if let Some((actor_enum, selector_expr)) = parse_actor_enum_selector(expr) {
        return template_selector_from_actor_enum_value(
            ctx,
            TemplateSelectorRequest {
                name,
                actor_enum_name: actor_enum,
                selector_expr,
                fixed_actor: None,
                expected_state,
                expected_actor_enum,
            },
        );
    }
    if let Some((actor_enum, variant)) = parse_actor_enum_variant(expr) {
        return template_selector_from_actor_enum_value(
            ctx,
            TemplateSelectorRequest {
                name,
                actor_enum_name: &actor_enum,
                selector_expr: "",
                fixed_actor: Some(&variant),
                expected_state,
                expected_actor_enum,
            },
        );
    }
    if let Some(actor_enum) = expected_actor_enum {
        return template_selector_from_actor_enum_value(
            ctx,
            TemplateSelectorRequest {
                name,
                actor_enum_name: actor_enum,
                selector_expr: expr,
                fixed_actor: None,
                expected_state,
                expected_actor_enum,
            },
        );
    }
    Err(ArgentError::new(format!(
        "entry `{}::{}` declares actor handle `{name}` without an actor enum initializer",
        ctx.actor.name, ctx.entry.name
    )))
}

fn template_selector_from_actor_enum_value(
    ctx: &TemplateSelectorContext<'_>,
    request: TemplateSelectorRequest<'_>,
) -> Result<TemplateSelector> {
    if let Some(expected_actor_enum) = request.expected_actor_enum
        && request.actor_enum_name != expected_actor_enum
    {
        return Err(ArgentError::new(format!(
            "entry `{}::{}` declares actor enum value `{name}` as `{expected_actor_enum}`, but initializes it from `{actor_enum_name}`",
            ctx.actor.name,
            ctx.entry.name,
            name = request.name,
            actor_enum_name = request.actor_enum_name
        )));
    }
    let actor_enum = ctx.actor_enums.get(request.actor_enum_name).ok_or_else(|| {
        ArgentError::new(format!(
            "entry `{}::{}` declares actor handle `{name}` from unknown actor enum `{actor_enum_name}`",
            ctx.actor.name,
            ctx.entry.name,
            name = request.name,
            actor_enum_name = request.actor_enum_name
        ))
    })?;
    if let Some(expected_state) = request.expected_state
        && actor_enum.state != expected_state
    {
        return Err(ArgentError::new(format!(
            "entry `{}::{}` declares actor handle `{name}` as {actor_type}<{expected_state}>, but `{actor_enum_name}` contains {actor_type}<{}>",
            ctx.actor.name,
            ctx.entry.name,
            actor_enum.state,
            actor_type = word::ACTOR_TYPE,
            name = request.name,
            actor_enum_name = request.actor_enum_name
        )));
    }
    if ctx.actor.state != actor_enum.state {
        return Err(ArgentError::new(format!(
            "entry `{}::{}` uses actor enum `{actor_enum_name}` for state `{}`, but the entry actor owns `{}`; selector values currently require the same state",
            ctx.actor.name,
            ctx.entry.name,
            actor_enum.state,
            ctx.actor.state,
            actor_enum_name = request.actor_enum_name
        )));
    }
    let fixed_actor = request.fixed_actor.map(str::to_string);
    let selector_expr = if let Some(fixed_actor) = &fixed_actor {
        actor_enum_variant_const_expr(actor_enum, fixed_actor).ok_or_else(|| {
            ArgentError::new(format!(
                "actor enum `{actor_enum_name}` has no variant `{fixed_actor}` in `{}::{}`",
                ctx.actor.name,
                ctx.entry.name,
                actor_enum_name = request.actor_enum_name
            ))
        })?
    } else {
        request.selector_expr.trim().to_string()
    };
    if selector_expr.is_empty() {
        return Err(ArgentError::new(format!(
            "entry `{}::{}` declares actor enum value `{name}` with an empty selector",
            ctx.actor.name,
            ctx.entry.name,
            name = request.name
        )));
    }
    Ok(TemplateSelector {
        name: request.name.to_string(),
        actor_enum: actor_enum.name.clone(),
        state: actor_enum.state.clone(),
        variants: actor_enum.variants.clone(),
        selector_expr,
        fixed_actor,
    })
}

/// Parse an indexed actor-enum expression such as `MoveActor[index]`.
pub(crate) fn parse_actor_enum_selector(expr: &str) -> Option<(&str, &str)> {
    let expr = expr.trim();
    let (actor_enum, rest) = expr.split_once('[')?;
    let actor_enum = actor_enum.trim();
    if !is_identifier(actor_enum) {
        return None;
    }
    let selector = rest.strip_suffix(']')?.trim();
    if selector.is_empty() {
        return None;
    }
    Some((actor_enum, selector))
}

/// Parse a fixed actor-enum variant such as `MoveActor::Knight`.
pub(crate) fn parse_actor_enum_variant(expr: &str) -> Option<(String, String)> {
    let expr = expr.trim();
    let (actor_enum, variant) = expr.split_once("::")?;
    let actor_enum = actor_enum.trim();
    let variant = variant.trim();
    if !is_identifier(actor_enum) || !is_identifier(variant) {
        return None;
    }
    Some((actor_enum.to_string(), variant.to_string()))
}

/// Return the generated integer expression for an actor-enum variant.
pub(crate) fn actor_enum_variant_const_expr(actor_enum: &ActorEnumInfo, variant: &str) -> Option<String> {
    actor_enum
        .variants
        .iter()
        .position(|candidate| candidate == variant)
        .map(|index| format!("{index} /*{}*/", to_snake(variant).to_ascii_uppercase()))
}

fn expand_routes<'a>(
    routes: impl Iterator<Item = &'a ResolvedRoute>,
    selectors: &BTreeMap<String, TemplateSelector>,
) -> Vec<ResolvedRoute> {
    let mut out = Vec::new();
    for route in routes {
        match &route.successor {
            ResolvedSuccessor::Constructed { actor, state } if selectors.contains_key(actor) => {
                let selector = selectors.get(actor).expect("checked selector exists");
                out.extend(selector.route_actors().into_iter().map(|actor| ResolvedRoute {
                    id: route.id,
                    output: route.output.clone(),
                    successor: ResolvedSuccessor::Constructed { actor, state: state.clone() },
                }));
            }
            ResolvedSuccessor::ExactSelf | ResolvedSuccessor::Constructed { .. } => out.push(route.clone()),
        }
    }
    out
}

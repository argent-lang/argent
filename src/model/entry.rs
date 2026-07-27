//! Source-backed entry interactions grouped by covenant context.

use std::collections::{BTreeMap, BTreeSet};

use crate::ast::{
    ActorDecl, ConsumeDecl, EmitOutput, EmitSpec, EntryDecl, ObserveDecl, ObservedActorDecl, RouteCall, SpawnDecl, SpawnOutputDecl,
};
use crate::error::{ArgentError, Result};
use crate::language::word;
use crate::lexer::{Token, TokenKind, lex};
use crate::naming::{is_identifier, to_snake};

use super::ActorEnumInfo;

/// The normalized interactions and selector-expanded routes for one entry.
#[derive(Debug)]
pub(crate) struct EntryModel<'a> {
    source: &'a EntryDecl,
    groups: Vec<CovenantGroup<'a>>,
    template_selectors: BTreeMap<String, TemplateSelector>,
}

impl<'a> EntryModel<'a> {
    /// Build an entry model from its source actor and declaration.
    pub(crate) fn build(actor: &'a ActorDecl, source: &'a EntryDecl, actor_enums: &BTreeMap<String, ActorEnumInfo>) -> Result<Self> {
        Ok(Self::new(source, actor_enums, template_selectors_for_entry(actor, source, actor_enums)?))
    }

    fn new(
        source: &'a EntryDecl,
        actor_enums: &BTreeMap<String, ActorEnumInfo>,
        template_selectors: BTreeMap<String, TemplateSelector>,
    ) -> Self {
        let current_inputs = source
            .consumes
            .iter()
            .enumerate()
            .map(|(index, consume)| EntryInteraction {
                source: InteractionSource::Consume(consume),
                handle: Some(&consume.name),
                index,
                target: ActorTarget::concrete(&consume.actor),
            })
            .collect();
        let current_outputs = match &source.emits {
            EmitSpec::None => Vec::new(),
            EmitSpec::One { actors } => {
                vec![EntryInteraction {
                    source: InteractionSource::CurrentOutput { declaration: &source.emits, output: None },
                    handle: None,
                    index: 0,
                    target: ActorTarget::domain(actors, actor_enums),
                }]
            }
            EmitSpec::Outputs(outputs) => outputs
                .iter()
                .map(|output| EntryInteraction {
                    source: InteractionSource::CurrentOutput { declaration: &source.emits, output: Some(output) },
                    handle: Some(&output.name),
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
                        handle: Some(&input.name),
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
                        handle: Some(&output.name),
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
                        handle: Some(&output.name),
                        index: output.group_index,
                        target: ActorTarget::source_or_concrete(source, &output.actor),
                    })
                    .collect(),
            }
        }));
        Self { source, groups, template_selectors }
    }

    /// Return the source entry declaration.
    pub(crate) fn source(&self) -> &'a EntryDecl {
        self.source
    }

    /// Return the interaction group governed by the current covenant.
    pub(crate) fn current(&self) -> &CovenantGroup<'a> {
        self.groups.first().expect("entry model always contains its current covenant")
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

    /// Expand body-derived routes to their concrete artifact targets.
    pub(crate) fn expanded_routes(&self) -> Vec<RouteCall> {
        expand_routes(self.source.routes.iter(), &self.template_selectors)
    }

    /// Collect concrete app actors whose templates this entry reads or writes.
    ///
    /// Current outputs follow the body-selected routes; existing and genesis
    /// outputs are exhaustive in their covenant groups.
    pub(crate) fn actor_template_uses(&self, source_actor: &str, app_actors: &[String]) -> ActorTemplateUses {
        let app_actor_set = app_actors.iter().map(String::as_str).collect::<BTreeSet<_>>();
        let uses_current_template = |target: &str| app_actors.len() == 1 && app_actors[0] == source_actor && target == source_actor;
        let mut uses = ActorTemplateUses::default();

        for group in std::iter::once(self.current()).chain(self.existing_groups()) {
            for interaction in group.inputs() {
                for target in interaction.target().concrete_actors() {
                    if app_actor_set.contains(target) && !uses_current_template(target) {
                        uses.reads.insert(target.to_string());
                    }
                }
            }
        }

        for route in &self.source.routes {
            if !self.template_selectors.contains_key(&route.actor)
                && app_actor_set.contains(route.actor.as_str())
                && route.actor != source_actor
            {
                uses.writes.insert(route.actor.clone());
            }
        }
        for group in self.existing_groups().chain(self.genesis_groups()) {
            for interaction in group.outputs() {
                for target in interaction.target().concrete_actors() {
                    if app_actor_set.contains(target) && target != source_actor {
                        uses.writes.insert(target.to_string());
                    }
                }
            }
        }

        uses
    }
}

/// Actor template capabilities used while lowering one entry.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ActorTemplateUses {
    pub(crate) reads: BTreeSet<String>,
    pub(crate) writes: BTreeSet<String>,
}

/// Entry inputs and outputs governed by one covenant ID.
///
/// The shared covenant places every concrete target in the same app domain.
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
    handle: Option<&'a str>,
    index: usize,
    target: ActorTarget,
}

impl<'a> EntryInteraction<'a> {
    /// Return the exact source node that declared this interaction.
    pub(crate) fn source(&self) -> InteractionSource<'a> {
        self.source
    }

    /// Return the declared handle, or `None` for an implicit output.
    pub(crate) fn handle(&self) -> Option<&'a str> {
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
    /// A current-covenant output and its optional named source node.
    CurrentOutput { declaration: &'a EmitSpec, output: Option<&'a EmitOutput> },
    /// An input from an `observes` clause.
    ObserveInput(&'a ObservedActorDecl),
    /// An output from an `observes` clause.
    ObserveOutput(&'a ObservedActorDecl),
    /// An output from a `spawns` clause.
    SpawnOutput(&'a SpawnOutputDecl),
}

/// Compiler-known actor candidates for an interaction target.
///
/// The original spelling is retained even when a source value prevents the
/// target from participating in static app planning.
#[derive(Debug)]
pub(crate) struct ActorTarget {
    actors: Vec<String>,
    concrete: bool,
}

impl ActorTarget {
    fn concrete(actor: &str) -> Self {
        Self { actors: vec![actor.to_string()], concrete: true }
    }

    fn source_or_concrete(entry: &EntryDecl, expression: &str) -> Self {
        let concrete = is_actor_reference(expression)
            && !entry.params.iter().any(|param| param.name == expression)
            && !expression.strip_prefix("self.").is_some_and(is_identifier);
        Self { actors: vec![expression.to_string()], concrete }
    }

    fn observed(entry: &EntryDecl, observe: &ObserveDecl, observed: &ObservedActorDecl) -> Self {
        let open_binding = observed.open_state.is_some()
            || observe.inputs.iter().any(|input| input.actor == observed.actor && input.open_state.is_some());
        let mut target = Self::source_or_concrete(entry, &observed.actor);
        target.concrete &= !open_binding;
        target
    }

    fn domain(actors: &[String], actor_enums: &BTreeMap<String, ActorEnumInfo>) -> Self {
        Self {
            actors: actors
                .iter()
                .flat_map(|actor| actor_enums.get(actor).map_or_else(|| vec![actor.clone()], |actor_enum| actor_enum.variants.clone()))
                .collect(),
            concrete: true,
        }
    }

    /// Iterate candidate actor names or unresolved target expressions.
    pub(crate) fn actors(&self) -> impl Iterator<Item = &str> {
        self.actors.iter().map(String::as_str)
    }

    /// Iterate only compiler-known concrete actor candidates.
    pub(crate) fn concrete_actors(&self) -> impl Iterator<Item = &str> {
        self.actors().filter(|_| self.concrete)
    }
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

    // TODO(route clauses): Derive route domains only from entry clauses. Keep
    // body-local selector resolution in body analysis/codegen, then remove
    // this raw-body scan from EntryModel.
    let tokens = lex(&entry.body)
        .map_err(|err| ArgentError::new(format!("failed to lex body for `{}::{}`: {}", actor.name, entry.name, err.message)))?;
    let mut pos = 0usize;
    while pos + 3 < tokens.len() {
        let is_actor_type = matches!(&tokens[pos].kind, TokenKind::Ident(name) if name == word::ACTOR_TYPE)
            && matches!(tokens[pos + 1].kind, TokenKind::Symbol('<'))
            && matches!(tokens[pos + 3].kind, TokenKind::Symbol('>'))
            && matches!(tokens.get(pos + 4).map(|token| &token.kind), Some(TokenKind::Ident(_)))
            && matches!(tokens.get(pos + 5).map(|token| &token.kind), Some(TokenKind::Symbol('=')));
        if is_actor_type {
            let state = match &tokens[pos + 2].kind {
                TokenKind::Ident(state) => state.clone(),
                _ => {
                    pos += 1;
                    continue;
                }
            };
            let name = match &tokens[pos + 4].kind {
                TokenKind::Ident(name) => name.clone(),
                _ => {
                    pos += 1;
                    continue;
                }
            };
            let (expr, end_pos) = take_expr_until_semicolon(&entry.body, &tokens, pos + 6, actor, entry)?;
            let selector = template_selector_from_initializer(&ctx, &name, Some(&state), None, &expr)?;
            insert_template_selector(actor, entry, &mut selectors, selector)?;
            pos = end_pos + 1;
            continue;
        }

        let is_actor_enum_local = matches!(&tokens[pos].kind, TokenKind::Ident(source_ty) if actor_enums.contains_key(source_ty))
            && matches!(tokens.get(pos + 1).map(|token| &token.kind), Some(TokenKind::Ident(_)))
            && matches!(tokens.get(pos + 2).map(|token| &token.kind), Some(TokenKind::Symbol('=')));
        if is_actor_enum_local {
            let actor_enum_name = match &tokens[pos].kind {
                TokenKind::Ident(actor_enum_name) => actor_enum_name.clone(),
                _ => unreachable!("checked actor enum local type"),
            };
            let name = match &tokens[pos + 1].kind {
                TokenKind::Ident(name) => name.clone(),
                _ => unreachable!("checked actor enum local name"),
            };
            let (expr, end_pos) = take_expr_until_semicolon(&entry.body, &tokens, pos + 3, actor, entry)?;
            let mut selector = template_selector_from_initializer(&ctx, &name, None, Some(&actor_enum_name), &expr)?;
            selector.selector_expr = name.clone();
            insert_template_selector(actor, entry, &mut selectors, selector)?;
            pos = end_pos + 1;
            continue;
        }

        pos += 1;
    }
    Ok(selectors)
}

fn take_expr_until_semicolon(
    body: &str,
    tokens: &[Token],
    start_pos: usize,
    actor: &ActorDecl,
    entry: &EntryDecl,
) -> Result<(String, usize)> {
    let start = tokens
        .get(start_pos)
        .ok_or_else(|| ArgentError::new(format!("entry `{}::{}` has an incomplete actor enum selector", actor.name, entry.name)))?
        .span
        .start;
    let mut depth = 0usize;
    let mut scan = start_pos;
    while scan < tokens.len() {
        match tokens[scan].kind {
            TokenKind::Symbol('{') | TokenKind::Symbol('(') | TokenKind::Symbol('[') => depth += 1,
            TokenKind::Symbol('}') | TokenKind::Symbol(')') | TokenKind::Symbol(']') => depth = depth.saturating_sub(1),
            TokenKind::Symbol(';') if depth == 0 => {
                return Ok((body[start..tokens[scan].span.start].trim().to_string(), scan));
            }
            TokenKind::Eof => break,
            _ => {}
        }
        scan += 1;
    }
    Err(ArgentError::new(format!("entry `{}::{}` has an unterminated actor enum selector", actor.name, entry.name)))
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

fn expand_routes<'a>(routes: impl Iterator<Item = &'a RouteCall>, selectors: &BTreeMap<String, TemplateSelector>) -> Vec<RouteCall> {
    let mut out = Vec::new();
    for route in routes {
        if let Some(selector) = selectors.get(&route.actor) {
            let actors = selector.route_actors();
            out.extend(actors.into_iter().map(|actor| RouteCall { output: route.output.clone(), actor, state: route.state.clone() }));
        } else {
            out.push(route.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{EmitOutput, EmitSpec, EntryKind};

    #[test]
    fn models_covenant_groups_and_preserves_source_nodes() {
        let entry = EntryDecl {
            kind: EntryKind::Leader,
            name: "step".to_string(),
            params: Vec::new(),
            consumes: vec![ConsumeDecl { name: "peer".to_string(), actor: "Peer".to_string() }],
            observes: vec![ObserveDecl {
                name: "remote".to_string(),
                covenant_expr: "self.remote_id".to_string(),
                inputs: vec![ObservedActorDecl { name: "before".to_string(), actor: "Remote".to_string(), open_state: None }],
                outputs: vec![ObservedActorDecl { name: "after".to_string(), actor: "Remote".to_string(), open_state: None }],
            }],
            spawns: vec![SpawnDecl {
                name: "launch".to_string(),
                covenant: "child".to_string(),
                outputs: vec![SpawnOutputDecl { name: "child".to_string(), actor: "Child".to_string(), group_index: 0 }],
            }],
            emits: EmitSpec::One { actors: vec!["Move".to_string()] },
            body: String::new(),
            routes: vec![RouteCall { output: None, actor: "target".to_string(), state: "next".to_string() }],
            terminal_route_sets: Vec::new(),
        };
        let selectors = BTreeMap::from([(
            "target".to_string(),
            TemplateSelector {
                name: "target".to_string(),
                actor_enum: "Move".to_string(),
                state: "Game".to_string(),
                variants: vec!["Pawn".to_string(), "King".to_string()],
                selector_expr: "selector".to_string(),
                fixed_actor: Some("King".to_string()),
            },
        )]);
        let actor_enums = BTreeMap::from([(
            "Move".to_string(),
            ActorEnumInfo {
                name: "Move".to_string(),
                state: "Game".to_string(),
                variants: vec!["Pawn".to_string(), "King".to_string()],
            },
        )]);

        let model = EntryModel::new(&entry, &actor_enums, selectors);

        let InteractionSource::Consume(consume) = model.current().inputs()[0].source() else {
            panic!("current input must retain its consume declaration");
        };
        assert!(std::ptr::eq(consume, &entry.consumes[0]));
        assert_eq!(model.current().inputs()[0].handle(), Some("peer"));
        assert_eq!(model.current().inputs()[0].index(), 0);
        let InteractionSource::CurrentOutput { declaration, output: None } = model.current().outputs()[0].source() else {
            panic!("current output must retain its emits declaration");
        };
        assert!(std::ptr::eq(declaration, &entry.emits));
        assert_eq!(model.current().outputs()[0].handle(), None);
        assert_eq!(model.current().outputs()[0].index(), 0);
        assert_eq!(model.current().outputs()[0].target().actors().collect::<Vec<_>>(), ["Pawn", "King"]);
        let observe_group = model.existing_groups().next().expect("observe group");
        assert!(std::ptr::eq(observe_group.observe().expect("observe source"), &entry.observes[0]));
        let InteractionSource::ObserveInput(observed) = observe_group.inputs()[0].source() else {
            panic!("observe input must retain its source declaration");
        };
        assert!(std::ptr::eq(observed, &entry.observes[0].inputs[0]));
        assert_eq!(observe_group.inputs()[0].handle(), Some("before"));
        assert_eq!(observe_group.inputs()[0].index(), 0);
        assert_eq!(observe_group.inputs()[0].target().concrete_actors().collect::<Vec<_>>(), ["Remote"]);

        let spawn_group = model.genesis_groups().next().expect("spawn group");
        assert!(std::ptr::eq(spawn_group.spawn().expect("spawn source"), &entry.spawns[0]));
        let InteractionSource::SpawnOutput(output) = spawn_group.outputs()[0].source() else {
            panic!("spawn output must retain its source declaration");
        };
        assert!(std::ptr::eq(output, &entry.spawns[0].outputs[0]));
        assert_eq!(spawn_group.outputs()[0].handle(), Some("child"));
        assert_eq!(spawn_group.outputs()[0].index(), 0);
        assert_eq!(spawn_group.outputs()[0].target().concrete_actors().collect::<Vec<_>>(), ["Child"]);

        assert_eq!(model.expanded_routes()[0].actor, "King");
        assert_eq!(
            model.actor_template_uses(
                "Source",
                &["Source", "Peer", "Remote", "Child", "Pawn", "King"].into_iter().map(str::to_string).collect::<Vec<_>>(),
            ),
            ActorTemplateUses {
                reads: ["Peer", "Remote"].into_iter().map(str::to_string).collect(),
                writes: ["Remote", "Child"].into_iter().map(str::to_string).collect(),
            }
        );
    }

    #[test]
    fn actor_targets_keep_source_expressions_out_of_concrete_planning() {
        let entry = EntryDecl {
            kind: EntryKind::Leader,
            name: "step".to_string(),
            params: Vec::new(),
            consumes: Vec::new(),
            observes: Vec::new(),
            spawns: Vec::new(),
            emits: EmitSpec::None,
            body: String::new(),
            routes: Vec::new(),
            terminal_route_sets: Vec::new(),
        };
        let observe = ObserveDecl {
            name: "remote".to_string(),
            covenant_expr: "self.remote_id".to_string(),
            inputs: vec![ObservedActorDecl {
                name: "before".to_string(),
                actor: "Foreign".to_string(),
                open_state: Some("ForeignState".to_string()),
            }],
            outputs: Vec::new(),
        };

        let source = ActorTarget::source_or_concrete(&entry, "self.foreign_type");
        assert_eq!(source.actors().collect::<Vec<_>>(), ["self.foreign_type"]);
        assert!(source.concrete_actors().next().is_none());

        let open = ActorTarget::observed(&entry, &observe, &observe.inputs[0]);
        assert_eq!(open.actors().collect::<Vec<_>>(), ["Foreign"]);
        assert!(open.concrete_actors().next().is_none());

        assert_eq!(ActorTarget::concrete("Foreign").concrete_actors().collect::<Vec<_>>(), ["Foreign"]);
    }

    #[test]
    fn models_named_and_empty_emit_domains() {
        let entry = EntryDecl {
            kind: EntryKind::Leader,
            name: "step".to_string(),
            params: Vec::new(),
            consumes: Vec::new(),
            observes: Vec::new(),
            spawns: Vec::new(),
            emits: EmitSpec::Outputs(vec![
                EmitOutput { name: "first".to_string(), actors: vec!["Pawn".to_string()], auth_index: 0 },
                EmitOutput { name: "second".to_string(), actors: vec!["Move".to_string()], auth_index: 1 },
            ]),
            body: String::new(),
            routes: Vec::new(),
            terminal_route_sets: Vec::new(),
        };
        let actor_enums = BTreeMap::from([(
            "Move".to_string(),
            ActorEnumInfo {
                name: "Move".to_string(),
                state: "Game".to_string(),
                variants: vec!["Pawn".to_string(), "King".to_string()],
            },
        )]);

        let model = EntryModel::new(&entry, &actor_enums, BTreeMap::new());
        let EmitSpec::Outputs(outputs) = &entry.emits else {
            panic!("test entry must have named outputs");
        };
        let InteractionSource::CurrentOutput { declaration: first_declaration, output: Some(first) } =
            model.current().outputs()[0].source()
        else {
            panic!("named output must retain its emit output");
        };
        let InteractionSource::CurrentOutput { declaration: second_declaration, output: Some(second) } =
            model.current().outputs()[1].source()
        else {
            panic!("named output must retain its emit output");
        };
        assert!(std::ptr::eq(first_declaration, &entry.emits));
        assert!(std::ptr::eq(second_declaration, &entry.emits));
        assert!(std::ptr::eq(first, &outputs[0]));
        assert!(std::ptr::eq(second, &outputs[1]));
        assert_eq!(model.current().outputs()[0].handle(), Some("first"));
        assert_eq!(model.current().outputs()[0].index(), 0);
        assert_eq!(model.current().outputs()[0].target().actors().collect::<Vec<_>>(), ["Pawn"]);
        assert_eq!(model.current().outputs()[1].handle(), Some("second"));
        assert_eq!(model.current().outputs()[1].index(), 1);
        assert_eq!(model.current().outputs()[1].target().actors().collect::<Vec<_>>(), ["Pawn", "King"]);

        let mut empty_entry = entry.clone();
        empty_entry.emits = EmitSpec::None;
        let empty_model = EntryModel::new(&empty_entry, &actor_enums, BTreeMap::new());
        assert!(empty_model.current().outputs().is_empty());
    }
}

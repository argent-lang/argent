//! Source-backed entry interactions grouped by covenant context.

use std::collections::{BTreeMap, BTreeSet};

use silverscript_lang::ast::visit::{AstVisitorMut, walk_statement_mut};
use silverscript_lang::ast::{Expr as SilExpr, ExprKind as SilExprKind, Span as SilSpan, Statement as SilStatement};

use crate::artifact::MAX_ENTRY_RANGE_CARDINALITY;
use crate::compiler::resolve::{Binding, ClauseReference, LocalId, ResolvedDeclaration, ResolvedModules, ResolvedName};
use crate::compiler::syntax::body::{EntrySuccessor, RouteArity};
use crate::compiler::syntax::node::{ChildEdge, DeclId, EntryId, NodeId, ReferenceRole, RootSlot, SourceNodeCursor};
use crate::compiler::syntax::source::Origin;
use crate::compiler::syntax::word;
use crate::compiler::syntax::{
    ActorDecl, AuthoredEntryStatement, Cardinality, CardinalityBound, ConsumeDecl, EmitOutput, EmitSpec, EntryDecl, ObserveDecl,
    ObservedActorDecl, RouteId, SpawnDecl, SpawnOutputDecl, TypeRef,
};
use crate::error::{ArgentError, Result};

use super::types::{BoundRouteActor, BoundRouteValue, ResolvedType, ResolvedTypeBase, TypeTable};
use super::{AppActors, AppCompilationContext, ConstIntError, ConstResolver, SourceFieldId, SourceStateId, StaticActorId};

#[cfg(test)]
mod tests;

/// The normalized interactions and selector-expanded routes for one entry.
#[derive(Debug)]
pub(crate) struct EntryModel<'a> {
    pub(crate) id: EntryId,
    source: &'a EntryDecl,
    groups: Vec<CovenantGroup<'a>>,
    template_selectors: BTreeMap<String, TemplateSelector>,
    handle_value_initializers: BTreeMap<LocalId, NodeId>,
    routes: Vec<ResolvedRoute>,
    route_indexes: BTreeMap<RouteId, usize>,
    foreign_groups: BTreeMap<NodeId, CovenantGroupId>,
    route_outputs: BTreeMap<RouteId, (CovenantGroupId, InteractionId)>,
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
    Constructed { actor: RouteActorSource, arity: RouteArity, bound: Option<BoundRouteValue> },
}

/// The authored actor site or one concrete selector expansion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RouteActorSource {
    Authored,
    Expanded(StaticActorId),
}

impl RouteActorSource {
    pub(crate) fn display(
        &self,
        _entry: &EntryDecl,
        entry_id: EntryId,
        route: RouteId,
        model: &AppCompilationContext<'_>,
    ) -> Result<String> {
        match self {
            Self::Authored => Ok(model.resolution.route_texts(entry_id, route)?.0.trim().to_string()),
            Self::Expanded(actor) => model.static_actor_reference(actor),
        }
    }
}

impl<'a> EntryModel<'a> {
    /// Build an entry model from its source actor and declaration.
    pub(crate) fn build(id: EntryId, actor: &'a ActorDecl, source: &'a EntryDecl, const_resolver: &ConstResolver) -> Result<Self> {
        Self::new(id, actor, source, BTreeMap::new(), const_resolver)
    }

    /// Attach the resolved declaration identity of each actor selector.
    pub(super) fn bind_selectors(
        &mut self,
        actor: &ActorDecl,
        resolution: &ResolvedModules<'_>,
        types: &TypeTable,
        actor_ids: &BTreeMap<String, StaticActorId>,
    ) -> Result<()> {
        let bindings = resolution.bindings(self.id.actor);
        let assigned_locals = bindings
            .sites
            .iter()
            .filter_map(|(site, binding)| match binding {
                Binding::Local(local) if resolution.nodes().node(*site).reference_role == Some(ReferenceRole::AssignmentTarget) => {
                    Some(*local)
                }
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        let ResolvedDeclaration::Actor(authored_actor) = resolution.declaration(self.id.actor) else {
            return Err(ArgentError::new("actor selector owner is not an actor declaration"));
        };
        let authored_entry = &authored_actor.entries[self.id.index];
        let body = resolution.entry_body(self.id)?;
        struct SelectorCollector<'src> {
            locals: Vec<(String, SilSpan<'src>, bool, SilExpr<'src>)>,
        }

        impl<'src> AstVisitorMut<'src> for SelectorCollector<'src> {
            fn visit_statement(&mut self, statement: &mut SilStatement<'src>) {
                if let SilStatement::VariableDefinition { name, name_span, type_ref, expr: Some(expr), .. } = statement {
                    self.locals.push((name.clone(), *name_span, !type_ref.array_dims.is_empty(), expr.clone()));
                }
                walk_statement_mut(self, statement);
            }
        }

        let mut selectors = BTreeMap::new();
        let mut enum_values = BTreeMap::new();
        let mut handle_values = BTreeSet::new();
        for (index, param) in authored_entry.params.iter().enumerate() {
            if matches!(
                types.entry_params.get(&(self.id.actor, self.id.index, index)),
                Some(ResolvedType { base: ResolvedTypeBase::ActorHandle(_), array: None })
            ) {
                if let Some(local) = bindings.parameter_ids.get(&(RootSlot::Entry(self.id.index), index)) {
                    handle_values.insert(*local);
                }
                continue;
            }
            let Some(ResolvedType { base: ResolvedTypeBase::ActorEnum(enumeration), array: None }) =
                types.entry_params.get(&(self.id.actor, self.id.index, index))
            else {
                continue;
            };
            let mut selector = self.bound_selector(&param.name, *enumeration, None, *enumeration, types, actor_ids)?;
            selector.binding = bindings.parameter_ids.get(&(RootSlot::Entry(self.id.index), index)).copied();
            if let Some(local) = selector.binding {
                enum_values.insert(local, *enumeration);
            }
            insert_template_selector(actor, self.source, &mut selectors, selector)?;
        }

        let mut collector = SelectorCollector { locals: Vec::new() };
        for statement in body {
            statement.visit_with(&mut collector);
        }
        for (name, name_span, is_array, expr) in collector.locals {
            if is_array {
                continue;
            }
            let Some((site, local)) = bindings.sites.iter().find_map(|(site, binding)| {
                let Binding::Local(local) = binding else { return None };
                let node = resolution.nodes().node(*site);
                match node.origin {
                    Origin::Authored { start, end, .. }
                        if node.address.owner == self.id.actor
                            && node.address.root == RootSlot::Entry(self.id.index)
                            && start == name_span.start()
                            && end == name_span.end() =>
                    {
                        Some((*site, *local))
                    }
                    _ => None,
                }
            }) else {
                return Err(ArgentError::new(format!("entry `{}` has an unbound actor selector `{name}`", self.source.name)));
            };
            let mut address = resolution.nodes().node(site).address.clone();
            if address.children.pop() != Some(ChildEdge::BindingName) {
                return Err(ArgentError::new("actor selector has no binding-name site"));
            }
            address.children.push(ChildEdge::TypeUse);
            let type_site = resolution.nodes().find(&address).ok_or_else(|| ArgentError::new("actor selector has no type site"))?;
            let expected_state = types.actor_handle_type_uses.get(&type_site).copied();
            let expected_actor_enum = match bindings.sites.get(&type_site) {
                Some(Binding::Source(ResolvedName::Declaration(id)))
                    if id.kind() == crate::compiler::syntax::node::SymbolKind::ActorEnum =>
                {
                    Some(*id)
                }
                Some(Binding::Source(ResolvedName::AppMember(member))) => {
                    (member.actor.kind() == crate::compiler::syntax::node::SymbolKind::ActorEnum).then_some(member.actor)
                }
                _ => None,
            };
            if expected_state.is_none() && expected_actor_enum.is_none() {
                continue;
            }
            address.children.pop();
            address.children.push(ChildEdge::Expression);
            let expr_cursor = SourceNodeCursor { address };
            let expression_site = resolution
                .nodes()
                .find(&expr_cursor.address)
                .ok_or_else(|| ArgentError::new("actor selector has no indexed initializer"))?;
            let value_site = match &expr.kind {
                SilExprKind::Identifier(_) => Some(expression_site),
                SilExprKind::Call { .. } => resolution.nodes().find(&expr_cursor.child(ChildEdge::CallTarget).address),
                _ => None,
            };
            if expected_state.is_some()
                && match (&expr.kind, value_site.and_then(|site| bindings.sites.get(&site))) {
                    (SilExprKind::Identifier(_), Some(Binding::Source(ResolvedName::AppMember(member)))) => {
                        member.actor.kind() == crate::compiler::syntax::node::SymbolKind::Actor
                    }
                    (SilExprKind::Identifier(_), Some(Binding::Local(source))) => handle_values.contains(source),
                    (SilExprKind::Identifier(_), Some(binding @ Binding::Source(ResolvedName::Declaration(id))))
                        if id.kind() == crate::compiler::syntax::node::SymbolKind::Const =>
                    {
                        types.scalar_actor_handle_state(self.id.actor, binding).is_some()
                    }
                    (SilExprKind::Call { .. }, Some(binding @ Binding::Source(ResolvedName::Declaration(id))))
                        if id.kind() == crate::compiler::syntax::node::SymbolKind::Function =>
                    {
                        types.scalar_actor_handle_state(self.id.actor, binding).is_some()
                    }
                    (SilExprKind::Call { .. }, Some(binding @ Binding::ActorHelper(_))) => {
                        types.scalar_actor_handle_state(self.id.actor, binding).is_some()
                    }
                    _ => false,
                }
            {
                // Handle bytes are ordinary local values, not enum ordinals or
                // permission to route outside this application's actor domain.
                handle_values.insert(local);
                self.handle_value_initializers.insert(local, value_site.expect("ordinary handle initializer has a bound value site"));
                continue;
            }
            if expected_state.is_some() {
                return Err(ArgentError::new(format!(
                    "entry `{}::{}` declares actor handle `{name}` without a linked actor or a typed external handle initializer",
                    authored_actor.name, authored_entry.name
                )));
            }
            let (enumeration, fixed_actor) = match &expr.kind {
                SilExprKind::Identifier(_) => match bindings.sites.get(&expression_site) {
                    Some(Binding::EnumVariant { enumeration, actor }) => (*enumeration, Some(*actor)),
                    Some(Binding::Local(source)) if enum_values.contains_key(source) => (enum_values[source], None),
                    _ => {
                        return Err(ArgentError::new(format!(
                            "entry `{}::{}` declares actor handle `{name}` without an actor enum initializer",
                            authored_actor.name, authored_entry.name
                        )));
                    }
                },
                SilExprKind::ArrayIndex { source, .. } if matches!(&source.kind, SilExprKind::Identifier(_)) => {
                    let source_site = resolution.nodes().find(&expr_cursor.child(ChildEdge::ExpressionSource).address);
                    let actor_enum = source_site.and_then(|site| bindings.sites.get(&site)).and_then(|binding| match binding {
                        Binding::Source(ResolvedName::Declaration(id))
                            if id.kind() == crate::compiler::syntax::node::SymbolKind::ActorEnum =>
                        {
                            Some(*id)
                        }
                        Binding::Source(ResolvedName::AppMember(member)) => {
                            (member.actor.kind() == crate::compiler::syntax::node::SymbolKind::ActorEnum).then_some(member.actor)
                        }
                        _ => None,
                    });
                    (
                        actor_enum.ok_or_else(|| {
                            ArgentError::new(format!(
                                "entry `{}::{}` declares actor handle `{name}` without an actor enum initializer",
                                authored_actor.name, authored_entry.name
                            ))
                        })?,
                        None,
                    )
                }
                _ => {
                    return Err(ArgentError::new(format!(
                        "entry `{}::{}` declares actor handle `{name}` without an actor enum initializer",
                        authored_actor.name, authored_entry.name
                    )));
                }
            };
            let declared = expected_actor_enum.expect("selector declarations have an explicit actor-enum type");
            // A reassigned enum local routes through its current value, not its initializer.
            let fixed_actor = if assigned_locals.contains(&local) { None } else { fixed_actor };
            let mut selector = self.bound_selector(&name, enumeration, fixed_actor, declared, types, actor_ids)?;
            selector.binding = Some(local);
            enum_values.insert(local, enumeration);
            insert_template_selector(actor, self.source, &mut selectors, selector)?;
        }
        self.template_selectors = selectors;
        for selector in self.template_selectors.values_mut() {
            if selector.binding.is_none() {
                return Err(ArgentError::new(format!(
                    "entry `{}` has an unbound actor selector `{}`",
                    self.source.name, selector.name
                )));
            }
            if selector.targets.is_none() {
                selector.targets = Some(
                    selector
                        .variants
                        .iter()
                        .map(|name| {
                            actor_ids.get(name).cloned().ok_or_else(|| {
                                ArgentError::new(format!("selector `{}` expands to unknown actor `{name}`", selector.name))
                            })
                        })
                        .collect::<Result<_>>()?,
                );
            }
        }
        Ok(())
    }

    /// Construct one selector from declaration identities and their shared state.
    fn bound_selector(
        &self,
        name: &str,
        enumeration: DeclId,
        fixed_actor: Option<DeclId>,
        declared: DeclId,
        types: &TypeTable,
        actor_ids: &BTreeMap<String, StaticActorId>,
    ) -> Result<TemplateSelector> {
        let actor = &types.display_names[&self.id.actor];
        let actor_enum = types
            .display_names
            .get(&enumeration)
            .ok_or_else(|| ArgentError::new(format!("entry `{actor}::{}` has an unknown actor enum identity", self.source.name)))?;
        if declared != enumeration {
            let expected = &types.display_names[&declared];
            return Err(ArgentError::new(format!(
                "entry `{actor}::{}` declares actor enum value `{name}` as `{expected}`, but initializes it from `{actor_enum}`",
                self.source.name
            )));
        }
        let variants = types.enum_variants.get(&enumeration).ok_or_else(|| {
            ArgentError::new(format!(
                "entry `{actor}::{}` declares actor handle `{name}` from unknown actor enum `{actor_enum}`",
                self.source.name
            ))
        })?;
        let first = variants.first().ok_or_else(|| ArgentError::new(format!("actor enum `{actor_enum}` has no variants")))?;
        let state = types
            .actor_states
            .get(first)
            .ok_or_else(|| ArgentError::new(format!("actor enum `{actor_enum}` variant has no state identity")))?;
        if variants.iter().any(|variant| types.actor_states.get(variant) != Some(state)) {
            return Err(ArgentError::new(format!("actor enum `{actor_enum}` variants do not share one state identity")));
        }
        let state_name = &types.display_names[state];
        if types.actor_states.get(&self.id.actor) != Some(state) {
            let owned = &types.display_names[&types.actor_states[&self.id.actor]];
            return Err(ArgentError::new(format!(
                "entry `{actor}::{}` uses actor enum `{actor_enum}` for state `{state_name}`, but the entry actor owns `{owned}`; selector values currently require the same state",
                self.source.name
            )));
        }
        let fixed_index = fixed_actor
            .map(|fixed| {
                variants.iter().position(|variant| *variant == fixed).ok_or_else(|| {
                    ArgentError::new(format!(
                        "actor enum `{actor_enum}` has no variant `{}` in `{actor}::{}`",
                        types.display_names[&fixed], self.source.name
                    ))
                })
            })
            .transpose()?;
        let targets = variants
            .iter()
            .map(|variant| {
                actor_ids
                    .values()
                    .any(|candidate| candidate == &StaticActorId::InApp(*variant))
                    .then_some(StaticActorId::InApp(*variant))
                    .ok_or_else(|| {
                        ArgentError::new(format!("selector `{name}` expands to unknown actor `{}`", types.display_names[variant]))
                    })
            })
            .collect::<Result<Vec<_>>>()?;
        let variants = variants.iter().map(|variant| types.display_names[variant].clone()).collect::<Vec<_>>();
        Ok(TemplateSelector {
            name: name.to_string(),
            binding: None,
            actor_enum: actor_enum.clone(),
            state: state_name.clone(),
            variants,
            fixed_actor: fixed_actor.map(|id| types.display_names[&id].clone()),
            fixed_index,
            targets: Some(targets),
        })
    }

    /// Complete observed and spawned targets from the binding at each clause site.
    pub(super) fn bind_actor_targets(
        &mut self,
        resolution: &ResolvedModules<'_>,
        types: &TypeTable,
        actor_ids: &BTreeMap<String, StaticActorId>,
    ) -> Result<()> {
        let bindings = resolution.bindings(self.id.actor);
        let current = &mut self.groups[0];
        for interaction in &mut current.inputs {
            let InteractionId::CurrentInput(index) = interaction.id else {
                unreachable!("current input has a current input identity")
            };
            let target = bindings
                .entry_consume_targets
                .get(&(self.id.index, index))
                .ok_or_else(|| ArgentError::new(format!("entry `{}` consume has no bound actor target", self.source.name)))?;
            let name = types
                .display_names
                .get(target)
                .ok_or_else(|| ArgentError::new(format!("entry `{}` consume has no actor display name", self.source.name)))?;
            let actor = actor_ids
                .values()
                .any(|candidate| candidate == &StaticActorId::InApp(*target))
                .then_some(StaticActorId::InApp(*target))
                .ok_or_else(|| ArgentError::new(format!("entry `{}` consume targets unknown actor `{name}`", self.source.name)))?;
            interaction.target = ActorTarget::Static(vec![actor]);
        }
        for interaction in &mut current.outputs {
            let InteractionId::CurrentOutput(index) = interaction.id else {
                unreachable!("current output has a current output identity")
            };
            let InteractionSource::CurrentOutput(output) = interaction.source else {
                unreachable!("current output has an emit source")
            };
            let mut targets = Vec::new();
            for (actor_index, _) in output.actors.iter().enumerate() {
                let id = bindings.entry_emit_targets.get(&(self.id.index, index, actor_index)).ok_or_else(|| {
                    ArgentError::new(format!("entry `{}` output `{}` has no bound actor target", self.source.name, output.name))
                })?;
                let variants = if id.kind() == crate::compiler::syntax::node::SymbolKind::ActorEnum {
                    types
                        .enum_variants
                        .get(id)
                        .ok_or_else(|| {
                            ArgentError::new(format!(
                                "entry `{}` output `{}` has no bound actor enum domain",
                                self.source.name, output.name
                            ))
                        })?
                        .as_slice()
                } else {
                    std::slice::from_ref(id)
                };
                for variant in variants {
                    let name = types.display_names.get(variant).ok_or_else(|| {
                        ArgentError::new(format!("entry `{}` output `{}` has no actor display name", self.source.name, output.name))
                    })?;
                    let actor = actor_ids
                        .values()
                        .any(|candidate| candidate == &StaticActorId::InApp(*variant))
                        .then_some(StaticActorId::InApp(*variant))
                        .ok_or_else(|| {
                            ArgentError::new(format!(
                                "entry `{}` output `{}` targets unknown actor `{name}`",
                                self.source.name, output.name
                            ))
                        })?;
                    targets.push(actor);
                }
            }
            interaction.target = ActorTarget::Static(targets);
        }
        for interaction in self.groups.iter_mut().flat_map(|group| group.inputs.iter_mut().chain(group.outputs.iter_mut())) {
            let expression = match (interaction.id, interaction.source) {
                (InteractionId::ObservedInput { .. }, InteractionSource::ObserveInput(observed))
                | (InteractionId::ObservedOutput { .. }, InteractionSource::ObserveOutput(observed)) => observed.actor.as_str(),
                (InteractionId::SpawnedOutput { .. }, InteractionSource::SpawnOutput(spawned)) => spawned.actor.as_str(),
                _ => continue,
            };
            let site = interaction.id.actor_target_site(self.id, resolution)?;
            if bindings.local_actor_targets.contains_key(&site) {
                interaction.target = ActorTarget::Source(expression.to_string());
                continue;
            }
            let target = bindings
                .actor_targets
                .get(&site)
                .ok_or_else(|| ArgentError::new(format!("entry `{}` actor clause has no bound target", self.source.name)))?;
            let id = match target {
                ResolvedName::Declaration(id) => actor_ids
                    .values()
                    .any(|candidate| matches!(candidate, StaticActorId::InApp(selected) if selected == id))
                    .then_some(StaticActorId::InApp(*id)),
                ResolvedName::AppMember(member) => {
                    let id = StaticActorId::Linked(super::link::LinkedActorId::from_member(*member, resolution));
                    actor_ids.values().any(|candidate| candidate == &id).then_some(id)
                }
                ResolvedName::Module(_) => None,
            };
            interaction.target =
                id.map_or_else(|| ActorTarget::UnresolvedStatic(vec![expression.to_string()]), |id| ActorTarget::Static(vec![id]));
        }
        for route in &self.routes {
            if !matches!(route.successor, ResolvedSuccessor::ExactSelf) {
                continue;
            }
            let actor = &types.display_names[&self.id.actor];
            let Some((CovenantGroupId::Current, output_id)) = self.route_outputs.get(&route.id) else {
                let message = if matches!(self.source.emits, EmitSpec::None) {
                    format!("entry `{actor}::{}` cannot use exact successor `self` because it declares `emits none`", self.source.name)
                } else {
                    format!("entry `{actor}::{}` routes through unknown output `{}`", self.source.name, route.output)
                };
                return Err(ArgentError::new(message));
            };
            let output = self.groups[0]
                .outputs()
                .iter()
                .find(|output| output.id() == *output_id)
                .ok_or_else(|| ArgentError::new("exact self route has no bound current output"))?;
            if output.target().static_actors().any(|target| target == &StaticActorId::InApp(self.id.actor)) {
                continue;
            }
            let InteractionSource::CurrentOutput(source) = output.source() else { unreachable!("current output has an emit source") };
            return Err(ArgentError::new(format!(
                "entry `{actor}::{}` cannot preserve exact self through output `{}` because it allows only {}",
                self.source.name,
                route.output,
                source.actors.join(" | ")
            )));
        }
        Ok(())
    }

    /// Bind authored foreign route sites to their covenant and output declarations.
    pub(super) fn bind_foreign_routes(&mut self, resolution: &ResolvedModules<'_>) -> Result<()> {
        let body = resolution.entry_body(self.id)?;
        let bindings = resolution.bindings(self.id.actor);
        let body_cursor = SourceNodeCursor::new(self.id.actor, RootSlot::Entry(self.id.index)).child(ChildEdge::Body);
        let mut pending = body
            .iter()
            .enumerate()
            .rev()
            .map(|(index, statement)| (statement, body_cursor.child(ChildEdge::Statement(index))))
            .collect::<Vec<_>>();
        while let Some((statement, cursor)) = pending.pop() {
            match statement {
                AuthoredEntryStatement::Block { statements, .. } => {
                    pending.extend(
                        statements
                            .iter()
                            .enumerate()
                            .rev()
                            .map(|(index, statement)| (statement, cursor.child(ChildEdge::Statement(index)))),
                    );
                }
                AuthoredEntryStatement::If { then_branch, else_branch, .. } => {
                    if let Some(else_branch) = else_branch {
                        pending.push((else_branch.as_ref(), cursor.child(ChildEdge::ElseBranch)));
                    }
                    pending.push((then_branch.as_ref(), cursor.child(ChildEdge::ThenBranch)));
                }
                AuthoredEntryStatement::ForeignBecome { group, routes, .. } => {
                    let site = resolution
                        .nodes()
                        .find(&cursor.child(ChildEdge::ForeignGroup).address)
                        .ok_or_else(|| ArgentError::new("foreign output route has no indexed group"))?;
                    let name = group.segments.first().ok_or_else(|| ArgentError::new("foreign output route has no group name"))?;
                    let Some(Binding::Local(local)) = bindings.sites.get(&site) else {
                        continue;
                    };
                    let Some(group_id) = bindings
                        .entry_observes
                        .iter()
                        .find_map(|(&(entry, observe), binding)| {
                            (entry == self.id.index && binding == local).then_some(CovenantGroupId::Existing(observe))
                        })
                        .or_else(|| {
                            bindings.entry_spawns.iter().find_map(|(&(entry, spawn), binding)| {
                                (entry == self.id.index && binding == local).then_some(CovenantGroupId::Genesis(spawn))
                            })
                        })
                    else {
                        continue;
                    };
                    let covenant = self.group(group_id).ok_or_else(|| ArgentError::new("foreign route has no covenant group"))?;
                    let label = if covenant.spawn().is_some() { "spawn" } else { "observe" };
                    let output_ids = routes
                        .iter()
                        .map(|route| {
                            let handle =
                                route.output.segments.first().ok_or_else(|| {
                                    ArgentError::new(format!("{label} `{name}` has an output route without a handle"))
                                })?;
                            let output = covenant
                                .outputs()
                                .iter()
                                .find(|output| output.handle() == handle)
                                .ok_or_else(|| ArgentError::new(format!("{label} `{name}` has no output `{handle}`")))?;
                            Ok((route.id, output.id()))
                        })
                        .collect::<Result<Vec<_>>>()?;
                    self.foreign_groups.insert(site, group_id);
                    for (route, output) in output_ids {
                        self.route_outputs.insert(route, (group_id, output));
                    }
                }
                AuthoredEntryStatement::Become { .. } | AuthoredEntryStatement::Sil(_) => {}
            }
        }
        Ok(())
    }

    fn new(
        id: EntryId,
        actor: &'a ActorDecl,
        entry: &'a EntryDecl,
        template_selectors: BTreeMap<String, TemplateSelector>,
        const_resolver: &ConstResolver,
    ) -> Result<Self> {
        let routes = entry
            .routes
            .iter()
            .map(|route| {
                let successor = match route.successor {
                    EntrySuccessor::ExactSelf => ResolvedSuccessor::ExactSelf,
                    EntrySuccessor::Constructed { arity, .. } => {
                        ResolvedSuccessor::Constructed { actor: RouteActorSource::Authored, arity, bound: None }
                    }
                };
                ResolvedRoute { id: route.id, output: route.output.clone(), successor }
            })
            .collect::<Vec<_>>();
        let route_indexes = routes.iter().enumerate().map(|(index, route)| (route.id, index)).collect();
        let actor_name = actor.name.as_str();
        let make_interaction = |id: InteractionId, source: InteractionSource<'a>, handle: &'a str, target, location| {
            let cardinality = ResolvedCardinality::resolve(source.cardinality(), actor_name, entry, handle, const_resolver)?;
            Ok(EntryInteraction { id, source, handle, target, cardinality, location })
        };
        let current_input_locations = plan_interaction_locations(
            entry.consumes.iter().map(|consume| (consume.name.as_str(), &consume.cardinality)),
            actor_name,
            &entry.name,
            word::CONSUMES,
        )?;
        let current_inputs = entry
            .consumes
            .iter()
            .zip(current_input_locations)
            .enumerate()
            .map(|(index, (consume, location))| {
                let target = ActorTarget::UnresolvedStatic(vec![consume.actor.clone()]);
                make_interaction(
                    InteractionId::CurrentInput(index),
                    InteractionSource::Consume(consume),
                    &consume.name,
                    target,
                    location,
                )
            })
            .collect::<Result<Vec<_>>>()?;
        let current_outputs = match &entry.emits {
            EmitSpec::None => Vec::new(),
            EmitSpec::Outputs(outputs) => {
                let locations = plan_interaction_locations(
                    outputs.iter().map(|output| (output.name.as_str(), &output.cardinality)),
                    actor_name,
                    &entry.name,
                    word::EMITS,
                )?;
                outputs
                    .iter()
                    .zip(locations)
                    .enumerate()
                    .map(|(index, (output, location))| {
                        let target = ActorTarget::UnresolvedStatic(output.actors.clone());
                        make_interaction(
                            InteractionId::CurrentOutput(index),
                            InteractionSource::CurrentOutput(output),
                            &output.name,
                            target,
                            location,
                        )
                    })
                    .collect::<Result<Vec<_>>>()?
            }
        };
        let mut groups = vec![CovenantGroup {
            id: CovenantGroupId::Current,
            covenant: CovenantContext::Current,
            inputs: current_inputs,
            outputs: current_outputs,
        }];
        groups.extend(
            entry
                .observes
                .iter()
                .enumerate()
                .map(|(observe_index, observe)| {
                    let input_locations = plan_interaction_locations(
                        observe.inputs.iter().map(|input| (input.name.as_str(), &input.cardinality)),
                        actor_name,
                        &entry.name,
                        &format!("{} {}.{}", word::OBSERVES, observe.name, word::INPUTS),
                    )?;
                    let output_locations = plan_interaction_locations(
                        observe.outputs.iter().map(|output| (output.name.as_str(), &output.cardinality)),
                        actor_name,
                        &entry.name,
                        &format!("{} {}.{}", word::OBSERVES, observe.name, word::OUTPUTS),
                    )?;
                    Ok(CovenantGroup {
                        id: CovenantGroupId::Existing(observe_index),
                        covenant: CovenantContext::Existing(observe),
                        inputs: observe
                            .inputs
                            .iter()
                            .zip(input_locations)
                            .enumerate()
                            .map(|(index, (input, location))| {
                                make_interaction(
                                    InteractionId::ObservedInput { observe: observe_index, input: index },
                                    InteractionSource::ObserveInput(input),
                                    &input.name,
                                    ActorTarget::Source(input.actor.clone()),
                                    location,
                                )
                            })
                            .collect::<Result<Vec<_>>>()?,
                        outputs: observe
                            .outputs
                            .iter()
                            .zip(output_locations)
                            .enumerate()
                            .map(|(index, (output, location))| {
                                make_interaction(
                                    InteractionId::ObservedOutput { observe: observe_index, output: index },
                                    InteractionSource::ObserveOutput(output),
                                    &output.name,
                                    ActorTarget::Source(output.actor.clone()),
                                    location,
                                )
                            })
                            .collect::<Result<Vec<_>>>()?,
                    })
                })
                .collect::<Result<Vec<_>>>()?,
        );
        groups.extend(
            entry
                .spawns
                .iter()
                .enumerate()
                .map(|(spawn_index, spawn)| {
                    let output_locations = plan_interaction_locations(
                        spawn.outputs.iter().map(|output| (output.name.as_str(), &output.cardinality)),
                        actor_name,
                        &entry.name,
                        &format!("{} {}.{}", word::SPAWNS, spawn.name, word::OUTPUTS),
                    )?;
                    Ok(CovenantGroup {
                        id: CovenantGroupId::Genesis(spawn_index),
                        covenant: CovenantContext::Genesis(spawn),
                        inputs: Vec::new(),
                        outputs: spawn
                            .outputs
                            .iter()
                            .zip(output_locations)
                            .enumerate()
                            .map(|(index, (output, location))| {
                                make_interaction(
                                    InteractionId::SpawnedOutput { spawn: spawn_index, output: index },
                                    InteractionSource::SpawnOutput(output),
                                    &output.name,
                                    ActorTarget::Source(output.actor.clone()),
                                    location,
                                )
                            })
                            .collect::<Result<Vec<_>>>()?,
                    })
                })
                .collect::<Result<Vec<_>>>()?,
        );
        let route_outputs = routes
            .iter()
            .filter_map(|route| {
                groups[0]
                    .outputs()
                    .iter()
                    .find(|output| output.handle() == route.output)
                    .map(|output| (route.id, (CovenantGroupId::Current, output.id())))
            })
            .collect();
        Ok(Self {
            id,
            source: entry,
            groups,
            template_selectors,
            handle_value_initializers: BTreeMap::new(),
            routes,
            route_indexes,
            foreign_groups: BTreeMap::new(),
            route_outputs,
        })
    }

    /// Return the source entry declaration.
    pub(crate) fn source(&self) -> &'a EntryDecl {
        self.source
    }

    /// Identify an observed clause by its retained declaration, including its side.
    pub(crate) fn observed_interaction_id(&self, observe: &ObserveDecl, observed: &ObservedActorDecl) -> Result<InteractionId> {
        for (observe_index, candidate) in self.source.observes.iter().enumerate() {
            if !std::ptr::eq(candidate, observe) {
                continue;
            }
            if let Some(input) = candidate.inputs.iter().position(|item| std::ptr::eq(item, observed)) {
                return Ok(InteractionId::ObservedInput { observe: observe_index, input });
            }
            if let Some(output) = candidate.outputs.iter().position(|item| std::ptr::eq(item, observed)) {
                return Ok(InteractionId::ObservedOutput { observe: observe_index, output });
            }
        }
        Err(ArgentError::new("observed actor is not part of its entry model"))
    }

    /// Return the interaction group governed by the current covenant.
    pub(crate) fn current(&self) -> &CovenantGroup<'a> {
        self.groups.first().expect("entry model always contains its current covenant")
    }

    /// Look up one planned covenant group by semantic position.
    pub(crate) fn group(&self, id: CovenantGroupId) -> Option<&CovenantGroup<'a>> {
        self.groups.iter().find(|group| group.id() == id)
    }

    /// Look up the bound covenant at a foreign route statement.
    pub(crate) fn foreign_group(&self, site: NodeId) -> Option<CovenantGroupId> {
        self.foreign_groups.get(&site).copied()
    }

    /// Look up the completed output identity for a current or foreign route.
    pub(crate) fn route_output(&self, id: RouteId) -> Option<(CovenantGroupId, InteractionId)> {
        self.route_outputs.get(&id).copied()
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

    /// Resolve a selector by the bound local that carries its actor choice.
    pub(crate) fn selector_for_local(&self, local: LocalId) -> Option<&TemplateSelector> {
        self.template_selectors.values().find(|selector| selector.binding == Some(local))
    }

    /// Ordinary handle declarations retain their binding and initializer identities.
    pub(super) fn handle_value_initializers(&self) -> &BTreeMap<LocalId, NodeId> {
        &self.handle_value_initializers
    }

    /// Return resolved routes in terminal source order.
    pub(crate) fn routes(&self) -> &[ResolvedRoute] {
        &self.routes
    }

    /// Resolve a syntax route through its stable source identity.
    pub(crate) fn route(&self, id: RouteId) -> Option<&ResolvedRoute> {
        self.route_indexes.get(&id).and_then(|index| self.routes.get(*index))
    }

    pub(super) fn attach_bound_route(&mut self, id: RouteId, value: BoundRouteValue) {
        let route = &mut self.routes[self.route_indexes[&id]];
        let ResolvedSuccessor::Constructed { bound, .. } = &mut route.successor else {
            unreachable!("only constructed routes have bound actor and state sites")
        };
        *bound = Some(value);
    }

    /// Collect concrete app actors whose templates this entry reads or writes.
    ///
    /// Current outputs follow the body-selected routes; existing and genesis
    /// outputs are exhaustive in their covenant groups.
    pub(crate) fn actor_template_uses(&self, source_actor: DeclId, app_actors: &AppActors) -> ActorTemplateUses {
        let mut uses = ActorTemplateUses::default();

        for group in self.groups() {
            for interaction in group.inputs() {
                for target in interaction.target().static_actors() {
                    if let StaticActorId::InApp(id) = target
                        && app_actors.name(*id).is_some()
                        && !(app_actors.iter().count() == 1 && *id == source_actor)
                    {
                        uses.reads.insert(target.clone());
                    }
                }
            }
        }

        // Current declarations define allowed output domains; body routes identify
        // concrete template writes, while selector routes use selector witnesses.
        for route in &self.routes {
            let ResolvedSuccessor::Constructed { bound: Some(bound), .. } = &route.successor else {
                continue;
            };
            if let BoundRouteActor::Fixed(id) = bound.actor_target
                && app_actors.name(id).is_some()
                && id != source_actor
            {
                uses.writes.insert(StaticActorId::InApp(id));
            }
        }
        for group in self.existing_groups().chain(self.genesis_groups()) {
            for interaction in group.outputs() {
                for target in interaction.target().static_actors() {
                    if let StaticActorId::InApp(id) = target
                        && app_actors.name(*id).is_some()
                        && *id != source_actor
                    {
                        uses.writes.insert(target.clone());
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
    pub(crate) reads: BTreeSet<StaticActorId>,
    pub(crate) writes: BTreeSet<StaticActorId>,
}

/// Ordered interactions governed by one covenant ID.
///
/// The current covenant and each observe or spawn clause form separate groups.
#[derive(Debug)]
pub(crate) struct CovenantGroup<'a> {
    id: CovenantGroupId,
    covenant: CovenantContext<'a>,
    inputs: Vec<EntryInteraction<'a>>,
    outputs: Vec<EntryInteraction<'a>>,
}

impl<'a> CovenantGroup<'a> {
    /// Return this covenant group's identity within its entry.
    pub(crate) fn id(&self) -> CovenantGroupId {
        self.id
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

/// Stable covenant clause position within one completed entry model.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum CovenantGroupId {
    Current,
    Existing(usize),
    Genesis(usize),
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
    id: InteractionId,
    source: InteractionSource<'a>,
    handle: &'a str,
    target: ActorTarget,
    cardinality: ResolvedCardinality,
    location: InteractionLocation,
}

impl<'a> EntryInteraction<'a> {
    /// Return this interaction's identity within its entry.
    pub(crate) fn id(&self) -> InteractionId {
        self.id
    }

    /// Return the exact source node that declared this interaction.
    pub(crate) fn source(&self) -> InteractionSource<'a> {
        self.source
    }

    /// Return the declared interaction handle.
    pub(crate) fn handle(&self) -> &'a str {
        self.handle
    }

    /// Return the compiler-known target candidates.
    pub(crate) fn target(&self) -> &ActorTarget {
        &self.target
    }

    /// Return the transaction cardinality resolved for this interaction.
    pub(crate) fn cardinality(&self) -> ResolvedCardinality {
        self.cardinality
    }

    /// Return this interaction's symbolic position within its covenant side.
    pub(crate) fn location(&self) -> InteractionLocation {
        self.location
    }
}

/// Stable declaration position within one completed entry model.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum InteractionId {
    CurrentInput(usize),
    CurrentOutput(usize),
    ObservedInput { observe: usize, input: usize },
    ObservedOutput { observe: usize, output: usize },
    SpawnedOutput { spawn: usize, output: usize },
}

impl InteractionId {
    /// Resolve the indexed actor target owned by an observed or spawned interaction.
    pub(crate) fn actor_target_site(self, entry: EntryId, resolution: &ResolvedModules<'_>) -> Result<NodeId> {
        let cursor = SourceNodeCursor::new(entry.actor, RootSlot::Entry(entry.index));
        let cursor = match self {
            Self::ObservedInput { observe, input } => {
                cursor.child(ChildEdge::Observe(observe)).child(ChildEdge::ObservedInput(input)).child(ChildEdge::ActorTarget)
            }
            Self::ObservedOutput { observe, output } => {
                cursor.child(ChildEdge::Observe(observe)).child(ChildEdge::ObservedOutput(output)).child(ChildEdge::ActorTarget)
            }
            Self::SpawnedOutput { spawn, output } => {
                cursor.child(ChildEdge::Spawn(spawn)).child(ChildEdge::SpawnOutput(output)).child(ChildEdge::ActorTarget)
            }
            Self::CurrentInput(_) | Self::CurrentOutput(_) => {
                return Err(ArgentError::new("current entry interaction has no actor target site"));
            }
        };
        resolution.nodes().find(&cursor.address).ok_or_else(|| ArgentError::new("entry actor target has no indexed source site"))
    }
}

/// A transaction position derived from one ordered clause section.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InteractionLocation {
    /// A singleton at a fixed zero-based index from the section start.
    FromStart(usize),
    /// The ranged span; its actual length is the section count minus `singleton_count`.
    Range { start: usize, singleton_count: usize },
    /// A singleton after the range, where one is the last section item.
    FromEnd(usize),
}

impl InteractionLocation {
    pub(crate) fn is_range(self) -> bool {
        matches!(self, Self::Range { .. })
    }
}

fn plan_interaction_locations<'a>(
    items: impl IntoIterator<Item = (&'a str, &'a Cardinality)>,
    actor_name: &str,
    entry_name: &str,
    section: &str,
) -> Result<Vec<InteractionLocation>> {
    let items = items.into_iter().collect::<Vec<_>>();
    let mut range = None;
    for (index, (handle, cardinality)) in items.iter().enumerate() {
        if !matches!(cardinality, Cardinality::Range { .. }) {
            continue;
        }
        if let Some((_, first_handle)) = range {
            return Err(ArgentError::new(format!(
                "entry `{actor_name}::{entry_name}` `{section}` supports at most one range, found `{first_handle}` and `{handle}`"
            )));
        }
        range = Some((index, *handle));
    }

    let Some((range_index, _)) = range else {
        return Ok((0..items.len()).map(InteractionLocation::FromStart).collect());
    };
    let singleton_count = items.len() - 1;
    Ok((0..items.len())
        .map(|index| match index.cmp(&range_index) {
            std::cmp::Ordering::Less => InteractionLocation::FromStart(index),
            std::cmp::Ordering::Equal => InteractionLocation::Range { start: index, singleton_count },
            std::cmp::Ordering::Greater => InteractionLocation::FromEnd(items.len() - index),
        })
        .collect())
}

/// Clause cardinality after all source bounds have been resolved.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ResolvedCardinality {
    One,
    Range { minimum: i64, maximum: i64 },
}

impl ResolvedCardinality {
    pub(crate) fn is_range(self) -> bool {
        matches!(self, Self::Range { .. })
    }

    pub(crate) fn range_bounds(self) -> Option<(i64, i64)> {
        match self {
            Self::One => None,
            Self::Range { minimum, maximum } => Some((minimum, maximum)),
        }
    }

    fn resolve(
        cardinality: &Cardinality,
        actor_name: &str,
        entry: &EntryDecl,
        handle: &str,
        const_resolver: &ConstResolver,
    ) -> Result<Self> {
        let Cardinality::Range { minimum, maximum } = cardinality else {
            return Ok(Self::One);
        };
        let minimum = resolve_cardinality_bound(minimum, actor_name, entry, handle, const_resolver)?;
        let maximum = resolve_cardinality_bound(maximum, actor_name, entry, handle, const_resolver)?;
        if minimum < 0 || maximum < 0 {
            return Err(ArgentError::new(format!(
                "entry `{actor_name}::{}` range `{handle}` must have non-negative bounds, found {minimum}..={maximum}",
                entry.name
            )));
        }
        if minimum > maximum {
            return Err(ArgentError::new(format!(
                "entry `{actor_name}::{}` range `{handle}` minimum {minimum} exceeds maximum {maximum}",
                entry.name
            )));
        }
        if maximum > MAX_ENTRY_RANGE_CARDINALITY {
            return Err(ArgentError::new(format!(
                "entry `{actor_name}::{}` range `{handle}` maximum {maximum} exceeds compiler limit {MAX_ENTRY_RANGE_CARDINALITY}",
                entry.name
            )));
        }
        Ok(Self::Range { minimum, maximum })
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

impl<'a> InteractionSource<'a> {
    fn cardinality(self) -> &'a Cardinality {
        match self {
            Self::Consume(consume) => &consume.cardinality,
            Self::CurrentOutput(output) => &output.cardinality,
            Self::ObserveInput(observed) | Self::ObserveOutput(observed) => &observed.cardinality,
            Self::SpawnOutput(output) => &output.cardinality,
        }
    }
}

fn resolve_cardinality_bound(
    bound: &CardinalityBound,
    actor_name: &str,
    entry: &EntryDecl,
    handle: &str,
    const_resolver: &ConstResolver,
) -> Result<i64> {
    let name = match bound {
        CardinalityBound::Literal(value) => return Ok(*value),
        CardinalityBound::Const(name) => name,
    };
    const_resolver.resolve_int(name).map_err(|err| match err {
        ConstIntError::Unknown => {
            ArgentError::new(format!("entry `{actor_name}::{}` range `{handle}` references unknown constant `{name}`", entry.name))
        }
        ConstIntError::WrongType(actual) => ArgentError::new(format!(
            "entry `{actor_name}::{}` range `{handle}` bound `{name}` must have type `int`, found `{actual}`",
            entry.name
        )),
        ConstIntError::InvalidLiteral => ArgentError::new(format!(
            "entry `{actor_name}::{}` range `{handle}` bound `{name}` must be initialized with a valid `int` literal",
            entry.name
        )),
        ConstIntError::Cycle => {
            ArgentError::new(format!("entry `{actor_name}::{}` range `{handle}` bound `{name}` has a constant cycle", entry.name))
        }
        ConstIntError::Overflow => {
            ArgentError::new(format!("entry `{actor_name}::{}` range `{handle}` bound `{name}` overflows `int`", entry.name))
        }
        ConstIntError::DivisionByZero => {
            ArgentError::new(format!("entry `{actor_name}::{}` range `{handle}` bound `{name}` divides by zero", entry.name))
        }
    })
}

/// A source-selected target or compiler-known static actor domain.
#[derive(Debug)]
pub(crate) enum ActorTarget {
    /// A runtime actor-type value or open observed binding.
    Source(String),
    /// One fixed actor or an expanded actor-enum domain.
    Static(Vec<StaticActorId>),
    /// An invalid fixed reference retained until source validation reports it.
    UnresolvedStatic(Vec<String>),
}

impl ActorTarget {
    /// Render actor references only at the artifact boundary.
    pub(crate) fn artifact_references(&self, model: &AppCompilationContext<'_>) -> Result<Vec<String>> {
        match self {
            Self::Source(actor) => Ok(vec![actor.clone()]),
            Self::Static(actors) => actors.iter().map(|id| model.static_actor_reference(id)).collect(),
            Self::UnresolvedStatic(actors) => Ok(actors.clone()),
        }
    }

    /// Iterate only bound static actor identities.
    pub(crate) fn static_actors(&self) -> impl Iterator<Item = &StaticActorId> {
        match self {
            Self::Source(_) | Self::UnresolvedStatic(_) => [].as_slice(),
            Self::Static(actors) => actors.as_slice(),
        }
        .iter()
    }

    /// Return the sole static actor, if this target is a singleton domain.
    pub(crate) fn single_static_actor(&self) -> Option<&StaticActorId> {
        match self {
            Self::Static(actors) if actors.len() == 1 => Some(&actors[0]),
            Self::Source(_) | Self::Static(_) | Self::UnresolvedStatic(_) => None,
        }
    }

    /// Return whether a source value selects this target.
    pub(crate) fn is_source(&self) -> bool {
        matches!(self, Self::Source(_))
    }
}

/// An actor-type source and the state selected by its declared type.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum ClauseActorTypeRef {
    StateField { field: SourceFieldId, state: SourceStateId },
    EntryArgument { index: usize, name: String, state: SourceStateId },
}

impl ClauseActorTypeRef {
    /// Return the actor state declared by this source value.
    pub(crate) fn state(&self) -> &SourceStateId {
        match self {
            Self::StateField { state, .. } | Self::EntryArgument { state, .. } => state,
        }
    }
}

/// A source value supplying an observed covenant ID.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CovenantIdSource {
    StateField { field: SourceFieldId },
    EntryArgument { index: usize },
}

pub(crate) fn clause_actor_type_ref(
    entry_id: EntryId,
    interaction: InteractionId,
    expr: &str,
    actor: &ActorDecl,
    entry: &EntryDecl,
    model: &AppCompilationContext<'_>,
) -> Result<Option<ClauseActorTypeRef>> {
    let site = interaction.actor_target_site(entry_id, model.resolution)?;
    let reference = model
        .resolution
        .bindings(entry_id.actor)
        .clause_actor_references
        .get(&site)
        .ok_or_else(|| ArgentError::new("actor-type clause has no retained source reference"))?;
    let actor_state =
        model.types.actor_states.get(&entry_id.actor).ok_or_else(|| ArgentError::new("actor-type clause actor has no bound state"))?;
    let actor_source = model.source_state_id_by_decl(*actor_state)?;
    let storage_source = model.storage_source_id(&actor_source);
    let storage = model.state_by_source(storage_source)?;
    let storage_id = model
        .state_decl_id_by_source(storage_source)
        .ok_or_else(|| ArgentError::new("actor-type clause storage state has no bound identity"))?;
    let (ty, authored_type) = match reference {
        Some(ClauseReference::StateField { index: Some(index), .. }) => {
            let field =
                storage.fields.get(*index).ok_or_else(|| ArgentError::new("actor-type clause field index is out of bounds"))?;
            let ty = model
                .types
                .state_fields
                .get(&(storage_id, *index))
                .ok_or_else(|| ArgentError::new("actor-type clause field has no resolved type"))?;
            (ty, &field.ty)
        }
        Some(ClauseReference::StateField { name, index: None }) => {
            return Err(ArgentError::new(format!(
                "entry `{}::{}` references unknown state field `{}.{name}`",
                actor.name,
                entry.name,
                word::SELF
            )));
        }
        Some(ClauseReference::EntryArgument { index, .. }) => {
            let param =
                entry.params.get(*index).ok_or_else(|| ArgentError::new("actor-type clause parameter index is out of bounds"))?;
            let ty = model
                .types
                .entry_params
                .get(&(entry_id.actor, entry_id.index, *index))
                .ok_or_else(|| ArgentError::new("actor-type clause parameter has no resolved type"))?;
            (ty, &param.ty)
        }
        Some(ClauseReference::BareStateField { name, .. }) => {
            return Err(ArgentError::new(format!(
                "entry `{}::{}` state field `{name}` must be referenced as `{}.{name}` in entry clauses",
                actor.name,
                entry.name,
                word::SELF
            )));
        }
        Some(ClauseReference::Bare(_)) | None => return Ok(None),
    };

    let ResolvedTypeBase::ActorHandle(actor_state) = &ty.base else {
        return Err(ArgentError::new(format!(
            "entry `{}::{}` clause reference `{}` has type `{}`; expected `{}<State>`",
            actor.name,
            entry.name,
            expr.trim(),
            authored_type.to_source(),
            word::ACTOR_TYPE
        )));
    };
    let state = model.source_state_id_by_decl(*actor_state)?;
    Ok(Some(match reference {
        Some(ClauseReference::StateField { name, index: Some(_) }) => ClauseActorTypeRef::StateField {
            field: SourceFieldId::new(model.source_state_id_by_decl(storage_id)?, name.clone()),
            state,
        },
        Some(ClauseReference::EntryArgument { name, index }) => {
            ClauseActorTypeRef::EntryArgument { index: *index, name: name.clone(), state }
        }
        Some(ClauseReference::StateField { index: None, .. })
        | Some(ClauseReference::BareStateField { .. })
        | Some(ClauseReference::Bare(_))
        | None => unreachable!("unbound clause reference returned above"),
    }))
}

pub(crate) fn source_actor_type_state_for_expr(
    entry_id: EntryId,
    interaction: InteractionId,
    expr: &str,
    actor: &ActorDecl,
    entry: &EntryDecl,
    model: &AppCompilationContext<'_>,
) -> Result<Option<SourceStateId>> {
    Ok(clause_actor_type_ref(entry_id, interaction, expr, actor, entry, model)?.map(|source| source.state().clone()))
}

// Spawn targets may be an explicitly dynamic actor_type value or any fixed
// actor resolved by the selected app. Linked templates remain imported
// capabilities and do not enter the selected app's route graph.
pub(crate) fn spawn_target_state(
    entry_id: EntryId,
    interaction: InteractionId,
    target: &ActorTarget,
    expr: &str,
    actor: &ActorDecl,
    entry: &EntryDecl,
    model: &AppCompilationContext<'_>,
) -> Result<Option<SourceStateId>> {
    if let Some(target) = model.resolve_static_actor_target(target) {
        return Ok(Some(model.static_actor_source_state(&target.id())?));
    }
    source_actor_type_state_for_expr(entry_id, interaction, expr, actor, entry, model)
}

pub(crate) fn observed_open_bindings(observe: &ObserveDecl) -> BTreeMap<&str, &str> {
    observe.inputs.iter().filter_map(|input| input.open_state.as_deref().map(|state| (input.actor.as_str(), state))).collect()
}

pub(crate) fn observed_dynamic_binding_state(
    entry_id: EntryId,
    observe: &ObserveDecl,
    observed: &ObservedActorDecl,
    model: &AppCompilationContext<'_>,
) -> Result<Option<SourceStateId>> {
    let entry_model = model.entry_model_by_id(entry_id)?;
    let interaction = entry_model.observed_interaction_id(observe, observed)?;
    let bindings = model.resolution.bindings(entry_model.id.actor);
    if observed.open_state.is_some() {
        if !matches!(interaction, InteractionId::ObservedInput { .. } | InteractionId::ObservedOutput { .. }) {
            return Err(ArgentError::new("observed open state has the wrong interaction kind"));
        }
        let site = interaction.actor_target_site(entry_model.id, model.resolution)?;
        let state =
            bindings.open_state_targets.get(&site).ok_or_else(|| ArgentError::new("observed open state has no bound declaration"))?;
        return Ok(Some(model.source_state_id_by_decl(*state)?));
    }
    let InteractionId::ObservedOutput { observe: observe_index, .. } = interaction else { return Ok(None) };
    let site = interaction.actor_target_site(entry_model.id, model.resolution)?;
    let Some(source) = bindings.local_actor_targets.get(&site) else { return Ok(None) };
    for (input, candidate) in observe.inputs.iter().enumerate() {
        if candidate.open_state.is_none() {
            continue;
        }
        let input_site =
            InteractionId::ObservedInput { observe: observe_index, input }.actor_target_site(entry_model.id, model.resolution)?;
        if bindings.local_actor_targets.get(&input_site) == Some(source) {
            let state = bindings
                .open_state_targets
                .get(&input_site)
                .ok_or_else(|| ArgentError::new("observed open state has no bound declaration"))?;
            return Ok(Some(model.source_state_id_by_decl(*state)?));
        }
    }
    Ok(None)
}

pub(crate) fn observed_open_state_for_decl(
    entry_id: EntryId,
    actor: &ActorDecl,
    entry: &EntryDecl,
    observe: &ObserveDecl,
    observed: &ObservedActorDecl,
    model: &AppCompilationContext<'_>,
) -> Result<Option<SourceStateId>> {
    if let Some(state) = observed_dynamic_binding_state(entry_id, observe, observed, model)? {
        return Ok(Some(state));
    }
    let interaction = model.entry_model_by_id(entry_id)?.observed_interaction_id(observe, observed)?;
    source_actor_type_state_for_expr(entry_id, interaction, &observed.actor, actor, entry, model)
}

pub(crate) fn observed_is_dynamic_binding(
    entry_id: EntryId,
    observe: &ObserveDecl,
    observed: &ObservedActorDecl,
    model: &AppCompilationContext<'_>,
) -> Result<bool> {
    Ok(observed_dynamic_binding_state(entry_id, observe, observed, model)?.is_some())
}

pub(crate) fn resolve_observe_covenant_id_source(
    entry_id: EntryId,
    actor: &ActorDecl,
    entry: &EntryDecl,
    model: &AppCompilationContext<'_>,
    observe: &ObserveDecl,
) -> Result<CovenantIdSource> {
    let observe_index = entry
        .observes
        .iter()
        .position(|candidate| std::ptr::eq(candidate, observe))
        .ok_or_else(|| ArgentError::new("observed covenant is not part of its entry model"))?;
    let reference = model
        .resolution
        .bindings(entry_id.actor)
        .observe_covenant_references
        .get(&(entry_id.index, observe_index))
        .ok_or_else(|| ArgentError::new("observed covenant has no retained source reference"))?;
    let actor_state =
        model.types.actor_states.get(&entry_id.actor).ok_or_else(|| ArgentError::new("observed covenant actor has no bound state"))?;
    let actor_source = model.source_state_id_by_decl(*actor_state)?;
    let storage_source = model.storage_source_id(&actor_source);
    let storage = model.state_by_source(storage_source)?;
    let storage_id = model
        .state_decl_id_by_source(storage_source)
        .ok_or_else(|| ArgentError::new("observed covenant storage state has no bound identity"))?;
    match reference {
        Some(ClauseReference::StateField { name, index: Some(index) }) => {
            let field =
                storage.fields.get(*index).ok_or_else(|| ArgentError::new("observed covenant field index is out of bounds"))?;
            let ty = model
                .types
                .state_fields
                .get(&(storage_id, *index))
                .ok_or_else(|| ArgentError::new("observed covenant field has no resolved type"))?;
            require_covenant_id_source_type(actor, entry, observe, &format!("{}.{name}", word::SELF), ty, &field.ty)?;
            Ok(CovenantIdSource::StateField { field: SourceFieldId::new(storage_source.clone(), name.clone()) })
        }
        Some(ClauseReference::StateField { name, index: None }) => Err(ArgentError::new(format!(
            "entry `{}::{}` observe `{}` references unknown state field `{}.{name}`",
            actor.name,
            entry.name,
            observe.name,
            word::SELF
        ))),
        Some(ClauseReference::EntryArgument { name, index }) => {
            let param =
                entry.params.get(*index).ok_or_else(|| ArgentError::new("observed covenant parameter index is out of bounds"))?;
            let ty = model
                .types
                .entry_params
                .get(&(entry_id.actor, entry_id.index, *index))
                .ok_or_else(|| ArgentError::new("observed covenant parameter has no resolved type"))?;
            require_covenant_id_source_type(actor, entry, observe, name, ty, &param.ty)?;
            Ok(CovenantIdSource::EntryArgument { index: *index })
        }
        Some(ClauseReference::BareStateField { name, .. }) => Err(ArgentError::new(format!(
            "entry `{}::{}` observe `{}` state field `{name}` must be referenced as `{}.{name}`",
            actor.name,
            entry.name,
            observe.name,
            word::SELF
        ))),
        Some(ClauseReference::Bare(_)) => Err(unsupported_observe_covenant_id_source(actor, entry, observe)),
        None => Err(unsupported_observe_covenant_id_source(actor, entry, observe)),
    }
}

fn require_covenant_id_source_type(
    actor: &ActorDecl,
    entry: &EntryDecl,
    observe: &ObserveDecl,
    source: &str,
    resolved: &ResolvedType,
    authored: &TypeRef,
) -> Result<()> {
    if matches!(&resolved.base, ResolvedTypeBase::Builtin(name) if name == word::COVENANT_ID) && resolved.array.is_none() {
        return Ok(());
    }
    Err(ArgentError::new(format!(
        "entry `{}::{}` observe `{}` covenant id source `{source}` has type `{}`; expected `{}`",
        actor.name,
        entry.name,
        observe.name,
        authored.to_source(),
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

/// An actor-enum value selecting a template within one state domain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TemplateSelector {
    pub(crate) name: String,
    pub(crate) binding: Option<LocalId>,
    pub(crate) actor_enum: String,
    pub(crate) state: String,
    pub(crate) variants: Vec<String>,
    pub(crate) fixed_actor: Option<String>,
    pub(crate) fixed_index: Option<usize>,
    targets: Option<Vec<StaticActorId>>,
}

impl TemplateSelector {
    /// Resolve the shared source state of the selector's bound variants.
    pub(crate) fn source_state(&self, model: &AppCompilationContext<'_>) -> Result<SourceStateId> {
        let mut targets = self.variant_actor_ids()?.iter();
        let first = targets.next().ok_or_else(|| ArgentError::new(format!("actor selector `{}` has no variants", self.name)))?;
        let source = model.static_actor_source_state(first)?;
        for target in targets {
            if model.static_actor_source_state(target)? != source {
                return Err(ArgentError::new(format!("actor selector `{}` variants do not share one source state", self.name)));
            }
        }
        Ok(source)
    }

    /// Return the selected targets resolved during model construction.
    pub(crate) fn route_actor_ids(&self) -> Result<&[StaticActorId]> {
        let variants = self.variant_actor_ids()?;
        match self.fixed_index {
            Some(index) => variants
                .get(index)
                .map(std::slice::from_ref)
                .ok_or_else(|| ArgentError::new(format!("selector `{}` has no bound fixed actor target", self.name))),
            None => Ok(variants),
        }
    }

    pub(crate) fn variant_actor_ids(&self) -> Result<&[StaticActorId]> {
        self.targets.as_deref().ok_or_else(|| ArgentError::new(format!("selector `{}` has no bound actor targets", self.name)))
    }
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

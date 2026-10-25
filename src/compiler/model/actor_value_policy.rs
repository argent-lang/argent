//! Keeps in-app enum selectors distinct from external actor handles.

use std::collections::{BTreeMap, BTreeSet};

use silverscript_lang::ast::visit::{AstVisitorMut, walk_expr_mut, walk_statement_mut};
use silverscript_lang::ast::{Expr, ExprKind, Span, Statement};

use crate::compiler::resolve::{Binding, LocalId, ResolvedDeclaration, ResolvedModules, ResolvedName};
use crate::compiler::syntax::node::{ChildEdge, DeclId, EntryId, ReferenceRole, RootSlot, SymbolKind};
use crate::compiler::syntax::source::Origin;
use crate::error::{ArgentError, Result};

use super::types::{CallableId, CallableSignature, ResolvedType, ResolvedTypeBase, TypeTable};

#[cfg(test)]
mod tests;

#[derive(Clone, Copy)]
enum ActorValueCategory {
    Handle,
    Enum(DeclId),
}

impl ActorValueCategory {
    fn declared(ty: &ResolvedType) -> Option<Self> {
        match ty.base {
            ResolvedTypeBase::ActorHandle(_) => Some(Self::Handle),
            ResolvedTypeBase::ActorEnum(enumeration) => Some(Self::Enum(enumeration)),
            _ => None,
        }
    }
}

struct ActorValuePolicy<'a, 'src> {
    program: &'a ResolvedModules<'src>,
    types: &'a TypeTable,
    owner: DeclId,
    references: BTreeMap<(usize, usize), Binding>,
    locals: BTreeMap<LocalId, ActorValueCategory>,
    handle_types: BTreeSet<usize>,
    fields: BTreeMap<String, ActorValueCategory>,
    result: Option<ActorValueCategory>,
    error: Option<ArgentError>,
}

impl<'a, 'src> ActorValuePolicy<'a, 'src> {
    fn new(program: &'a ResolvedModules<'src>, types: &'a TypeTable, owner: DeclId) -> Result<Self> {
        let bindings = program.bindings(owner);
        let mut references = BTreeMap::new();
        let mut locals = BTreeMap::new();
        let mut handle_types = BTreeSet::new();
        for (site, binding) in &bindings.sites {
            let node = program.nodes().node(*site);
            if let Origin::Authored { start, end, .. } = node.origin {
                references.insert((start, end), binding.clone());
                if types.actor_handle_type_uses.contains_key(site) {
                    handle_types.insert(start);
                }
                if node.reference_role == Some(ReferenceRole::Value)
                    && let Binding::Source(ResolvedName::Declaration(actor)) = binding
                    && actor.kind() == SymbolKind::Actor
                {
                    return Err(ArgentError::at_source(
                        program.declaration_path(owner),
                        program.source_text(owner.module),
                        start,
                        format!(
                            "in-app actor `{}` can only be referenced as a value through an actor enum group",
                            program.declaration(*actor).name()
                        ),
                    ));
                }
            }
            let Binding::Local(local) = binding else { continue };
            let mut address = node.address.clone();
            if address.children.pop() != Some(ChildEdge::BindingName) {
                continue;
            }
            address.children.push(ChildEdge::TypeUse);
            let Some(type_site) = program.nodes().find(&address) else { continue };
            let category = if types.actor_handle_type_uses.contains_key(&type_site) {
                Some(ActorValueCategory::Handle)
            } else {
                match bindings.sites.get(&type_site) {
                    Some(Binding::Source(ResolvedName::Declaration(id))) if id.kind() == SymbolKind::ActorEnum => {
                        Some(ActorValueCategory::Enum(*id))
                    }
                    Some(Binding::Source(ResolvedName::AppMember(member))) if member.actor.kind() == SymbolKind::ActorEnum => {
                        Some(ActorValueCategory::Enum(member.actor))
                    }
                    _ => None,
                }
            };
            if let Some(category) = category {
                locals.insert(*local, category);
            }
        }
        for ((root, index), local) in &bindings.parameter_ids {
            let ty = match root {
                RootSlot::Declaration => {
                    types.callables.get(&CallableId { owner, member: None }).and_then(|sig| sig.params.get(*index))
                }
                RootSlot::ActorFunction(member) => {
                    types.callables.get(&CallableId { owner, member: Some(*member) }).and_then(|sig| sig.params.get(*index))
                }
                RootSlot::Entry(entry) => types.entry_params.get(&(owner, *entry, *index)),
                _ => None,
            };
            if let Some(category) = ty.and_then(ActorValueCategory::declared) {
                locals.insert(*local, category);
            }
        }
        let mut fields = BTreeMap::new();
        if let Some(state) = types.actor_states.get(&owner) {
            let mut state = *state;
            let mut visited = BTreeSet::new();
            loop {
                if !visited.insert(state) {
                    return Err(ArgentError::new("actor state expansion has a cycle"));
                }
                let ResolvedDeclaration::State(declaration) = program.declaration(state) else { break };
                if let Some(expansion) = &declaration.expansion {
                    match program.bindings(state).names.get(&expansion.base) {
                        Some(ResolvedName::Declaration(base)) if *base != state => state = *base,
                        _ => break,
                    }
                    continue;
                }
                for (index, field) in declaration.fields.iter().enumerate() {
                    if let Some(category) = types.state_fields.get(&(state, index)).and_then(ActorValueCategory::declared) {
                        fields.insert(field.name.clone(), category);
                    }
                }
                break;
            }
        }
        Ok(Self { program, types, owner, references, locals, handle_types, fields, result: None, error: None })
    }

    fn binding(&self, span: Span<'_>) -> Option<&Binding> {
        self.references.get(&(span.start(), span.end()))
    }

    fn callable(&self, span: Span<'_>) -> Option<&CallableSignature> {
        let id = match self.binding(span)? {
            Binding::Source(ResolvedName::Declaration(owner)) if owner.kind() == SymbolKind::Function => {
                CallableId { owner: *owner, member: None }
            }
            Binding::ActorHelper(member) => CallableId { owner: self.owner, member: Some(*member) },
            _ => return None,
        };
        self.types.callables.get(&id)
    }

    fn category(&self, expr: &Expr<'_>) -> Option<ActorValueCategory> {
        match &expr.kind {
            ExprKind::Identifier(name) => match self.binding(expr.span)? {
                Binding::EnumVariant { enumeration, .. } => Some(ActorValueCategory::Enum(*enumeration)),
                Binding::Local(local) => self.locals.get(local).copied(),
                Binding::Source(ResolvedName::Declaration(id)) if id.kind() == SymbolKind::Const => {
                    self.types.constants.get(id).and_then(ActorValueCategory::declared)
                }
                Binding::Source(ResolvedName::Declaration(id)) if id.kind() == SymbolKind::ActorEnum => {
                    Some(ActorValueCategory::Enum(*id))
                }
                Binding::Source(ResolvedName::AppMember(member)) if member.actor.kind() == SymbolKind::ActorEnum => {
                    Some(ActorValueCategory::Enum(member.actor))
                }
                Binding::ActorField => self.fields.get(name).copied(),
                _ => None,
            },
            ExprKind::ArrayIndex { source, .. } => self.category(source),
            ExprKind::Call { name_span, .. } => self.callable(*name_span)?.result.as_ref().and_then(ActorValueCategory::declared),
            ExprKind::FieldAccess { source, field, .. }
                if matches!(&source.kind, ExprKind::Identifier(name) if name == "self")
                    && matches!(self.binding(source.span), Some(Binding::RuntimeRoot)) =>
            {
                self.fields.get(field).copied()
            }
            _ => None,
        }
    }

    fn check_handle(&mut self, expr: &Expr<'_>, declaration: Option<&str>) {
        let Some(ActorValueCategory::Enum(enumeration)) = self.category(expr) else { return };
        let enumeration = self
            .program
            .source_declaration_name(self.owner.module, enumeration)
            .unwrap_or_else(|| self.program.declaration(enumeration).name().to_string());
        let suggestion = declaration.map_or_else(
            || format!("use the explicit actor-enum type `{enumeration}` for in-app selection"),
            |name| format!("declare `{enumeration} {name} = {};` instead", expr.span.as_str()),
        );
        self.error.get_or_insert_with(|| {
            ArgentError::at_source(
                self.program.declaration_path(self.owner),
                self.program.source_text(self.owner.module),
                expr.span.start(),
                format!("actor_type<State> is an external actor handle, not an actor-enum selector; {suggestion}"),
            )
        });
    }

    fn check_arguments(&mut self, name: Span<'_>, args: &[Expr<'_>]) {
        let expected = self.callable(name).map(|sig| sig.params.iter().map(ActorValueCategory::declared).collect::<Vec<_>>());
        if let Some(expected) = expected {
            for (expected, actual) in expected.into_iter().zip(args) {
                if matches!(expected, Some(ActorValueCategory::Handle)) {
                    self.check_handle(actual, None);
                }
            }
        }
    }
}

impl<'src> AstVisitorMut<'src> for ActorValuePolicy<'_, '_> {
    fn visit_statement(&mut self, statement: &mut Statement<'src>) {
        match statement {
            Statement::VariableDefinition { name, type_span, expr: Some(expr), .. }
                if self.handle_types.contains(&type_span.start()) =>
            {
                self.check_handle(expr, Some(name));
            }
            Statement::Assign { name, name_span, expr, .. } => {
                let category = match self.binding(*name_span) {
                    Some(Binding::Local(local)) => self.locals.get(local),
                    Some(Binding::ActorField) => self.fields.get(name),
                    _ => None,
                };
                if matches!(category, Some(ActorValueCategory::Handle)) {
                    self.check_handle(expr, None);
                }
            }
            Statement::Return { exprs, .. } if matches!(self.result, Some(ActorValueCategory::Handle)) => {
                for expr in exprs {
                    self.check_handle(expr, None);
                }
            }
            Statement::FunctionCall { name_span, args, .. } => {
                self.check_arguments(*name_span, args);
            }
            Statement::FunctionCallAssign { bindings, name, name_span, args, span } => {
                self.check_arguments(*name_span, args);
                if bindings.iter().any(|binding| self.handle_types.contains(&binding.type_span.start())) {
                    // The declared result type is enough; arguments are checked separately.
                    let call = Expr::new(ExprKind::Call { name: name.clone(), args: Vec::new(), name_span: *name_span }, *span);
                    self.check_handle(&call, None);
                }
            }
            _ => {}
        }
        walk_statement_mut(self, statement);
    }

    fn visit_expr(&mut self, expr: &mut Expr<'src>) {
        if let ExprKind::Call { name_span, args, .. } = &expr.kind {
            self.check_arguments(*name_span, args);
        }
        walk_expr_mut(self, expr);
    }
}

pub(super) fn validate_actor_value_policy(program: &ResolvedModules<'_>, types: &TypeTable, actors: &[DeclId]) -> Result<()> {
    for owner in types
        .display_names
        .keys()
        .copied()
        .filter(|owner| matches!(owner.kind(), SymbolKind::Const | SymbolKind::Function) || actors.contains(owner))
    {
        let mut policy = ActorValuePolicy::new(program, types, owner)?;
        match program.declaration(owner) {
            ResolvedDeclaration::Const(_) => {
                let mut expression = program.const_expression(owner)?;
                if matches!(types.constants.get(&owner).and_then(ActorValueCategory::declared), Some(ActorValueCategory::Handle)) {
                    policy.check_handle(&expression, Some(program.declaration(owner).name()));
                }
                policy.visit_expr(&mut expression);
            }
            ResolvedDeclaration::Function(_) => {
                policy.result =
                    types.callables[&CallableId { owner, member: None }].result.as_ref().and_then(ActorValueCategory::declared);
                for statement in program.function_body(owner, None)? {
                    policy.visit_statement(&mut statement.clone());
                }
            }
            ResolvedDeclaration::Actor(actor) => {
                for (index, _) in actor.functions.iter().enumerate() {
                    policy.result = types.callables[&CallableId { owner, member: Some(index) }]
                        .result
                        .as_ref()
                        .and_then(ActorValueCategory::declared);
                    for statement in program.function_body(owner, Some(index))? {
                        policy.visit_statement(&mut statement.clone());
                    }
                }
                policy.result = None;
                for (index, _) in actor.entries.iter().enumerate() {
                    for statement in program.entry_body(EntryId { actor: owner, index })? {
                        statement.visit_with(&mut policy);
                    }
                }
            }
            _ => {}
        }
        if let Some(error) = policy.error {
            return Err(error);
        }
    }
    Ok(())
}

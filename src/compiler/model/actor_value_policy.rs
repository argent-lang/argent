//! Checks source-level actor handle transfers before Sil erases their nominal types.

use silverscript_lang::ast::visit::{AstVisitorMut, walk_expr_mut, walk_statement_mut};
use silverscript_lang::ast::{Expr, ExprKind, Span, Statement};

use crate::compiler::resolve::{Binding, ResolvedDeclaration, ResolvedModules, ResolvedName};
use crate::compiler::syntax::node::{DeclId, EntryId, ReferenceRole, SymbolKind};
use crate::compiler::syntax::source::Origin;
use crate::error::{ArgentError, Result};

use super::ConstResolver;
use super::source_values::{SourceValueResolver, SourceValueType};
use super::types::{CallableId, ResolvedTypeBase, TypeTable};

#[cfg(test)]
mod tests;

struct ActorValuePolicy<'a, 'src> {
    values: SourceValueResolver<'a, 'src>,
    result: Option<SourceValueType>,
    error: Option<ArgentError>,
}

impl<'a, 'src> ActorValuePolicy<'a, 'src> {
    fn new(program: &'a ResolvedModules<'src>, types: &'a TypeTable, owner: DeclId, constants: &ConstResolver) -> Result<Self> {
        for (site, binding) in &program.bindings(owner).sites {
            let node = program.nodes().node(*site);
            if node.reference_role == Some(ReferenceRole::Value)
                && let Binding::Source(ResolvedName::Declaration(actor)) = binding
                && actor.kind() == SymbolKind::Actor
                && let Origin::Authored { start, .. } = node.origin
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
        Ok(Self { values: SourceValueResolver::new(program, types, owner, constants), result: None, error: None })
    }

    fn reject(&mut self, span: Span<'_>, message: String) {
        let program = self.values.program;
        let owner = self.values.owner;
        self.error.get_or_insert_with(|| {
            ArgentError::at_source(program.declaration_path(owner), program.source_text(owner.module), span.start(), message)
        });
    }

    fn check_known(&mut self, expected: &SourceValueType, actual: &SourceValueType, span: Span<'_>, declaration: Option<&str>) {
        if matches!(expected.base, ResolvedTypeBase::ActorHandle(_)) && matches!(actual.base, ResolvedTypeBase::ActorEnum(_)) {
            let enumeration = self.values.display(actual);
            let suggestion = declaration.map_or_else(
                || format!("use the explicit actor-enum type `{enumeration}` for in-app selection"),
                |name| format!("declare `{enumeration} {name} = {};` instead", span.as_str()),
            );
            self.reject(span, format!("actor_type<State> is an external actor handle, not an actor-enum selector; {suggestion}"));
            return;
        }
        let incompatible = match (&expected.base, &actual.base) {
            (ResolvedTypeBase::ActorHandle(expected_state), ResolvedTypeBase::ActorHandle(actual_state)) => {
                (*expected_state != *actual_state && actual.literal_base != Some(*expected_state))
                    || expected.has_proven_shape_mismatch(actual)
            }
            (ResolvedTypeBase::ActorHandle(_), ResolvedTypeBase::State(_))
            | (ResolvedTypeBase::State(_) | ResolvedTypeBase::ActorEnum(_), ResolvedTypeBase::ActorHandle(_)) => true,
            _ => false,
        };
        if incompatible {
            let expected = self.values.display(expected);
            let actual = self.values.display(actual);
            let message = declaration.map_or_else(
                || format!("cannot use `{actual}` as `{expected}`"),
                |name| format!("declares actor handle `{name}` as {expected}, but its initializer has {actual}"),
            );
            self.reject(span, message);
        }
    }

    fn check_value(&mut self, expected: &SourceValueType, expression: &Expr<'_>, declaration: Option<&str>) {
        if let Some(actual) = self.values.expression(expression) {
            self.check_known(expected, &actual, expression.span, declaration);
        }
        match &expression.kind {
            ExprKind::Array { values, .. } => {
                if let Some(element) = expected.element() {
                    for value in values {
                        self.check_value(&element, value, None);
                    }
                }
            }
            ExprKind::StructLiteral { fields, .. } if expected.dimensions.is_empty() => {
                for field in fields {
                    if let Some(ty) = self.values.field(expected, &field.name) {
                        self.check_value(&ty, &field.expr, None);
                    }
                }
            }
            _ => {}
        }
    }

    fn check_arguments(&mut self, name: Span<'_>, args: &[Expr<'_>]) {
        let expected =
            self.values.callable(name).map(|signature| signature.params.iter().map(SourceValueType::declared).collect::<Vec<_>>());
        if let Some(expected) = expected {
            for (expected, actual) in expected.iter().zip(args) {
                self.check_value(expected, actual, None);
            }
        }
    }

    fn check_destructure(&mut self, source: Option<SourceValueType>, bindings: &[silverscript_lang::ast::StructBindingAst<'_>]) {
        let Some(source) = source else { return };
        for binding in bindings {
            if let Some(expected) = self.values.type_at(binding.type_span)
                && let Some(actual) = self.values.field(&source, &binding.field_name)
            {
                self.check_known(&expected, &actual, binding.span, Some(&binding.name));
            }
        }
    }
}

impl<'src> AstVisitorMut<'src> for ActorValuePolicy<'_, '_> {
    fn visit_statement(&mut self, statement: &mut Statement<'src>) {
        match statement {
            Statement::VariableDefinition { name, type_span, expr: Some(expression), .. } => {
                if let Some(expected) = self.values.type_at(*type_span) {
                    self.check_value(&expected, expression, Some(name));
                }
            }
            Statement::Assign { name, name_span, expr, .. } => {
                if let Some(expected) = self.values.named_value(*name_span, name) {
                    self.check_value(&expected, expr, None);
                }
            }
            Statement::Return { exprs, .. } => {
                if let Some(expected) = self.result.clone() {
                    for expr in exprs {
                        self.check_value(&expected, expr, None);
                    }
                }
            }
            Statement::FunctionCall { name_span, args, .. } => self.check_arguments(*name_span, args),
            Statement::FunctionCallAssign { bindings, name_span, args, span, .. } => {
                self.check_arguments(*name_span, args);
                if let [binding] = bindings.as_slice()
                    && let Some(expected) = self.values.type_at(binding.type_span)
                    && let Some(actual) =
                        self.values.callable(*name_span).and_then(|signature| signature.result.as_ref()).map(SourceValueType::declared)
                {
                    self.check_known(&expected, &actual, *span, Some(&binding.name));
                }
            }
            Statement::StructDestructure { bindings, expr, struct_name_span, .. } => {
                self.check_destructure(self.values.type_at(*struct_name_span), bindings);
                self.check_destructure(self.values.expression(expr), bindings);
            }
            Statement::StateFunctionCallAssign { bindings, name_span, args, target_struct_span, .. } => {
                self.check_arguments(*name_span, args);
                self.check_destructure(self.values.type_at(*target_struct_span), bindings);
                let source =
                    self.values.callable(*name_span).and_then(|signature| signature.result.as_ref()).map(SourceValueType::declared);
                self.check_destructure(source, bindings);
            }
            Statement::TupleAssignment { left_type_span, left_name, right_type_span, right_name, expr, .. } => {
                if matches!(expr.kind, ExprKind::Split { .. })
                    && let Some(part) = self.values.expression(expr)
                {
                    for (span, name) in [(*left_type_span, left_name), (*right_type_span, right_name)] {
                        if let Some(expected) = self.values.type_at(span) {
                            self.check_known(&expected, &part, expr.span, Some(name));
                        }
                    }
                }
            }
            _ => {}
        }
        walk_statement_mut(self, statement);
    }

    fn visit_expr(&mut self, expression: &mut Expr<'src>) {
        match &expression.kind {
            ExprKind::Call { name_span, args, .. } => self.check_arguments(*name_span, args),
            ExprKind::Append { source, args, .. } => {
                if let Some(expected) = self.values.expression(source).and_then(|ty| ty.element()) {
                    for argument in args {
                        self.check_value(&expected, argument, None);
                    }
                }
            }
            ExprKind::StructLiteral { name_span, .. } => {
                if let Some(expected) = self.values.type_at(*name_span) {
                    self.check_value(&expected, expression, None);
                }
            }
            _ => {}
        }
        walk_expr_mut(self, expression);
    }
}

pub(super) fn validate_actor_value_policy(program: &ResolvedModules<'_>, types: &TypeTable, actors: &[DeclId]) -> Result<()> {
    let constants = ConstResolver::from_resolved(program, &types.display_names);
    for owner in types
        .display_names
        .keys()
        .copied()
        .filter(|owner| matches!(owner.kind(), SymbolKind::Const | SymbolKind::Function) || actors.contains(owner))
    {
        let mut policy = ActorValuePolicy::new(program, types, owner, &constants)?;
        match program.declaration(owner) {
            ResolvedDeclaration::Const(_) => {
                let mut expression = program.const_expression(owner)?;
                policy.check_value(
                    &SourceValueType::declared(&types.constants[&owner]),
                    &expression,
                    Some(program.declaration(owner).name()),
                );
                policy.visit_expr(&mut expression);
            }
            ResolvedDeclaration::Function(_) => {
                policy.result = types.callables[&CallableId { owner, member: None }].result.as_ref().map(SourceValueType::declared);
                for statement in program.function_body(owner, None)? {
                    policy.visit_statement(&mut statement.clone());
                }
            }
            ResolvedDeclaration::Actor(actor) => {
                for (index, _) in actor.functions.iter().enumerate() {
                    policy.result =
                        types.callables[&CallableId { owner, member: Some(index) }].result.as_ref().map(SourceValueType::declared);
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

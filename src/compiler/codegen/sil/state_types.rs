//! Checked AST-directed lowering of contract-local authored state types.
//!
//! Some Sil AST nodes classify a type name without exposing its exact span.
//! For those nodes, `EquivalentStateLowerer` uses the grammar guarantee that
//! the name starts the node span. Checked edits, reparsing, and the final audit
//! make this workaround fail closed if that guarantee changes.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

use silverscript_lang::ast::visit::{AstVisitorMut, walk_expr_mut, walk_function_mut, walk_param_mut, walk_statement_mut};
use silverscript_lang::ast::{
    Expr, ExprKind, FunctionAst, ParamAst, Statement, TypeBase, TypeRef, parse_contract_ast, parse_expression_ast, parse_function_ast,
};

use crate::error::{ArgentError, Result};

use super::state_values::ContractStateValuePlan;

#[derive(Clone, Debug, Eq, PartialEq)]
struct SourceEdit {
    span: Range<usize>,
    expected: String,
    replacement: String,
}

struct EquivalentStateLowerer {
    replacements: BTreeMap<String, String>,
    edits: Vec<SourceEdit>,
}

impl EquivalentStateLowerer {
    fn new(state_values: &ContractStateValuePlan) -> Self {
        let replacements =
            state_values.equivalent_state_sources().map(|source| (source.as_str().to_string(), "State".to_string())).collect();
        Self { replacements, edits: Vec::new() }
    }

    fn restricted(state_values: &ContractStateValuePlan, sources: &BTreeSet<String>) -> Self {
        let replacements = state_values
            .equivalent_state_sources()
            .filter(|source| sources.contains(source.as_str()))
            .map(|source| (source.as_str().to_string(), "State".to_string()))
            .collect();
        Self { replacements, edits: Vec::new() }
    }

    fn push_type(&mut self, ty: &TypeRef, start: usize) {
        let TypeBase::Custom(name) = &ty.base else {
            return;
        };
        self.push_name(name, start);
    }

    fn push_name(&mut self, name: &str, start: usize) {
        let Some(replacement) = self.replacements.get(name) else {
            return;
        };
        self.edits.push(SourceEdit { span: start..start + name.len(), expected: name.to_string(), replacement: replacement.clone() });
    }
}

impl<'i> AstVisitorMut<'i> for EquivalentStateLowerer {
    fn visit_function(&mut self, function: &mut FunctionAst<'i>) {
        for (ty, span) in function.return_types.iter().zip(&function.return_type_spans) {
            self.push_type(ty, span.start());
        }
        walk_function_mut(self, function);
    }

    fn visit_param(&mut self, param: &mut ParamAst<'i>) {
        self.push_type(&param.type_ref, param.type_span.start());
        walk_param_mut(self, param);
    }

    fn visit_statement(&mut self, statement: &mut Statement<'i>) {
        match statement {
            Statement::VariableDefinition { type_ref, type_span, .. } => self.push_type(type_ref, type_span.start()),
            Statement::TupleAssignment { left_type_ref, left_type_span, right_type_ref, right_type_span, .. } => {
                self.push_type(left_type_ref, left_type_span.start());
                self.push_type(right_type_ref, right_type_span.start());
            }
            Statement::FunctionCallAssign { bindings, .. } => {
                for binding in bindings {
                    self.push_type(&binding.type_ref, binding.type_span.start());
                }
            }
            Statement::StateFunctionCallAssign { target_struct, bindings, span, .. } => {
                // Sil classifies the target struct and its grammar places that
                // name at the beginning of this statement.
                self.push_name(target_struct, span.start());
                for binding in bindings {
                    self.push_type(&binding.type_ref, binding.type_span.start());
                }
            }
            Statement::StructDestructure { struct_name, bindings, span, .. } => {
                // Sil classifies the struct owner and its grammar places that
                // name at the beginning of this statement.
                self.push_name(struct_name, span.start());
                for binding in bindings {
                    self.push_type(&binding.type_ref, binding.type_span.start());
                }
            }
            Statement::FunctionCall { .. }
            | Statement::Assign { .. }
            | Statement::RequireAgeDaa { .. }
            | Statement::RequireTxDaa { .. }
            | Statement::RequireTxTime { .. }
            | Statement::Require { .. }
            | Statement::Block { .. }
            | Statement::If { .. }
            | Statement::For { .. }
            | Statement::Return { .. }
            | Statement::Console { .. } => {}
        }
        walk_statement_mut(self, statement);
    }

    fn visit_expr(&mut self, expr: &mut Expr<'i>) {
        match &expr.kind {
            ExprKind::Array { type_ref, .. } => {
                // Sil classifies the array type and its grammar places that
                // type at the beginning of the expression.
                self.push_type(type_ref, expr.span.start());
            }
            ExprKind::StructLiteral { name, name_span, .. } => self.push_name(name, name_span.start()),
            _ => {}
        }
        walk_expr_mut(self, expr);
    }
}

fn apply_checked_edits(source: &str, mut edits: Vec<SourceEdit>) -> Result<String> {
    edits.sort_by(|left, right| {
        left.span
            .start
            .cmp(&right.span.start)
            .then_with(|| left.span.end.cmp(&right.span.end))
            .then_with(|| left.expected.cmp(&right.expected))
            .then_with(|| left.replacement.cmp(&right.replacement))
    });
    edits.dedup();

    for edit in &edits {
        let actual = source.get(edit.span.clone()).ok_or_else(|| {
            ArgentError::new(format!(
                "internal equivalent-State edit for `{}` points outside parsed Sil source at {:?}",
                edit.expected, edit.span
            ))
        })?;
        if actual != edit.expected {
            return Err(ArgentError::new(format!(
                "internal equivalent-State edit expected `{}` at {:?}, found `{actual}`",
                edit.expected, edit.span
            )));
        }
    }
    for pair in edits.windows(2) {
        if pair[0].span.end > pair[1].span.start {
            return Err(ArgentError::new(format!(
                "internal equivalent-State edits overlap at {:?} and {:?}",
                pair[0].span, pair[1].span
            )));
        }
    }

    let mut lowered = source.to_string();
    for edit in edits.into_iter().rev() {
        lowered.replace_range(edit.span, &edit.replacement);
    }
    Ok(lowered)
}

fn lower_function_source(source: &str, state_values: &ContractStateValuePlan) -> Result<String> {
    let mut function = parse_function_ast(source)
        .map_err(|err| ArgentError::new(format!("cannot classify Sil function for equivalent-State lowering: {err}")))?;
    let mut lowerer = EquivalentStateLowerer::new(state_values);
    lowerer.visit_function(&mut function);
    let lowered = apply_checked_edits(source, lowerer.edits)?;
    let mut reparsed = parse_function_ast(&lowered)
        .map_err(|err| ArgentError::new(format!("equivalent-State lowering produced an invalid Sil function: {err}")))?;
    let mut audit = EquivalentStateLowerer::new(state_values);
    audit.visit_function(&mut reparsed);
    if let Some(edit) = audit.edits.first() {
        return Err(ArgentError::new(format!(
            "equivalent-State lowering left authored type `{}` in a Sil function at {:?}",
            edit.expected, edit.span
        )));
    }
    Ok(lowered)
}

pub(in crate::compiler::codegen) fn lower_function_body_state_types(
    name: &str,
    params: &[String],
    return_ty: Option<&str>,
    body: &str,
    state_values: &ContractStateValuePlan,
) -> Result<String> {
    let return_type = return_ty.map(|ty| format!(" : {ty}")).unwrap_or_default();
    let prefix = format!("function {name}({}){return_type} {{", params.join(", "));
    let source = format!("{prefix}{body}}}");
    let lowered = lower_function_source(&source, state_values)?;
    lowered
        .strip_prefix(&prefix)
        .and_then(|body| body.strip_suffix('}'))
        .map(str::to_string)
        .ok_or_else(|| ArgentError::new("equivalent-State function lowering changed generated wrapper text"))
}

pub(in crate::compiler::codegen) fn lower_expression_state_types(
    source: &str,
    state_values: &ContractStateValuePlan,
) -> Result<String> {
    let mut expr = parse_expression_ast(source)
        .map_err(|err| ArgentError::new(format!("cannot classify Sil expression for equivalent-State lowering: {err}")))?;
    let mut lowerer = EquivalentStateLowerer::new(state_values);
    lowerer.visit_expr(&mut expr);
    let lowered = apply_checked_edits(source, lowerer.edits)?;
    parse_expression_ast(&lowered)
        .map_err(|err| ArgentError::new(format!("equivalent-State lowering produced an invalid Sil expression: {err}")))?;
    Ok(lowered)
}

pub(in crate::compiler::codegen) fn lower_statement_state_types(
    statement: &str,
    state_values: &ContractStateValuePlan,
) -> Result<String> {
    let prefix = "function gen__equivalent_state_statement() { ";
    let suffix = "; }";
    let source = format!("{prefix}{statement}{suffix}");
    let lowered = lower_function_source(&source, state_values)?;
    lowered
        .strip_prefix(prefix)
        .and_then(|statement| statement.strip_suffix(suffix))
        .map(str::to_string)
        .ok_or_else(|| ArgentError::new("equivalent-State statement lowering changed generated wrapper text"))
}

pub(in crate::compiler::codegen) fn audit_omitted_equivalent_state_structs(
    source: &str,
    omitted: &BTreeSet<String>,
    state_values: &ContractStateValuePlan,
) -> Result<()> {
    if omitted.is_empty() {
        return Ok(());
    }
    let mut contract = parse_contract_ast(source)
        .map_err(|err| ArgentError::new(format!("cannot audit generated Sil for equivalent-State lowering: {err}")))?;
    if let Some(state) = contract.structs.iter().find(|item| omitted.contains(&item.name)) {
        return Err(ArgentError::new(format!(
            "optimized authored struct `{}` was declared after being omitted from generated Sil",
            state.name
        )));
    }

    let mut audit = EquivalentStateLowerer::restricted(state_values, omitted);
    for state in &contract.structs {
        for field in &state.fields {
            audit.push_type(&field.type_ref, field.type_span.start());
        }
    }
    for field in &contract.fields {
        audit.push_type(&field.type_ref, field.type_span.start());
    }
    for constant in &contract.constants {
        audit.push_type(&constant.type_ref, constant.type_span.start());
    }
    audit.visit_contract(&mut contract);
    if let Some(edit) = audit.edits.first() {
        return Err(ArgentError::new(format!(
            "omitted authored state `{}` remains in a generated Sil type or constructor at {:?}",
            edit.expected, edit.span
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::model::SourceStateId;

    fn state_values() -> ContractStateValuePlan {
        ContractStateValuePlan::with_authored_sil_types(
            [(SourceStateId::new("CounterState"), "State".to_string())].into_iter().collect(),
        )
    }

    #[test]
    fn rewrites_only_ast_classified_state_type_and_constructor_sites() {
        let source = r#"function inspect(CounterState value) : CounterState {
    // CounterState remains in comments.
    string note = "CounterState remains in strings";
    int CounterState = 1;
    CounterState[2] numeric = CounterState[2]{ value, value };
    CounterState[_] inferred = CounterState[_]{ value, value };
    CounterState[COUNT] fixed = CounterState[COUNT]{ value };
    CounterState[] dynamic = CounterState[]{ CounterState { count: value.count } };
    CounterStateSuffix suffix = CounterStateSuffix { count: value.count };
    CounterState { count: int copied } = dynamic[0];
    CounterState { count: int from_call } = identity(value);
    return CounterState { count: copied + from_call + suffix.count + CounterState };
}"#;
        let lowered = lower_function_source(source, &state_values()).expect("classified state sites lower");

        assert!(lowered.contains("function inspect(State value) : State"), "{lowered}");
        assert!(lowered.contains("State[2] numeric = State[2]{ value, value };"), "{lowered}");
        assert!(lowered.contains("State[_] inferred = State[_]{ value, value };"), "{lowered}");
        assert!(lowered.contains("State[COUNT] fixed = State[COUNT]{ value };"), "{lowered}");
        assert!(lowered.contains("State[] dynamic = State[]{ State {"), "{lowered}");
        assert!(lowered.contains("CounterStateSuffix suffix = CounterStateSuffix {"), "{lowered}");
        assert!(lowered.contains("State { count: int copied } = dynamic[0];"), "{lowered}");
        assert!(lowered.contains("State { count: int from_call } = identity(value);"), "{lowered}");
        assert!(lowered.contains("// CounterState remains in comments."), "{lowered}");
        assert!(lowered.contains("\"CounterState remains in strings\""), "{lowered}");
        assert!(lowered.contains("int CounterState = 1;"), "{lowered}");
        assert!(lowered.contains("suffix.count + CounterState"), "{lowered}");
    }

    #[test]
    fn checked_edits_fail_closed_on_an_unexpected_slice() {
        let err = apply_checked_edits(
            "OtherState value",
            vec![SourceEdit { span: 0..12, expected: "CounterState".to_string(), replacement: "State".to_string() }],
        )
        .expect_err("mismatched edit must fail");
        assert!(err.to_string().contains("expected `CounterState`"), "unexpected error: {err}");
    }

    #[test]
    fn omitted_name_audit_includes_struct_field_types() {
        let source = r#"contract Inspect() {
    struct Wrapper {
        CounterState value;
    }
}"#;
        let omitted = ["CounterState".to_string()].into_iter().collect();
        let err = audit_omitted_equivalent_state_structs(source, &omitted, &state_values())
            .expect_err("an omitted authored name cannot remain in a struct field type");
        assert!(err.to_string().contains("omitted authored state `CounterState` remains"), "unexpected error: {err}");
    }
}

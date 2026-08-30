//! Lowers structured Argent entry bodies into generated Sil source.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

use crate::compiler::model::{
    ActorTarget, CovenantGroup, EntryInteraction, InteractionLocation, InteractionSource, Model, ResolvedRoute, ResolvedSuccessor,
    RouteFamily, SourceStateId, StaticActorTarget, TemplateSelector, actor_enum_variant_const_expr, clause_actor_type_ref,
    observed_is_dynamic_binding, observed_open_bindings, observed_open_state_for_decl, parse_actor_enum_selector,
    parse_actor_enum_variant, spawn_target_state,
};
use crate::compiler::naming::{is_identifier, to_snake};
use crate::compiler::syntax::body::{
    EntryBinding, EntryLocalDecl, EntryRoute, EntryStatement, EntryStructDestructure, EntrySuccessor, RouteArity,
};
use crate::compiler::syntax::lexer::{RESERVED_GENERATED_PREFIX, Span, Token, TokenKind, lex, parse_int_literal};
use crate::compiler::syntax::word;
use crate::compiler::syntax::*;
use crate::error::{ArgentError, Result};
use silverscript_lang::ast::visit::{AstVisitorMut, visit_function_mut, walk_expr_mut};
use silverscript_lang::ast::{
    Expr as SilExpr, ExprKind as SilExprKind, Statement as SilStatement, parse_expression_ast, parse_function_ast, parse_statement_ast,
};

// Body lowering uses the surrounding Sil emitter's shared witness plans,
// layout helpers, and generated names.
use super::super::emitter::*;
use super::state_boundary::{
    AuthoredStateExpr, EntryInputReferencePlan, EntryInputReferenceView, EntryInputScopeId, OutputStateTarget,
    OutputValidationContext, PlannedEntryInputReference, authored_state_payload_digest_expr, materialize_output_state,
    plan_actor_output_state, plan_open_output_state, plan_output_validation, plan_selector_output_state,
    plan_static_actor_output_state, preserve_exact_self,
};
use super::state_types::{lower_expression_state_types, lower_statement_state_types};
use super::state_values::{ContractStateValuePlan, PlannedStateValue, StateValueShape};
use super::token_refs::{RefReplacements, count_qualified_ref};

#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "body/range_tests.rs"]
mod range_tests;

pub(in crate::compiler::codegen) struct LoweredEntryBody {
    pub(in crate::compiler::codegen) sil: String,
    pub(in crate::compiler::codegen) digest_helpers: BTreeSet<SourceStateId>,
}

pub(in crate::compiler::codegen) fn lower_entry_body(
    actor: &ActorDecl,
    entry: &EntryDecl,
    model: &Model<'_>,
    input_references: &EntryInputReferencePlan,
    state_values: &ContractStateValuePlan,
) -> Result<LoweredEntryBody> {
    BodyLowerer::new(actor, entry, model, EntryInputReferenceView::Complete(input_references), state_values)?.lower()
}

/// Source details needed while lowering one non-current covenant output.
#[derive(Clone, Copy)]
enum CovenantOutputContext<'a> {
    Existing { observe: &'a ObserveDecl, output: &'a ObservedActorDecl },
    Genesis { spawn: &'a SpawnDecl, output: &'a SpawnOutputDecl },
}

impl<'a> CovenantOutputContext<'a> {
    fn group_name(self) -> &'a str {
        match self {
            Self::Existing { observe, .. } => &observe.name,
            Self::Genesis { spawn, .. } => &spawn.name,
        }
    }

    fn route_label(self) -> &'static str {
        match self {
            Self::Existing { .. } => "observed",
            Self::Genesis { .. } => "spawned",
        }
    }

    fn output_name(self) -> &'a str {
        match self {
            Self::Existing { output, .. } => &output.name,
            Self::Genesis { output, .. } => &output.name,
        }
    }

    fn actor(self) -> &'a str {
        match self {
            Self::Existing { output, .. } => &output.actor,
            Self::Genesis { output, .. } => &output.actor,
        }
    }
}

struct BodyLowerer<'a, 'm, 'p> {
    actor: &'a ActorDecl,
    entry: &'a EntryDecl,
    model: &'m Model<'a>,
    input_references: EntryInputReferenceView<'p>,
    active_reference: PlannedEntryInputReference,
    state_values: &'p ContractStateValuePlan,
    bindings: BodyBindings,
    reserved_entry_names: ReservedEntryNames,
    /// Entry-wide candidates; the current binding decides selector visibility.
    selector_catalog: BTreeMap<String, TemplateSelector>,
    output_values: Vec<OutputValueRef>,
    fixed_ref_replacements: Vec<(String, String)>,
    observed_output_fields: Vec<ObservedOutputFieldWitnessSpec>,
    validated_spawns: BTreeSet<String>,
    // Expression lowering records contract-level helpers without making the
    // otherwise read-only lowering API mutable.
    digest_helpers: RefCell<BTreeSet<SourceStateId>>,
    conditional_depth: usize,
    current_statement: Option<Span>,
}

#[derive(Clone)]
struct ConstructedRoute {
    output: String,
    actor: String,
    state: String,
    arity: RouteArity,
}

struct PlannedStateValueSite {
    span: Range<usize>,
    expected: PlannedStateValue,
}

struct StateValueSiteCollector<'a> {
    state_values: &'a ContractStateValuePlan,
    bindings: &'a BodyBindings,
    sites: Vec<PlannedStateValueSite>,
}

struct NamedCallSiteCollector<'a> {
    name: &'a str,
    sites: Vec<Range<usize>>,
}

#[derive(Default)]
struct PhysicalStateConstructorDetector {
    found: bool,
}

impl<'i> AstVisitorMut<'i> for PhysicalStateConstructorDetector {
    fn visit_expr(&mut self, expr: &mut SilExpr<'i>) {
        if matches!(&expr.kind, SilExprKind::StructLiteral { name, .. } if name == "State") {
            self.found = true;
        }
        walk_expr_mut(self, expr);
    }
}

impl<'i> AstVisitorMut<'i> for StateValueSiteCollector<'_> {
    fn visit_expr(&mut self, expr: &mut SilExpr<'i>) {
        match &expr.kind {
            SilExprKind::Call { name, args, .. } => {
                if let Some(signature) = self.state_values.signature(name) {
                    for (index, arg) in args.iter().enumerate() {
                        if let Some(expected) = signature.param(index) {
                            self.sites
                                .push(PlannedStateValueSite { span: arg.span.start()..arg.span.end(), expected: expected.clone() });
                        }
                    }
                }
            }
            SilExprKind::Array { type_ref, values, .. } => {
                if let Some(element) =
                    self.state_values.plan_ast_type_ref(type_ref, Some(values.len())).and_then(|value| value.element())
                {
                    self.sites.extend(
                        values.iter().map(|value| PlannedStateValueSite {
                            span: value.span.start()..value.span.end(),
                            expected: element.clone(),
                        }),
                    );
                }
            }
            SilExprKind::Append { source, args, .. } => {
                if let Some(element) =
                    planned_state_value_for_expr(source, self.state_values, self.bindings).and_then(|value| value.element())
                {
                    self.sites.extend(
                        args.iter()
                            .map(|arg| PlannedStateValueSite { span: arg.span.start()..arg.span.end(), expected: element.clone() }),
                    );
                }
            }
            _ => {}
        }
        walk_expr_mut(self, expr);
    }
}

impl<'i> AstVisitorMut<'i> for NamedCallSiteCollector<'_> {
    fn visit_expr(&mut self, expr: &mut SilExpr<'i>) {
        if matches!(&expr.kind, SilExprKind::Call { name, .. } if name == self.name) {
            self.sites.push(expr.span.start()..expr.span.end());
        }
        walk_expr_mut(self, expr);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct BodyBindingId(usize);

/// Type and lowering facts attached to one lexically resolved value.
struct BodyBinding {
    source_type: Option<String>,
    lowered_type: Option<String>,
    state_value: Option<PlannedStateValue>,
    selector: Option<TemplateSelector>,
    input_scope: Option<EntryInputScopeId>,
    /// Projection through the authenticated active input for this root field.
    active_field_projection: Option<ActiveFieldProjection>,
    input_value: Option<InputValueLocation>,
}

struct ActiveFieldProjection {
    field: String,
    /// Whole-value projection reconstructs an authored expansion value.
    expanded: bool,
}

impl BodyBinding {
    fn typed(source_type: impl Into<String>, lowered_type: impl Into<String>, state_values: &ContractStateValuePlan) -> Self {
        let source_type = source_type.into();
        let state_value = state_values.plan_source_sil_type(&source_type);
        Self {
            source_type: Some(source_type),
            lowered_type: Some(lowered_type.into()),
            state_value,
            selector: None,
            input_scope: None,
            active_field_projection: None,
            input_value: None,
        }
    }

    fn source_typed(source_type: impl Into<String>, state_values: &ContractStateValuePlan) -> Self {
        let source_type = source_type.into();
        let state_value = state_values.plan_source_sil_type(&source_type);
        Self {
            source_type: Some(source_type),
            lowered_type: None,
            state_value,
            selector: None,
            input_scope: None,
            active_field_projection: None,
            input_value: None,
        }
    }

    fn lowered_typed(lowered_type: impl Into<String>) -> Self {
        Self {
            source_type: None,
            lowered_type: Some(lowered_type.into()),
            state_value: None,
            selector: None,
            input_scope: None,
            active_field_projection: None,
            input_value: None,
        }
    }

    fn input_root(scope: EntryInputScopeId) -> Self {
        Self {
            source_type: None,
            lowered_type: None,
            state_value: None,
            selector: None,
            input_scope: Some(scope),
            active_field_projection: None,
            input_value: None,
        }
    }

    fn with_selector(mut self, selector: TemplateSelector) -> Self {
        self.selector = Some(selector);
        self
    }

    fn with_planned_state_value(mut self, state_value: Option<PlannedStateValue>) -> Self {
        self.state_value = state_value;
        self
    }

    fn with_active_field_projection(mut self, field: impl Into<String>, expanded: bool) -> Self {
        self.active_field_projection = Some(ActiveFieldProjection { field: field.into(), expanded });
        self
    }

    fn with_input_value(mut self, input_value: InputValueLocation) -> Self {
        self.input_value = Some(input_value);
        self
    }

    fn with_input_scope(mut self, input_scope: EntryInputScopeId) -> Self {
        self.input_scope = Some(input_scope);
        self
    }
}

/// Generated transaction positions and resolved bounds for one ranged handle.
#[derive(Clone, Debug)]
struct RangeValueLocation {
    first_index: usize,
    count: String,
    minimum: i64,
    maximum: i64,
}

/// Locates a source input's transaction value from its generated index data.
#[derive(Clone, Debug)]
enum InputValueLocation {
    Singleton { input_index: String },
    Range { covenant_id: String, range: RangeValueLocation },
}

struct ScopedBodyBinding {
    id: BodyBindingId,
    value: BodyBinding,
}

#[derive(Default)]
struct BodyScope {
    bindings: BTreeMap<String, ScopedBodyBinding>,
    output_ranges: BTreeMap<String, RangeValueLocation>,
    /// Generated selector locals available in this emitted Sil scope.
    materialized_selectors: BTreeSet<BodyBindingId>,
}

/// Resolves body values through the same lexical scopes emitted to Sil.
struct BodyBindings {
    scopes: Vec<BodyScope>,
    next_id: usize,
}

impl BodyBindings {
    fn new() -> Self {
        Self { scopes: vec![BodyScope::default()], next_id: 0 }
    }

    fn enter_scope(&mut self) {
        self.scopes.push(BodyScope::default());
    }

    fn exit_scope(&mut self) {
        assert!(self.scopes.len() > 1, "cannot exit the entry body root scope");
        self.scopes.pop();
    }

    fn declare(&mut self, name: impl Into<String>, binding: BodyBinding) -> BodyBindingId {
        let id = BodyBindingId(self.next_id);
        self.next_id += 1;
        let name = name.into();
        let scope = self.scopes.last_mut().expect("body bindings retain a root scope");
        scope.output_ranges.remove(&name);
        scope.bindings.insert(name, ScopedBodyBinding { id, value: binding });
        id
    }

    fn get(&self, name: &str) -> Option<&ScopedBodyBinding> {
        self.scopes.iter().rev().find_map(|scope| scope.bindings.get(name))
    }

    fn lowered_type(&self, name: &str) -> Option<&str> {
        self.get(name).and_then(|binding| binding.value.lowered_type.as_deref())
    }

    fn lowered_type_for_expr(&self, expr: &str) -> Option<String> {
        let expr = strip_outer_parentheses(expr);
        if let Some(ty) = self.lowered_type(expr) {
            return Some(ty.to_string());
        }
        let root = indexed_root_binding(expr)?;
        array_element_type(self.lowered_type(root)?).map(str::to_string)
    }

    fn source_type(&self, name: &str) -> Option<&str> {
        self.get(name).and_then(|binding| binding.value.source_type.as_deref())
    }

    fn state_value(&self, name: &str) -> Option<&PlannedStateValue> {
        self.get(name)?.value.state_value.as_ref()
    }

    fn active_field_projection(&self, name: &str) -> Option<&ActiveFieldProjection> {
        self.get(name)?.value.active_field_projection.as_ref()
    }

    fn input_reference_is_visible(&self, reference: &PlannedEntryInputReference) -> bool {
        reference.is_active()
            || self.get(reference.lexical_root()).and_then(|binding| binding.value.input_scope) == Some(reference.scope())
    }

    fn selector(&self, name: &str) -> Option<(BodyBindingId, &TemplateSelector)> {
        let binding = self.get(name)?;
        Some((binding.id, binding.value.selector.as_ref()?))
    }

    fn input_value(&self, name: &str) -> Option<&InputValueLocation> {
        self.get(name)?.value.input_value.as_ref()
    }

    fn output_range(&self, name: &str) -> Option<&RangeValueLocation> {
        for scope in self.scopes.iter().rev() {
            if let Some(output_range) = scope.output_ranges.get(name) {
                return Some(output_range);
            }
            if scope.bindings.contains_key(name) {
                return None;
            }
        }
        None
    }

    fn attach_output_range(&mut self, name: &str, output_range: RangeValueLocation) {
        self.scopes.first_mut().expect("body bindings retain a root scope").output_ranges.insert(name.to_string(), output_range);
    }

    fn selector_is_materialized(&self, id: BodyBindingId) -> bool {
        self.scopes.iter().rev().any(|scope| scope.materialized_selectors.contains(&id))
    }

    fn mark_selector_materialized(&mut self, id: BodyBindingId) {
        self.scopes.last_mut().expect("body bindings retain a root scope").materialized_selectors.insert(id);
    }
}

fn planned_state_value_for_expr(
    expr: &SilExpr<'_>,
    state_values: &ContractStateValuePlan,
    bindings: &BodyBindings,
) -> Option<PlannedStateValue> {
    match &expr.kind {
        SilExprKind::Identifier(name) => match bindings.get(name) {
            Some(binding) => binding.value.state_value.clone(),
            None => state_values.constant(name).cloned(),
        },
        SilExprKind::Call { name, .. } => state_values.signature(name)?.result().cloned(),
        SilExprKind::Array { type_ref, values, .. } => state_values.plan_ast_type_ref(type_ref, Some(values.len())),
        SilExprKind::Append { source, args, .. } => planned_state_value_for_expr(source, state_values, bindings)?.appended(args.len()),
        SilExprKind::ArrayIndex { source, .. } => planned_state_value_for_expr(source, state_values, bindings)?.element(),
        _ => None,
    }
}

#[derive(Debug)]
enum OutputValueRef {
    Singleton { source: String, output_index: String },
    Range { handle: String },
}

impl OutputValueRef {
    fn policy_ref(&self) -> String {
        match self {
            Self::Singleton { source, .. } => source.clone(),
            Self::Range { handle } => format!("{handle}[i].{}", word::VALUE),
        }
    }

    fn is_referenced(&self, tokens: &[Token]) -> bool {
        match self {
            Self::Singleton { source, .. } => count_qualified_ref(tokens, source) != 0,
            Self::Range { handle } => tokens.iter().enumerate().any(|(pos, _)| match_indexed_value_ref(tokens, pos, handle).is_some()),
        }
    }

    fn matches_exact(&self, input: &str) -> bool {
        match self {
            Self::Singleton { source, .. } => source == input,
            Self::Range { handle } => {
                let Ok(tokens) = lex(input) else {
                    return false;
                };
                match_indexed_value_ref(&tokens, 0, handle)
                    .is_some_and(|close| matches!(tokens.get(close + 3).map(|token| &token.kind), Some(TokenKind::Eof)))
            }
        }
    }
}

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
    fn role_for_body_binding(&self, name: &str) -> Option<&ReservedEntryNameRole> {
        self.body_bindings.get(name)
    }

    fn reserve_unique(&mut self, actor: &ActorDecl, entry: &EntryDecl, name: &str, role: ReservedEntryNameRole) -> Result<()> {
        if let Some(previous) = self.body_bindings.get(name) {
            return Err(entry_name_collision(actor, entry, name, &role, previous));
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
        Err(entry_name_collision(actor, entry, name, &role, previous))
    }
}

fn reserved_entry_names(actor: &ActorDecl, entry: &EntryDecl) -> Result<ReservedEntryNames> {
    let mut names = ReservedEntryNames::default();
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
            names.reserve_unique(actor, entry, binding, ReservedEntryNameRole::OpenActorBinding { observe: observe.name.clone() })?;
        }
    }
    for spawn in &entry.spawns {
        names.reserve_unique(actor, entry, &spawn.name, ReservedEntryNameRole::SpawnRoot)?;
        names.reserve_unique(actor, entry, &spawn.covenant, ReservedEntryNameRole::CovenantBinding { spawn: spawn.name.clone() })?;
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

fn entry_name_collision(
    actor: &ActorDecl,
    entry: &EntryDecl,
    name: &str,
    role: &ReservedEntryNameRole,
    previous: &ReservedEntryNameRole,
) -> ArgentError {
    ArgentError::new(format!(
        "entry `{}::{}` {} `{name}` collides with {} of the same name",
        actor.name,
        entry.name,
        role.description(),
        previous.description(),
    ))
}

/// Builds non-input dotted-reference lowering for one entry body.
fn entry_ref_replacements(output_values: &[OutputValueRef]) -> Vec<(String, String)> {
    output_values
        .iter()
        .filter_map(|output| match output {
            OutputValueRef::Singleton { source, output_index } => Some((source.clone(), format!("tx.outputs[{output_index}].value"))),
            OutputValueRef::Range { .. } => None,
        })
        .collect()
}

impl<'a, 'm, 'p> BodyLowerer<'a, 'm, 'p> {
    fn new(
        actor: &'a ActorDecl,
        entry: &'a EntryDecl,
        model: &'m Model<'a>,
        input_references: EntryInputReferenceView<'p>,
        state_values: &'p ContractStateValuePlan,
    ) -> Result<Self> {
        let selector_catalog = model.template_selectors_for_entry(actor, entry)?;
        let active_reference = input_references.active(actor, model, state_values)?;
        let reserved_entry_names = reserved_entry_names(actor, entry)?;
        let mut bindings = BodyBindings::new();
        let expanded_digest_fields = state_expansion_digest_fields_for_state(&actor.state, model);
        for field in &model.storage_state(&actor.state)?.fields {
            if expanded_digest_fields.contains(field.name.as_str()) {
                continue;
            }
            bindings.declare(
                field.name.clone(),
                BodyBinding::typed(source_type_ref(&field.ty), lower_type_ref(&field.ty, model), state_values)
                    .with_active_field_projection(&field.name, false),
            );
        }
        if let Some(expansion) = model.state(&actor.state)?.expansion.as_ref() {
            for digest in &expansion.digests {
                bindings.declare(
                    digest.field.clone(),
                    BodyBinding::source_typed(digest.state.clone(), state_values).with_active_field_projection(&digest.field, true),
                );
            }
        }
        for param in &entry.params {
            let ty = state_values.sil_type_for_type_ref(&param.ty).unwrap_or_else(|| lower_type_ref(&param.ty, model));
            let mut binding = BodyBinding::typed(source_type_ref(&param.ty), ty, state_values);
            if param.ty.array.is_none()
                && let Some(selector) = selector_catalog.get(&param.name)
                && selector.actor_enum == param.ty.name
            {
                binding = binding.with_selector(selector.clone());
            }
            bindings.declare(param.name.clone(), binding);
        }
        for observe in &entry.observes {
            for (binding, state) in observed_open_bindings(observe) {
                bindings.declare(
                    binding.to_string(),
                    BodyBinding::typed(format!("{}<{state}>", word::ACTOR_TYPE), "byte[32]", state_values),
                );
            }
        }
        for spawn in &entry.spawns {
            bindings.declare(spawn.covenant.clone(), BodyBinding::typed(word::COVENANT_ID, "byte[32]", state_values));
        }

        let entry_model = model.entry_model(actor, entry)?;
        for interaction in entry_model.current().inputs() {
            if let Some(input) = input_references.consumed(interaction.handle())? {
                let input_value = if interaction.cardinality().is_range() {
                    let InteractionLocation::Range { start, .. } = interaction.location() else {
                        unreachable!("ranged interaction retains a ranged location");
                    };
                    let Some((minimum, maximum)) = interaction.cardinality().range_bounds() else {
                        unreachable!("ranged interaction retains resolved bounds");
                    };
                    let slot_offset = match entry.kind {
                        EntryKind::Leader => 1,
                        EntryKind::Delegate => 0,
                    };
                    InputValueLocation::Range {
                        covenant_id: hidden_cov_id_name(),
                        range: RangeValueLocation {
                            first_index: slot_offset + start,
                            count: hidden_input_count_name(interaction.handle()),
                            minimum,
                            maximum,
                        },
                    }
                } else {
                    InputValueLocation::Singleton { input_index: hidden_input_idx_name(interaction.handle()) }
                };
                let target = interaction.target().single_static_actor().expect("current entry input has one fixed actor target");
                let source_type = model.actor(target)?.state.clone();
                if interaction.cardinality().is_range() {
                    bindings.declare(interaction.handle(), BodyBinding::input_root(input.scope()).with_input_value(input_value));
                } else {
                    bindings.declare(
                        interaction.handle(),
                        BodyBinding::typed(source_type, input.physical_type(), state_values)
                            .with_input_scope(input.scope())
                            .with_input_value(input_value),
                    );
                    bindings.declare(
                        hidden_consumed_input_state_name(interaction.handle()),
                        BodyBinding::lowered_typed(input.physical_type()),
                    );
                }
            }
        }
        for group in entry_model.existing_groups() {
            let observe = group.observe().expect("existing covenant group retains its observe clause");
            let mut observe_scope = None;
            for interaction in group.inputs() {
                let InteractionSource::ObserveInput(input) = interaction.source() else {
                    unreachable!("existing covenant inputs are observed inputs");
                };
                let lowered_ref = hidden_observed_input_state_name(&observe.name, &input.name);
                if let Some(input) = input_references.observed(&observe.name, &input.name)? {
                    if let Some(scope) = observe_scope {
                        debug_assert_eq!(scope, input.scope());
                    } else {
                        observe_scope = Some(input.scope());
                        bindings.declare(observe.name.clone(), BodyBinding::input_root(input.scope()));
                    }
                    let physical_type = if interaction.cardinality().is_range() {
                        format!("{}[]", input.physical_type())
                    } else {
                        input.physical_type().to_string()
                    };
                    bindings.declare(lowered_ref, BodyBinding::lowered_typed(physical_type));
                }
            }
        }

        let mut output_values = Vec::new();
        for interaction in entry_model.current().outputs() {
            if let Some((minimum, maximum)) = interaction.cardinality().range_bounds() {
                let InteractionLocation::Range { start, .. } = interaction.location() else {
                    unreachable!("ranged output retains a ranged location");
                };
                bindings.attach_output_range(
                    interaction.handle(),
                    RangeValueLocation { first_index: start, count: hidden_output_count_name(interaction.handle()), minimum, maximum },
                );
                // A statically empty range has no output value to dispose of.
                if maximum > 0 {
                    output_values.push(OutputValueRef::Range { handle: interaction.handle().to_string() });
                }
            } else {
                output_values.push(OutputValueRef::Singleton {
                    source: format!("{}.{}", interaction.handle(), word::VALUE),
                    output_index: hidden_output_idx_name(interaction.handle()),
                });
            }
        }
        // This entry creates spawned outputs, so it owns their value policy.
        // Observed outputs remain the responsibility of their emitting contracts.
        for spawn in &entry.spawns {
            output_values.extend(spawn.outputs.iter().map(|output| OutputValueRef::Singleton {
                source: format!("{}.{}.{}.{}", spawn.name, word::OUTPUTS, output.name, word::VALUE),
                output_index: hidden_spawn_output_idx_name(&spawn.name, &output.name),
            }));
        }
        // Preserve most-specific-first ordering in value-policy diagnostics.
        output_values.sort_by_key(|value| value.policy_ref());

        let fixed_ref_replacements = entry_ref_replacements(&output_values);
        let observed_output_fields = observed_output_field_witness_specs(actor, entry, model);

        Ok(Self {
            actor,
            entry,
            model,
            input_references,
            active_reference,
            state_values,
            bindings,
            reserved_entry_names,
            selector_catalog,
            output_values,
            fixed_ref_replacements,
            observed_output_fields,
            validated_spawns: BTreeSet::new(),
            digest_helpers: RefCell::new(BTreeSet::new()),
            conditional_depth: 0,
            current_statement: None,
        })
    }

    fn lower(mut self) -> Result<LoweredEntryBody> {
        let mut out = String::new();
        self.lower_statements(&mut out, 8, self.entry.body.statements())?;
        self.current_statement = None;
        if out.trim().is_empty() {
            out.push_str("        require(1 == 1);\n");
        }
        for spawn in &self.entry.spawns {
            if !self.validated_spawns.contains(&spawn.name) {
                return Err(
                    self.error(format!("spawn `{}` must be validated with `require {}.outputs become`", spawn.name, spawn.name))
                );
            }
        }
        self.validate_output_value_refs()?;
        Ok(LoweredEntryBody { sil: out, digest_helpers: self.digest_helpers.into_inner() })
    }

    fn validate_output_value_refs(&self) -> Result<()> {
        let missing = self
            .output_values
            .iter()
            .filter(|value| !value.is_referenced(self.entry.body.tokens()))
            .map(OutputValueRef::policy_ref)
            .collect::<Vec<_>>();
        if missing.is_empty() {
            return Ok(());
        }
        let missing_values = missing.iter().map(|value| format!("`{value}`")).collect::<Vec<_>>().join(", ");
        let declarations = missing.iter().map(|value| format!("`{}({value});`", word::UNRESTRICTED)).collect::<Vec<_>>().join(", ");
        let noun = if missing.len() == 1 { "output value" } else { "output values" };
        Err(self.error(format!(
            "entry `{}::{}` must reference {noun} {missing_values}; if intentionally unrestricted, add {declarations}",
            self.actor.name, self.entry.name,
        )))
    }

    fn lower_statements(&mut self, out: &mut String, indent: usize, statements: &[EntryStatement]) -> Result<()> {
        for statement in statements {
            self.current_statement = Some(statement.span());
            if let Some((name, role)) = binds_reserved_entry_name(statement, &self.reserved_entry_names) {
                return Err(self.error(format!("entry binding `{name}` collides with {} of the same name", role.description())));
            }
            match statement {
                EntryStatement::If { condition, then_branch, else_branch, .. } => {
                    self.lower_if(out, indent, *condition, then_branch, else_branch.as_deref(), true)?;
                }
                EntryStatement::For { binding, header, body, .. } => self.lower_for(out, indent, binding, *header, body)?,
                EntryStatement::Become { routes, .. } => self.lower_become(out, indent, routes)?,
                EntryStatement::ValidateOutputsBecome { group, routes, .. } => {
                    self.lower_outputs_become(out, indent, group, routes)?;
                }
                EntryStatement::Local { declaration, span } => {
                    self.lower_local_declaration(out, indent, declaration, *span)?;
                }
                EntryStatement::Plain { bindings, destructuring, span, .. } => {
                    self.lower_plain_statement(out, indent, bindings, destructuring.as_ref(), *span)?;
                }
                EntryStatement::Block { statements, .. } => {
                    push_indent(out, indent);
                    out.push_str("{\n");
                    self.lower_scoped_statements(out, indent + 4, statements)?;
                    push_indent(out, indent);
                    out.push_str("}\n");
                }
            }
        }
        Ok(())
    }

    fn lower_scoped_statements(&mut self, out: &mut String, indent: usize, statements: &[EntryStatement]) -> Result<()> {
        self.bindings.enter_scope();
        let result = self.lower_statements(out, indent, statements);
        self.bindings.exit_scope();
        result
    }

    fn lower_if(
        &mut self,
        out: &mut String,
        indent: usize,
        condition: Span,
        then_branch: &EntryStatement,
        else_branch: Option<&EntryStatement>,
        push_leading_indent: bool,
    ) -> Result<()> {
        let EntryStatement::Block { statements: then_statements, .. } = then_branch else {
            return Err(self.error("expected `{`"));
        };
        if push_leading_indent {
            push_indent(out, indent);
        }
        let condition = self.entry.body.span_text(condition).trim();
        out.push_str(&format!("if ({}) {{\n", self.lower_expr(condition, None, indent)?));
        self.conditional_depth += 1;
        self.lower_scoped_statements(out, indent + 4, then_statements)?;
        self.conditional_depth -= 1;
        push_indent(out, indent);
        out.push('}');

        if let Some(else_branch) = else_branch {
            self.current_statement = Some(else_branch.span());
            if let EntryStatement::If { condition, then_branch, else_branch, .. } = else_branch {
                out.push_str(" else ");
                self.lower_if(out, indent, *condition, then_branch, else_branch.as_deref(), false)?;
                return Ok(());
            }
            let EntryStatement::Block { statements: else_statements, .. } = else_branch else {
                return Err(self.error("expected `{`"));
            };
            out.push_str(" else {\n");
            self.conditional_depth += 1;
            self.lower_scoped_statements(out, indent + 4, else_statements)?;
            self.conditional_depth -= 1;
            push_indent(out, indent);
            out.push('}');
        }
        out.push('\n');
        Ok(())
    }

    fn lower_for(
        &mut self,
        out: &mut String,
        indent: usize,
        binding: &EntryBinding,
        header: Span,
        body: &EntryStatement,
    ) -> Result<()> {
        let header = self.entry.body.span_text(header).trim();
        let header = split_top_level_commas(header)
            .into_iter()
            .map(|component| self.lower_expr(component.trim(), None, indent))
            .collect::<Result<Vec<_>>>()?
            .join(", ");
        push_indent(out, indent);
        out.push_str(&format!("for ({header}) {{\n"));

        self.conditional_depth += 1;
        self.bindings.enter_scope();
        self.declare_sil_binding(binding);
        let result = match body {
            EntryStatement::Block { statements, .. } => self.lower_statements(out, indent + 4, statements),
            statement => self.lower_statements(out, indent + 4, std::slice::from_ref(statement)),
        };
        self.bindings.exit_scope();
        self.conditional_depth -= 1;
        result?;

        push_indent(out, indent);
        out.push_str("}\n");
        Ok(())
    }

    fn lower_local_declaration(&mut self, out: &mut String, indent: usize, declaration: &EntryLocalDecl, span: Span) -> Result<()> {
        let source = self.entry.body.span_text(span).trim();
        source.strip_suffix(';').ok_or_else(|| self.error("unterminated statement"))?;

        let source_ty = declaration.binding.source_type.as_str();
        let name = declaration.binding.name.as_str();
        let expr = declaration.initializer.map(|initializer| self.entry.body.span_text(initializer).trim().to_string());
        if let Some(state) = declaration.binding.actor_type_state.as_deref() {
            let expr = expr.as_deref().ok_or_else(|| self.error(format!("actor handle `{name}` requires an initializer")))?;
            self.lower_actor_type_statement(out, indent, state, name, expr)?;
            return Ok(());
        }
        let lowered_ty = if self.model.has_state(source_ty) && expr.is_some() {
            self.bindings
                .lowered_type_for_expr(expr.as_deref().expect("checked above"))
                .unwrap_or_else(|| self.lower_local_type(source_ty))
        } else {
            self.lower_local_type(source_ty)
        };
        let declared_type = self.entry.body.span_text(declaration.declared_type).trim();
        let type_suffix = declared_type.strip_prefix(source_ty).expect("declared type starts with its parsed source type");
        let emitted_type = format!("{lowered_ty}{type_suffix}");
        push_indent(out, indent);
        let lowered = match expr {
            Some(expr) => {
                let lowered = self.lower_typed_local_initializer(source_ty, &lowered_ty, &expr, indent)?;
                out.push_str(&format!("{emitted_type} {name} = {lowered};\n"));
                Some(lowered)
            }
            None => {
                out.push_str(&format!("{emitted_type} {name};\n"));
                None
            }
        };
        let mut binding = BodyBinding::typed(source_ty, lowered_ty, self.state_values);
        if let Some(lowered) = lowered {
            let initializer_state_value =
                parse_expression_ast(&lowered).ok().and_then(|initializer| self.planned_state_value_for_expr(&initializer));
            let planned_state_value = self.state_values.plan_initialized_sil_type(source_ty, initializer_state_value.as_ref());
            binding = binding.with_planned_state_value(planned_state_value);
        }
        if let Some(selector) = self.selector_catalog.get(name)
            && selector.actor_enum == source_ty
        {
            binding = binding.with_selector(selector.clone());
        }
        self.bindings.declare(name, binding);
        Ok(())
    }

    fn lower_plain_statement(
        &mut self,
        out: &mut String,
        indent: usize,
        bindings: &[EntryBinding],
        destructuring: Option<&EntryStructDestructure>,
        span: Span,
    ) -> Result<()> {
        let source = self.entry.body.span_text(span).trim();
        let mut statement = source.strip_suffix(';').ok_or_else(|| self.error("unterminated statement"))?.trim().to_string();
        self.reject_physical_state_constructors(&statement)?;
        let mut value_lowered = false;

        if let Some(destructuring) = destructuring {
            let source_type = self.entry.body.span_text(destructuring.declared_type);
            let value = self.entry.body.span_text(destructuring.value).trim();
            if self.bindings.active_field_projection(value).is_some_and(|projection| projection.expanded) {
                let projected_state =
                    self.bindings.source_type(value).expect("active expanded-field bindings retain their source state");
                let example = self
                    .model
                    .state(projected_state)?
                    .fields
                    .first()
                    .map(|field| {
                        format!(
                            "; project its fields directly, for example `{} opened_{} = {value}.{};`",
                            source_type_ref(&field.ty),
                            field.name,
                            field.name,
                        )
                    })
                    .unwrap_or_else(|| "; project its fields directly".to_string());
                return Err(
                    self.error(format!("active expanded state field `{value}` cannot be destructured as a whole value{example}"))
                );
            }
            let authored_state = self.model.has_state(source_type).then_some(source_type);
            let lowered_type = authored_state
                .map(|state| self.lower_local_type(state))
                .or_else(|| self.bindings.lowered_type_for_expr(value))
                .unwrap_or_else(|| source_type.to_string());
            let lowered_value = self.lower_expr(value, authored_state, indent)?;
            let relative = (destructuring.value.start - span.start)..(destructuring.value.end - span.start);
            statement.replace_range(relative, &lowered_value);
            value_lowered = true;
            let relative = (destructuring.declared_type.start - span.start)..(destructuring.declared_type.end - span.start);
            statement.replace_range(relative, &lowered_type);
        } else if let Some((range, expected_state)) = self.plain_assignment_expr(&statement) {
            let expected_type = expected_state.as_ref().map(|value| self.state_values.sil_type(value));
            let lowered = self.lower_expr(&statement[range.clone()], expected_type.as_deref(), indent)?;
            statement.replace_range(range, &lowered);
            value_lowered = true;
        }

        if let Some(value) = parse_unrestricted_output_value(&statement) {
            self.validate_unrestricted_output_value(value)?;
            return Ok(());
        }

        if let Some(require_expr) = parse_require_statement(&statement) {
            for covenant_id in co_spent_covenant_ids(require_expr, &self.bindings)? {
                push_indent(out, indent);
                out.push_str(&format!("// :: co-spent with {}\n", covenant_id.trim()));
            }
        }

        push_indent(out, indent);
        let statement = lower_co_spent_calls(&statement, &self.bindings)?;
        let statement = lower_actor_enum_literals(&statement, self.model)?;
        let statement = if value_lowered { statement } else { self.lower_nested_state_values(&statement, indent)? };
        let statement = if self.state_values.has_equivalent_state_sources() {
            lower_statement_state_types(&statement, self.state_values).map_err(|err| self.error(err.to_string()))?
        } else {
            statement
        };
        let statement = self.lower_refs(&statement, indent)?;
        out.push_str(&statement);
        out.push_str(";\n");
        for binding in bindings {
            self.declare_sil_binding(binding);
        }
        Ok(())
    }

    fn plain_assignment_expr(&self, statement: &str) -> Option<(Range<usize>, Option<PlannedStateValue>)> {
        let source = format!("{statement};");
        let SilStatement::Assign { name, expr, .. } = parse_statement_ast(&source).ok()? else {
            return None;
        };
        let range = expr.span.start()..expr.span.end();
        let expected_state = self.bindings.state_value(&name).cloned();
        Some((range, expected_state))
    }

    fn declare_sil_binding(&mut self, binding: &EntryBinding) {
        let lowered_type = self.lower_local_type(&binding.source_type);
        self.bindings.declare(&binding.name, BodyBinding::typed(&binding.source_type, lowered_type, self.state_values));
    }

    fn validate_unrestricted_output_value(&self, value: &str) -> Result<()> {
        let value = value.trim();
        for output in &self.output_values {
            if !output.matches_exact(value) {
                continue;
            }
            if let OutputValueRef::Range { handle } = output {
                let tokens = lex(value)?;
                let close =
                    match_indexed_value_ref(&tokens, 0, handle).expect("an exact ranged output value retains its closing bracket");
                let index = &value[tokens[1].span.end..tokens[close].span.start];
                let location = self
                    .bindings
                    .output_range(handle)
                    .ok_or_else(|| self.error(format!("missing range metadata for output `{handle}`")))?;
                self.lower_range_index(handle, index, location, 0)?;
            }
            return Ok(());
        }
        Err(self.error(format!(
            "`{}(...)` expects exactly one current emit or spawn output value; `{value}` is not one",
            word::UNRESTRICTED,
        )))
    }

    fn lower_actor_type_statement(&mut self, out: &mut String, indent: usize, state: &str, name: &str, expr: &str) -> Result<()> {
        let selector = self
            .selector_catalog
            .get(name)
            .ok_or_else(|| ArgentError::new(format!("actor handle `{name}` must be initialized as `ActorEnum[selector]`")))?
            .clone();
        if selector.state != state {
            return Err(ArgentError::new(format!(
                "actor handle `{name}` is declared as {actor_type}<{state}>, but `{}` contains {actor_type}<{}>",
                selector.actor_enum,
                selector.state,
                actor_type = word::ACTOR_TYPE,
            )));
        }
        self.validate_actor_type_initializer(name, expr, &selector)?;
        self.materialize_selector_template(out, indent, &selector)?;
        let id = self.bindings.declare(
            name,
            BodyBinding::source_typed(format!("{}<{state}>", word::ACTOR_TYPE), self.state_values).with_selector(selector),
        );
        self.bindings.mark_selector_materialized(id);
        Ok(())
    }

    fn validate_actor_type_initializer(&self, name: &str, expr: &str, selector: &TemplateSelector) -> Result<()> {
        if let Some((actor_enum, _)) = parse_actor_enum_selector(expr) {
            if actor_enum != selector.actor_enum {
                return Err(ArgentError::new(format!(
                    "actor handle `{name}` was analyzed as `{}`, but lowers from `{actor_enum}`",
                    selector.actor_enum
                )));
            }
            return Ok(());
        }
        if let Some((actor_enum, _)) = parse_actor_enum_variant(expr) {
            if actor_enum != selector.actor_enum {
                return Err(ArgentError::new(format!(
                    "actor handle `{name}` was analyzed as `{}`, but lowers from `{actor_enum}`",
                    selector.actor_enum
                )));
            }
            return Ok(());
        }
        Err(ArgentError::new(format!("actor handle `{name}` must be initialized as `ActorEnum[selector]` or `ActorEnum::Variant`")))
    }

    fn ensure_selector_template(&mut self, out: &mut String, indent: usize, selector_name: &str) -> Result<String> {
        let (binding_id, selector) = self
            .bindings
            .selector(selector_name)
            .map(|(id, selector)| (id, selector.clone()))
            .ok_or_else(|| ArgentError::new(format!("unknown actor handle `{selector_name}`")))?;
        let template_var = hidden_template_selector_template_name(&selector.name);
        if self.bindings.selector_is_materialized(binding_id) {
            return Ok(template_var);
        }
        self.materialize_selector_template(out, indent, &selector)?;
        self.bindings.mark_selector_materialized(binding_id);
        Ok(template_var)
    }

    fn input_reference_for_expr(&self, expr: &str, indent: usize) -> Result<Option<PlannedEntryInputReference>> {
        let expr = strip_outer_parentheses(expr);
        if let Some(reference) = self.input_references.reference(expr) {
            return Ok(self.bindings.input_reference_is_visible(reference).then(|| reference.clone()));
        }
        let Some((handle, index)) = exact_indexed_root(expr) else {
            return Ok(None);
        };
        let Some(InputValueLocation::Range { covenant_id, range }) = self.bindings.input_value(handle) else {
            return Ok(None);
        };
        let index = self.lower_range_index(handle, index, range, indent)?;
        let cov_index = offset_index_expr(range.first_index, index.clone());
        let input_index = format!("OpCovInputIdx({covenant_id}, {cov_index})");
        self.input_references.consumed_range_item(handle, expr.to_string(), index, input_index)
    }

    fn input_reference_from_state_call(&self, expr: &str, indent: usize) -> Result<Option<PlannedEntryInputReference>> {
        let Some(reference_expr) = parse_state_reference_call(expr).map_err(|err| self.error(err))? else {
            return Ok(None);
        };
        self.input_reference_for_expr(reference_expr, indent)?.map(Some).ok_or_else(|| {
            self.error(format!("`{}(...)` requires one visible entry input reference, but `{reference_expr}` is not one", word::STATE))
        })
    }

    fn materialize_selector_template(&self, out: &mut String, indent: usize, selector: &TemplateSelector) -> Result<()> {
        let selector_name = &selector.name;
        let template_var = hidden_template_selector_template_name(selector_name);
        let family = self.selector_family(selector)?;
        if !family.table_actors().starts_with(&selector.variants) {
            return Err(ArgentError::new(format!(
                "actor enum `{}` order must prefix route family `{}` table order for selector lowering",
                selector.actor_enum, family.id
            )));
        }

        let selector_var = hidden_template_selector_index_name(selector_name);
        let selector_expr = self.lower_expr(&selector.selector_expr, None, indent)?;
        let table = hidden_route_family_table_name(family);
        push_indent(out, indent);
        out.push_str(&format!("int {selector_var} = {selector_expr};\n"));
        push_indent(out, indent);
        out.push_str(&format!("require({selector_var} >= 0);\n"));
        push_indent(out, indent);
        out.push_str(&format!("require({selector_var} < {});\n", selector.variants.len()));
        push_generated_call(
            out,
            indent,
            &format!("byte[32] {template_var} = "),
            "byte[32]",
            &[format!("{table}.slice({selector_var} * 32, {selector_var} * 32 + 32)")],
        );
        Ok(())
    }

    fn lower_become(&mut self, out: &mut String, indent: usize, routes: &[EntryRoute]) -> Result<()> {
        let entry_model = self.model.entry_model(self.actor, self.entry)?;
        for route in routes {
            let route =
                entry_model.route(route.id).ok_or_else(|| self.error(format!("missing resolved route {}", route.id.index())))?.clone();
            self.lower_route(out, indent, route)?;
        }
        Ok(())
    }

    fn constructed_route(&self, route: &EntryRoute) -> Result<ConstructedRoute> {
        let EntrySuccessor::Constructed { actor, state, arity } = route.successor else {
            return Err(self.error("exact successor `self` is only valid for current emitted outputs"));
        };
        Ok(ConstructedRoute {
            output: route.output.clone(),
            actor: self.entry.body.span_text(actor).trim().to_string(),
            state: self.entry.body.span_text(state).trim().to_string(),
            arity,
        })
    }

    fn lower_outputs_become(&mut self, out: &mut String, indent: usize, group_name: &str, routes: &[EntryRoute]) -> Result<()> {
        let entry_model = self.model.entry_model(self.actor, self.entry)?;
        let group = entry_model
            .existing_groups()
            .chain(entry_model.genesis_groups())
            .find(|group| group.name() == Some(group_name))
            .ok_or_else(|| self.error(format!("unknown observe or spawn `{group_name}`")))?;
        if group.spawn().is_some() {
            if self.conditional_depth != 0 {
                return Err(self.error(format!("spawn `{group_name}` output validation must be unconditional")));
            }
            if !self.validated_spawns.insert(group_name.to_string()) {
                return Err(self.error(format!("spawn `{group_name}` outputs are validated more than once")));
            }
        }
        let routes = routes.iter().map(|route| self.constructed_route(route)).collect::<Result<Vec<_>>>()?;
        self.lower_covenant_outputs_become(out, indent, group, routes)
    }

    fn lower_covenant_outputs_become(
        &mut self,
        out: &mut String,
        indent: usize,
        group: &CovenantGroup<'a>,
        routes: Vec<ConstructedRoute>,
    ) -> Result<()> {
        let outputs_by_name = group.outputs().iter().map(|output| (output.handle(), output)).collect::<BTreeMap<_, _>>();
        let group_name = group.name().expect("current covenant is not lowered through an external output clause");
        let group_label = if group.observe().is_some() { "observe" } else { "spawn" };
        let mut seen = BTreeSet::new();

        for route in routes {
            let handle = route.output.as_str();
            let Some(output) = outputs_by_name.get(handle).copied() else {
                return Err(self.error(format!("{group_label} `{group_name}` has no output `{handle}`")));
            };
            if !seen.insert(handle.to_string()) {
                return Err(self.error(format!("{group_label} `{group_name}` validates output `{handle}` more than once")));
            }
            let context = Self::covenant_output_context(group, output);
            if route.actor != context.actor() {
                return Err(self.error(format!(
                    "{group_label} `{group_name}` output `{handle}` expects `{}`, but route uses `{}`",
                    context.actor(),
                    route.actor
                )));
            }
            self.lower_covenant_output_route(out, indent, context, output.target(), route)?;
        }

        for output in group.outputs() {
            let handle = output.handle();
            if !seen.contains(handle) {
                return Err(self.error(format!("{group_label} `{group_name}` does not validate output `{handle}`")));
            }
        }
        Ok(())
    }

    fn covenant_output_context(group: &CovenantGroup<'a>, interaction: &EntryInteraction<'a>) -> CovenantOutputContext<'a> {
        match (group.observe(), group.spawn(), interaction.source()) {
            (Some(observe), None, InteractionSource::ObserveOutput(output)) => CovenantOutputContext::Existing { observe, output },
            (None, Some(spawn), InteractionSource::SpawnOutput(output)) => CovenantOutputContext::Genesis { spawn, output },
            _ => unreachable!("external covenant output retains its matching source clause"),
        }
    }

    fn lower_covenant_output_route(
        &mut self,
        out: &mut String,
        indent: usize,
        context: CovenantOutputContext<'a>,
        target: &ActorTarget,
        route: ConstructedRoute,
    ) -> Result<()> {
        let actor_expr = context.actor();
        let static_target = match context {
            CovenantOutputContext::Existing { observe, output } => {
                static_observed_actor_target(self.actor, self.entry, observe, output, self.model)?
            }
            CovenantOutputContext::Genesis { .. } => self.model.resolve_static_actor_target(target),
        };
        let state_name = match (context, static_target) {
            (_, Some(target)) => target.state().to_string(),
            (CovenantOutputContext::Existing { observe, output }, None) => {
                observed_open_state_for_decl(self.actor, self.entry, observe, output, self.model)?
                    .expect("dynamic observed actor has a validated state")
            }
            (CovenantOutputContext::Genesis { .. }, None) => {
                spawn_target_state(target, actor_expr, self.actor, self.entry, self.model)?
                    .expect("spawn target checked during model validation")
            }
        };
        let output_target = match static_target {
            Some(target) => plan_static_actor_output_state(self.actor, target, self.model)?,
            None => plan_open_output_state(self.actor, &state_name, self.model)?,
        };
        let state_ty = output_target.physical_type().to_string();
        let authored = self.require_route_authored_state(out, indent, &route, &output_target)?;
        let physical =
            materialize_output_state(&output_target, authored, self.model.state_lowering(&self.actor.name)?, self.model, indent)?;

        let observed_spec = match context {
            CovenantOutputContext::Existing { observe, output } => {
                Some(observed_output_spec(self.actor, self.entry, observe, output, self.model)?)
            }
            CovenantOutputContext::Genesis { .. } => None,
        };
        let spawn_spec = match context {
            CovenantOutputContext::Existing { .. } => None,
            CovenantOutputContext::Genesis { spawn, output } => {
                let source =
                    if target.is_source() { clause_actor_type_ref(actor_expr, self.actor, self.entry, self.model)? } else { None };
                Some(SpawnActorWitnessSpec {
                    spawn: spawn.name.clone(),
                    handle: output.name.clone(),
                    actor: output.actor.clone(),
                    source,
                })
            }
        };
        let template = match static_target {
            Some(StaticActorTarget::InApp(actor)) => hidden_template_name(&actor.name),
            Some(StaticActorTarget::CrossApp(actor)) => hidden_imported_template_name(&ImportedTemplateSpec::from_linked(actor)),
            None => match context {
                CovenantOutputContext::Existing { observe, output } => self.observed_actor_template_expr(
                    observe,
                    output,
                    observed_spec.as_ref().expect("observed output has a witness spec"),
                    indent,
                )?,
                CovenantOutputContext::Genesis { .. } => self.lower_expr(actor_expr, Some("byte[32]"), indent)?,
            },
        };

        let output_idx = match context {
            CovenantOutputContext::Existing { .. } => hidden_observed_output_idx_name(context.group_name(), context.output_name()),
            CovenantOutputContext::Genesis { .. } => hidden_spawn_output_idx_name(context.group_name(), context.output_name()),
        };
        let state_binding = generated_state_name(&route, &state_ty);
        let validation_context = match context {
            CovenantOutputContext::Existing { observe, output } => OutputValidationContext::Observed {
                observe,
                output,
                static_target,
                witness: observed_spec.as_ref().expect("observed output has a witness spec"),
                template,
            },
            CovenantOutputContext::Genesis { .. } => OutputValidationContext::Spawned {
                static_target,
                witness: spawn_spec.as_ref().expect("spawn output has a witness spec"),
                template,
            },
        };
        let validation =
            plan_output_validation(self.actor, self.entry, validation_context, output_idx, physical, state_binding, self.model)?
                .stabilize(out, indent);

        push_indent(out, indent);
        out.push_str(&format!(
            "// :: {} become {}.{} -> {}\n",
            context.route_label(),
            context.group_name(),
            context.output_name(),
            actor_expr
        ));
        validation.emit(out, indent);
        Ok(())
    }

    fn observed_actor_template_expr(
        &self,
        observe: &ObserveDecl,
        observed: &ObservedActorDecl,
        spec: &ObservedActorWitnessSpec,
        indent: usize,
    ) -> Result<String> {
        if let Some(target) = static_observed_actor_target(self.actor, self.entry, observe, observed, self.model)? {
            return Ok(match target {
                StaticActorTarget::InApp(target) => hidden_template_name(&target.name),
                StaticActorTarget::CrossApp(target) => hidden_imported_template_name(&ImportedTemplateSpec::from_linked(target)),
            });
        }
        if observed_is_dynamic_binding(observe, observed) {
            return Ok(observed.actor.clone());
        }
        if observed_is_source_actor_type(self.actor, self.entry, observed, self.model)? {
            return self.lower_expr(&observed.actor, Some("byte[32]"), indent);
        }
        Ok(hidden_observed_actor_template_name(spec))
    }

    /// Produce one stable authored value before crossing the storage boundary.
    fn require_route_authored_state(
        &mut self,
        out: &mut String,
        indent: usize,
        route: &ConstructedRoute,
        target: &OutputStateTarget,
    ) -> Result<AuthoredStateExpr> {
        let expr = strip_outer_parentheses(route.state.trim());
        self.reject_legacy_input_state_members(expr)?;
        let source_state = target.source_identity().to_string();
        let authored_sil_type = target.authored_sil_type().to_string();
        let is_source_constructor = split_state_constructor(expr).is_some_and(|(state, _)| state == source_state);
        let parsed_source = parse_expression_ast(expr)
            .map_err(|err| self.error(format!("cannot classify route state `{expr}` as an authored value: {err}")))?;
        let is_matching_input_state =
            self.input_reference_from_state_call(expr, indent)?.is_some_and(|reference| reference.source_identity() == source_state);
        let is_planned_authored_value = self
            .planned_state_value_for_expr(&parsed_source)
            .is_some_and(|value| value.source().as_str() == source_state && value.shape().is_scalar());
        if !is_source_constructor && !is_matching_input_state && !is_planned_authored_value {
            if let SilExprKind::Identifier(name) = &parsed_source.kind
                && self.bindings.lowered_type(name).is_some()
            {
                return Err(self.error(format!(
                    "route state `{name}` is not an authored `{source_state}` value; construct `{source_state} {{ ... }}` explicitly"
                )));
            }
            return Err(self.error(format!(
                "route state `{expr}` is not a proven authored `{source_state}` value; bind or construct an authored value explicitly"
            )));
        }
        let authored = target.require_authored_value(|expected| self.lower_expr(expr, Some(expected), indent))?;
        let lowered = authored.sil().to_string();
        let parsed = parse_expression_ast(&lowered)
            .map_err(|err| self.error(format!("lowered route state `{expr}` is not a valid Sil expression: {err}")))?;
        if let SilExprKind::Identifier(name) = &parsed.kind {
            if is_matching_input_state
                || self
                    .planned_state_value_for_expr(&parsed)
                    .is_some_and(|value| value.source().as_str() == source_state && value.shape().is_scalar())
            {
                return Ok(authored.rebound(name.as_str()));
            }
            if self.bindings.lowered_type(name).is_some() {
                return Err(self.error(format!(
                    "route state `{name}` is not an authored `{source_state}` value; construct `{source_state} {{ ... }}` explicitly"
                )));
            }
        }
        let name = format!("{RESERVED_GENERATED_PREFIX}source_{}_{}", to_snake(&route.output), to_snake(&source_state));
        push_indent(out, indent);
        out.push_str(&format!("{authored_sil_type} {name} = {lowered};\n"));
        self.bindings.declare(name.clone(), BodyBinding::typed(source_state, authored_sil_type, self.state_values));
        Ok(authored.rebound(name))
    }

    fn lower_route(&mut self, out: &mut String, indent: usize, route: ResolvedRoute) -> Result<()> {
        let entry_model = self.model.entry_model(self.actor, self.entry)?;
        let output = entry_model
            .current()
            .outputs()
            .iter()
            .find(|output| output.handle() == route.output)
            .ok_or_else(|| self.error(format!("entry has no output `{}`", route.output)))?;
        if matches!(route.successor, ResolvedSuccessor::ExactSelf) {
            if output.cardinality().is_range() {
                return Err(self.error(format!(
                    "range output `{}` must use bulk become syntax `{} <- Actor[](states)`",
                    output.handle(),
                    output.handle()
                )));
            }
            let output_idx = hidden_output_idx_name(&route.output);
            push_indent(out, indent);
            out.push_str(&format!("// :: become {}\n", self.actor.name));
            preserve_exact_self(out, indent, &output_idx);
            return Ok(());
        }
        let ResolvedSuccessor::Constructed { actor, state, arity } = route.successor else {
            unreachable!("exact successor returned above")
        };
        let route = ConstructedRoute { output: route.output, actor, state, arity };
        if output.cardinality().is_range() {
            self.lower_ranged_route(out, indent, output, route)
        } else {
            self.lower_constructed_route(out, indent, route)
        }
    }

    fn lower_constructed_route(&mut self, out: &mut String, indent: usize, route: ConstructedRoute) -> Result<()> {
        let state = strip_outer_parentheses(route.state.trim());
        let reconstructs_self =
            self.input_reference_from_state_call(state, indent)?.is_some_and(|reference| reference.reference() == word::SELF);
        if route.actor == self.actor.name && (state == "self.state" || reconstructs_self) {
            return Err(self.error(format!(
                "`{}({state})` reconstructs the current actor state; use `{} <- self` for exact continuation",
                route.actor, route.output
            )));
        }
        if route.arity != RouteArity::One {
            return Err(self.error(format!(
                "singleton output `{}` must use scalar become syntax `{} <- Actor(state)`",
                route.output, route.output
            )));
        }
        let output_idx = hidden_output_idx_name(&route.output);
        self.lower_constructed_route_at_output(out, indent, route, output_idx)
    }

    fn lower_ranged_route(
        &mut self,
        out: &mut String,
        indent: usize,
        output: &EntryInteraction<'a>,
        route: ConstructedRoute,
    ) -> Result<()> {
        if route.arity != RouteArity::Many {
            return Err(self.error(format!(
                "range output `{}` must use bulk become syntax `{} <- Actor[](states)`",
                output.handle(),
                output.handle()
            )));
        }
        let target = output.target().single_static_actor().ok_or_else(|| {
            self.error(format!("range output `{}` must use one fixed actor target in this compiler version", output.handle()))
        })?;
        if route.actor != target {
            return Err(self.error(format!(
                "range output `{}` expects `{target}`, but route uses `{}`",
                output.handle(),
                route.actor
            )));
        }
        let states = route.state.trim();
        if !is_identifier(states) {
            return Err(
                self.error(format!("range output `{}` must become from a named state array, found `{states}`", output.handle()))
            );
        }
        let target_plan = plan_actor_output_state(self.actor, target, self.model)?;
        let state_value = self
            .bindings
            .state_value(states)
            .ok_or_else(|| self.error(format!("range output `{}` uses unknown authored state array `{states}`", output.handle())))?;
        if state_value.source().as_str() != target_plan.source_identity() || state_value.shape() != StateValueShape::DynamicArray {
            return Err(self.error(format!(
                "range output `{}` expects authored `{}[]`, but `{states}` has type `{}`",
                output.handle(),
                target_plan.source_identity(),
                self.state_values.sil_type(state_value)
            )));
        }

        let Some((_, maximum)) = output.cardinality().range_bounds() else {
            unreachable!("ranged output has resolved range bounds");
        };
        let InteractionLocation::Range { start, .. } = output.location() else {
            unreachable!("ranged output has a ranged location");
        };
        let count = hidden_output_count_name(output.handle());
        push_indent(out, indent);
        out.push_str(&format!("require({states}.length == {count});\n"));
        let position = hidden_output_position_name(output.handle());
        push_indent(out, indent);
        out.push_str(&format!("for ({position}, 0, {count}, {maximum}) {{\n"));
        let auth_index = offset_index_expr(start, position.clone());
        let output_idx = format!("OpAuthOutputIdx(this.activeInputIndex, {auth_index})");
        let item_route = ConstructedRoute { state: format!("{states}[{position}]"), arity: RouteArity::One, ..route };
        self.lower_constructed_route_at_output(out, indent + 4, item_route, output_idx)?;
        push_indent(out, indent);
        out.push_str("}\n");
        Ok(())
    }

    fn lower_constructed_route_at_output(
        &mut self,
        out: &mut String,
        indent: usize,
        route: ConstructedRoute,
        output_idx: String,
    ) -> Result<()> {
        if self.bindings.selector(&route.actor).is_some() {
            return self.lower_selector_route(out, indent, route, output_idx);
        }
        if self.selector_catalog.contains_key(&route.actor) {
            let reason =
                if self.bindings.get(&route.actor).is_some() { "is shadowed by a non-selector binding" } else { "is not visible" };
            return Err(self.error(format!("actor handle `{}` {reason} in this scope", route.actor)));
        }
        self.model.actor_state(&route.actor)?;
        let output_target = plan_actor_output_state(self.actor, &route.actor, self.model)?;
        let state_ty = output_target.physical_type().to_string();
        let authored = self.require_route_authored_state(out, indent, &route, &output_target)?;
        let physical =
            materialize_output_state(&output_target, authored, self.model.state_lowering(&self.actor.name)?, self.model, indent)?;
        let validation = plan_output_validation(
            self.actor,
            self.entry,
            OutputValidationContext::Actor { target: &route.actor },
            output_idx,
            physical,
            generated_state_name(&route, &state_ty),
            self.model,
        )?
        .stabilize(out, indent);

        push_indent(out, indent);
        out.push_str(&format!("// :: become {}\n", route.actor));
        validation.emit(out, indent);
        Ok(())
    }

    fn lower_selector_route(&mut self, out: &mut String, indent: usize, route: ConstructedRoute, output_idx: String) -> Result<()> {
        let selector = self
            .bindings
            .selector(&route.actor)
            .map(|(_, selector)| selector)
            .ok_or_else(|| ArgentError::new(format!("unknown actor handle `{}`", route.actor)))?
            .clone();
        let output_target = plan_selector_output_state(self.actor, &selector, self.model)?;
        let state_ty = output_target.physical_type().to_string();
        let authored = self.require_route_authored_state(out, indent, &route, &output_target)?;
        let physical =
            materialize_output_state(&output_target, authored, self.model.state_lowering(&self.actor.name)?, self.model, indent)?;

        let template = hidden_template_selector_template_name(&selector.name);
        let validation = plan_output_validation(
            self.actor,
            self.entry,
            OutputValidationContext::Selector { selector: &route.actor, template },
            output_idx,
            physical,
            generated_state_name(&route, &state_ty),
            self.model,
        )?
        .stabilize(out, indent);
        self.ensure_selector_template(out, indent, &route.actor)?;
        push_indent(out, indent);
        out.push_str(&format!("// :: become {}\n", route.actor));
        validation.emit(out, indent);
        Ok(())
    }

    fn selector_family(&self, selector: &TemplateSelector) -> Result<&RouteFamily> {
        self.model
            .route_families_for_state(&selector.state)
            .into_iter()
            .find(|family| selector.variants.iter().all(|variant| family.table_actors().contains(variant)))
            .ok_or_else(|| {
                ArgentError::new(format!(
                    "actor enum `{}` variants are not available as a selector table for state `{}`",
                    selector.actor_enum, selector.state
                ))
            })
    }

    fn lower_expr(&self, expr: &str, expected_ty: Option<&str>, indent: usize) -> Result<String> {
        let expr = expr.trim();
        self.reject_legacy_input_state_members(expr)?;
        self.reject_physical_state_constructors(expr)?;
        let expected_state_value = expected_ty.and_then(|ty| self.state_values.plan_sil_type(ty));
        if let Some(projection) = self.bindings.active_field_projection(expr) {
            if let Some(expected) = expected_state_value.as_ref()
                && let Some(actual) = self.bindings.state_value(expr)
                && actual.is_proven_incompatible_with(expected)
            {
                return Err(self.state_value_mismatch(actual, expected));
            }
            return self.active_reference.project_field(&projection.field, indent).map_err(|err| self.error(err.to_string()));
        }
        if let Some(expected) = expected_state_value.as_ref().filter(|value| !value.shape().is_scalar()) {
            return self.lower_authored_state_array_expr(expr, expected, indent);
        }
        if let Some(value) = parse_digest_call(expr).map_err(|err| self.error(err))? {
            return self.lower_digest_expr(value, indent);
        }
        if let Some(reference) = self.input_reference_from_state_call(expr, indent)? {
            let authored = reference.complete_authored_state(indent).map_err(|err| self.error(err.to_string()))?;
            if let Some(expected) = expected_state_value.as_ref()
                && (expected.source().as_str() != authored.source().as_str() || !expected.shape().is_scalar())
            {
                return Err(self.error(format!(
                    "`{}({})` has authored type `{}`, not `{}`",
                    word::STATE,
                    reference.reference(),
                    authored.source().as_str(),
                    self.state_values.sil_type(expected)
                )));
            }
            return Ok(authored.into_sil());
        }
        if let Some((state_name, body)) = split_state_constructor(expr) {
            if let Some(expected) = expected_state_value.as_ref()
                && expected.shape().is_scalar()
                && state_name != expected.source().as_str()
            {
                return Err(self
                    .error(format!("state constructor `{state_name}` cannot initialize authored `{}`", expected.source().as_str())));
            }
            return self.lower_state_constructor(state_name, body, indent);
        }
        if let Some(reference) = self.input_reference_for_expr(expr, indent)? {
            return Err(self.error(format!(
                "input reference `{expr}` is not an authored state value; use `{}({})` to reconstruct `{}`",
                word::STATE,
                reference.reference(),
                reference.source_identity()
            )));
        }
        // Normalize Argent-only syntax before asking Sil to classify calls and
        // their exact argument spans.
        let expr = lower_co_spent_calls(expr, &self.bindings)?;
        let expr = lower_actor_enum_literals(&expr, self.model)?;
        if let Some(expected) = expected_state_value.as_ref() {
            let parsed = parse_expression_ast(&expr).ok();
            if let Some(actual) = parsed.as_ref().and_then(|expr| self.planned_state_value_for_expr(expr))
                && actual.is_proven_incompatible_with(expected)
            {
                return Err(self.state_value_mismatch(&actual, expected));
            }
            if let Some(parsed) = parsed.as_ref()
                && matches!(parsed.kind, SilExprKind::ArrayIndex { .. })
            {
                return self.lower_authored_state_array_index(&expr, parsed, expected, indent);
            }
        }
        let expr = self.lower_nested_state_values(&expr, indent)?;
        let expr = lower_expression_state_types(&expr, self.state_values).map_err(|err| self.error(err.to_string()))?;
        self.lower_refs(&expr, indent)
    }

    fn reject_physical_state_constructors(&self, source: &str) -> Result<()> {
        if contains_physical_state_constructor(source) {
            return Err(self.error("physical `State` is compiler-owned and cannot be constructed in Argent source"));
        }
        Ok(())
    }

    fn lower_authored_state_array_expr(&self, expr: &str, expected: &PlannedStateValue, indent: usize) -> Result<String> {
        debug_assert!(!expected.shape().is_scalar());
        let expr = lower_co_spent_calls(expr, &self.bindings)?;
        let expr = lower_actor_enum_literals(&expr, self.model)?;
        let mut parsed = parse_expression_ast(&expr).map_err(|err| {
            self.error(format!(
                "cannot classify authored state-array expression `{expr}` as `{}`: {err}",
                self.state_values.sil_type(expected)
            ))
        })?;
        let actual = self.planned_state_value_for_expr(&parsed).ok_or_else(|| {
            self.error(format!(
                "unsupported authored state-array expression `{expr}` for `{}`; use a named array, a state-array function result, a typed array literal, or `append(...)`",
                self.state_values.sil_type(expected)
            ))
        })?;
        if actual.is_proven_incompatible_with(expected) {
            return Err(self.state_value_mismatch(&actual, expected));
        }

        if !matches!(
            &parsed.kind,
            SilExprKind::Array { .. } | SilExprKind::Append { .. } | SilExprKind::Identifier(_) | SilExprKind::Call { .. }
        ) {
            return Err(self.error(format!(
                "unsupported authored state-array expression shape in `{expr}` for `{}`",
                self.state_values.sil_type(expected)
            )));
        }
        let mut collector = StateValueSiteCollector { state_values: self.state_values, bindings: &self.bindings, sites: Vec::new() };
        collector.visit_expr(&mut parsed);
        let lowered = self.lower_planned_state_value_sites(&expr, collector.sites, indent)?;
        let lowered = lower_expression_state_types(&lowered, self.state_values).map_err(|err| self.error(err.to_string()))?;
        self.lower_refs(&lowered, indent)
    }

    fn lower_authored_state_array_index(
        &self,
        expr: &str,
        parsed: &SilExpr<'_>,
        expected: &PlannedStateValue,
        indent: usize,
    ) -> Result<String> {
        debug_assert!(expected.shape().is_scalar());
        let SilExprKind::ArrayIndex { source, index } = &parsed.kind else {
            unreachable!("array index lowering requires an array index expression");
        };
        let source_plan = self
            .planned_state_value_for_expr(source)
            .ok_or_else(|| self.error(format!("cannot classify the authored state-array source indexed by `{expr}`")))?;
        let Some(element) = source_plan.element() else {
            return Err(self.error(format!("state value `{}` is scalar and cannot be indexed", source_plan.source().as_str())));
        };
        if element.is_proven_incompatible_with(expected) {
            return Err(self.state_value_mismatch(&element, expected));
        }

        let source_type = self.state_values.sil_type(&source_plan);
        let source_span = source.span.start()..source.span.end();
        let index_span = index.span.start()..index.span.end();
        let replacements = vec![
            (source_span.clone(), self.lower_expr(&expr[source_span], Some(&source_type), indent)?),
            (index_span.clone(), self.lower_expr(&expr[index_span], None, indent)?),
        ];
        let lowered = apply_expr_replacements(expr, replacements);
        self.lower_refs(&lowered, indent)
    }

    fn planned_state_value_for_expr(&self, expr: &SilExpr<'_>) -> Option<PlannedStateValue> {
        planned_state_value_for_expr(expr, self.state_values, &self.bindings)
    }

    fn state_value_mismatch(&self, actual: &PlannedStateValue, expected: &PlannedStateValue) -> ArgentError {
        self.error(format!(
            "authored state value has type `{}`, not `{}`",
            self.state_values.sil_type(actual),
            self.state_values.sil_type(expected)
        ))
    }

    fn lower_nested_state_values(&self, expr: &str, indent: usize) -> Result<String> {
        if !self.contains_state_value_context(expr)? {
            return Ok(expr.to_string());
        }
        let mut collector = StateValueSiteCollector { state_values: self.state_values, bindings: &self.bindings, sites: Vec::new() };
        if let Ok(mut parsed) = parse_expression_ast(expr) {
            collector.visit_expr(&mut parsed);
        } else {
            let source = format!("{expr};");
            let mut statement = parse_statement_ast(&source).map_err(|err| {
                self.error(format!("cannot classify state-valued function arguments in expression or statement `{expr}`: {err}"))
            })?;
            collector.visit_statement(&mut statement);
        }
        self.lower_planned_state_value_sites(expr, collector.sites, indent)
    }

    fn lower_planned_state_value_sites(&self, expr: &str, mut sites: Vec<PlannedStateValueSite>, indent: usize) -> Result<String> {
        sites.sort_by(|left, right| left.span.start.cmp(&right.span.start).then_with(|| right.span.end.cmp(&left.span.end)));

        let mut outermost: Vec<PlannedStateValueSite> = Vec::new();
        for site in sites {
            if outermost.iter().any(|outer| outer.span.start <= site.span.start && site.span.end <= outer.span.end) {
                continue;
            }
            outermost.push(site);
        }

        let mut out = expr.to_string();
        for site in outermost.into_iter().rev() {
            let source = &expr[site.span.clone()];
            let expected_type = self.state_values.sil_type(&site.expected);
            let lowered = self.lower_expr(source, Some(&expected_type), indent)?;
            out.replace_range(site.span, &lowered);
        }
        Ok(out)
    }

    fn contains_state_value_context(&self, expr: &str) -> Result<bool> {
        let tokens =
            lex(expr).map_err(|err| self.error(format!("cannot inspect function calls in expression `{expr}`: {}", err.message)))?;
        Ok(tokens.windows(2).any(|tokens| {
            matches!(tokens, [Token { kind: TokenKind::Symbol('.'), .. }, Token { kind: TokenKind::Ident(name), .. }] if name == "append")
                || matches!(tokens, [Token { kind: TokenKind::Ident(name), .. }, Token { kind: TokenKind::Symbol('('), .. }]
                    if self.state_values.signature(name).is_some_and(|signature| signature.has_state_param()))
                || matches!(tokens, [Token { kind: TokenKind::Ident(name), .. }, Token { kind: TokenKind::Symbol('['), .. }]
                    if self.state_values.plan_sil_type(name).is_some())
        }))
    }

    fn lower_state_constructor(&self, state_name: &str, body: &str, indent: usize) -> Result<String> {
        self.model.state(state_name)?;
        let sil_type = self
            .state_values
            .authored_sil_type_for_name(state_name)
            .ok_or_else(|| self.error(format!("state `{state_name}` has no contract-local authored representation")))?;
        self.lower_authored_state_object(state_name, sil_type, body, indent)
    }

    fn lower_digest_expr(&self, value: &str, indent: usize) -> Result<String> {
        let value = value.trim();
        let lowering = self.model.state_lowering(&self.actor.name)?;
        if let Some(reference) = self.input_reference_from_state_call(value, indent)? {
            return reference.authored_payload_digest(lowering, self.model).map_err(|err| self.error(err.to_string()));
        }
        if let Some(reference) = self.input_reference_for_expr(value, indent)? {
            return Err(self.error(format!(
                "`{}(...)` requires an authored state value; use `{}({}({}))` for input reference `{}`",
                word::DIGEST,
                word::DIGEST,
                word::STATE,
                reference.reference(),
                reference.reference()
            )));
        }
        let parsed = parse_expression_ast(value)
            .map_err(|err| self.error(format!("cannot classify authored state digest value `{value}`: {err}")))?;
        let planned = self.planned_state_value_for_expr(&parsed).ok_or_else(|| {
            self.error(format!(
                "`{}(...)` requires a proven authored state value, but `{value}` has no known source type",
                word::DIGEST
            ))
        })?;
        if !planned.shape().is_scalar() {
            return Err(self.error(format!(
                "`{}(...)` requires one scalar authored state value, but `{value}` has type `{}`",
                word::DIGEST,
                self.state_values.sil_type(&planned)
            )));
        }
        self.model.state(planned.source().as_str())?;
        let expected = self.state_values.sil_type(&planned);
        let lowered = self.lower_expr(value, Some(&expected), indent)?;
        if matches!(parsed.kind, SilExprKind::Identifier(_)) {
            return authored_state_payload_digest_expr(planned.source(), &lowered, lowering, self.model);
        }
        self.digest_helpers.borrow_mut().insert(planned.source().clone());
        Ok(format!("{}({lowered})", self.state_values.digest_helper_name(planned.source())))
    }

    fn lower_typed_local_initializer(&self, source_ty: &str, lowered_ty: &str, expr: &str, indent: usize) -> Result<String> {
        if source_ty == "State" && split_state_object_literal(expr).is_some() {
            return Err(self.error("physical `State` is compiler-owned and cannot be constructed in Argent source"));
        }
        if self.model.actor_enums.contains_key(source_ty) {
            return self.lower_actor_enum_initializer(source_ty, expr, indent);
        }
        if self.model.has_state(source_ty)
            && let Some(body) = split_state_object_literal(expr)
        {
            return self.lower_authored_state_object(source_ty, lowered_ty, body, indent);
        }
        if self.model.has_state(source_ty) {
            return self.lower_expr(expr, Some(source_ty), indent);
        }
        self.lower_expr(expr, Some(lowered_ty), indent)
    }

    fn lower_actor_enum_initializer(&self, actor_enum_name: &str, expr: &str, indent: usize) -> Result<String> {
        if let Some((source_actor_enum, selector_expr)) = parse_actor_enum_selector(expr) {
            if source_actor_enum != actor_enum_name {
                return Err(ArgentError::new(format!(
                    "actor enum value `{actor_enum_name}` cannot be initialized from `{source_actor_enum}`"
                )));
            }
            return self.lower_expr(selector_expr, Some("int"), indent);
        }
        if let Some((source_actor_enum, variant)) = parse_actor_enum_variant(expr) {
            if source_actor_enum != actor_enum_name {
                return Err(ArgentError::new(format!(
                    "actor enum value `{actor_enum_name}` cannot be initialized from `{source_actor_enum}`"
                )));
            }
            let actor_enum = self
                .model
                .actor_enums
                .get(actor_enum_name)
                .ok_or_else(|| ArgentError::new(format!("unknown actor enum `{actor_enum_name}`")))?;
            let value = actor_enum_variant_const_expr(actor_enum, &variant)
                .ok_or_else(|| ArgentError::new(format!("actor enum `{actor_enum_name}` has no variant `{variant}`")))?;
            return Ok(value);
        }
        self.lower_expr(expr, Some("int"), indent)
    }

    fn lower_authored_state_object(&self, state_name: &str, sil_type: &str, body: &str, indent: usize) -> Result<String> {
        if let Some(expansion) = self.model.state(state_name)?.expansion.as_ref() {
            let fields = parse_state_fields(body)?;
            let mut pending = fields.iter().cloned().collect::<BTreeMap<_, _>>();
            if pending.len() != fields.len() {
                return Err(ArgentError::new(format!("state `{state_name}` constructor contains duplicate fields")));
            }
            let mut lowered_fields = Vec::new();
            for field in &self.model.storage_state(state_name)?.fields {
                let Some(raw_expr) = pending.remove(&field.name) else {
                    if field.virtual_slot {
                        continue;
                    }
                    return Err(ArgentError::new(format!("state `{state_name}` constructor is missing field `{}`", field.name)));
                };
                let lowered = if let Some(digest) = expansion.digests.iter().find(|digest| digest.field == field.name) {
                    if split_state_object_literal(&raw_expr).is_some() {
                        return Err(ArgentError::new(format!(
                            "state `{state_name}` constructor slot `{}` must use `{} {{ ... }}`",
                            digest.field, digest.state
                        )));
                    }
                    self.lower_expr(&raw_expr, Some(&digest.state), indent + 4)?
                } else {
                    let expected = self.state_values.plan_type_ref(&field.ty).map(|value| self.state_values.sil_type(&value));
                    self.lower_expr(&raw_expr, expected.as_deref(), indent + 4)?
                };
                lowered_fields.push((field.name.clone(), lowered));
            }
            if let Some(extra) = pending.keys().next() {
                return Err(ArgentError::new(format!("state `{state_name}` constructor has unknown field `{extra}`")));
            }
            return self.render_state_object(state_name, sil_type, &lowered_fields, indent);
        }
        let state = self.model.storage_state(state_name)?;
        let fields = parse_state_fields(body)?
            .into_iter()
            .map(|(name, expr)| {
                let expected = state
                    .fields
                    .iter()
                    .find(|field| field.name == name)
                    .and_then(|field| self.state_values.plan_type_ref(&field.ty))
                    .map(|value| self.state_values.sil_type(&value));
                self.lower_expr(&expr, expected.as_deref(), indent + 4).map(|lowered| (name, lowered))
            })
            .collect::<Result<Vec<_>>>()?;
        self.render_state_object(state_name, sil_type, &fields, indent)
    }

    fn lower_local_type(&self, source_ty: &str) -> String {
        if self.model.actor_enums.contains_key(source_ty) {
            return "int".to_string();
        }
        if source_ty == word::COVENANT_ID {
            return "byte[32]".to_string();
        }
        self.state_values.sil_type_for_sil_type(source_ty).unwrap_or_else(|| source_ty.to_string())
    }

    fn render_state_object(&self, state_name: &str, sil_type: &str, fields: &[(String, String)], indent: usize) -> Result<String> {
        let field_indent = " ".repeat(indent + 4);
        let close_indent = " ".repeat(indent);
        let mut pending = fields.iter().cloned().collect::<BTreeMap<_, _>>();
        if pending.len() != fields.len() {
            return Err(ArgentError::new(format!("state `{state_name}` constructor contains duplicate fields")));
        }
        let mut out = format!("{sil_type} {{\n");
        let state = self.model.storage_state(state_name)?;
        if !state.fields.is_empty() {
            out.push_str(&format!("{field_indent}// :: user declared fields\n"));
        }
        for field in &state.fields {
            let expr = if let Some(expr) = pending.remove(&field.name) {
                expr
            } else if field.virtual_slot {
                self.observed_output_field_expr(state_name, &field.name)?
            } else {
                return Err(ArgentError::new(format!("state `{state_name}` constructor is missing field `{}`", field.name)));
            };
            out.push_str(&format!("{field_indent}{}: {expr},\n", field.name));
        }
        if let Some(extra) = pending.keys().next() {
            return Err(ArgentError::new(format!("state `{state_name}` constructor has unknown field `{extra}`")));
        }
        out.push_str(&close_indent);
        out.push('}');
        Ok(out)
    }

    fn observed_output_field_expr(&self, state_name: &str, field_name: &str) -> Result<String> {
        let matches =
            self.observed_output_fields.iter().filter(|spec| spec.state == state_name && spec.field == field_name).collect::<Vec<_>>();
        match matches.as_slice() {
            [spec] => Ok(hidden_observed_output_field_name(spec)),
            [] => Err(ArgentError::new(format!(
                "state `{state_name}` constructor is missing virtual slot `{field_name}`, but this entry has no observed output that can provide it"
            ))),
            _ => Err(ArgentError::new(format!(
                "state `{state_name}` constructor is missing virtual slot `{field_name}`, but multiple observed outputs could provide it"
            ))),
        }
    }

    fn lower_refs(&self, expr: &str, indent: usize) -> Result<String> {
        self.reject_legacy_input_state_members(expr)?;
        let expr = self.lower_digest_calls(expr, indent)?;
        let expr = self.lower_input_state_calls(&expr, indent)?;
        // Output references win when a transition intentionally reuses one
        // handle for both a consumed and emitted actor.
        let expr = self.lower_output_value_refs(&expr, indent)?;
        let expr = self.lower_input_value_refs(&expr, indent)?;
        let mut replacements = self.fixed_ref_replacements.clone();
        replacements.extend(self.active_reference.operation_replacements(indent)?);
        for reference in self.input_references.external_references() {
            if !self.bindings.input_reference_is_visible(reference) {
                continue;
            }
            replacements.extend(reference.operation_replacements(indent)?);
        }
        RefReplacements::new(replacements)?.rewrite(&expr)
    }

    fn reject_legacy_input_state_members(&self, expr: &str) -> Result<()> {
        self.active_reference.reject_unavailable_field_refs(expr).map_err(|err| self.error(err.to_string()))?;
        for reference in self.input_references.external_references() {
            if self.bindings.input_reference_is_visible(reference) {
                reference.reject_unavailable_field_refs(expr).map_err(|err| self.error(err.to_string()))?;
            }
        }
        Ok(())
    }

    /// Lower ranged-output length and value references through generated output data.
    fn lower_output_value_refs(&self, input: &str, indent: usize) -> Result<String> {
        let tokens = lex(input)?;
        let mut out = String::with_capacity(input.len());
        let mut cursor = 0usize;
        let mut pos = 0usize;

        while pos < tokens.len() {
            let TokenKind::Ident(handle) = &tokens[pos].kind else {
                pos += 1;
                continue;
            };
            if pos > 0 && matches!(tokens[pos - 1].kind, TokenKind::Symbol('.')) {
                pos += 1;
                continue;
            }
            let Some(location) = self.bindings.output_range(handle) else {
                pos += 1;
                continue;
            };

            let matched = if is_symbol(&tokens, pos + 1, '.') && is_ident(&tokens, pos + 2, "length") {
                Some((pos + 3, Span { start: tokens[pos].span.start, end: tokens[pos + 2].span.end }, location.count.clone()))
            } else if let Some(close) = match_indexed_value_ref(&tokens, pos, handle) {
                let index = &input[tokens[pos + 1].span.end..tokens[close].span.start];
                let index = self.lower_range_index(handle, index, location, indent)?;
                let auth_index = offset_index_expr(location.first_index, index);
                Some((
                    close + 3,
                    Span { start: tokens[pos].span.start, end: tokens[close + 2].span.end },
                    format!("tx.outputs[OpAuthOutputIdx(this.activeInputIndex, {auth_index})].value"),
                ))
            } else {
                None
            };
            let Some((next_pos, span, replacement)) = matched else {
                pos += 1;
                continue;
            };

            out.push_str(&input[cursor..span.start]);
            out.push_str(&replacement);
            cursor = span.end;
            pos = next_pos;
        }

        out.push_str(&input[cursor..]);
        Ok(out)
    }

    fn lower_digest_calls(&self, expr: &str, indent: usize) -> Result<String> {
        if !contains_call_named(expr, word::DIGEST)? {
            return Ok(expr.to_string());
        }
        let mut lowered = expr.to_string();
        for site in self.named_call_sites(expr, word::DIGEST)?.into_iter().rev() {
            let call = &expr[site.clone()];
            let value = parse_digest_call(call)
                .map_err(|err| self.error(err))?
                .ok_or_else(|| self.error(format!("cannot classify `{}` call `{call}`", word::DIGEST)))?;
            lowered.replace_range(site, &self.lower_digest_expr(value, indent)?);
        }
        Ok(lowered)
    }

    fn lower_input_state_calls(&self, expr: &str, indent: usize) -> Result<String> {
        if !contains_call_named(expr, word::STATE)? {
            return Ok(expr.to_string());
        }
        let mut lowered = expr.to_string();
        for site in self.named_call_sites(expr, word::STATE)?.into_iter().rev() {
            let call = &expr[site.clone()];
            let reference = self.input_reference_from_state_call(call, indent)?.expect("collector found a state call");
            let authored = reference.complete_authored_state(indent).map_err(|err| self.error(err.to_string()))?;
            lowered.replace_range(site, authored.sil());
        }
        Ok(lowered)
    }

    fn named_call_sites(&self, expr: &str, name: &str) -> Result<Vec<Range<usize>>> {
        let mut collector = NamedCallSiteCollector { name, sites: Vec::new() };
        if let Ok(mut parsed) = parse_expression_ast(expr) {
            collector.visit_expr(&mut parsed);
        } else {
            let source = format!("{expr};");
            let mut statement = parse_statement_ast(&source)
                .map_err(|err| self.error(format!("cannot classify `{name}` calls in `{expr}`: {err}")))?;
            collector.visit_statement(&mut statement);
        }
        collector.sites.sort_by_key(|site| site.start);
        let mut outermost = Vec::<Range<usize>>::new();
        for site in collector.sites {
            if outermost.iter().any(|outer| outer.start <= site.start && site.end <= outer.end) {
                continue;
            }
            outermost.push(site);
        }
        Ok(outermost)
    }

    /// Lower scalar input values and every operation on a ranged input item.
    fn lower_input_value_refs(&self, input: &str, indent: usize) -> Result<String> {
        let tokens = lex(input)?;
        let mut out = String::with_capacity(input.len());
        let mut cursor = 0usize;
        let mut pos = 0usize;

        while pos < tokens.len() {
            let TokenKind::Ident(handle) = &tokens[pos].kind else {
                pos += 1;
                continue;
            };
            // A member with the same name is not rooted at the input binding.
            if pos > 0 && matches!(tokens[pos - 1].kind, TokenKind::Symbol('.')) {
                pos += 1;
                continue;
            }
            let Some(location) = self.bindings.input_value(handle).cloned() else {
                pos += 1;
                continue;
            };

            let matched = match &location {
                InputValueLocation::Singleton { input_index }
                    if is_symbol(&tokens, pos + 1, '.') && is_ident(&tokens, pos + 2, word::VALUE) =>
                {
                    Some((
                        pos + 3,
                        Span { start: tokens[pos].span.start, end: tokens[pos + 2].span.end },
                        format!("tx.inputs[{input_index}].value"),
                    ))
                }
                InputValueLocation::Range { range, .. }
                    if is_symbol(&tokens, pos + 1, '.') && is_ident(&tokens, pos + 2, "length") =>
                {
                    Some((pos + 3, Span { start: tokens[pos].span.start, end: tokens[pos + 2].span.end }, range.count.clone()))
                }
                InputValueLocation::Range { .. } if is_symbol(&tokens, pos + 1, '[') => {
                    let Some(close) = matching_symbol(&tokens, pos + 1, '[', ']') else {
                        return Err(self.error(format!("unterminated range input reference `{handle}[...]`")));
                    };
                    let reference_end = tokens[close].span.end;
                    let reference_expr = &input[tokens[pos].span.start..reference_end];
                    let reference = self
                        .input_reference_for_expr(reference_expr, indent)?
                        .ok_or_else(|| self.error(format!("`{reference_expr}` is not a visible ranged input reference")))?;
                    if !is_symbol(&tokens, close + 1, '.') {
                        return Err(self.error(format!(
                            "input reference `{reference_expr}` is not an authored state value; use `{}({reference_expr})` to reconstruct `{}`",
                            word::STATE,
                            reference.source_identity()
                        )));
                    }
                    let TokenKind::Ident(member) = &tokens[close + 2].kind else {
                        return Err(self.error(format!("input reference `{reference_expr}` must be followed by a field name")));
                    };
                    let replacement = match member.as_str() {
                        word::VALUE => reference.native_value(),
                        word::COVENANT_ID => reference.covenant_id(),
                        word::STATE => {
                            return Err(self.error(format!(
                                "input reference `{reference_expr}` has no `.state` member; use `{}({reference_expr})` for complete authored state or project a field directly",
                                word::STATE
                            )));
                        }
                        field => reference.project_field(field, indent).map_err(|err| self.error(err.to_string()))?,
                    };
                    Some((close + 3, Span { start: tokens[pos].span.start, end: tokens[close + 2].span.end }, replacement))
                }
                InputValueLocation::Range { .. } => {
                    return Err(self.error(format!(
                        "range input `{handle}` is a collection of input references; use `{handle}.length` or index one item"
                    )));
                }
                InputValueLocation::Singleton { .. } => None,
            };
            let Some((next_pos, span, replacement)) = matched else {
                pos += 1;
                continue;
            };

            out.push_str(&input[cursor..span.start]);
            out.push_str(&replacement);
            cursor = span.end;
            pos = next_pos;
        }

        out.push_str(&input[cursor..]);
        Ok(out)
    }

    fn lower_range_index(&self, handle: &str, input: &str, location: &RangeValueLocation, indent: usize) -> Result<String> {
        let literal = parse_int_literal(input);
        if let Some(index) = literal
            && (index < 0 || index >= location.maximum)
        {
            return Err(
                self.error(format!("range `{handle}` index `{index}` is outside its declared positions `0..{}`", location.maximum))
            );
        }

        let index = self.lower_refs(input, indent)?;
        if literal.is_some_and(|index| index < location.minimum) {
            return Ok(index);
        }

        Ok(format!("{}({index}, {})", hidden_checked_range_index_name(), location.count))
    }

    fn error(&self, message: impl Into<String>) -> ArgentError {
        ArgentError::new(format!(
            "{} in `{}::{}`{}",
            message.into(),
            self.actor.name,
            self.entry.name,
            self.current_statement.map(|span| format!(" at body bytes {span}")).unwrap_or_default(),
        ))
    }
}

fn binds_reserved_entry_name<'s, 'r>(
    statement: &'s EntryStatement,
    reserved_names: &'r ReservedEntryNames,
) -> Option<(&'s str, &'r ReservedEntryNameRole)> {
    let bindings = match statement {
        EntryStatement::Local { declaration, .. } => std::slice::from_ref(&declaration.binding),
        EntryStatement::For { binding, .. } => std::slice::from_ref(binding),
        EntryStatement::Plain { bindings, .. } => bindings,
        EntryStatement::If { .. }
        | EntryStatement::Block { .. }
        | EntryStatement::Become { .. }
        | EntryStatement::ValidateOutputsBecome { .. } => return None,
    };
    bindings.iter().find_map(|binding| reserved_names.role_for_body_binding(&binding.name).map(|role| (binding.name.as_str(), role)))
}

fn contains_physical_state_constructor(source: &str) -> bool {
    let mut detector = PhysicalStateConstructorDetector::default();
    if let Ok(mut expr) = parse_expression_ast(source) {
        detector.visit_expr(&mut expr);
        return detector.found;
    }
    let statement_source = format!("{source};");
    if let Ok(mut statement) = parse_statement_ast(&statement_source) {
        detector.visit_statement(&mut statement);
    }
    detector.found
}

pub(in crate::compiler::codegen) fn reject_function_physical_state_constructors(
    function_name: &str,
    body: &str,
    context: &str,
) -> Result<()> {
    let source = format!("function gen__physical_state_constructor_check() {{ {body} }}");
    let Ok(mut function) = parse_function_ast(&source) else {
        return Ok(());
    };
    let mut detector = PhysicalStateConstructorDetector::default();
    visit_function_mut(&mut detector, &mut function);
    if detector.found {
        return Err(ArgentError::new(format!(
            "physical `State` is compiler-owned and cannot be constructed in Argent {context} function `{function_name}`"
        )));
    }
    Ok(())
}

pub(in crate::compiler::codegen) fn reject_function_input_state_calls(function_name: &str, body: &str, context: &str) -> Result<()> {
    if !contains_call_named(body, word::STATE)? {
        return Ok(());
    }
    let source = format!("function gen__input_state_call_check() {{ {body} }}");
    let Ok(mut function) = parse_function_ast(&source) else {
        return Ok(());
    };
    let mut collector = NamedCallSiteCollector { name: word::STATE, sites: Vec::new() };
    visit_function_mut(&mut collector, &mut function);
    if !collector.sites.is_empty() {
        return Err(ArgentError::new(format!(
            "`{}(...)` input-state reconstruction is only available in entry bodies, not {context} function `{function_name}`",
            word::STATE
        )));
    }
    Ok(())
}

fn offset_index_expr(offset: usize, index: String) -> String {
    if offset == 0 {
        index
    } else if is_identifier(index.trim()) || index.trim().bytes().all(|byte| byte.is_ascii_digit()) {
        format!("{offset} + {}", index.trim())
    } else {
        format!("{offset} + ({index})")
    }
}

pub(in crate::compiler::codegen) fn lower_entry_expr(
    actor: &ActorDecl,
    entry: &EntryDecl,
    model: &Model<'_>,
    input_references: EntryInputReferenceView<'_>,
    expr: &str,
    expected_ty: Option<&str>,
) -> Result<String> {
    let state_values = ContractStateValuePlan::new(actor, model)?;
    BodyLowerer::new(actor, entry, model, input_references, &state_values)?.lower_expr(expr, expected_ty, 8)
}

fn generated_state_name(route: &ConstructedRoute, state_ty: &str) -> String {
    format!("{RESERVED_GENERATED_PREFIX}state_{}_{}", to_snake(&route.output), to_snake(state_ty))
}

fn split_state_constructor(expr: &str) -> Option<(&str, &str)> {
    let expr = expr.trim();
    let brace_idx = expr.find('{')?;
    let state_name = expr[..brace_idx].trim();
    if state_name.is_empty() || !state_name.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
        return None;
    }
    let body = expr[brace_idx + 1..].trim();
    let body = body.strip_suffix('}')?.trim();
    Some((state_name, body))
}

fn split_state_object_literal(expr: &str) -> Option<&str> {
    let expr = expr.trim();
    if !expr.starts_with('{') {
        return None;
    }
    expr.strip_prefix('{')?.strip_suffix('}').map(str::trim)
}

fn parse_digest_call(expr: &str) -> std::result::Result<Option<&str>, String> {
    let expr = expr.trim();
    if !expr.starts_with(word::DIGEST) {
        return Ok(None);
    }
    let tokens = lex(expr).map_err(|err| err.message)?;
    if !matches!(tokens.as_slice(), [Token { kind: TokenKind::Ident(name), .. }, Token { kind: TokenKind::Symbol('('), .. }, ..]
        if name == word::DIGEST)
    {
        return Ok(None);
    }
    let parsed =
        parse_expression_ast(expr).map_err(|err| format!("invalid `{}(...)` state-digest expression: {err}", word::DIGEST))?;
    let SilExprKind::Call { name, args, .. } = &parsed.kind else {
        return Ok(None);
    };
    if name != word::DIGEST {
        return Ok(None);
    }
    let [value] = args.as_slice() else {
        return Err(format!("`{}(...)` requires exactly one authored state value", word::DIGEST));
    };
    Ok(Some(value.span.as_str().trim()))
}

fn parse_state_reference_call(expr: &str) -> std::result::Result<Option<&str>, String> {
    let expr = expr.trim();
    if !expr.starts_with(word::STATE) {
        return Ok(None);
    }
    let tokens = lex(expr).map_err(|err| err.message)?;
    if !matches!(tokens.as_slice(), [Token { kind: TokenKind::Ident(name), .. }, Token { kind: TokenKind::Symbol('('), .. }, ..]
        if name == word::STATE)
    {
        return Ok(None);
    }
    let parsed =
        parse_expression_ast(expr).map_err(|err| format!("invalid `{}(...)` input-reference expression: {err}", word::STATE))?;
    let SilExprKind::Call { name, args, .. } = &parsed.kind else {
        return Ok(None);
    };
    if name != word::STATE {
        return Ok(None);
    }
    let [reference] = args.as_slice() else {
        return Err(format!("`{}(...)` requires exactly one input reference", word::STATE));
    };
    Ok(Some(reference.span.as_str().trim()))
}

fn contains_call_named(expr: &str, name: &str) -> Result<bool> {
    let tokens = lex(expr)?;
    Ok(tokens.windows(2).any(|tokens| {
        matches!(tokens, [Token { kind: TokenKind::Ident(candidate), .. }, Token { kind: TokenKind::Symbol('('), .. }]
            if candidate == name)
    }))
}

fn parse_state_fields(body: &str) -> Result<Vec<(String, String)>> {
    // TODO: Replace the character-based field splitters with Sil's structured
    // struct-literal AST. The current code rejects comments before fields and
    // silently ignores malformed empty comma components.
    let mut fields = Vec::new();
    for component in split_top_level_commas(body) {
        let component = component.trim();
        if component.is_empty() {
            continue;
        }
        let Some((name, expr)) = split_top_level_colon(component) else {
            if matches!(lex(component)?.as_slice(), [Token { kind: TokenKind::Eof, .. }]) {
                continue;
            }
            return Err(ArgentError::new(format!("state constructor component `{component}` must use `name: expression`")));
        };
        let name = name.trim();
        let expr = expr.trim();
        if !is_identifier(name) {
            return Err(ArgentError::new(format!("state constructor component `{component}` has invalid field name `{name}`")));
        }
        if expr.is_empty() {
            return Err(ArgentError::new(format!("state constructor component `{component}` has an empty expression")));
        }
        fields.push((name.to_string(), expr.to_string()));
    }
    Ok(fields)
}

fn split_top_level_colon(input: &str) -> Option<(&str, &str)> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (idx, ch) in input.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' | '(' | '[' => depth += 1,
            '}' | ')' | ']' => depth = depth.saturating_sub(1),
            ':' if depth == 0 => return Some((&input[..idx], &input[idx + 1..])),
            _ => {}
        }
    }
    None
}

fn split_top_level_commas(input: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (idx, ch) in input.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' | '(' | '[' => depth += 1,
            '}' | ')' | ']' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(&input[start..idx]);
                start = idx + 1;
            }
            _ => {}
        }
    }
    parts.push(&input[start..]);
    parts
}

fn lower_co_spent_calls(expr: &str, bindings: &BodyBindings) -> Result<String> {
    let method = format!(".{}", word::CO_SPENT);
    if !expr.contains(&method) {
        return Ok(expr.to_string());
    }
    let tokens =
        lex(expr).map_err(|err| ArgentError::new(format!("failed to lex covenant co-spend expression `{expr}`: {}", err.message)))?;
    let mut out = String::new();
    let mut cursor = 0usize;
    let mut pos = 0usize;
    while pos < tokens.len() {
        if let Some((replacement_start, replacement_end, next_pos, covenant_id)) = parse_co_spent_call(expr, &tokens, pos, bindings)? {
            out.push_str(&expr[cursor..replacement_start]);
            out.push_str(&format!("OpCovInputCount({}) > 0", covenant_id.trim()));
            cursor = replacement_end;
            pos = next_pos;
            continue;
        }
        if matches!(tokens[pos].kind, TokenKind::Eof) {
            break;
        }
        pos += 1;
    }
    out.push_str(&expr[cursor..]);
    if out.contains(&method) {
        return Err(ArgentError::new(format!(
            "`.{}()` is only available on `{}` values or explicit `{}(expr)` casts",
            word::CO_SPENT,
            word::COVENANT_ID,
            word::COVENANT_ID,
        )));
    }
    Ok(out)
}

fn co_spent_covenant_ids(expr: &str, bindings: &BodyBindings) -> Result<Vec<String>> {
    if !expr.contains(&format!(".{}", word::CO_SPENT)) {
        return Ok(Vec::new());
    }
    let tokens =
        lex(expr).map_err(|err| ArgentError::new(format!("failed to lex covenant co-spend expression `{expr}`: {}", err.message)))?;
    let mut ids = Vec::new();
    let mut pos = 0usize;
    while pos < tokens.len() {
        if let Some((_replacement_start, _replacement_end, next_pos, covenant_id)) = parse_co_spent_call(expr, &tokens, pos, bindings)?
        {
            ids.push(covenant_id.trim().to_string());
            pos = next_pos;
            continue;
        }
        if matches!(tokens[pos].kind, TokenKind::Eof) {
            break;
        }
        pos += 1;
    }
    Ok(ids)
}

fn parse_require_statement(statement: &str) -> Option<&str> {
    parse_call_statement(statement, word::REQUIRE)
}

fn parse_unrestricted_output_value(statement: &str) -> Option<&str> {
    parse_call_statement(statement, word::UNRESTRICTED)
}

/// Match `handle[index].value` at a token position and return the closing bracket.
fn match_indexed_value_ref(tokens: &[Token], pos: usize, handle: &str) -> Option<usize> {
    if !is_ident(tokens, pos, handle) || pos > 0 && is_symbol(tokens, pos - 1, '.') || !is_symbol(tokens, pos + 1, '[') {
        return None;
    }
    let close = matching_symbol(tokens, pos + 1, '[', ']')?;
    if close == pos + 2 || !is_symbol(tokens, close + 1, '.') || !is_ident(tokens, close + 2, word::VALUE) {
        return None;
    }
    Some(close)
}

fn parse_call_statement<'a>(statement: &'a str, callee: &str) -> Option<&'a str> {
    let tail = statement.trim().strip_prefix(callee)?;
    tail.strip_prefix('(')?.strip_suffix(')').map(str::trim)
}

fn parse_co_spent_call(
    expr: &str,
    tokens: &[Token],
    pos: usize,
    bindings: &BodyBindings,
) -> Result<Option<(usize, usize, usize, String)>> {
    if is_ident(tokens, pos, word::COVENANT_ID) && is_symbol(tokens, pos + 1, '(') {
        let close = matching_symbol(tokens, pos + 1, '(', ')')
            .ok_or_else(|| ArgentError::new(format!("unterminated {}(...) co-spend expression `{expr}`", word::COVENANT_ID)))?;
        if is_symbol(tokens, close + 1, '.')
            && is_ident(tokens, close + 2, word::CO_SPENT)
            && is_symbol(tokens, close + 3, '(')
            && is_symbol(tokens, close + 4, ')')
        {
            return Ok(Some((
                tokens[pos].span.start,
                tokens[close + 4].span.end,
                close + 5,
                expr[tokens[pos + 1].span.end..tokens[close].span.start].to_string(),
            )));
        }
        return Ok(None);
    }

    if matches!(tokens.get(pos).map(|token| &token.kind), Some(TokenKind::Ident(_)))
        && is_symbol(tokens, pos + 1, '.')
        && is_ident(tokens, pos + 2, word::CO_SPENT)
        && is_symbol(tokens, pos + 3, '(')
        && is_symbol(tokens, pos + 4, ')')
    {
        let ident = expr[tokens[pos].span.start..tokens[pos].span.end].to_string();
        if bindings.source_type(&ident).is_none_or(|ty| ty != word::COVENANT_ID) {
            return Err(ArgentError::new(format!(
                "`.{}()` is only available on `{}` values, found `{ident}`",
                word::CO_SPENT,
                word::COVENANT_ID,
            )));
        }
        return Ok(Some((tokens[pos].span.start, tokens[pos + 4].span.end, pos + 5, ident)));
    }

    Ok(None)
}

fn matching_symbol(tokens: &[Token], open_pos: usize, open: char, close: char) -> Option<usize> {
    let mut depth = 0usize;
    for (pos, token) in tokens.iter().enumerate().skip(open_pos) {
        match token.kind {
            TokenKind::Symbol(symbol) if symbol == open => depth += 1,
            TokenKind::Symbol(symbol) if symbol == close => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(pos);
                }
            }
            TokenKind::Eof => return None,
            _ => {}
        }
    }
    None
}

/// Remove parentheses that enclose the complete expression.
fn strip_outer_parentheses(mut expr: &str) -> &str {
    loop {
        let Ok(tokens) = lex(expr) else {
            return expr;
        };
        if !is_symbol(&tokens, 0, '(') {
            return expr;
        }
        let Some(close) = matching_symbol(&tokens, 0, '(', ')') else {
            return expr;
        };
        if !matches!(tokens.get(close + 1).map(|token| &token.kind), Some(TokenKind::Eof)) {
            return expr;
        }
        expr = expr[tokens[0].span.end..tokens[close].span.start].trim();
    }
}

fn apply_expr_replacements(source: &str, mut replacements: Vec<(Range<usize>, String)>) -> String {
    replacements.sort_by_key(|(span, _)| span.start);
    debug_assert!(replacements.windows(2).all(|pair| pair[0].0.end <= pair[1].0.start));
    let mut out = source.to_string();
    for (span, replacement) in replacements.into_iter().rev() {
        out.replace_range(span, &replacement);
    }
    out
}

/// Return the bound root when the entire expression is one index access.
fn indexed_root_binding(expr: &str) -> Option<&str> {
    exact_indexed_root(expr).map(|(root, _)| root)
}

/// Return the root and index when the entire expression is one index access.
fn exact_indexed_root(expr: &str) -> Option<(&str, &str)> {
    let tokens = lex(expr).ok()?;
    let TokenKind::Ident(_) = tokens.first()?.kind else {
        return None;
    };
    if !is_symbol(&tokens, 1, '[') {
        return None;
    }
    let close = matching_symbol(&tokens, 1, '[', ']')?;
    if !matches!(tokens.get(close + 1).map(|token| &token.kind), Some(TokenKind::Eof)) {
        return None;
    }
    Some((&expr[tokens[0].span.start..tokens[0].span.end], expr[tokens[1].span.end..tokens[close].span.start].trim()))
}

/// Return the element type of a one-dimensional Sil array type.
fn array_element_type(ty: &str) -> Option<&str> {
    let tokens = lex(ty).ok()?;
    let TokenKind::Ident(_) = tokens.first()?.kind else {
        return None;
    };
    if !is_symbol(&tokens, 1, '[') {
        return None;
    }
    let close = matching_symbol(&tokens, 1, '[', ']')?;
    if !matches!(tokens.get(close + 1).map(|token| &token.kind), Some(TokenKind::Eof)) {
        return None;
    }
    Some(&ty[tokens[0].span.start..tokens[0].span.end])
}

fn is_ident(tokens: &[Token], pos: usize, ident: &str) -> bool {
    matches!(tokens.get(pos).map(|token| &token.kind), Some(TokenKind::Ident(candidate)) if candidate == ident)
}

fn is_symbol(tokens: &[Token], pos: usize, symbol: char) -> bool {
    matches!(tokens.get(pos).map(|token| &token.kind), Some(TokenKind::Symbol(candidate)) if *candidate == symbol)
}

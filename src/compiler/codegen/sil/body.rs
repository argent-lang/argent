//! Lowers structured Argent entry bodies into generated Sil source.

use std::collections::{BTreeMap, BTreeSet};

use crate::compiler::model::{
    ActorTarget, CompilerRouteTransition, CovenantGroup, EntryInteraction, InteractionSource, Model, RouteFamily, StaticActorTarget,
    TemplateSelector, actor_enum_variant_const_expr, clause_actor_type_ref, observed_is_dynamic_binding, observed_open_bindings,
    observed_open_state_for_decl, parse_actor_enum_selector, parse_actor_enum_variant, spawn_target_state,
};
use crate::compiler::naming::to_snake;
use crate::compiler::syntax::body::{EntryBinding, EntryLocalDecl, EntryRoute, EntryStatement};
use crate::compiler::syntax::lexer::{RESERVED_GENERATED_PREFIX, Span, Token, TokenKind, lex};
use crate::compiler::syntax::word;
use crate::compiler::syntax::*;
use crate::error::{ArgentError, Result};

// Body lowering uses the surrounding Sil emitter's shared witness plans,
// layout helpers, and generated names.
use super::super::emitter::*;
use super::token_refs::{RefReplacements, count_qualified_ref};

pub(in crate::compiler::codegen) struct LoweredEntryBody {
    pub(in crate::compiler::codegen) sil: String,
}

pub(in crate::compiler::codegen) fn lower_entry_body(
    actor: &ActorDecl,
    entry: &EntryDecl,
    model: &Model<'_>,
) -> Result<LoweredEntryBody> {
    BodyLowerer::new(actor, entry, model)?.lower()
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

struct BodyLowerer<'a, 'm> {
    actor: &'a ActorDecl,
    entry: &'a EntryDecl,
    model: &'m Model<'a>,
    bindings: BodyBindings,
    /// Entry-wide candidates; the current binding decides selector visibility.
    selector_catalog: BTreeMap<String, TemplateSelector>,
    output_values: Vec<OutputValueRef>,
    ref_replacements: RefReplacements,
    observed_output_fields: Vec<ObservedOutputFieldWitnessSpec>,
    validated_spawns: BTreeSet<String>,
    conditional_depth: usize,
    current_statement: Option<Span>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct BodyBindingId(usize);

/// Type and lowering facts attached to one lexically resolved value.
struct BodyBinding {
    source_type: Option<String>,
    lowered_type: Option<String>,
    selector: Option<TemplateSelector>,
}

impl BodyBinding {
    fn typed(source_type: impl Into<String>, lowered_type: impl Into<String>) -> Self {
        Self { source_type: Some(source_type.into()), lowered_type: Some(lowered_type.into()), selector: None }
    }

    fn source_typed(source_type: impl Into<String>) -> Self {
        Self { source_type: Some(source_type.into()), lowered_type: None, selector: None }
    }

    fn lowered_typed(lowered_type: impl Into<String>) -> Self {
        Self { source_type: None, lowered_type: Some(lowered_type.into()), selector: None }
    }

    fn with_selector(mut self, selector: TemplateSelector) -> Self {
        self.selector = Some(selector);
        self
    }
}

struct ScopedBodyBinding {
    id: BodyBindingId,
    value: BodyBinding,
}

#[derive(Default)]
struct BodyScope {
    bindings: BTreeMap<String, ScopedBodyBinding>,
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
        self.scopes
            .last_mut()
            .expect("body bindings retain a root scope")
            .bindings
            .insert(name.into(), ScopedBodyBinding { id, value: binding });
        id
    }

    fn get(&self, name: &str) -> Option<&ScopedBodyBinding> {
        self.scopes.iter().rev().find_map(|scope| scope.bindings.get(name))
    }

    fn lowered_type(&self, name: &str) -> Option<&str> {
        self.get(name).and_then(|binding| binding.value.lowered_type.as_deref())
    }

    fn source_type(&self, name: &str) -> Option<&str> {
        self.get(name).and_then(|binding| binding.value.source_type.as_deref())
    }

    fn selector(&self, name: &str) -> Option<(BodyBindingId, &TemplateSelector)> {
        let binding = self.get(name)?;
        Some((binding.id, binding.value.selector.as_ref()?))
    }

    fn selector_is_materialized(&self, id: BodyBindingId) -> bool {
        self.scopes.iter().rev().any(|scope| scope.materialized_selectors.contains(&id))
    }

    fn mark_selector_materialized(&mut self, id: BodyBindingId) {
        self.scopes.last_mut().expect("body bindings retain a root scope").materialized_selectors.insert(id);
    }
}

#[derive(Debug)]
struct OutputValueRef {
    source: String,
    lowered: String,
}

/// Builds the fixed dotted-reference lowering plan for one entry body.
fn entry_ref_replacements(
    actor: &ActorDecl,
    model: &Model<'_>,
    input_names: BTreeSet<String>,
    output_values: &[OutputValueRef],
    observed_input_state_refs: Vec<(String, String)>,
) -> Result<RefReplacements> {
    assert_eq!(word::SELF, "self");
    assert_eq!(word::VALUE, "value");
    assert_eq!(word::COVENANT_ID, "cov_id");
    let mut replacements = vec![
        ("self.value".to_string(), "tx.inputs[this.activeInputIndex].value".to_string()),
        ("self.cov_id".to_string(), "OpInputCovenantId(this.activeInputIndex)".to_string()),
    ];
    for spec in state_expansion_witness_specs_for_actor(actor, model) {
        for field in &model.state(&spec.memory_state)?.fields {
            let local = hidden_state_expansion_field_name(&spec, &field.name);
            replacements.push((format!("self.{}.{}", spec.field, field.name), local.clone()));
            replacements.push((format!("{}.{}", spec.field, field.name), local));
        }
    }
    for field in &model.storage_state(&actor.state)?.fields {
        replacements.push((format!("self.{}", field.name), field.name.clone()));
    }
    replacements.extend(observed_input_state_refs);
    replacements.extend(output_values.iter().map(|output| (output.source.clone(), output.lowered.clone())));
    replacements.extend(
        input_names.into_iter().map(|name| (format!("{name}.value"), format!("tx.inputs[{}].value", hidden_input_idx_name(&name)))),
    );
    RefReplacements::new(replacements)
}

impl<'a, 'm> BodyLowerer<'a, 'm> {
    fn new(actor: &'a ActorDecl, entry: &'a EntryDecl, model: &'m Model<'a>) -> Result<Self> {
        let selector_catalog = model.template_selectors_for_entry(actor, entry)?;
        let mut bindings = BodyBindings::new();
        let expanded_digest_fields = state_expansion_digest_fields_for_state(&actor.state, model);
        for field in &model.storage_state(&actor.state)?.fields {
            if expanded_digest_fields.contains(field.name.as_str()) {
                continue;
            }
            bindings.declare(field.name.clone(), BodyBinding::typed(source_type_ref(&field.ty), lower_type_ref(&field.ty, model)));
        }
        if let Some(expansion) = model.state(&actor.state)?.expansion.as_ref() {
            for digest in &expansion.digests {
                bindings.declare(digest.field.clone(), BodyBinding::source_typed(digest.state.clone()));
            }
        }
        for param in &entry.params {
            let ty = lower_entry_param_type(actor, &param.ty, model);
            let mut binding = BodyBinding::typed(source_type_ref(&param.ty), ty);
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
                bindings.declare(binding.to_string(), BodyBinding::typed(format!("{}<{state}>", word::ACTOR_TYPE), "byte[32]"));
            }
        }
        for spawn in &entry.spawns {
            bindings.declare(spawn.covenant.clone(), BodyBinding::typed(word::COVENANT_ID, "byte[32]"));
        }

        let mut input_names = BTreeSet::new();
        for consume in &entry.consumes {
            input_names.insert(consume.name.clone());
            let state = model.actor(&consume.actor)?.state.clone();
            let ty = contract_state_type_for_actor(&consume.actor, actor, model)?;
            bindings.declare(consume.name.clone(), BodyBinding::typed(state, ty));
        }
        let mut observed_input_state_refs = Vec::new();
        for observe in &entry.observes {
            for input in &observe.inputs {
                let source_ref = format!("{}.inputs.{}.state", observe.name, input.name);
                let lowered_ref = hidden_observed_input_state_name(&observe.name, &input.name);
                observed_input_state_refs.push((source_ref.clone(), lowered_ref.clone()));
                let state = if let Some(state) = observed_open_state_for_decl(actor, entry, observe, input, model)? {
                    state.to_string()
                } else {
                    model.actor(&input.actor)?.state.clone()
                };
                let ty = contract_state_type_for_observed_actor(actor, entry, observe, input, model)?;
                bindings.declare(source_ref, BodyBinding::typed(state, ty.clone()));
                bindings.declare(lowered_ref, BodyBinding::lowered_typed(ty));
            }
        }

        let mut output_values = Vec::new();
        match &entry.emits {
            EmitSpec::None => {}
            EmitSpec::Outputs(outputs) => {
                output_values.extend(outputs.iter().map(|output| OutputValueRef {
                    source: format!("{}.{}", output.name, word::VALUE),
                    lowered: format!("tx.outputs[{}].value", hidden_output_idx_name(&output.name)),
                }));
            }
        }
        // This entry creates spawned outputs, so it owns their value policy.
        // Observed outputs remain the responsibility of their emitting contracts.
        for spawn in &entry.spawns {
            output_values.extend(spawn.outputs.iter().map(|output| OutputValueRef {
                source: format!("{}.{}.{}.{}", spawn.name, word::OUTPUTS, output.name, word::VALUE),
                lowered: format!("tx.outputs[{}].value", hidden_spawn_output_idx_name(&spawn.name, &output.name)),
            }));
        }
        // Preserve most-specific-first ordering in value-policy diagnostics.
        output_values.sort_by(|left, right| right.source.len().cmp(&left.source.len()).then_with(|| left.source.cmp(&right.source)));

        let ref_replacements = entry_ref_replacements(actor, model, input_names, &output_values, observed_input_state_refs)?;
        let observed_output_fields = observed_output_field_witness_specs(actor, entry, model);

        Ok(Self {
            actor,
            entry,
            model,
            bindings,
            selector_catalog,
            output_values,
            ref_replacements,
            observed_output_fields,
            validated_spawns: BTreeSet::new(),
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
        Ok(LoweredEntryBody { sil: out })
    }

    fn validate_output_value_refs(&self) -> Result<()> {
        let missing = self
            .output_values
            .iter()
            .filter(|value| count_qualified_ref(self.entry.body.tokens(), &value.source) == 0)
            .map(|value| value.source.as_str())
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
                EntryStatement::Plain { bindings, span, .. } => self.lower_plain_statement(out, indent, bindings, *span)?,
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
        push_indent(out, indent);
        out.push_str(&format!("for ({}) {{\n", self.lower_expr(header, None, indent)?));

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
        let expr = self.entry.body.span_text(declaration.initializer).trim().to_string();
        if let Some(state) = declaration.binding.actor_type_state.as_deref() {
            self.lower_actor_type_statement(out, indent, state, name, &expr)?;
            return Ok(());
        }
        let lowered_ty = if self.model.has_state(source_ty) {
            self.bindings.lowered_type(expr.trim()).map(str::to_string).unwrap_or_else(|| self.lower_local_type(source_ty))
        } else {
            self.lower_local_type(source_ty)
        };
        let declared_type = self.entry.body.span_text(declaration.declared_type).trim();
        let type_suffix = declared_type.strip_prefix(source_ty).expect("declared type starts with its parsed source type");
        let emitted_type = format!("{lowered_ty}{type_suffix}");
        let lowered = self.lower_typed_local_initializer(source_ty, &lowered_ty, &expr, indent)?;

        push_indent(out, indent);
        out.push_str(&format!("{emitted_type} {name} = {lowered};\n"));
        let mut binding = BodyBinding::typed(source_ty, lowered_ty);
        if let Some(selector) = self.selector_catalog.get(name)
            && selector.actor_enum == source_ty
        {
            binding = binding.with_selector(selector.clone());
        }
        self.bindings.declare(name, binding);
        Ok(())
    }

    fn lower_plain_statement(&mut self, out: &mut String, indent: usize, bindings: &[EntryBinding], span: Span) -> Result<()> {
        let source = self.entry.body.span_text(span).trim();
        let statement = source.strip_suffix(';').ok_or_else(|| self.error("unterminated statement"))?.trim().to_string();

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
        out.push_str(&self.lower_expr(&statement, None, indent)?);
        out.push_str(";\n");
        for binding in bindings {
            self.declare_sil_binding(binding);
        }
        Ok(())
    }

    fn declare_sil_binding(&mut self, binding: &EntryBinding) {
        let lowered_type = self.lower_local_type(&binding.source_type);
        self.bindings.declare(&binding.name, BodyBinding::typed(&binding.source_type, lowered_type));
    }

    fn validate_unrestricted_output_value(&self, value: &str) -> Result<()> {
        let value = value.trim();
        if !self.output_values.iter().any(|output| output.source == value) {
            return Err(self.error(format!(
                "`{}(...)` expects exactly one current emit or spawn output value; `{value}` is not one",
                word::UNRESTRICTED,
            )));
        }
        Ok(())
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
        let id =
            self.bindings.declare(name, BodyBinding::source_typed(format!("{}<{state}>", word::ACTOR_TYPE)).with_selector(selector));
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
        let template_var = hidden_template_selector_template_name(selector_name);
        let (binding_id, selector) = self
            .bindings
            .selector(selector_name)
            .map(|(id, selector)| (id, selector.clone()))
            .ok_or_else(|| ArgentError::new(format!("unknown actor handle `{selector_name}`")))?;
        if self.bindings.selector_is_materialized(binding_id) {
            return Ok(template_var);
        }
        self.materialize_selector_template(out, indent, &selector)?;
        self.bindings.mark_selector_materialized(binding_id);
        Ok(template_var)
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
        for route in routes {
            let route = self.route_call(route);
            self.lower_route(out, indent, route)?;
        }
        Ok(())
    }

    fn route_call(&self, route: &EntryRoute) -> RouteCall {
        RouteCall {
            output: route.output.clone(),
            actor: self.entry.body.span_text(route.actor).trim().to_string(),
            state: self.entry.body.span_text(route.state).trim().to_string(),
        }
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
        let routes = routes.iter().map(|route| self.route_call(route)).collect();
        self.lower_covenant_outputs_become(out, indent, group, routes)
    }

    fn lower_covenant_outputs_become(
        &mut self,
        out: &mut String,
        indent: usize,
        group: &CovenantGroup<'a>,
        routes: Vec<RouteCall>,
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
        route: RouteCall,
    ) -> Result<()> {
        let actor_expr = context.actor();
        let static_target = match context {
            CovenantOutputContext::Existing { observe, output } => {
                static_observed_actor_target(self.actor, self.entry, observe, output, self.model)?
            }
            CovenantOutputContext::Genesis { .. } => self.model.resolve_static_actor_target(target),
        };
        let local_actor = static_target.and_then(|target| target.in_app_actor()).map(|actor| actor.name.as_str());
        let transition = match local_actor.filter(|target| *target != self.actor.name) {
            Some(target) => Some(self.model.route_transition(&self.actor.name, target).ok_or_else(|| {
                self.error(format!("entry model has no route transition from `{}` to in-app target `{target}`", self.actor.name))
            })?),
            None => None,
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
        let state_ty = if static_target.is_some() {
            contract_state_type_for_actor(actor_expr, self.actor, self.model)?
        } else if state_name == self.actor.state {
            "State".to_string()
        } else {
            state_name.clone()
        };
        let state_expr = route.state.trim();
        let packs_family = transition.is_some_and(|transition| !transition.families_to_pack.is_empty());
        let state_arg = if !packs_family && self.bindings.lowered_type(state_expr).is_some_and(|ty| ty == state_ty) {
            self.lower_expr(state_expr, Some(&state_ty), indent)?
        } else {
            let name = generated_state_name(&route, &state_ty);
            let lowered = if static_target.is_some() {
                self.lower_state_expr_for_actor(actor_expr, transition, state_expr, indent)?
            } else {
                self.lower_state_expr_for_dynamic_state(&state_name, &state_ty, state_expr, indent)?
            };
            push_indent(out, indent);
            out.push_str(&format!("{state_ty} {name} = {lowered};\n"));
            name
        };

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

        push_indent(out, indent);
        out.push_str(&format!(
            "// :: {} become {}.{} -> {}\n",
            context.route_label(),
            context.group_name(),
            context.output_name(),
            actor_expr
        ));
        let output_idx = match context {
            CovenantOutputContext::Existing { .. } => hidden_observed_output_idx_name(context.group_name(), context.output_name()),
            CovenantOutputContext::Genesis { .. } => hidden_spawn_output_idx_name(context.group_name(), context.output_name()),
        };
        if local_actor == Some(self.actor.name.as_str()) {
            push_generated_call(out, indent, "", "validateOutputState", &[output_idx, state_arg]);
        } else if let Some(target) = static_target
            && let Some(input_idx) = template_input_index_for_target(self.actor, self.entry, target, self.model)?
        {
            let target_reference = target.artifact_reference();
            push_generated_call(
                out,
                indent,
                "",
                "validateOutputStateWithInputTemplate",
                &[
                    output_idx,
                    state_arg,
                    input_idx,
                    hidden_witness_prefix_len_name(&target_reference),
                    hidden_witness_suffix_len_name(&target_reference),
                    template,
                ],
            );
        } else if let CovenantOutputContext::Existing { observe, output } = context
            && observed_reuses_input_template(observe, output)
        {
            let input =
                first_observed_input_for_actor(observe, actor_expr).expect("input-template reuse requires a matching observed input");
            let input_spec = observed_input_spec(self.actor, self.entry, observe, input, self.model)?;
            push_generated_call(
                out,
                indent,
                "",
                "validateOutputStateWithInputTemplate",
                &[
                    output_idx,
                    state_arg,
                    hidden_observed_input_idx_name(context.group_name(), &input.name),
                    hidden_observed_actor_prefix_len_name(&input_spec),
                    hidden_observed_actor_suffix_len_name(&input_spec),
                    template,
                ],
            );
        } else {
            let target_reference = static_target.map(|target| target.artifact_reference());
            let prefix = target_reference.as_deref().map_or_else(
                || match (&observed_spec, &spawn_spec) {
                    (Some(spec), None) => hidden_observed_actor_prefix_name(spec),
                    (None, Some(spec)) => hidden_spawn_actor_prefix_name(spec),
                    _ => unreachable!("output context has exactly one witness spec"),
                },
                hidden_witness_prefix_name,
            );
            let suffix = target_reference.as_deref().map_or_else(
                || match (&observed_spec, &spawn_spec) {
                    (Some(spec), None) => hidden_observed_actor_suffix_name(spec),
                    (None, Some(spec)) => hidden_spawn_actor_suffix_name(spec),
                    _ => unreachable!("output context has exactly one witness spec"),
                },
                hidden_witness_suffix_name,
            );
            push_generated_call(
                out,
                indent,
                "",
                "validateOutputStateWithTemplate",
                &[output_idx, state_arg, prefix, suffix, template],
            );
        }
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

    fn lower_state_expr_for_actor(
        &self,
        actor: &str,
        transition: Option<&CompilerRouteTransition>,
        expr: &str,
        indent: usize,
    ) -> Result<String> {
        let state_name = &self.model.actor(actor)?.state;
        let state_ty = contract_state_type_for_actor(actor, self.actor, self.model)?;
        let generated_fields = hidden_template_object_fields_for_actor(self.actor, actor, transition, self.model);
        let force_materialization = transition.is_some_and(|transition| !transition.families_to_pack.is_empty());
        self.lower_state_expr_for_layout(state_name, &state_ty, generated_fields, force_materialization, expr, indent)
    }

    fn lower_state_expr_for_dynamic_state(&self, state_name: &str, state_ty: &str, expr: &str, indent: usize) -> Result<String> {
        let generated_fields = hidden_template_object_fields(self.actor, RouteFieldKind::None, &[], self.model);
        self.lower_state_expr_for_layout(state_name, state_ty, generated_fields, false, expr, indent)
    }

    fn lower_state_expr_for_layout(
        &self,
        state_name: &str,
        state_ty: &str,
        generated_fields: Vec<(String, String)>,
        force_materialization: bool,
        expr: &str,
        indent: usize,
    ) -> Result<String> {
        let expr = expr.trim();
        if !force_materialization && self.bindings.lowered_type(expr).is_some_and(|ty| ty == state_ty) {
            return self.lower_expr(expr, Some(state_ty), indent);
        }
        if let Some((source_state, body)) = split_state_constructor(expr) {
            if self.model.storage_state_name(source_state)? != self.model.storage_state_name(state_name)? {
                return Err(ArgentError::new(format!("state `{source_state}` cannot initialize contract state `{state_name}`")));
            }
            return self.lower_state_object(source_state, body, generated_fields, indent);
        }

        let source_state = if expr == "self.state" {
            Some(self.actor.state.as_str())
        } else {
            self.bindings.source_type(expr).filter(|source_ty| self.model.has_state(source_ty))
        };
        if let Some(source_state) = source_state {
            if self.model.storage_state_name(source_state)? != self.model.storage_state_name(state_name)? {
                return Err(ArgentError::new(format!("state `{source_state}` cannot initialize contract state `{state_name}`")));
            }
            let fields = self
                .model
                .storage_state(state_name)?
                .fields
                .iter()
                .map(|field| {
                    let expr = if expr == "self.state" { field.name.clone() } else { format!("{expr}.{}", field.name) };
                    (field.name.clone(), expr)
                })
                .collect::<Vec<_>>();
            return self.render_state_object(state_name, &fields, generated_fields, indent);
        }

        self.lower_expr(expr, Some(state_ty), indent)
    }

    fn lower_route(&mut self, out: &mut String, indent: usize, route: RouteCall) -> Result<()> {
        if self.bindings.selector(&route.actor).is_some() {
            return self.lower_selector_route(out, indent, route);
        }
        if self.selector_catalog.contains_key(&route.actor) {
            let reason =
                if self.bindings.get(&route.actor).is_some() { "is shadowed by a non-selector binding" } else { "is not visible" };
            return Err(self.error(format!("actor handle `{}` {reason} in this scope", route.actor)));
        }
        self.model.actor_state(&route.actor)?;
        let output_idx = hidden_output_idx_name(&route.output);
        let validation = route_validation_kind(self.actor, &route);

        if validation == RouteValidationKind::ExactScriptPublicKey {
            push_indent(out, indent);
            out.push_str(&format!("// :: become {}\n", route.actor));
            push_generated_binary_require(
                out,
                indent,
                &format!("tx.outputs[{output_idx}].scriptPubKey"),
                "==",
                "tx.inputs[this.activeInputIndex].scriptPubKey",
            );
            return Ok(());
        }

        let transition = (route.actor != self.actor.name).then(|| {
            self.model.route_transition(&self.actor.name, &route.actor).expect("validated non-self route has a planned cut transition")
        });
        let state_ty = contract_state_type_for_actor(&route.actor, self.actor, self.model)?;
        let state_expr = route.state.trim();
        let packs_family = transition.is_some_and(|transition| !transition.families_to_pack.is_empty());
        let state_arg = if !packs_family && self.bindings.lowered_type(state_expr).is_some_and(|ty| ty == state_ty) {
            self.lower_expr(state_expr, Some(&state_ty), indent)?
        } else {
            let name = generated_state_name(&route, &state_ty);
            let lowered = self.lower_state_expr_for_actor(&route.actor, transition, state_expr, indent)?;
            push_indent(out, indent);
            out.push_str(&format!("{state_ty} {name} = {lowered};\n"));
            name
        };

        push_indent(out, indent);
        out.push_str(&format!("// :: become {}\n", route.actor));
        match validation {
            RouteValidationKind::ExactScriptPublicKey => unreachable!("exact continuation returned before state lowering"),
            RouteValidationKind::SameTemplate => {
                push_generated_call(out, indent, "", "validateOutputState", &[output_idx, state_arg]);
            }
            RouteValidationKind::ForeignTemplate => {
                let template = hidden_template_name(&route.actor);
                if let Some(input_idx) = template_input_index_for_actor(self.actor, self.entry, &route.actor, self.model)? {
                    push_generated_call(
                        out,
                        indent,
                        "",
                        "validateOutputStateWithInputTemplate",
                        &[
                            output_idx,
                            state_arg,
                            input_idx,
                            hidden_witness_prefix_len_name(&route.actor),
                            hidden_witness_suffix_len_name(&route.actor),
                            template,
                        ],
                    );
                } else {
                    push_generated_call(
                        out,
                        indent,
                        "",
                        "validateOutputStateWithTemplate",
                        &[
                            output_idx,
                            state_arg,
                            hidden_witness_prefix_name(&route.actor),
                            hidden_witness_suffix_name(&route.actor),
                            template,
                        ],
                    );
                }
            }
        }
        Ok(())
    }

    fn lower_selector_route(&mut self, out: &mut String, indent: usize, route: RouteCall) -> Result<()> {
        let selector = self
            .bindings
            .selector(&route.actor)
            .map(|(_, selector)| selector)
            .ok_or_else(|| ArgentError::new(format!("unknown actor handle `{}`", route.actor)))?
            .clone();
        let output_idx = hidden_output_idx_name(&route.output);
        let layout_actor = selector.variants.first().expect("validated actor selector has at least one variant");
        let transition = self
            .model
            .route_transition(&self.actor.name, layout_actor)
            .expect("validated selector route has a planned cut transition");
        debug_assert!(
            selector.variants.iter().skip(1).all(|actor| self.model.route_transition(&self.actor.name, actor) == Some(transition)),
            "selector variants must use one cut transition"
        );
        let state_ty = contract_state_type_for_actor(layout_actor, self.actor, self.model)?;
        let state_expr = route.state.trim();
        let packs_family = !transition.families_to_pack.is_empty();
        let state_arg = if !packs_family && self.bindings.lowered_type(state_expr).is_some_and(|ty| ty == state_ty) {
            self.lower_expr(state_expr, Some(&state_ty), indent)?
        } else {
            let name = generated_state_name(&route, &state_ty);
            let lowered = self.lower_state_expr_for_actor(layout_actor, Some(transition), state_expr, indent)?;
            push_indent(out, indent);
            out.push_str(&format!("{state_ty} {name} = {lowered};\n"));
            name
        };

        let template = self.ensure_selector_template(out, indent, &route.actor)?;
        push_indent(out, indent);
        out.push_str(&format!("// :: become {}\n", route.actor));
        push_generated_call(
            out,
            indent,
            "",
            "validateOutputStateWithTemplate",
            &[
                output_idx,
                state_arg,
                hidden_template_selector_prefix_name(&route.actor),
                hidden_template_selector_suffix_name(&route.actor),
                template,
            ],
        );
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
        if expr == "self.state" {
            let ty = expected_ty.ok_or_else(|| ArgentError::new("`self.state` requires a target state type during lowering"))?;
            return self.lower_self_state_expr(ty, indent);
        }
        if let Some(value) = parse_digest_call(expr) {
            return self.lower_digest_expr(value);
        }
        if let Some((state_name, body)) = split_state_constructor(expr) {
            return self.lower_state_constructor(state_name, body, indent);
        }
        self.lower_refs(expr)
    }

    fn lower_self_state_expr(&self, ty: &str, indent: usize) -> Result<String> {
        let state_name = if ty == "State" { &self.actor.state } else { ty };
        let fields = self
            .model
            .storage_state(state_name)?
            .fields
            .iter()
            .map(|field| (field.name.clone(), field.name.clone()))
            .collect::<Vec<_>>();
        self.render_state_object_for_state(state_name, &fields, indent)
    }

    fn lower_state_constructor(&self, state_name: &str, body: &str, indent: usize) -> Result<String> {
        self.model.state(state_name)?;
        self.lower_state_object_for_state(state_name, body, indent)
    }

    fn lower_digest_expr(&self, value: &str) -> Result<String> {
        let value = value.trim();
        let state_name = self.bindings.source_type(value).ok_or_else(|| {
            ArgentError::new(format!("`digest(...)` requires a named state value, but `{value}` has no known source type"))
        })?;
        self.model.state(state_name)?;
        state_payload_digest_expr(state_name, value, self.model)
    }

    fn lower_typed_local_initializer(&self, source_ty: &str, lowered_ty: &str, expr: &str, indent: usize) -> Result<String> {
        if self.model.actor_enums.contains_key(source_ty) {
            return self.lower_actor_enum_initializer(source_ty, expr, indent);
        }
        if let Some(state_name) = self.source_state_for_local_type(source_ty)
            && let Some(body) = split_state_object_literal(expr)
        {
            return self.lower_state_object_for_state(&state_name, body, indent);
        }
        if self.model.has_state(source_ty) && self.bindings.lowered_type(expr.trim()).is_none_or(|ty| ty != lowered_ty) {
            let lowered_expr = self.lower_expr(expr, None, indent)?;
            let fields = self
                .model
                .storage_state(source_ty)?
                .fields
                .iter()
                .map(|field| (field.name.clone(), format!("{lowered_expr}.{}", field.name)))
                .collect::<Vec<_>>();
            let generated_fields = hidden_template_object_fields_for_state(self.actor, source_ty, self.model);
            return self.render_state_object(source_ty, &fields, generated_fields, indent);
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

    fn lower_state_object_for_state(&self, state_name: &str, body: &str, indent: usize) -> Result<String> {
        self.model.state(state_name)?;
        let generated_fields = hidden_template_object_fields_for_state(self.actor, state_name, self.model);
        self.lower_state_object(state_name, body, generated_fields, indent)
    }

    fn lower_state_object(
        &self,
        state_name: &str,
        body: &str,
        generated_fields: Vec<(String, String)>,
        indent: usize,
    ) -> Result<String> {
        let raw_fields = parse_state_fields(body);
        if self.model.state(state_name)?.expansion.is_some() {
            return self.render_expanded_state_object(state_name, &raw_fields, generated_fields, indent);
        }
        let fields = raw_fields
            .into_iter()
            .map(|(name, expr)| self.lower_expr(&expr, None, indent + 4).map(|lowered| (name, lowered)))
            .collect::<Result<Vec<_>>>()?;
        self.render_state_object(state_name, &fields, generated_fields, indent)
    }

    fn lower_local_type(&self, source_ty: &str) -> String {
        if self.model.actor_enums.contains_key(source_ty) {
            return "int".to_string();
        }
        if source_ty == word::COVENANT_ID {
            return "byte[32]".to_string();
        }
        let same_storage = match (self.model.storage_state_name(&self.actor.state), self.model.storage_state_name(source_ty)) {
            (Ok(current_storage), Ok(source_storage)) => current_storage == source_storage,
            _ => false,
        };
        if source_ty == self.actor.state || same_storage { "State".to_string() } else { source_ty.to_string() }
    }

    fn source_state_for_local_type(&self, source_ty: &str) -> Option<String> {
        if source_ty == "State" {
            Some(self.actor.state.clone())
        } else if self.model.has_state(source_ty) {
            Some(source_ty.to_string())
        } else {
            None
        }
    }

    fn render_state_object_for_state(&self, state_name: &str, fields: &[(String, String)], indent: usize) -> Result<String> {
        let generated_fields = hidden_template_object_fields_for_state(self.actor, state_name, self.model);
        self.render_state_object(state_name, fields, generated_fields, indent)
    }

    fn render_state_object(
        &self,
        state_name: &str,
        fields: &[(String, String)],
        generated_fields: Vec<(String, String)>,
        indent: usize,
    ) -> Result<String> {
        let field_indent = " ".repeat(indent + 4);
        let close_indent = " ".repeat(indent);
        let mut pending = fields.iter().cloned().collect::<BTreeMap<_, _>>();
        if pending.len() != fields.len() {
            return Err(ArgentError::new(format!("state `{state_name}` constructor contains duplicate fields")));
        }
        let mut out = String::new();
        out.push_str("{\n");
        if !generated_fields.is_empty() {
            out.push_str(&format!("{field_indent}// :: generated fields\n"));
        }
        for (field, expr) in generated_fields {
            out.push_str(&format!("{field_indent}{field}: {expr},\n"));
        }
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

    fn render_expanded_state_object(
        &self,
        state_name: &str,
        fields: &[(String, String)],
        generated_fields: Vec<(String, String)>,
        indent: usize,
    ) -> Result<String> {
        let state = self.model.state(state_name)?;
        let expansion = state.expansion.as_ref().ok_or_else(|| ArgentError::new(format!("state `{state_name}` is not expanded")))?;
        let storage_state = self.model.storage_state(state_name)?;
        let mut pending = fields.iter().cloned().collect::<BTreeMap<_, _>>();
        if pending.len() != fields.len() {
            return Err(ArgentError::new(format!("state `{state_name}` constructor contains duplicate fields")));
        }
        let field_indent = " ".repeat(indent + 4);
        let close_indent = " ".repeat(indent);
        let mut out = String::new();
        out.push_str("{\n");
        if !generated_fields.is_empty() {
            out.push_str(&format!("{field_indent}// :: generated fields\n"));
        }
        for (field, expr) in generated_fields {
            out.push_str(&format!("{field_indent}{field}: {expr},\n"));
        }
        if !storage_state.fields.is_empty() {
            out.push_str(&format!("{field_indent}// :: user declared fields\n"));
        }

        for field in &storage_state.fields {
            if let Some(digest) = expansion.digests.iter().find(|digest| digest.field == field.name) {
                let expr = pending.remove(&digest.field).ok_or_else(|| {
                    ArgentError::new(format!("state `{state_name}` constructor is missing expanded slot `{}`", digest.field))
                })?;
                if expr.trim() == digest.field {
                    out.push_str(&format!("{field_indent}{}: {},\n", field.name, digest.field));
                    continue;
                }
                let (slot_state, slot_body) = split_state_constructor(&expr).ok_or_else(|| {
                    ArgentError::new(format!(
                        "state `{state_name}` constructor slot `{}` must use `{} {{ ... }}`",
                        digest.field, digest.state
                    ))
                })?;
                if slot_state != digest.state {
                    return Err(ArgentError::new(format!(
                        "state `{state_name}` constructor slot `{}` expects `{}`, got `{slot_state}`",
                        digest.field, digest.state
                    )));
                }
                let mut slot_fields = parse_state_fields(slot_body).into_iter().collect::<BTreeMap<_, _>>();
                let payload = state_packed_bytes_expr(&digest.state, self.model, |memory_field, _, _| {
                    let expr = slot_fields.remove(&memory_field.name).ok_or_else(|| {
                        ArgentError::new(format!(
                            "state `{state_name}` constructor slot `{}` is missing field `{}`",
                            digest.field, memory_field.name
                        ))
                    })?;
                    let lowered = self.lower_expr(&expr, None, indent + 4)?;
                    packed_field_expr(&memory_field.ty, &lowered)
                })?;
                if let Some(extra) = slot_fields.keys().next() {
                    return Err(ArgentError::new(format!(
                        "state `{state_name}` constructor slot `{}` has unknown field `{extra}`",
                        digest.field
                    )));
                }
                out.push_str(&format!("{field_indent}{}: blake2b({payload}),\n", field.name));
            } else if field.virtual_slot {
                let raw_expr = pending.remove(&field.name).unwrap_or_else(|| field.name.clone());
                let expr = self.lower_expr(&raw_expr, None, indent + 4)?;
                out.push_str(&format!("{field_indent}{}: {expr},\n", field.name));
            } else {
                let raw_expr = pending
                    .remove(&field.name)
                    .ok_or_else(|| ArgentError::new(format!("state `{state_name}` constructor is missing field `{}`", field.name)))?;
                let expr = self.lower_expr(&raw_expr, None, indent + 4)?;
                out.push_str(&format!("{field_indent}{}: {expr},\n", field.name));
            }
        }
        if let Some(extra) = pending.keys().next() {
            return Err(ArgentError::new(format!("state `{state_name}` constructor has unknown field `{extra}`")));
        }
        out.push_str(&close_indent);
        out.push('}');
        Ok(out)
    }

    fn lower_refs(&self, expr: &str) -> Result<String> {
        // Lower source-level co-spend calls before general reference rewrites.
        let out = lower_co_spent_calls(expr, &self.bindings)?;
        let out = self.ref_replacements.rewrite(&out)?;
        lower_actor_enum_literals(&out, self.model)
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

pub(in crate::compiler::codegen) fn lower_entry_expr(
    actor: &ActorDecl,
    entry: &EntryDecl,
    model: &Model<'_>,
    expr: &str,
    expected_ty: Option<&str>,
) -> Result<String> {
    BodyLowerer::new(actor, entry, model)?.lower_expr(expr, expected_ty, 8)
}

fn generated_state_name(route: &RouteCall, state_ty: &str) -> String {
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

fn parse_digest_call(expr: &str) -> Option<&str> {
    let expr = expr.trim();
    expr.strip_prefix("digest(")?.strip_suffix(')').map(str::trim)
}

fn parse_state_fields(body: &str) -> Vec<(String, String)> {
    split_top_level_commas(body)
        .into_iter()
        .filter_map(|entry| {
            let entry = entry.trim();
            if entry.is_empty() {
                return None;
            }
            let (name, expr) = split_top_level_colon(entry)?;
            Some((name.trim().to_string(), expr.trim().to_string()))
        })
        .collect()
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

fn is_ident(tokens: &[Token], pos: usize, ident: &str) -> bool {
    matches!(tokens.get(pos).map(|token| &token.kind), Some(TokenKind::Ident(candidate)) if candidate == ident)
}

fn is_symbol(tokens: &[Token], pos: usize, symbol: char) -> bool {
    matches!(tokens.get(pos).map(|token| &token.kind), Some(TokenKind::Symbol(candidate)) if *candidate == symbol)
}

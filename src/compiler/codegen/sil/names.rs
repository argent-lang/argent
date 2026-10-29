//! Contract-local names for bound authored references.

use std::collections::{BTreeMap, BTreeSet};

use silverscript_lang::ast::visit::{AstVisitorMut, NameKind, walk_expr_mut};
use silverscript_lang::ast::{ArrayDim, Expr, ExprKind, TypeRef as SilTypeRef};
use silverscript_lang::span::Span;

use crate::codec::encode_hex;
use crate::compiler::model::link::LinkedActor;
use crate::compiler::model::{
    AppCompilationContext, ClauseActorTypeRef, GeneratedFieldId, ObservedActorSide, ObservedActorWitnessSpec,
    ObservedOutputFieldWitnessSpec, PhysicalFieldId, PlannedStateValue, RouteFamily, SourceFieldId, SourceStateId,
    SpawnActorWitnessSpec, StateExpansionWitnessSpec, TemplateWitnessForm, WitnessComponent, WitnessPlan, WitnessRole,
};
use crate::compiler::naming::{is_identifier, to_snake};
use crate::compiler::resolve::{Binding, LocalId, ResolvedName};
use crate::compiler::syntax::body::RouteId;
use crate::compiler::syntax::lexer::{RESERVED_GENERATED_PREFIX, RESERVED_GENERATED_TYPE_PREFIX};
use crate::compiler::syntax::node::{DeclId, ReferenceRole, RootSlot, SymbolKind};
use crate::compiler::syntax::source::Origin;
use crate::error::{ArgentError, Result};

pub(in crate::compiler::codegen) fn witness_role_name(plan: &WitnessPlan, role: WitnessRole) -> String {
    match role {
        WitnessRole::Template { index, component } => {
            let spec = &plan.templates[index];
            match (spec.form, component) {
                (TemplateWitnessForm::Bytes, WitnessComponent::Prefix) => hidden_witness_prefix_name(&spec.actor),
                (TemplateWitnessForm::Bytes, WitnessComponent::Suffix) => hidden_witness_suffix_name(&spec.actor),
                (TemplateWitnessForm::Len, WitnessComponent::Prefix) => hidden_witness_prefix_len_name(&spec.actor),
                (TemplateWitnessForm::Len, WitnessComponent::Suffix) => hidden_witness_suffix_len_name(&spec.actor),
                (_, WitnessComponent::TemplateHash) => unreachable!("template roles have no hash component"),
            }
        }
        WitnessRole::RouteFamily { index } => hidden_route_family_table_name_by_id(&plan.families[index].family_id),
        WitnessRole::Selector { index, component } => {
            let name = &plan.selectors[index].name;
            match component {
                WitnessComponent::Prefix => hidden_template_selector_prefix_name(name),
                WitnessComponent::Suffix => hidden_template_selector_suffix_name(name),
                WitnessComponent::TemplateHash => unreachable!("selector roles have no hash component"),
            }
        }
        WitnessRole::Observed { index, component } => {
            let spec = &plan.observed_actors[index];
            match (spec.side, component) {
                (ObservedActorSide::Input, WitnessComponent::Prefix) => hidden_observed_actor_prefix_len_name(spec),
                (ObservedActorSide::Input, WitnessComponent::Suffix) => hidden_observed_actor_suffix_len_name(spec),
                (ObservedActorSide::Input, WitnessComponent::TemplateHash) => hidden_observed_actor_template_name(spec),
                (ObservedActorSide::Output, WitnessComponent::Prefix) => hidden_observed_actor_prefix_name(spec),
                (ObservedActorSide::Output, WitnessComponent::Suffix) => hidden_observed_actor_suffix_name(spec),
                (ObservedActorSide::Output, WitnessComponent::TemplateHash) => unreachable!("output roles have no hash component"),
            }
        }
        WitnessRole::SpawnIndex { index } => {
            let spec = &plan.spawn_outputs[index];
            hidden_spawn_output_idx_name(&spec.spawn, &spec.handle)
        }
        WitnessRole::ActorType { index, component } => {
            let spec = &plan.actor_type_source_templates[index];
            match (spec.form, component) {
                (TemplateWitnessForm::Bytes, WitnessComponent::Prefix) => hidden_actor_type_source_prefix_name(&spec.source),
                (TemplateWitnessForm::Bytes, WitnessComponent::Suffix) => hidden_actor_type_source_suffix_name(&spec.source),
                (TemplateWitnessForm::Len, WitnessComponent::Prefix) => hidden_actor_type_source_prefix_len_name(&spec.source),
                (TemplateWitnessForm::Len, WitnessComponent::Suffix) => hidden_actor_type_source_suffix_len_name(&spec.source),
                (_, WitnessComponent::TemplateHash) => unreachable!("actor-type roles have no hash component"),
            }
        }
        WitnessRole::StateExpansion { index } => hidden_state_expansion_preimage_name(&plan.state_expansions[index]),
        WitnessRole::ObservedOutputField { index } => hidden_observed_output_field_name(&plan.observed_output_fields[index]),
    }
}

pub(in crate::compiler::codegen) fn hidden_checked_range_index_name() -> String {
    format!("{RESERVED_GENERATED_PREFIX}checked_range_index")
}

pub(super) enum OutputTemporaryRole {
    Source,
    Physical,
}

pub(super) fn hidden_output_temporary_name(route: RouteId, output: &str, role: OutputTemporaryRole) -> String {
    let role = match role {
        OutputTemporaryRole::Source => "source",
        OutputTemporaryRole::Physical => "physical",
    };
    // route id first for uniqueness. authored label is useless from compiler, but useful to readers
    format!("{RESERVED_GENERATED_PREFIX}{}_route_{role}_{}", route.0, to_snake(output))
}

pub(in crate::compiler::codegen) fn hidden_physical_field_init_name(field: &str) -> String {
    let suffix = field.strip_prefix(RESERVED_GENERATED_PREFIX).expect("generated physical fields use the reserved namespace");
    format!("{RESERVED_GENERATED_PREFIX}init_{suffix}")
}

pub(in crate::compiler::codegen) struct SilNames<'m, 'src> {
    model: &'m AppCompilationContext<'src>,
    sites: BTreeMap<(usize, usize), Binding>,
    type_sites: BTreeMap<usize, DeclId>,
    actor_handle_type_sites: BTreeSet<usize>,
    dimension_types: BTreeMap<usize, SilTypeRef>,
    co_spent_sites: BTreeSet<(usize, usize)>,
    digest_operands: BTreeMap<(usize, usize), PlannedStateValue>,
    actor_field_uses: BTreeMap<(usize, usize), SourceFieldId>,
    linked_actor_values: BTreeMap<(usize, usize), String>,
    local_names: BTreeMap<LocalId, String>,
    parameter_ids: BTreeMap<usize, LocalId>,
    generated_fields: BTreeMap<GeneratedFieldId, String>,
}

impl<'m, 'src> SilNames<'m, 'src> {
    pub(in crate::compiler::codegen) fn new(model: &'m AppCompilationContext<'src>, owner: DeclId, root: RootSlot) -> Self {
        let bindings = model.resolution.bindings(owner);
        let global = root == RootSlot::Declaration && owner.kind() == SymbolKind::Function;
        let sites = model
            .resolution
            .bindings(owner)
            .sites
            .iter()
            .filter_map(|(id, binding)| match model.resolution.nodes().node(*id).origin {
                Origin::Authored { start, end, .. } => Some(((start, end), binding.clone())),
                Origin::Generated { .. } => None,
            })
            .collect();
        let type_sites = model
            .resolution
            .bindings(owner)
            .sites
            .iter()
            .filter_map(|(id, binding)| {
                if model.resolution.nodes().node(*id).reference_role != Some(ReferenceRole::Type) {
                    return None;
                }
                let Binding::Source(ResolvedName::Declaration(target)) = binding else {
                    return None;
                };
                match model.resolution.nodes().node(*id).origin {
                    Origin::Authored { start, .. } => Some((start, *target)),
                    Origin::Generated { .. } => None,
                }
            })
            .collect();
        let mut dimension_types = BTreeMap::new();
        let actor_handle_type_sites = model
            .types
            .actor_handle_type_uses
            .keys()
            .filter_map(|site| {
                let node = model.resolution.nodes().node(*site);
                if node.address.owner != owner || node.address.root != root {
                    return None;
                }
                match node.origin {
                    Origin::Authored { start, .. } => Some(start),
                    Origin::Generated { .. } => None,
                }
            })
            .collect();
        for ((site, index), constant) in &bindings.dimension_constants {
            let Origin::Authored { start, .. } = model.resolution.nodes().node(*site).origin else {
                unreachable!("bound array dimensions belong to authored type sites");
            };
            let ty = dimension_types
                .entry(start)
                .or_insert_with(|| model.resolution.type_use(*site).expect("bound array dimension has a parsed type").ty.clone());
            ty.array_dims[*index] = ArrayDim::Constant(model.types.display_names[constant].clone());
        }
        let co_spent_sites = model
            .types
            .co_spent_sites
            .iter()
            .filter_map(|site| {
                let node = model.resolution.nodes().node(*site);
                if node.address.owner != owner || node.address.root != root {
                    return None;
                }
                match node.origin {
                    Origin::Authored { start, end, .. } => Some((start, end)),
                    Origin::Generated { .. } => None,
                }
            })
            .collect();
        let digest_operands = model
            .types
            .digest_operands
            .iter()
            .filter_map(|(site, value)| {
                let node = model.resolution.nodes().node(*site);
                if node.address.owner != owner || node.address.root != root {
                    return None;
                }
                match node.origin {
                    Origin::Authored { start, end, .. } => Some(((start, end), value.clone())),
                    Origin::Generated { .. } => None,
                }
            })
            .collect();
        let actor_field_uses = model
            .types
            .actor_field_uses
            .iter()
            .filter_map(|(site, field)| {
                let node = model.resolution.nodes().node(*site);
                if node.address.owner != owner || node.address.root != root {
                    return None;
                }
                match node.origin {
                    Origin::Authored { start, end, .. } => Some(((start, end), field.clone())),
                    Origin::Generated { .. } => None,
                }
            })
            .collect();
        let linked_actor_values = model
            .linked_actor_values
            .expressions
            .iter()
            .filter_map(|(site, linked)| {
                let node = model.resolution.nodes().node(*site);
                if node.address.owner != owner || node.address.root != root {
                    return None;
                }
                match node.origin {
                    Origin::Authored { start, end, .. } => Some((
                        (start, end),
                        hidden_imported_template_const_name(&ImportedTemplateSpec::from_linked(&model.linked_actors[linked])),
                    )),
                    Origin::Generated { .. } => None,
                }
            })
            .collect();
        let generated_fields = if owner.kind() == SymbolKind::Actor {
            model
                .state_lowering_by_actor
                .get(&owner)
                .into_iter()
                .flat_map(|lowering| lowering.active().physical().fields())
                .filter_map(|field| match field.id() {
                    PhysicalFieldId::Generated(id) => Some((id.clone(), field.sil_name().to_string())),
                    PhysicalFieldId::Storage(_) => None,
                })
                .collect()
        } else {
            BTreeMap::new()
        };

        let mut occupied = model.types.display_names.values().cloned().collect::<BTreeSet<_>>();
        occupied.extend(["State", "this", "tx"].into_iter().map(str::to_string));
        occupied.extend(generated_fields.values().cloned());

        let mut source_names = BTreeMap::new();
        // keep track of constant declared name, which will detects shadowing
        source_names.extend(model.consts.iter().map(|(id, _)| {
            let name = &model.types.display_names[id];
            (name, name.clone())
        }));

        if let Some(actor) = model.actor_models.get(&owner) {
            occupied.extend(actor.functions().map(|function| function.name.clone()));
            if let Ok(source) = model.source_state_id_by_decl(actor.state)
                && let Ok(state) = model.storage_state_by_source(&source)
            {
                occupied.extend(state.fields.iter().map(|field| field.name.clone()));
                source_names.extend(state.fields.iter().map(|field| (&field.name, field.name.clone())));
            }
        }

        let authored = bindings
            .local_names
            .iter()
            .filter(|(id, _)| id.callable == root)
            .map(|(_, source)| if global { format!("gen__glob_{source}") } else { source.clone() })
            .collect::<BTreeSet<_>>();

        let mut local_names = BTreeMap::new();
        for (id, source) in &bindings.local_names {
            if id.callable != root {
                continue;
            }

            // Preserve one spelling for every authored name so Silverscript
            // detects source shadowing, including after hygiene renames.
            if let Some(name) = source_names.get(source) {
                local_names.insert(*id, String::clone(name));
                continue;
            }

            let base = if global { format!("gen__glob_{source}") } else { source.clone() };
            let mut name = base.clone();
            if !matches!(root, RootSlot::Entry(_)) {
                let mut suffix = id.index;
                while occupied.contains(&name) || name != base && authored.contains(&name) {
                    name = format!("{base}__{suffix}");
                    suffix += 1;
                }
            }
            occupied.insert(name.clone());
            source_names.insert(source, name.clone());
            local_names.insert(*id, name);
        }
        let parameter_ids =
            bindings.parameter_ids.iter().filter_map(|((callable, index), id)| (*callable == root).then_some((*index, *id))).collect();
        Self {
            model,
            sites,
            type_sites,
            actor_handle_type_sites,
            dimension_types,
            co_spent_sites,
            digest_operands,
            actor_field_uses,
            linked_actor_values,
            local_names,
            parameter_ids,
            generated_fields,
        }
    }

    pub(in crate::compiler::codegen) fn generated_field_name(&self, id: &GeneratedFieldId) -> Option<&str> {
        self.generated_fields.get(id).map(String::as_str)
    }

    pub(super) fn parameter_name(&self, index: usize) -> Option<&str> {
        self.parameter_ids.get(&index).and_then(|id| self.local_name(*id))
    }

    pub(super) fn local_name(&self, id: LocalId) -> Option<&str> {
        self.local_names.get(&id).map(String::as_str)
    }

    pub(super) fn name(&self, source: &str, _kind: NameKind, span: Span<'_>) -> String {
        match self.sites.get(&(span.start(), span.end())) {
            Some(Binding::Source(ResolvedName::Declaration(id))) => self.model.types.display_names[id].clone(),
            Some(Binding::Source(ResolvedName::AppMember(member))) => self
                .linked_actor_values
                .get(&(span.start(), span.end()))
                .cloned()
                .unwrap_or_else(|| self.model.types.app_member_display_names[member].clone()),
            Some(Binding::Local(id)) => self.local_names.get(id).cloned().unwrap_or_else(|| source.to_string()),
            _ => source.to_string(),
        }
    }

    pub(super) fn lower_type_dimensions(&self, ty: &mut SilTypeRef, span: Span<'_>) {
        let Some(bound) = self.dimension_types.get(&span.start()) else { return };
        for (dimension, lowered) in ty.array_dims.iter_mut().zip(&bound.array_dims) {
            if matches!(dimension, ArrayDim::Constant(_)) {
                *dimension = lowered.clone();
            }
        }
    }

    pub(super) fn as_cast_name(&self, span: Span<'_>) -> Option<String> {
        let ty = self.dimension_types.get(&span.start())?;
        Some(format!("__as_cast_{}", ty.type_name()))
    }

    pub(super) fn type_name(&self, source: &str, span: Span<'_>) -> String {
        match self.type_sites.get(&span.start()) {
            Some(id) => self.model.types.display_names[id].clone(),
            _ => source.to_string(),
        }
    }

    pub(super) fn type_target(&self, span: Span<'_>) -> Option<DeclId> {
        self.type_sites.get(&span.start()).copied()
    }

    pub(super) fn is_actor_handle_type(&self, span: Span<'_>) -> bool {
        self.actor_handle_type_sites.contains(&span.start())
    }

    pub(super) fn co_spent_approved(&self, span: Span<'_>) -> bool {
        self.co_spent_sites.contains(&(span.start(), span.end()))
    }

    pub(super) fn digest_operand(&self, span: Span<'_>) -> Option<&PlannedStateValue> {
        self.digest_operands.get(&(span.start(), span.end()))
    }

    pub(super) fn state_type_source(&self, span: Span<'_>) -> Result<Option<SourceStateId>> {
        let Some(id) = self.type_target(span).filter(|id| id.kind() == SymbolKind::State) else {
            return Ok(None);
        };
        self.model.source_state_id_by_decl(id).map(Some)
    }

    pub(super) fn is_builtin(&self, span: Span<'_>) -> bool {
        matches!(self.sites.get(&(span.start(), span.end())), Some(Binding::Builtin))
    }

    pub(super) fn binding(&self, span: Span<'_>) -> Option<&Binding> {
        self.sites.get(&(span.start(), span.end()))
    }

    /// Return the source field proven at this authored expression site.
    pub(super) fn actor_field(&self, span: Span<'_>) -> Option<&SourceFieldId> {
        self.actor_field_uses.get(&(span.start(), span.end()))
    }

    pub(super) fn enum_ordinal(&self, span: Span<'_>) -> Option<i64> {
        let Some(Binding::EnumVariant { enumeration, actor }) = self.sites.get(&(span.start(), span.end())) else {
            return None;
        };
        let variants = self.model.types.enum_variants.get(enumeration)?;
        i64::try_from(variants.iter().position(|candidate| candidate == actor)?).ok()
    }
}

impl<'ast> AstVisitorMut<'ast> for SilNames<'_, '_> {
    fn visit_name(&mut self, name: &mut String, kind: NameKind, span: Span<'ast>) {
        *name = self.name(name, kind, span);
    }

    fn visit_expr(&mut self, expr: &mut Expr<'ast>) {
        match &mut expr.kind {
            ExprKind::Array { type_ref, type_span, .. } => self.lower_type_dimensions(type_ref, *type_span),
            ExprKind::Call { name, name_span, .. } if name.starts_with("__as_cast_") => {
                if let Some(lowered) = self.as_cast_name(*name_span) {
                    *name = lowered;
                }
            }
            _ => {}
        }
        if let ExprKind::ArrayIndex { source, index } = &expr.kind
            && matches!(&source.kind, ExprKind::Identifier(_))
            && matches!(
                self.binding(source.span),
                Some(Binding::Source(ResolvedName::Declaration(id))) if id.kind() == SymbolKind::ActorEnum
            )
        {
            *expr = (**index).clone();
            self.visit_expr(expr);
            return;
        }
        if let ExprKind::Identifier(_) = &expr.kind
            && let Some(ordinal) = self.enum_ordinal(expr.span)
        {
            expr.kind = ExprKind::Int(ordinal);
            return;
        }
        walk_expr_mut(self, expr);
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(in crate::compiler::codegen) struct ImportedTemplateSpec {
    pub(in crate::compiler::codegen) app: String,
    pub(in crate::compiler::codegen) actor: String,
    pub(in crate::compiler::codegen) hash_hex: String,
}

impl ImportedTemplateSpec {
    pub(in crate::compiler::codegen) fn from_witness_plan(
        witnesses: &WitnessPlan,
        model: &AppCompilationContext<'_>,
    ) -> Result<Vec<Self>> {
        witnesses
            .imported_templates
            .iter()
            .map(|reference| {
                model.linked_actors.get(reference).map(Self::from_linked).ok_or_else(|| {
                    ArgentError::new(format!("planned imported template `{}::{}` has no linked actor", reference.app, reference.actor))
                })
            })
            .collect()
    }

    pub(in crate::compiler::codegen) fn from_linked(actor: &LinkedActor) -> Self {
        Self { app: actor.app.clone(), actor: actor.actor.clone(), hash_hex: encode_hex(&actor.template.hash) }
    }

    pub(in crate::compiler::codegen) fn actor_reference(&self) -> String {
        format!("{}::{}", self.app, self.actor)
    }
}

fn to_upper_camel(input: &str) -> String {
    input
        .split('_')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            chars.next().map_or_else(String::new, |first| first.to_ascii_uppercase().to_string() + chars.as_str())
        })
        .collect()
}

pub(in crate::compiler::codegen) fn hidden_actor_suffix(actor: &str) -> String {
    to_snake(&actor.replace(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_', "_"))
}

pub(super) fn hidden_actor_state_type_name(actor: &str) -> String {
    format!("{RESERVED_GENERATED_TYPE_PREFIX}{}State", to_upper_camel(actor))
}

pub(super) fn hidden_storage_state_type_name(state: &str) -> String {
    format!("{RESERVED_GENERATED_TYPE_PREFIX}Physical{}", to_upper_camel(state))
}

pub(in crate::compiler::codegen) fn hidden_template_name(actor: &str) -> String {
    format!("{RESERVED_GENERATED_PREFIX}{}_template", hidden_actor_suffix(actor))
}

pub(in crate::compiler::codegen) fn route_family_suffix_by_id(family_id: &str) -> String {
    let hub = family_id.strip_prefix("route_family/").and_then(|rest| rest.rsplit('/').next()).unwrap_or(family_id);
    to_snake(hub)
}

pub(in crate::compiler::codegen) fn hidden_route_family_commitment_name_by_id(family_id: &str) -> String {
    format!("{RESERVED_GENERATED_PREFIX}{}_routes_digest", route_family_suffix_by_id(family_id))
}

pub(in crate::compiler::codegen) fn hidden_route_family_table_name(family: &RouteFamily) -> String {
    hidden_route_family_table_name_by_id(&family.id)
}

pub(in crate::compiler::codegen) fn hidden_route_family_table_name_by_id(family_id: &str) -> String {
    format!("{RESERVED_GENERATED_PREFIX}{}_routes", route_family_suffix_by_id(family_id))
}

pub(in crate::compiler::codegen) fn hidden_witness_prefix_name(actor: &str) -> String {
    format!("{RESERVED_GENERATED_PREFIX}{}_prefix", hidden_actor_suffix(actor))
}

pub(in crate::compiler::codegen) fn hidden_witness_suffix_name(actor: &str) -> String {
    format!("{RESERVED_GENERATED_PREFIX}{}_suffix", hidden_actor_suffix(actor))
}

pub(in crate::compiler::codegen) fn hidden_witness_prefix_len_name(actor: &str) -> String {
    format!("{RESERVED_GENERATED_PREFIX}{}_prefix_len", hidden_actor_suffix(actor))
}

pub(in crate::compiler::codegen) fn hidden_witness_suffix_len_name(actor: &str) -> String {
    format!("{RESERVED_GENERATED_PREFIX}{}_suffix_len", hidden_actor_suffix(actor))
}

pub(in crate::compiler::codegen) fn current_template_length_const_name(actor: &str, part: &str) -> String {
    format!("{RESERVED_GENERATED_PREFIX}const_{}_{part}_len", hidden_actor_suffix(actor))
}

pub(in crate::compiler::codegen) fn hidden_template_selector_prefix_name(selector: &str) -> String {
    format!("{RESERVED_GENERATED_PREFIX}{selector}_prefix")
}

pub(in crate::compiler::codegen) fn hidden_template_selector_suffix_name(selector: &str) -> String {
    format!("{RESERVED_GENERATED_PREFIX}{selector}_suffix")
}

pub(super) fn hidden_template_selector_index_name(selector: &str) -> String {
    format!("{RESERVED_GENERATED_PREFIX}{selector}_selector")
}

pub(super) fn hidden_template_selector_template_name(selector: &str) -> String {
    format!("{RESERVED_GENERATED_PREFIX}{selector}_template")
}

pub(in crate::compiler::codegen) fn observed_actor_spec_suffix(spec: &ObservedActorWitnessSpec) -> String {
    spec.source.as_ref().map_or_else(|| actor_expr_suffix(&spec.actor), clause_actor_type_witness_suffix)
}

fn actor_expr_suffix(actor: &str) -> String {
    if let Some(field) = actor.strip_prefix("self.")
        && is_identifier(field)
    {
        return to_snake(field);
    }
    if is_identifier(actor) {
        return hidden_actor_suffix(actor);
    }
    to_snake(&compact_expr(actor).replace(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_', "_"))
}

pub(in crate::compiler::codegen) fn hidden_observed_actor_prefix_name(spec: &ObservedActorWitnessSpec) -> String {
    if let Some(source) = &spec.source {
        return hidden_actor_type_source_prefix_name(source);
    }
    format!("{RESERVED_GENERATED_PREFIX}{}_{}_prefix", spec.observe, observed_actor_spec_suffix(spec))
}

pub(in crate::compiler::codegen) fn hidden_observed_actor_suffix_name(spec: &ObservedActorWitnessSpec) -> String {
    if let Some(source) = &spec.source {
        return hidden_actor_type_source_suffix_name(source);
    }
    format!("{RESERVED_GENERATED_PREFIX}{}_{}_suffix", spec.observe, observed_actor_spec_suffix(spec))
}

pub(in crate::compiler::codegen) fn hidden_observed_actor_prefix_len_name(spec: &ObservedActorWitnessSpec) -> String {
    if let Some(source) = &spec.source {
        return hidden_actor_type_source_prefix_len_name(source);
    }
    format!("{RESERVED_GENERATED_PREFIX}{}_{}_prefix_len", spec.observe, observed_actor_spec_suffix(spec))
}

pub(in crate::compiler::codegen) fn hidden_observed_actor_suffix_len_name(spec: &ObservedActorWitnessSpec) -> String {
    if let Some(source) = &spec.source {
        return hidden_actor_type_source_suffix_len_name(source);
    }
    format!("{RESERVED_GENERATED_PREFIX}{}_{}_suffix_len", spec.observe, observed_actor_spec_suffix(spec))
}

pub(in crate::compiler::codegen) fn hidden_observed_actor_template_name(spec: &ObservedActorWitnessSpec) -> String {
    format!("{RESERVED_GENERATED_PREFIX}{}_{}_template", spec.observe, observed_actor_spec_suffix(spec))
}

pub(in crate::compiler::codegen) fn hidden_imported_template_name(spec: &ImportedTemplateSpec) -> String {
    hidden_template_name(&spec.actor_reference())
}

pub(in crate::compiler::codegen) fn hidden_imported_template_const_name(spec: &ImportedTemplateSpec) -> String {
    format!("{}_const", hidden_imported_template_name(spec))
}

pub(super) fn hidden_spawn_actor_prefix_name(spec: &SpawnActorWitnessSpec) -> String {
    spec.source.as_ref().map_or_else(
        || format!("{RESERVED_GENERATED_PREFIX}spawn_{}_prefix", spawn_actor_spec_suffix(spec)),
        hidden_actor_type_source_prefix_name,
    )
}

pub(super) fn hidden_spawn_actor_suffix_name(spec: &SpawnActorWitnessSpec) -> String {
    spec.source.as_ref().map_or_else(
        || format!("{RESERVED_GENERATED_PREFIX}spawn_{}_suffix", spawn_actor_spec_suffix(spec)),
        hidden_actor_type_source_suffix_name,
    )
}

fn spawn_actor_spec_suffix(spec: &SpawnActorWitnessSpec) -> String {
    spec.source.as_ref().map_or_else(|| actor_expr_suffix(&spec.actor), clause_actor_type_witness_suffix)
}

pub(in crate::compiler::codegen) fn hidden_actor_type_source_prefix_name(source: &ClauseActorTypeRef) -> String {
    format!("{RESERVED_GENERATED_PREFIX}actor_type_{}_prefix", clause_actor_type_witness_suffix(source))
}

pub(in crate::compiler::codegen) fn hidden_actor_type_source_suffix_name(source: &ClauseActorTypeRef) -> String {
    format!("{RESERVED_GENERATED_PREFIX}actor_type_{}_suffix", clause_actor_type_witness_suffix(source))
}

pub(in crate::compiler::codegen) fn hidden_actor_type_source_prefix_len_name(source: &ClauseActorTypeRef) -> String {
    format!("{RESERVED_GENERATED_PREFIX}actor_type_{}_prefix_len", clause_actor_type_witness_suffix(source))
}

pub(in crate::compiler::codegen) fn hidden_actor_type_source_suffix_len_name(source: &ClauseActorTypeRef) -> String {
    format!("{RESERVED_GENERATED_PREFIX}actor_type_{}_suffix_len", clause_actor_type_witness_suffix(source))
}

pub(in crate::compiler::codegen) fn clause_actor_type_witness_suffix(source: &ClauseActorTypeRef) -> String {
    match source {
        ClauseActorTypeRef::StateField { field, .. } => format!("self_{}", field.field()),
        ClauseActorTypeRef::EntryArgument { name, .. } => format!("arg_{name}"),
    }
}

pub(in crate::compiler::codegen) fn hidden_state_expansion_preimage_name(spec: &StateExpansionWitnessSpec) -> String {
    format!("{RESERVED_GENERATED_PREFIX}{}_{}_preimage", to_snake(&spec.field), to_snake(&spec.memory_state))
}

pub(in crate::compiler::codegen) fn hidden_state_expansion_field_name(spec: &StateExpansionWitnessSpec, field: &str) -> String {
    format!("{RESERVED_GENERATED_PREFIX}{}_{}", to_snake(&spec.field), to_snake(field))
}

pub(in crate::compiler::codegen) fn hidden_observed_output_field_name(spec: &ObservedOutputFieldWitnessSpec) -> String {
    format!("{RESERVED_GENERATED_PREFIX}{}_{}_next_{}", spec.observe, spec.handle, to_snake(&spec.field))
}

pub(in crate::compiler::codegen) fn hidden_observe_cov_id_name(observe: &str) -> String {
    format!("{RESERVED_GENERATED_PREFIX}{observe}_cov_id")
}

pub(in crate::compiler::codegen) fn hidden_observed_input_idx_name(observe: &str, handle: &str) -> String {
    format!("{RESERVED_GENERATED_PREFIX}{observe}_{handle}_input_idx")
}

pub(in crate::compiler::codegen) fn hidden_observed_output_idx_name(observe: &str, handle: &str) -> String {
    format!("{RESERVED_GENERATED_PREFIX}{observe}_{handle}_output_idx")
}

pub(in crate::compiler::codegen) fn hidden_spawn_output_idx_name(spawn: &str, handle: &str) -> String {
    format!("{RESERVED_GENERATED_PREFIX}{spawn}_{handle}_output_idx")
}

pub(in crate::compiler::codegen) fn hidden_spawn_preimage_name(spawn: &str) -> String {
    format!("{RESERVED_GENERATED_PREFIX}{spawn}_genesis_preimage")
}

pub(super) fn hidden_observed_input_state_name(observe: &str, handle: &str) -> String {
    format!("{RESERVED_GENERATED_PREFIX}{observe}_{handle}_state")
}

pub(super) fn hidden_consumed_input_state_name(handle: &str) -> String {
    format!("{RESERVED_GENERATED_PREFIX}{handle}_state")
}

pub(super) fn hidden_consumed_input_authored_cache_name(handle: &str) -> String {
    format!("{RESERVED_GENERATED_PREFIX}{handle}_authored_states")
}

pub(super) fn hidden_consumed_input_field_cache_name(handle: &str, field: &str) -> String {
    format!("{RESERVED_GENERATED_PREFIX}{handle}_{field}_values")
}

pub(in crate::compiler::codegen) fn observed_actor_side_label(side: ObservedActorSide) -> &'static str {
    match side {
        ObservedActorSide::Input => "input",
        ObservedActorSide::Output => "output",
    }
}

pub(in crate::compiler::codegen) fn hidden_cov_id_name() -> String {
    format!("{RESERVED_GENERATED_PREFIX}cov_id")
}

pub(in crate::compiler::codegen) fn hidden_input_idx_name(input: &str) -> String {
    format!("{RESERVED_GENERATED_PREFIX}{input}_input_idx")
}

pub(in crate::compiler::codegen) fn hidden_input_count_name(input: &str) -> String {
    format!("{RESERVED_GENERATED_PREFIX}{input}_count")
}

pub(in crate::compiler::codegen) fn hidden_input_position_name(input: &str) -> String {
    format!("{RESERVED_GENERATED_PREFIX}{input}_position")
}

pub(in crate::compiler::codegen) fn hidden_output_idx_name(output: &str) -> String {
    format!("{RESERVED_GENERATED_PREFIX}{output}_output_idx")
}

pub(in crate::compiler::codegen) fn hidden_output_count_name(output: &str) -> String {
    format!("{RESERVED_GENERATED_PREFIX}{output}_output_count")
}

pub(super) fn hidden_output_position_name(output: &str) -> String {
    format!("{RESERVED_GENERATED_PREFIX}{output}_output_position")
}

pub(in crate::compiler::codegen) fn hidden_range_index_arg_name() -> String {
    format!("{RESERVED_GENERATED_PREFIX}range_index")
}

pub(in crate::compiler::codegen) fn hidden_range_count_arg_name() -> String {
    format!("{RESERVED_GENERATED_PREFIX}range_count")
}

pub(in crate::compiler::codegen) fn compact_expr(input: &str) -> String {
    let without_comments =
        input.lines().map(|line| line.split_once("//").map(|(code, _)| code).unwrap_or(line)).collect::<Vec<_>>().join(" ");
    let compact = without_comments.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = compact.chars();
    let prefix = chars.by_ref().take(96).collect::<String>();
    if chars.next().is_some() { format!("{prefix}...") } else { compact }
}

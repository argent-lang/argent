//! Declared source-value types before actor handles are erased to Sil byte arrays.

use std::collections::{BTreeMap, BTreeSet};

use silverscript_lang::ast::{ArrayDim, Expr, ExprKind, Span};

use crate::compiler::resolve::{ActorSourceBinding, Binding, LocalId, ResolvedDeclaration, ResolvedModules, ResolvedName};
use crate::compiler::syntax::node::{ChildEdge, DeclId, NodeId, RootSlot, SourceNodeCursor, SymbolKind};
use crate::compiler::syntax::source::Origin;
use crate::compiler::syntax::{Cardinality, word};

use super::ConstResolver;
use super::types::{CallableId, CallableSignature, ResolvedType, ResolvedTypeBase, TypeTable};

#[derive(Clone, Debug)]
pub(super) struct SourceValueType {
    pub(super) base: ResolvedTypeBase,
    pub(super) dimensions: Vec<ArrayDim>,
    /// Only a direct linked actor literal supports both its authored and base views.
    pub(super) literal_base: Option<DeclId>,
}

impl SourceValueType {
    pub(super) fn declared(ty: &ResolvedType) -> Self {
        let dimensions = match ty.array {
            Some(crate::compiler::syntax::ArrayDim::Dynamic) => vec![ArrayDim::Dynamic],
            Some(crate::compiler::syntax::ArrayDim::Fixed(length)) => vec![ArrayDim::Fixed(length)],
            None => Vec::new(),
        };
        Self { base: ty.base.clone(), dimensions, literal_base: None }
    }

    pub(super) fn element(&self) -> Option<Self> {
        let mut element = self.clone();
        element.dimensions.pop()?;
        Some(element)
    }

    pub(super) fn has_proven_shape_mismatch(&self, other: &Self) -> bool {
        self.dimensions.len() != other.dimensions.len()
            || self.dimensions.iter().zip(&other.dimensions).any(|(left, right)| match (left, right) {
                (ArrayDim::Fixed(left), ArrayDim::Fixed(right)) => left != right,
                (ArrayDim::Dynamic, ArrayDim::Fixed(_)) | (ArrayDim::Fixed(_), ArrayDim::Dynamic) => true,
                _ => false,
            })
    }
}

/// Queries bound declarations, not initializer provenance or generated Sil types.
pub(super) struct SourceValueResolver<'a, 'src> {
    pub(super) program: &'a ResolvedModules<'src>,
    types: &'a TypeTable,
    pub(super) owner: DeclId,
    references: BTreeMap<(usize, usize), Binding>,
    type_sites: BTreeMap<usize, SourceValueType>,
    locals: BTreeMap<LocalId, SourceValueType>,
    observed_inputs: BTreeMap<(LocalId, String), SourceValueType>,
    actor_state: Option<DeclId>,
}

impl<'a, 'src> SourceValueResolver<'a, 'src> {
    pub(super) fn new(program: &'a ResolvedModules<'src>, types: &'a TypeTable, owner: DeclId, constants: &ConstResolver) -> Self {
        let bindings = program.bindings(owner);
        let mut references = BTreeMap::new();
        let mut type_sites = BTreeMap::new();
        let mut indexed_types = BTreeMap::new();
        for (site, binding) in &bindings.sites {
            let node = program.nodes().node(*site);
            let Origin::Authored { start, end, .. } = node.origin else { continue };
            references.insert((start, end), binding.clone());
            let Some(authored) = program.type_use(*site) else { continue };
            let base = match binding {
                Binding::Source(ResolvedName::Declaration(id)) if authored.actor_state.is_some() => ResolvedTypeBase::ActorHandle(*id),
                Binding::Source(ResolvedName::Declaration(id)) if id.kind() == SymbolKind::State => ResolvedTypeBase::State(*id),
                Binding::Source(ResolvedName::Declaration(id)) if id.kind() == SymbolKind::ActorEnum => {
                    ResolvedTypeBase::ActorEnum(*id)
                }
                Binding::Source(ResolvedName::AppMember(member)) if member.actor.kind() == SymbolKind::ActorEnum => {
                    ResolvedTypeBase::ActorEnum(member.actor)
                }
                _ => ResolvedTypeBase::Builtin(authored.ty.base.type_name()),
            };
            let dimensions = authored
                .ty
                .array_dims
                .iter()
                .enumerate()
                .map(|(index, dimension)| {
                    if let ArrayDim::Constant(_) = dimension
                        && let Some(constant) = bindings.dimension_constants.get(&(*site, index))
                        && let Some(name) = types.display_names.get(constant)
                        && let Ok(value) = constants.resolve_int(name)
                        && let Ok(length) = usize::try_from(value)
                    {
                        return ArrayDim::Fixed(length);
                    }
                    dimension.clone()
                })
                .collect();
            let value = SourceValueType { base, dimensions, literal_base: None };
            type_sites.insert(start, value.clone());
            indexed_types.insert(*site, value);
        }
        let mut locals = BTreeMap::new();
        for (site, binding) in &bindings.sites {
            let Binding::Local(local) = binding else { continue };
            let mut address = program.nodes().node(*site).address.clone();
            if address.children.pop() != Some(ChildEdge::BindingName) {
                continue;
            }
            address.children.push(ChildEdge::TypeUse);
            if let Some(ty) = program.nodes().find(&address).and_then(|site| indexed_types.get(&site)) {
                locals.insert(*local, ty.clone());
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
            if let Some(ty) = ty {
                locals.insert(*local, SourceValueType::declared(ty));
            }
        }
        let mut resolver = Self {
            program,
            types,
            owner,
            references,
            type_sites,
            locals,
            observed_inputs: BTreeMap::new(),
            actor_state: types.actor_states.get(&owner).copied(),
        };
        resolver.seed_input_states();
        resolver
    }

    fn actor_state(&self, actor: DeclId) -> Option<DeclId> {
        let ResolvedDeclaration::Actor(declaration) = self.program.declaration(actor) else { return None };
        match self.program.bindings(actor).names.get(&declaration.state)? {
            ResolvedName::Declaration(state) => Some(*state),
            _ => None,
        }
    }

    fn interaction_state(&self, site: NodeId) -> Option<DeclId> {
        let bindings = self.program.bindings(self.owner);
        if let Some(state) = bindings.open_state_targets.get(&site) {
            return Some(*state);
        }
        if let Some(target) = bindings.actor_targets.get(&site) {
            let actor = match target {
                ResolvedName::Declaration(actor) => *actor,
                ResolvedName::AppMember(member) => member.actor,
                _ => return None,
            };
            return self.actor_state(actor);
        }
        let value = match bindings.local_actor_targets.get(&site)? {
            ActorSourceBinding::Local(local) => self.locals.get(local)?.clone(),
            ActorSourceBinding::StateField(index) => {
                let state = self.storage_state(self.actor_state?)?;
                SourceValueType::declared(self.types.state_fields.get(&(state, *index))?)
            }
        };
        match value.base {
            ResolvedTypeBase::ActorHandle(state) => Some(state),
            _ => None,
        }
    }

    fn seed_input_states(&mut self) {
        let ResolvedDeclaration::Actor(actor) = self.program.declaration(self.owner) else { return };
        let bindings = self.program.bindings(self.owner);
        let state_value = |state, cardinality: &Cardinality| SourceValueType {
            base: ResolvedTypeBase::State(state),
            dimensions: if matches!(cardinality, Cardinality::One) { Vec::new() } else { vec![ArrayDim::Dynamic] },
            literal_base: None,
        };
        for (entry_index, entry) in actor.entries.iter().enumerate() {
            for (index, input) in entry.consumes.iter().enumerate() {
                if let Some(local) = bindings.entry_consumes.get(&(entry_index, index))
                    && let Some(target) = bindings.entry_consume_targets.get(&(entry_index, index))
                    && let Some(state) = self.actor_state(*target)
                {
                    self.locals.insert(*local, state_value(state, &input.cardinality));
                }
            }
            for (observe_index, observe) in entry.observes.iter().enumerate() {
                let Some(group) = bindings.entry_observes.get(&(entry_index, observe_index)) else { continue };
                for (input_index, input) in observe.inputs.iter().enumerate() {
                    let cursor = SourceNodeCursor::new(self.owner, RootSlot::Entry(entry_index))
                        .child(ChildEdge::Observe(observe_index))
                        .child(ChildEdge::ObservedInput(input_index))
                        .child(ChildEdge::ActorTarget);
                    if let Some(site) = self.program.nodes().find(&cursor.address)
                        && let Some(state) = self.interaction_state(site)
                    {
                        if bindings.open_state_targets.contains_key(&site)
                            && let Some(ActorSourceBinding::Local(local)) = bindings.local_actor_targets.get(&site)
                        {
                            self.locals.insert(
                                *local,
                                SourceValueType {
                                    base: ResolvedTypeBase::ActorHandle(state),
                                    dimensions: Vec::new(),
                                    literal_base: None,
                                },
                            );
                        }
                        self.observed_inputs.insert((*group, input.name.clone()), state_value(state, &input.cardinality));
                    }
                }
            }
        }
    }

    pub(super) fn binding(&self, span: Span<'_>) -> Option<&Binding> {
        self.references.get(&(span.start(), span.end()))
    }

    pub(super) fn type_at(&self, span: Span<'_>) -> Option<SourceValueType> {
        self.type_sites.get(&span.start()).cloned()
    }

    pub(super) fn callable(&self, span: Span<'_>) -> Option<&CallableSignature> {
        let callable = match self.binding(span)? {
            Binding::Source(ResolvedName::Declaration(owner)) if owner.kind() == SymbolKind::Function => {
                CallableId { owner: *owner, member: None }
            }
            Binding::ActorHelper(member) => CallableId { owner: self.owner, member: Some(*member) },
            _ => return None,
        };
        self.types.callables.get(&callable)
    }

    pub(super) fn named_value(&self, span: Span<'_>, name: &str) -> Option<SourceValueType> {
        match self.binding(span)? {
            Binding::Local(local) => self.locals.get(local).cloned(),
            Binding::ActorField => self.actor_state.and_then(|state| {
                self.field(&SourceValueType { base: ResolvedTypeBase::State(state), dimensions: Vec::new(), literal_base: None }, name)
            }),
            Binding::Source(ResolvedName::Declaration(id)) if id.kind() == SymbolKind::Const => {
                self.types.constants.get(id).map(SourceValueType::declared)
            }
            Binding::EnumVariant { enumeration, .. } => {
                Some(SourceValueType { base: ResolvedTypeBase::ActorEnum(*enumeration), dimensions: Vec::new(), literal_base: None })
            }
            Binding::Source(ResolvedName::Declaration(id)) if id.kind() == SymbolKind::ActorEnum => Some(SourceValueType {
                base: ResolvedTypeBase::ActorEnum(*id),
                dimensions: vec![ArrayDim::Inferred],
                literal_base: None,
            }),
            Binding::Source(ResolvedName::AppMember(member)) if member.actor.kind() == SymbolKind::ActorEnum => {
                Some(SourceValueType {
                    base: ResolvedTypeBase::ActorEnum(member.actor),
                    dimensions: vec![ArrayDim::Inferred],
                    literal_base: None,
                })
            }
            Binding::Source(ResolvedName::AppMember(member)) if member.actor.kind() == SymbolKind::Actor => {
                let state = self.actor_state(member.actor)?;
                Some(SourceValueType {
                    base: ResolvedTypeBase::ActorHandle(state),
                    dimensions: Vec::new(),
                    literal_base: self.storage_state(state),
                })
            }
            _ => None,
        }
    }

    fn storage_state(&self, mut state: DeclId) -> Option<DeclId> {
        let mut visited = BTreeSet::new();
        loop {
            if !visited.insert(state) {
                return None;
            }
            let ResolvedDeclaration::State(declaration) = self.program.declaration(state) else { return None };
            let Some(expansion) = &declaration.expansion else { return Some(state) };
            let ResolvedName::Declaration(base) = *self.program.bindings(state).names.get(&expansion.base)? else { return None };
            state = base;
        }
    }

    pub(super) fn field(&self, source: &SourceValueType, field: &str) -> Option<SourceValueType> {
        if !source.dimensions.is_empty() {
            return None;
        }
        let ResolvedTypeBase::State(state) = source.base else { return None };
        let ResolvedDeclaration::State(declaration) = self.program.declaration(state) else { return None };
        if let Some(expansion) = &declaration.expansion {
            if let Some(digest) = expansion.digests.iter().find(|digest| digest.field == field) {
                let ResolvedName::Declaration(state) = *self.program.bindings(state).names.get(&digest.state)? else { return None };
                return Some(SourceValueType { base: ResolvedTypeBase::State(state), dimensions: Vec::new(), literal_base: None });
            }
            let storage = self.storage_state(state)?;
            return self.field(
                &SourceValueType { base: ResolvedTypeBase::State(storage), dimensions: Vec::new(), literal_base: None },
                field,
            );
        }
        let index = declaration.fields.iter().position(|candidate| candidate.name == field)?;
        if let Some(ty) = self.types.state_fields.get(&(state, index)) {
            return Some(SourceValueType::declared(ty));
        }
        // A linked actor's state need not belong to the selected source closure.
        TypeTable::resolve_type(self.program, state, &declaration.fields[index].ty).ok().as_ref().map(SourceValueType::declared)
    }

    pub(super) fn expression(&self, expr: &Expr<'_>) -> Option<SourceValueType> {
        match &expr.kind {
            ExprKind::Identifier(name) => self.named_value(expr.span, name),
            ExprKind::Call { name, name_span, args, .. } => {
                if name == word::STATE && matches!(self.binding(*name_span), Some(Binding::Builtin)) {
                    let [reference] = args.as_slice() else { return None };
                    return self.expression(reference);
                }
                self.callable(*name_span)?.result.as_ref().map(SourceValueType::declared)
            }
            ExprKind::ArrayIndex { source, .. } => self.expression(source)?.element(),
            ExprKind::FieldAccess { source, field, .. } => {
                if let ExprKind::FieldAccess { source: group, field: inputs, .. } = &source.kind
                    && inputs == "inputs"
                    && let Some(Binding::Local(group)) = self.binding(group.span)
                    && let Some(ty) = self.observed_inputs.get(&(*group, field.clone()))
                {
                    return Some(ty.clone());
                }
                if matches!(self.binding(source.span), Some(Binding::RuntimeRoot))
                    && matches!(&source.kind, ExprKind::Identifier(name) if name == "self")
                {
                    let state = self.actor_state?;
                    return self.field(
                        &SourceValueType { base: ResolvedTypeBase::State(state), dimensions: Vec::new(), literal_base: None },
                        field,
                    );
                }
                self.field(&self.expression(source)?, field)
            }
            ExprKind::StructLiteral { name_span, .. } | ExprKind::New { name_span, .. } => self.type_at(*name_span),
            ExprKind::Array { type_span, .. } => self.type_at(*type_span),
            ExprKind::Append { source, args, .. } => {
                let mut ty = self.expression(source)?;
                if let ArrayDim::Fixed(length) = ty.dimensions.last_mut()? {
                    *length = length.checked_add(args.len())?;
                }
                Some(ty)
            }
            ExprKind::Slice { source, .. } => {
                let mut ty = self.expression(source)?;
                *ty.dimensions.last_mut()? = ArrayDim::Dynamic;
                Some(ty)
            }
            ExprKind::Split { source, .. } => {
                let mut ty = self.expression(source)?;
                *ty.dimensions.last_mut()? = ArrayDim::Inferred;
                Some(ty)
            }
            _ => None,
        }
    }

    pub(super) fn display(&self, ty: &SourceValueType) -> String {
        let source_name = |id| {
            self.program
                .source_declaration_name(self.owner.module, id)
                .unwrap_or_else(|| self.program.declaration(id).name().to_string())
        };
        let mut name = match &ty.base {
            ResolvedTypeBase::ActorHandle(state) => format!("actor_type<{}>", source_name(*state)),
            ResolvedTypeBase::ActorEnum(id) | ResolvedTypeBase::State(id) | ResolvedTypeBase::Actor(id) => source_name(*id),
            ResolvedTypeBase::Builtin(name) => name.clone(),
        };
        for dimension in &ty.dimensions {
            match dimension {
                ArrayDim::Fixed(length) => name.push_str(&format!("[{length}]")),
                ArrayDim::Constant(length) => name.push_str(&format!("[{length}]")),
                ArrayDim::Dynamic | ArrayDim::Inferred => name.push_str("[]"),
            }
        }
        name
    }
}

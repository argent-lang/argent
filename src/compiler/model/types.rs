//! Source declaration and type identities used by the selected app model.

use std::collections::{BTreeMap, BTreeSet};

use crate::compiler::resolve::{AppMember, Binding, LocalId, ResolvedDeclaration, ResolvedModules, ResolvedName};
use crate::compiler::syntax::body::{EntrySuccessor, RouteArity};
use crate::compiler::syntax::node::{ChildEdge, DeclId, EntryId, NodeId, RootSlot, SourceNodeCursor, SymbolKind};
use crate::compiler::syntax::source::Origin;
use crate::compiler::syntax::{ActorDecl, ArrayDim, AuthoredEntryStatement, Cardinality, EntryDecl, RouteId, TypeRef, word};
use crate::error::{ArgentError, Result};
use silverscript_lang::ast::visit::{AstVisitorMut, walk_expr_mut};
use silverscript_lang::ast::{
    ArrayDim as SilArrayDim, Expr as SilExpr, ExprKind as SilExprKind, Statement as SilStatement, TypeBase as SilTypeBase,
    TypeRef as SilTypeRef,
};

use super::{
    AppCompilationContext, InteractionSource, ResolvedSuccessor, SourceFieldId, SourceStateId, observed_open_state_for_decl,
    spawn_target_state,
};

/// One authored state identity and its scalar or array shape.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PlannedStateValue {
    pub(crate) source: SourceStateId,
    pub(crate) shape: StateValueShape,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StateValueShape {
    Scalar,
    FixedArray(FixedArrayLength),
    DynamicArray,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FixedArrayLength {
    Known(usize),
    Unresolved,
}

/// State-valued positions in one callable's signature.
#[derive(Clone, Debug)]
pub(crate) struct CallableSignaturePlan {
    pub(crate) params: Vec<Option<PlannedStateValue>>,
    pub(crate) result: Option<PlannedStateValue>,
}

/// State-valued facts required by one actor before Sil representation is chosen.
#[derive(Clone, Debug)]
pub(crate) struct ActorValuePlan {
    pub(crate) required_sources: BTreeSet<SourceStateId>,
    pub(crate) constant_ids: BTreeMap<DeclId, PlannedStateValue>,
    pub(crate) signature_ids: BTreeMap<CallableId, CallableSignaturePlan>,
    pub(crate) entry_param_ids: BTreeMap<(EntryId, usize), PlannedStateValue>,
    pub(crate) field_values: BTreeMap<SourceFieldId, PlannedStateValue>,
}

impl ActorValuePlan {
    pub(crate) fn new(actor_id: DeclId, actor: &ActorDecl, model: &AppCompilationContext<'_>) -> Result<Self> {
        let mut required_sources = model.state_sources().cloned().collect::<BTreeSet<_>>();
        required_sources.extend(
            model.types.node_state_uses.values().map(|id| model.source_state_id_by_decl(*id)).collect::<Result<BTreeSet<_>>>()?,
        );
        let mut constant_ids = BTreeMap::new();
        for (id, ty) in &model.types.constants {
            if let Some(value) = Self::type_value(model, ty)? {
                required_sources.insert(value.source.clone());
                constant_ids.insert(*id, value.clone());
            }
        }
        let mut signature_ids = BTreeMap::new();
        let mut entry_param_ids = BTreeMap::new();
        for (id, signature) in model.types.callables.iter().filter(|(id, _)| id.member.is_none() || id.owner == actor_id) {
            let params = signature.params.iter().map(|param| Self::type_value(model, param)).collect::<Result<Vec<_>>>()?;
            let result = signature.result.as_ref().map(|ty| Self::type_value(model, ty)).transpose()?.flatten();
            required_sources.extend(params.iter().flatten().chain(result.iter()).map(|value| value.source.clone()));
            let plan = CallableSignaturePlan { params, result };
            signature_ids.insert(*id, plan.clone());
        }
        for (index, entry) in actor.entries.iter().enumerate() {
            let entry_model = model.entry_model_by_id(EntryId { actor: actor_id, index })?;
            for (index, _) in entry.params.iter().enumerate() {
                let ty = model.types.entry_params.get(&(entry_model.id.actor, entry_model.id.index, index)).ok_or_else(|| {
                    ArgentError::new(format!("entry `{}::{}` parameter {index} has no resolved type", actor.name, entry.name))
                })?;
                if let Some(value) = Self::type_value(model, ty)? {
                    required_sources.insert(value.source.clone());
                    entry_param_ids.insert((entry_model.id, index), value);
                }
            }
            for route in entry_model.routes() {
                let ResolvedSuccessor::Constructed { .. } = &route.successor else {
                    continue;
                };
                for target in model.route_target_ids_by_id(entry_model.id, route)? {
                    required_sources.insert(model.static_actor_source_state(&target)?);
                }
            }
            for group in entry_model.groups() {
                for interaction in group.inputs() {
                    for target in interaction.target().static_actors() {
                        required_sources.insert(model.static_actor_source_state(target)?);
                    }
                }
            }
            for group in entry_model.existing_groups().chain(entry_model.genesis_groups()) {
                for interaction in group.outputs() {
                    for target in interaction.target().static_actors() {
                        required_sources.insert(model.static_actor_source_state(target)?);
                    }
                }
            }
        }
        let mut pending = required_sources.iter().cloned().collect::<Vec<_>>();
        let mut cursor = 0;
        let mut field_values = BTreeMap::new();
        while let Some(source) = pending.get(cursor).cloned() {
            let storage = model.storage_state_by_source(&source)?;
            let relation = model
                .state_lowering_by_id(actor_id)?
                .source_representation(&source)
                .ok_or_else(|| ArgentError::new(format!("state `{}` has no source representation", source.as_str())))?
                .source_to_storage();
            let storage_source = model.storage_source_id(&source);
            let storage_id = model.state_decl_id_by_source(storage_source);
            let linked_storage = storage_id.is_none().then(|| storage_source.clone());
            for (index, field) in storage.fields.iter().enumerate() {
                let relation_field =
                    relation.fields().iter().find(|candidate| candidate.storage().field() == field.name).ok_or_else(|| {
                        ArgentError::new(format!("state `{}` field `{}` has no source relation", source.as_str(), field.name))
                    })?;
                let value = if let Some(expanded) = relation_field.expanded_state() {
                    Some(PlannedStateValue { source: expanded.clone(), shape: StateValueShape::Scalar })
                } else if let Some(id) = storage_id {
                    let ty = model.types.state_fields.get(&(id, index)).ok_or_else(|| {
                        ArgentError::new(format!("state `{}` field `{}` has no resolved type", storage.name, field.name))
                    })?;
                    Self::type_value(model, ty)?
                } else if let Some(target) = linked_storage.as_ref().and_then(|storage_source| {
                    model.linked_field_sources.get(&SourceFieldId::new(storage_source.clone(), &field.name))
                }) {
                    let shape = match field.ty.array {
                        None => StateValueShape::Scalar,
                        Some(ArrayDim::Fixed(len)) => StateValueShape::FixedArray(FixedArrayLength::Known(len)),
                        Some(ArrayDim::Dynamic) => StateValueShape::DynamicArray,
                    };
                    Some(PlannedStateValue { source: target.clone(), shape })
                } else {
                    None
                };
                if let Some(value) = value {
                    let id = value.source.clone();
                    field_values.insert(SourceFieldId::new(source.clone(), &field.name), value);
                    if required_sources.insert(id.clone()) {
                        pending.push(id);
                    }
                }
            }
            cursor += 1;
        }
        Ok(Self { required_sources, constant_ids, signature_ids, entry_param_ids, field_values })
    }

    fn type_value(model: &AppCompilationContext<'_>, ty: &ResolvedType) -> Result<Option<PlannedStateValue>> {
        let ResolvedTypeBase::State(state) = ty.base else { return Ok(None) };
        let shape = match ty.array {
            None => StateValueShape::Scalar,
            Some(ArrayDim::Fixed(len)) => StateValueShape::FixedArray(FixedArrayLength::Known(len)),
            Some(ArrayDim::Dynamic) => StateValueShape::DynamicArray,
        };
        Ok(Some(PlannedStateValue { source: model.source_state_id_by_decl(state)?, shape }))
    }
}

impl PlannedStateValue {
    /// Apply a bound source identity to an authored scalar or array type site.
    pub(crate) fn from_bound_type(source: SourceStateId, ty: &SilTypeRef, inferred_len: Option<usize>) -> Option<Self> {
        let shape = match ty.array_dims.as_slice() {
            [] => StateValueShape::Scalar,
            [SilArrayDim::Fixed(len)] => StateValueShape::FixedArray(FixedArrayLength::Known(*len)),
            [SilArrayDim::Dynamic] => StateValueShape::DynamicArray,
            [SilArrayDim::Inferred] => {
                StateValueShape::FixedArray(inferred_len.map_or(FixedArrayLength::Unresolved, FixedArrayLength::Known))
            }
            [SilArrayDim::Constant(_)] => StateValueShape::FixedArray(FixedArrayLength::Unresolved),
            _ => return None,
        };
        Some(Self { source, shape })
    }

    pub(crate) fn source(&self) -> &SourceStateId {
        &self.source
    }

    pub(crate) fn shape(&self) -> StateValueShape {
        self.shape
    }

    pub(crate) fn element(&self) -> Option<Self> {
        (!self.shape.is_scalar()).then(|| Self { source: self.source.clone(), shape: StateValueShape::Scalar })
    }

    pub(crate) fn appended(&self, count: usize) -> Option<Self> {
        let shape = match self.shape {
            StateValueShape::Scalar => return None,
            StateValueShape::FixedArray(FixedArrayLength::Known(len)) => {
                StateValueShape::FixedArray(FixedArrayLength::Known(len.checked_add(count)?))
            }
            StateValueShape::FixedArray(FixedArrayLength::Unresolved) => StateValueShape::FixedArray(FixedArrayLength::Unresolved),
            StateValueShape::DynamicArray => StateValueShape::DynamicArray,
        };
        Some(Self { source: self.source.clone(), shape })
    }

    pub(crate) fn is_proven_incompatible_with(&self, expected: &Self) -> bool {
        self.source != expected.source || self.shape.is_proven_incompatible_with(expected.shape)
    }
}

impl StateValueShape {
    pub(crate) fn is_scalar(self) -> bool {
        self == Self::Scalar
    }

    fn is_proven_incompatible_with(self, expected: Self) -> bool {
        match (self, expected) {
            (Self::Scalar, Self::Scalar) | (Self::DynamicArray, Self::DynamicArray) => false,
            (Self::FixedArray(FixedArrayLength::Known(actual)), Self::FixedArray(FixedArrayLength::Known(expected))) => {
                actual != expected
            }
            (Self::FixedArray(_), Self::FixedArray(_)) => false,
            _ => true,
        }
    }
}

impl CallableSignaturePlan {
    pub(crate) fn param(&self, index: usize) -> Option<&PlannedStateValue> {
        self.params.get(index)?.as_ref()
    }

    pub(crate) fn result(&self) -> Option<&PlannedStateValue> {
        self.result.as_ref()
    }
}

#[cfg(test)]
mod tests;

/// A type's meaning is independent of compatibility names assigned for Sil emission.
#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) enum ResolvedTypeBase {
    Builtin(String),
    State(DeclId),
    Actor(DeclId),
    ActorEnum(DeclId),
    ActorHandle(DeclId),
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct ResolvedType {
    pub(crate) base: ResolvedTypeBase,
    pub(crate) array: Option<ArrayDim>,
}

/// The resolved categories needed by co-spend receiver validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OperandType {
    CovenantId,
    CovenantIdArray,
    State(DeclId),
    StateArray(DeclId),
    Other,
}

impl OperandType {
    pub(crate) fn from_resolved(ty: &ResolvedType) -> Self {
        match (&ty.base, ty.array.is_some()) {
            (ResolvedTypeBase::Builtin(name), false) if name == word::COVENANT_ID => Self::CovenantId,
            (ResolvedTypeBase::Builtin(name), true) if name == word::COVENANT_ID => Self::CovenantIdArray,
            (ResolvedTypeBase::State(id), false) => Self::State(*id),
            (ResolvedTypeBase::State(id), true) => Self::StateArray(*id),
            _ => Self::Other,
        }
    }

    fn from_type_use(ty: &crate::compiler::syntax::ArgentTypeUse, binding: &Binding) -> Self {
        let is_array = !ty.ty.array_dims.is_empty();
        match (&ty.ty.base, binding) {
            (SilTypeBase::Custom(name), Binding::Builtin) if name == word::COVENANT_ID => {
                if is_array {
                    Self::CovenantIdArray
                } else {
                    Self::CovenantId
                }
            }
            (_, Binding::Source(ResolvedName::Declaration(id))) if id.kind() == SymbolKind::State && ty.actor_state.is_none() => {
                if is_array {
                    Self::StateArray(*id)
                } else {
                    Self::State(*id)
                }
            }
            _ => Self::Other,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct CallableId {
    pub(crate) owner: DeclId,
    /// `None` is a global function; `Some` indexes an actor helper.
    pub(crate) member: Option<usize>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct CallableSignature {
    pub(crate) params: Vec<ResolvedType>,
    pub(crate) result: Option<ResolvedType>,
}

/// Authored expression sites and bound meaning for a constructed successor.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) struct BoundRouteValue {
    pub(crate) actor_site: NodeId,
    pub(crate) state_site: NodeId,
    pub(crate) actor_target: BoundRouteActor,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum BoundRouteActor {
    Fixed(DeclId),
    Linked(AppMember),
    Selector(DeclId),
    Local(LocalId),
    Expression(NodeId),
}

/// Facts that can be established from source binding before routing or lowering.
#[derive(Debug)]
pub(crate) struct TypeTable {
    pub(crate) names: BTreeMap<String, DeclId>,
    pub(crate) display_names: BTreeMap<DeclId, String>,
    pub(crate) app_member_display_names: BTreeMap<AppMember, String>,
    pub(crate) actor_states: BTreeMap<DeclId, DeclId>,
    pub(crate) enum_variants: BTreeMap<DeclId, Vec<DeclId>>,
    pub(crate) constants: BTreeMap<DeclId, ResolvedType>,
    pub(crate) state_fields: BTreeMap<(DeclId, usize), ResolvedType>,
    pub(crate) callables: BTreeMap<CallableId, CallableSignature>,
    pub(crate) entry_params: BTreeMap<(DeclId, usize, usize), ResolvedType>,
    /// State types appearing in authored expression and statement nodes.
    pub(crate) node_state_uses: BTreeMap<NodeId, DeclId>,
    pub(crate) type_use_operands: BTreeMap<NodeId, OperandType>,
    pub(crate) local_operands: BTreeMap<LocalId, OperandType>,
    pub(crate) co_spent_sites: BTreeSet<NodeId>,
    pub(crate) digest_operands: BTreeMap<NodeId, PlannedStateValue>,
    pub(crate) actor_field_uses: BTreeMap<NodeId, SourceFieldId>,
    /// Actor-handle type sites, distinguished from actor-enum declarations.
    pub(crate) actor_handle_type_uses: BTreeMap<NodeId, DeclId>,
    pub(crate) local_actor_handle_states: BTreeMap<LocalId, DeclId>,
    pub(crate) route_values: BTreeMap<(EntryId, RouteId), BoundRouteValue>,
    pub(crate) body_values: BTreeMap<NodeId, PlannedStateValue>,
}

struct AuthoredBodyBinding<'src> {
    name: String,
    site: NodeId,
    type_site: Option<NodeId>,
    initializer: Option<(SilExpr<'src>, SourceNodeCursor)>,
}

type ExpressionSitePlans = (BTreeSet<NodeId>, BTreeMap<NodeId, PlannedStateValue>, BTreeMap<NodeId, SourceFieldId>);

/// Lexical source-value facts for one entry, computed before Sil body lowering.
struct BodyTypeAnalyzer<'a, 'm> {
    model: &'m AppCompilationContext<'a>,
    program: &'m ResolvedModules<'m>,
    values: &'m ActorValuePlan,
    entry: &'a EntryDecl,
    id: EntryId,
    local_values: BTreeMap<LocalId, PlannedStateValue>,
    planned_nodes: BTreeMap<NodeId, PlannedStateValue>,
    authored_bindings: Vec<AuthoredBodyBinding<'m>>,
    authored_routes: BTreeMap<RouteId, (SilExpr<'m>, SourceNodeCursor)>,
    authored_validation_routes: Vec<(RouteId, String, RouteArity, bool)>,
    next_binding: usize,
}

/// Validates co-spend receiver types against the bound authored expression tree.
struct ExpressionTypePlanner<'a, 'm> {
    model: &'m AppCompilationContext<'a>,
    owner: DeclId,
    root: RootSlot,
    context: String,
    references: BTreeMap<(usize, usize), Binding>,
    call_sites: BTreeMap<(usize, usize), NodeId>,
    actor_field_sites: BTreeMap<(usize, usize), NodeId>,
    actor_source: Option<SourceStateId>,
    operand_type_sites: BTreeMap<usize, OperandType>,
    state_type_sites: BTreeMap<usize, DeclId>,
    actor_fields: BTreeMap<String, OperandType>,
    local_state_values: BTreeMap<LocalId, PlannedStateValue>,
    accepted: BTreeSet<NodeId>,
    digest_operands: BTreeMap<NodeId, PlannedStateValue>,
    actor_field_uses: BTreeMap<NodeId, SourceFieldId>,
    error: Option<ArgentError>,
}

impl<'a, 'm> ExpressionTypePlanner<'a, 'm> {
    fn new(model: &'m AppCompilationContext<'a>, owner: DeclId, root: RootSlot) -> Result<Self> {
        let program = model.resolution;
        let context = match (root, program.declaration(owner)) {
            (RootSlot::Entry(index), ResolvedDeclaration::Actor(actor)) => {
                actor.entries.get(index).map(|entry| format!("{}::{}", actor.name, entry.name)).unwrap_or_else(|| actor.name.clone())
            }
            (RootSlot::ActorFunction(index), ResolvedDeclaration::Actor(actor)) => actor
                .functions
                .get(index)
                .map(|function| format!("{}::{}", actor.name, function.name))
                .unwrap_or_else(|| actor.name.clone()),
            _ => program.declaration(owner).name().to_string(),
        };
        let mut references = BTreeMap::new();
        let mut call_sites = BTreeMap::new();
        let mut actor_field_sites = BTreeMap::new();
        for (site, binding) in &program.bindings(owner).sites {
            let node = program.nodes().node(*site);
            if node.address.root != root {
                continue;
            }
            let Origin::Authored { start, end, .. } = node.origin else { continue };
            references.insert((start, end), binding.clone());
            if node.address.children.last() == Some(&ChildEdge::CallTarget) {
                call_sites.insert((start, end), *site);
            }
            if matches!(binding, Binding::ActorField) {
                actor_field_sites.insert((start, end), *site);
            }
            if matches!(binding, Binding::RuntimeRoot) && node.address.children.last() == Some(&ChildEdge::ExpressionSource) {
                let mut address = node.address.clone();
                address.children.pop();
                let cursor = SourceNodeCursor { address };
                if program.nodes().find(&cursor.child(ChildEdge::FieldLabel).address).is_some()
                    && let Some(parent) = program.nodes().find(&cursor.address)
                    && let Origin::Authored { start, end, .. } = program.nodes().node(parent).origin
                {
                    actor_field_sites.insert((start, end), parent);
                }
            }
        }
        let operand_type_sites = model
            .types
            .type_use_operands
            .iter()
            .filter_map(|(site, ty)| {
                let node = program.nodes().node(*site);
                if node.address.owner != owner || node.address.root != root {
                    return None;
                }
                match node.origin {
                    Origin::Authored { start, .. } => Some((start, *ty)),
                    Origin::Generated { .. } => None,
                }
            })
            .collect();
        let state_type_sites = model
            .types
            .node_state_uses
            .iter()
            .filter_map(|(site, state)| {
                let node = program.nodes().node(*site);
                if node.address.owner != owner || node.address.root != root {
                    return None;
                }
                match node.origin {
                    Origin::Authored { start, .. } => Some((start, *state)),
                    Origin::Generated { .. } => None,
                }
            })
            .collect();
        let mut actor_fields = BTreeMap::new();
        let mut actor_source = None;
        if owner.kind() == SymbolKind::Actor {
            let source_state =
                model.types.actor_states.get(&owner).ok_or_else(|| ArgentError::new("actor has no bound state identity"))?;
            actor_source = Some(model.source_state_id_by_decl(*source_state)?);
            let mut storage_id =
                *model.types.actor_states.get(&owner).ok_or_else(|| ArgentError::new("co-spend actor has no bound state identity"))?;
            let mut visited = BTreeSet::new();
            let storage = loop {
                if !visited.insert(storage_id) {
                    return Err(ArgentError::new("co-spend actor state expansion has a cycle"));
                }
                let ResolvedDeclaration::State(state) = program.declaration(storage_id) else {
                    return Err(ArgentError::new("co-spend actor storage identity is not a state"));
                };
                let Some(expansion) = &state.expansion else { break state };
                storage_id = TypeTable::bound_decl(program, storage_id, &expansion.base, SymbolKind::State)?;
            };
            for (index, field) in storage.fields.iter().enumerate() {
                let ty = model
                    .types
                    .state_fields
                    .get(&(storage_id, index))
                    .ok_or_else(|| ArgentError::new(format!("actor storage field `{}` has no resolved type", field.name)))?;
                actor_fields.insert(field.name.clone(), OperandType::from_resolved(ty));
            }
        }
        let mut local_state_values = BTreeMap::new();
        if let RootSlot::Entry(index) = root {
            let values = model
                .actor_value_plans
                .get(&owner)
                .ok_or_else(|| ArgentError::new("entry has no actor value plan by declaration identity"))?;
            for ((entry, parameter), value) in &values.entry_param_ids {
                if entry.actor == owner
                    && entry.index == index
                    && let Some(id) = program.bindings(owner).parameter_ids.get(&(root, *parameter))
                {
                    local_state_values.insert(*id, value.clone());
                }
            }
            for (site, value) in &model.types.body_values {
                let node = program.nodes().node(*site);
                if node.address.owner == owner
                    && node.address.root == root
                    && let Some(Binding::Local(id)) = program.bindings(owner).sites.get(site)
                {
                    local_state_values.insert(*id, value.clone());
                }
            }
        }
        Ok(Self {
            model,
            owner,
            root,
            context,
            references,
            call_sites,
            actor_field_sites,
            actor_source,
            operand_type_sites,
            state_type_sites,
            actor_fields,
            local_state_values,
            accepted: BTreeSet::new(),
            digest_operands: BTreeMap::new(),
            actor_field_uses: BTreeMap::new(),
            error: None,
        })
    }

    fn binding(&self, span: silverscript_lang::ast::Span<'_>) -> Option<&Binding> {
        self.references.get(&(span.start(), span.end()))
    }

    fn operand_type(&self, expr: &SilExpr<'_>) -> OperandType {
        match &expr.kind {
            SilExprKind::Identifier(name) => match self.binding(expr.span) {
                Some(Binding::Local(id)) => self.model.types.local_operands.get(id).copied().unwrap_or(OperandType::Other),
                Some(Binding::Source(ResolvedName::Declaration(id))) => {
                    self.model.types.constants.get(id).map(OperandType::from_resolved).unwrap_or(OperandType::Other)
                }
                Some(Binding::ActorField) => self.actor_fields.get(name).copied().unwrap_or(OperandType::Other),
                _ => OperandType::Other,
            },
            SilExprKind::Call { name, args, name_span }
                if name == word::COVENANT_ID && args.len() == 1 && matches!(self.binding(*name_span), Some(Binding::Builtin)) =>
            {
                OperandType::CovenantId
            }
            SilExprKind::Call { name_span, .. } => {
                let callable = match self.binding(*name_span) {
                    Some(Binding::Source(ResolvedName::Declaration(id))) if id.kind() == SymbolKind::Function => {
                        Some(CallableId { owner: *id, member: None })
                    }
                    Some(Binding::ActorHelper(member)) => Some(CallableId { owner: self.owner, member: Some(*member) }),
                    _ => None,
                };
                callable
                    .and_then(|id| self.model.types.callables.get(&id))
                    .and_then(|signature| signature.result.as_ref())
                    .map(OperandType::from_resolved)
                    .unwrap_or(OperandType::Other)
            }
            SilExprKind::Array { type_span, .. } => {
                self.operand_type_sites.get(&type_span.start()).copied().unwrap_or(OperandType::Other)
            }
            SilExprKind::New { name_span, .. } | SilExprKind::StructLiteral { name_span, .. } => {
                self.state_type_sites.get(&name_span.start()).copied().map(OperandType::State).unwrap_or(OperandType::Other)
            }
            SilExprKind::Append { source, .. } => self.operand_type(source),
            SilExprKind::FieldAccess { source, field, .. } => {
                if matches!(&source.kind, SilExprKind::Identifier(root) if root == word::SELF)
                    && matches!(self.binding(source.span), Some(Binding::RuntimeRoot))
                {
                    return self.actor_fields.get(field).copied().unwrap_or(OperandType::Other);
                }
                let OperandType::State(state) = self.operand_type(source) else { return OperandType::Other };
                let ResolvedDeclaration::State(declaration) = self.model.resolution.declaration(state) else {
                    return OperandType::Other;
                };
                declaration
                    .fields
                    .iter()
                    .position(|candidate| candidate.name == *field)
                    .and_then(|index| self.model.types.state_fields.get(&(state, index)))
                    .map(OperandType::from_resolved)
                    .unwrap_or(OperandType::Other)
            }
            SilExprKind::ArrayIndex { source, .. } => match self.operand_type(source) {
                OperandType::CovenantIdArray => OperandType::CovenantId,
                OperandType::StateArray(id) => OperandType::State(id),
                _ => OperandType::Other,
            },
            SilExprKind::Ternary { then_expr, else_expr, .. } => {
                let then_type = self.operand_type(then_expr);
                if then_type == self.operand_type(else_expr) { then_type } else { OperandType::Other }
            }
            _ => OperandType::Other,
        }
    }

    fn state_value(&self, expr: &SilExpr<'_>) -> Option<PlannedStateValue> {
        let values = self.model.actor_value_plans.get(&self.owner)?;
        match &expr.kind {
            SilExprKind::Identifier(name) => match self.binding(expr.span) {
                Some(Binding::Local(id)) => self.local_state_values.get(id).cloned(),
                Some(Binding::Source(ResolvedName::Declaration(id))) if id.kind() == SymbolKind::Const => {
                    values.constant_ids.get(id).cloned()
                }
                Some(Binding::ActorField) => {
                    let state = self.model.types.actor_states.get(&self.owner)?;
                    let source = self.model.source_state_id_by_decl(*state).ok()?;
                    values.field_values.get(&SourceFieldId::new(source, name)).cloned()
                }
                _ => None,
            },
            SilExprKind::Call { name_span, .. } => {
                let callable = match self.binding(*name_span) {
                    Some(Binding::Source(ResolvedName::Declaration(id))) if id.kind() == SymbolKind::Function => {
                        CallableId { owner: *id, member: None }
                    }
                    Some(Binding::ActorHelper(member)) => CallableId { owner: self.owner, member: Some(*member) },
                    _ => return None,
                };
                values.signature_ids.get(&callable)?.result().cloned()
            }
            SilExprKind::Array { type_ref, type_span, values: elements } => {
                let state = self.state_type_sites.get(&type_span.start())?;
                let source = self.model.source_state_id_by_decl(*state).ok()?;
                PlannedStateValue::from_bound_type(source, type_ref, Some(elements.len()))
            }
            SilExprKind::Append { source, args, .. } => self.state_value(source)?.appended(args.len()),
            SilExprKind::ArrayIndex { source, .. } => self.state_value(source)?.element(),
            _ => None,
        }
    }

    fn finish(self) -> Result<ExpressionSitePlans> {
        self.error.map_or(Ok((self.accepted, self.digest_operands, self.actor_field_uses)), Err)
    }

    fn reject(&mut self, message: &str) {
        self.error.get_or_insert_with(|| ArgentError::new(format!("{message} in `{}`", self.context)));
    }
}

impl<'src> AstVisitorMut<'src> for ExpressionTypePlanner<'_, '_> {
    fn visit_expr(&mut self, expr: &mut SilExpr<'src>) {
        let field = match &expr.kind {
            SilExprKind::Identifier(name) if matches!(self.binding(expr.span), Some(Binding::ActorField)) => Some(name.as_str()),
            SilExprKind::FieldAccess { source, field, .. }
                if matches!(&source.kind, SilExprKind::Identifier(root) if root == word::SELF)
                    && matches!(self.binding(source.span), Some(Binding::RuntimeRoot)) =>
            {
                Some(field.as_str())
            }
            _ => None,
        };
        if let Some(field) = field
            && self.actor_fields.contains_key(field)
            && let Some(site) = self.actor_field_sites.get(&(expr.span.start(), expr.span.end()))
            && let Some(source) = &self.actor_source
        {
            self.actor_field_uses.insert(*site, SourceFieldId::new(source.clone(), field));
        }
        if let SilExprKind::Call { name, args, name_span } = &expr.kind
            && name == word::CO_SPENT
        {
            let key = (name_span.start(), name_span.end());
            match (self.call_sites.get(&key), self.binding(*name_span)) {
                (Some(_), Some(Binding::Builtin)) if args.len() == 1 && self.operand_type(&args[0]) == OperandType::CovenantId => {
                    self.accepted.insert(self.call_sites[&key]);
                }
                (None, _) | (_, Some(Binding::Builtin)) => {
                    self.reject("`.co_spent()` requires one `cov_id` receiver");
                }
                _ => {
                    self.reject("co-spend call has no bound builtin identity");
                }
            }
        }
        if let SilExprKind::Call { name, args, name_span } = &expr.kind
            && name == word::DIGEST
            && let [value] = args.as_slice()
            && matches!(self.root, RootSlot::Entry(_))
            && !matches!(&value.kind, SilExprKind::Call { name, .. } if name == word::STATE)
            && !matches!(&value.kind, SilExprKind::FieldAccess { field, .. } if field == word::STATE)
        {
            if let Some(target) = self.call_sites.get(&(name_span.start(), name_span.end())) {
                let mut address = self.model.resolution.nodes().node(*target).address.clone();
                address.children.pop();
                address.children.push(ChildEdge::Argument(0));
                match (self.model.resolution.nodes().find(&address), self.state_value(value)) {
                    (Some(site), Some(planned)) if planned.shape().is_scalar() => {
                        self.digest_operands.insert(site, planned);
                    }
                    (_, Some(_)) => self.reject("`digest(...)` requires one scalar authored state value"),
                    _ => self.reject("`digest(...)` requires a proven authored state value"),
                }
            } else {
                self.reject("digest operand has no bound source site");
            }
        }
        walk_expr_mut(self, expr);
    }
}

impl TypeTable {
    /// Read the declared scalar handle type of a bound value producer.
    pub(super) fn scalar_actor_handle_state(&self, owner: DeclId, binding: &Binding) -> Option<DeclId> {
        let ty = match binding {
            Binding::Local(local) => return self.local_actor_handle_states.get(local).copied(),
            Binding::Source(ResolvedName::Declaration(id)) if id.kind() == SymbolKind::Const => self.constants.get(id),
            Binding::Source(ResolvedName::Declaration(id)) if id.kind() == SymbolKind::Function => {
                self.callables.get(&CallableId { owner: *id, member: None }).and_then(|signature| signature.result.as_ref())
            }
            Binding::ActorHelper(index) => {
                self.callables.get(&CallableId { owner, member: Some(*index) }).and_then(|signature| signature.result.as_ref())
            }
            _ => None,
        }?;
        match ty {
            ResolvedType { base: ResolvedTypeBase::ActorHandle(state), array: None } => Some(*state),
            _ => None,
        }
    }

    pub(crate) fn plan_expression_sites(&self, model: &AppCompilationContext<'_>) -> Result<ExpressionSitePlans> {
        let mut accepted = BTreeSet::new();
        let mut digest_operands = BTreeMap::new();
        let mut actor_field_uses = BTreeMap::new();
        for (owner, _) in model.app_actors.iter_with_ids() {
            let actor = model.actor_by_decl(owner)?;
            for (index, _) in actor.functions.iter().enumerate() {
                let body = model.resolution.function_body(owner, Some(index))?;
                let mut planner = ExpressionTypePlanner::new(model, owner, RootSlot::ActorFunction(index))?;
                for statement in body {
                    planner.visit_statement(&mut statement.clone());
                }
                let (sites, values, fields) = planner.finish()?;
                accepted.extend(sites);
                digest_operands.extend(values);
                actor_field_uses.extend(fields);
            }
            for (index, _) in actor.entries.iter().enumerate() {
                let body = model.resolution.entry_body(EntryId { actor: owner, index })?;
                let mut planner = ExpressionTypePlanner::new(model, owner, RootSlot::Entry(index))?;
                for statement in body {
                    statement.visit_with(&mut planner);
                }
                let (sites, values, fields) = planner.finish()?;
                accepted.extend(sites);
                digest_operands.extend(values);
                actor_field_uses.extend(fields);
            }
        }
        for (owner, _) in &model.functions {
            let owner = *owner;
            let body = model.resolution.function_body(owner, None)?;
            let mut planner = ExpressionTypePlanner::new(model, owner, RootSlot::Declaration)?;
            for statement in body {
                planner.visit_statement(&mut statement.clone());
            }
            let (sites, values, fields) = planner.finish()?;
            accepted.extend(sites);
            digest_operands.extend(values);
            actor_field_uses.extend(fields);
        }
        Ok((accepted, digest_operands, actor_field_uses))
    }

    pub(crate) fn plan_body_values(
        &self,
        model: &AppCompilationContext<'_>,
        program: &ResolvedModules<'_>,
    ) -> Result<BTreeMap<NodeId, PlannedStateValue>> {
        let mut planned_nodes = BTreeMap::new();
        for (actor_id, _) in model.app_actors.iter_with_ids() {
            let actor_model = &model.actor_models[&actor_id];
            let actor = actor_model.source();
            let values = model
                .actor_value_plans
                .get(&actor_id)
                .ok_or_else(|| ArgentError::new(format!("missing value plan for actor `{}`", actor.name)))?;
            for entry_model in actor_model.entries() {
                let entry = entry_model.source();
                let id = entry_model.id;
                let authored = program.entry_body(id)?;
                let mut analyzer = BodyTypeAnalyzer {
                    model,
                    program,
                    values,
                    entry,
                    id,
                    local_values: BTreeMap::new(),
                    planned_nodes: BTreeMap::new(),
                    authored_bindings: Vec::new(),
                    authored_routes: BTreeMap::new(),
                    authored_validation_routes: Vec::new(),
                    next_binding: 0,
                };
                let cursor = SourceNodeCursor::new(id.actor, RootSlot::Entry(id.index)).child(ChildEdge::Body);
                analyzer.collect_entry_bindings(&cursor, authored)?;
                analyzer.seed()?;
                let bindings = analyzer.authored_bindings.iter().map(|binding| binding.name.clone()).collect::<Vec<_>>();
                for name in bindings {
                    analyzer.record_binding(&name)?;
                }
                for (route, output, arity, foreign) in &analyzer.authored_validation_routes {
                    if *foreign {
                        analyzer.validate_foreign_route(*route, *arity)?;
                    } else {
                        analyzer.validate_current_route(*route, output, *arity)?;
                    }
                }
                if analyzer.next_binding != analyzer.authored_bindings.len() {
                    return Err(ArgentError::new(format!(
                        "entry `{}::{}` has inconsistent authored local bindings",
                        actor.name, entry.name
                    )));
                }
                planned_nodes.extend(analyzer.planned_nodes);
            }
        }
        Ok(planned_nodes)
    }

    /// Resolve types for the selected source closure.
    pub(crate) fn new(program: &ResolvedModules<'_>, names: &BTreeMap<DeclId, String>) -> Result<Self> {
        let mut table = Self {
            names: names.iter().map(|(id, name)| (name.clone(), *id)).collect(),
            display_names: names.clone(),
            app_member_display_names: BTreeMap::new(),
            actor_states: BTreeMap::new(),
            enum_variants: BTreeMap::new(),
            constants: BTreeMap::new(),
            state_fields: BTreeMap::new(),
            callables: BTreeMap::new(),
            entry_params: BTreeMap::new(),
            node_state_uses: BTreeMap::new(),
            type_use_operands: BTreeMap::new(),
            local_operands: BTreeMap::new(),
            co_spent_sites: BTreeSet::new(),
            digest_operands: BTreeMap::new(),
            actor_field_uses: BTreeMap::new(),
            actor_handle_type_uses: BTreeMap::new(),
            local_actor_handle_states: BTreeMap::new(),
            route_values: BTreeMap::new(),
            body_values: BTreeMap::new(),
        };
        for id in names.keys().copied() {
            for (site, binding) in &program.bindings(id).sites {
                if let Binding::Source(ResolvedName::AppMember(member)) = binding {
                    table.app_member_display_names.entry(*member).or_insert_with(|| {
                        format!("{}::{}", program.declaration(member.app).name(), program.declaration(member.actor).name())
                    });
                }
                if let Some(ty) = program.type_use(*site) {
                    table.type_use_operands.insert(*site, OperandType::from_type_use(ty, binding));
                }
                if let Some(ty) = program.type_use(*site)
                    && let Binding::Source(ResolvedName::Declaration(target)) = binding
                    && target.kind == SymbolKind::State
                {
                    table.node_state_uses.insert(*site, *target);
                    if ty.actor_state.is_some() {
                        table.actor_handle_type_uses.insert(*site, *target);
                    }
                }
            }
            for (site, binding) in &program.bindings(id).sites {
                let Binding::Local(local) = binding else { continue };
                let mut address = program.nodes().node(*site).address.clone();
                if address.children.pop() != Some(ChildEdge::BindingName) {
                    continue;
                }
                address.children.push(ChildEdge::TypeUse);
                if let Some(type_site) = program.nodes().find(&address) {
                    if let Some(ty) = table.type_use_operands.get(&type_site) {
                        table.local_operands.insert(*local, *ty);
                    }
                    if let Some(state) = table.actor_handle_type_uses.get(&type_site) {
                        table.local_actor_handle_states.insert(*local, *state);
                    }
                }
            }
            match program.declaration(id) {
                ResolvedDeclaration::Const(decl) => {
                    table.constants.insert(id, Self::resolve_type(program, id, &decl.ty)?);
                }
                ResolvedDeclaration::State(decl) => {
                    for (index, field) in decl.fields.iter().enumerate() {
                        table.state_fields.insert((id, index), Self::resolve_type(program, id, &field.ty)?);
                    }
                }
                ResolvedDeclaration::Function(decl) => {
                    let params: Vec<_> =
                        decl.params.iter().map(|param| Self::resolve_type(program, id, &param.ty)).collect::<Result<_>>()?;
                    for (index, ty) in params.iter().enumerate() {
                        if let Some(local) = program.bindings(id).parameter_ids.get(&(RootSlot::Declaration, index)) {
                            table.local_operands.insert(*local, OperandType::from_resolved(ty));
                        }
                    }
                    let result = decl.return_ty.as_ref().map(|ty| Self::resolve_type(program, id, ty)).transpose()?;
                    table.callables.insert(CallableId { owner: id, member: None }, CallableSignature { params, result });
                }
                ResolvedDeclaration::Actor(decl) => {
                    table.actor_states.insert(id, Self::bound_decl(program, id, &decl.state, SymbolKind::State)?);
                    for (index, function) in decl.functions.iter().enumerate() {
                        let params: Vec<_> =
                            function.params.iter().map(|param| Self::resolve_type(program, id, &param.ty)).collect::<Result<_>>()?;
                        for (param_index, ty) in params.iter().enumerate() {
                            if let Some(local) = program.bindings(id).parameter_ids.get(&(RootSlot::ActorFunction(index), param_index))
                            {
                                table.local_operands.insert(*local, OperandType::from_resolved(ty));
                            }
                        }
                        let result = function.return_ty.as_ref().map(|ty| Self::resolve_type(program, id, ty)).transpose()?;
                        table.callables.insert(CallableId { owner: id, member: Some(index) }, CallableSignature { params, result });
                    }
                    for (entry_index, entry) in decl.entries.iter().enumerate() {
                        let entry_id = EntryId { actor: id, index: entry_index };
                        for (param_index, param) in entry.params.iter().enumerate() {
                            let ty = Self::resolve_type(program, id, &param.ty)?;
                            if let Some(local) = program.bindings(id).parameter_ids.get(&(RootSlot::Entry(entry_index), param_index)) {
                                table.local_operands.insert(*local, OperandType::from_resolved(&ty));
                                if let ResolvedType { base: ResolvedTypeBase::ActorHandle(state), array: None } = &ty {
                                    table.local_actor_handle_states.insert(*local, *state);
                                }
                            }
                            table.entry_params.insert((id, entry_index, param_index), ty);
                        }
                        for (consume_index, consume) in entry.consumes.iter().enumerate() {
                            if !matches!(consume.cardinality, Cardinality::One) {
                                continue;
                            }
                            let Some(local) = program.bindings(id).entry_consumes.get(&(entry_index, consume_index)) else {
                                return Err(ArgentError::at(
                                    program.declaration_path(id),
                                    "consumed input has no bound local identity",
                                ));
                            };
                            let target = Self::bound_decl(program, id, &consume.actor, SymbolKind::Actor)?;
                            let ResolvedDeclaration::Actor(target_actor) = program.declaration(target) else {
                                return Err(ArgentError::at(program.declaration_path(id), "consumed input target is not an actor"));
                            };
                            let state = Self::bound_decl(program, target, &target_actor.state, SymbolKind::State)?;
                            table.local_operands.insert(*local, OperandType::State(state));
                        }
                        for (spawn_index, _) in entry.spawns.iter().enumerate() {
                            let Some(local) = program.bindings(id).entry_spawn_covenants.get(&(entry_index, spawn_index)) else {
                                return Err(ArgentError::at(
                                    program.declaration_path(id),
                                    "spawn covenant has no bound local identity",
                                ));
                            };
                            table.local_operands.insert(*local, OperandType::CovenantId);
                        }
                        for route in &entry.routes {
                            if !matches!(route.successor, EntrySuccessor::Constructed { .. }) {
                                continue;
                            }
                            let (actor_site, state_site) = program.nodes().route_sites(entry_id, route.id).ok_or_else(|| {
                                ArgentError::at(program.declaration_path(id), "missing indexed constructed successor")
                            })?;
                            let actor_target = match program.bindings(id).sites.get(&actor_site) {
                                Some(Binding::Source(ResolvedName::Declaration(target))) if target.kind() == SymbolKind::Actor => {
                                    BoundRouteActor::Fixed(*target)
                                }
                                Some(Binding::Source(ResolvedName::AppMember(target))) => BoundRouteActor::Linked(*target),
                                Some(Binding::EnumVariant { actor, .. }) => BoundRouteActor::Fixed(*actor),
                                Some(Binding::Local(local)) => BoundRouteActor::Local(*local),
                                _ => {
                                    let cursor = SourceNodeCursor { address: program.nodes().node(actor_site).address.clone() };
                                    let source_site = program.nodes().find(&cursor.child(ChildEdge::ExpressionSource).address);
                                    match source_site.and_then(|site| program.bindings(id).sites.get(&site)) {
                                        Some(Binding::Source(ResolvedName::Declaration(target)))
                                            if target.kind() == SymbolKind::ActorEnum =>
                                        {
                                            BoundRouteActor::Selector(*target)
                                        }
                                        _ => BoundRouteActor::Expression(actor_site),
                                    }
                                }
                            };
                            table.route_values.insert((entry_id, route.id), BoundRouteValue { actor_site, state_site, actor_target });
                        }
                    }
                }
                ResolvedDeclaration::ActorEnum(decl) => {
                    table.enum_variants.insert(
                        id,
                        decl.variants
                            .iter()
                            .map(|variant| Self::bound_decl(program, id, variant, SymbolKind::Actor))
                            .collect::<Result<_>>()?,
                    );
                }
                ResolvedDeclaration::App(_) => {}
            }
        }
        Ok(table)
    }

    fn bound_decl(program: &ResolvedModules<'_>, owner: DeclId, name: &str, expected: SymbolKind) -> Result<DeclId> {
        match program.bindings(owner).names.get(name) {
            Some(ResolvedName::Declaration(id)) if id.kind == expected => Ok(*id),
            _ => Err(ArgentError::at(program.declaration_path(owner), format!("unbound {expected:?} reference `{name}`"))),
        }
    }

    fn resolve_type(program: &ResolvedModules<'_>, owner: DeclId, ty: &TypeRef) -> Result<ResolvedType> {
        let base = if let Some(state) = &ty.actor_state {
            ResolvedTypeBase::ActorHandle(Self::bound_decl(program, owner, state, SymbolKind::State)?)
        } else if ty.is_builtin() {
            ResolvedTypeBase::Builtin(ty.name.clone())
        } else {
            let id = match program.bindings(owner).names.get(&ty.name) {
                Some(ResolvedName::Declaration(id)) => *id,
                _ => return Err(ArgentError::at(program.declaration_path(owner), format!("unbound type `{}`", ty.name))),
            };
            match id.kind {
                SymbolKind::State => ResolvedTypeBase::State(id),
                SymbolKind::Actor => ResolvedTypeBase::Actor(id),
                SymbolKind::ActorEnum => ResolvedTypeBase::ActorEnum(id),
                _ => return Err(ArgentError::at(program.declaration_path(owner), format!("`{}` is not a type", ty.name))),
            }
        };
        Ok(ResolvedType { base, array: ty.array })
    }
}

impl<'a, 'm> BodyTypeAnalyzer<'a, 'm> {
    fn binding_site(&self, cursor: &SourceNodeCursor) -> Result<NodeId> {
        self.program
            .nodes()
            .find(&cursor.address)
            .ok_or_else(|| ArgentError::at(self.program.declaration_path(self.id.actor), "missing indexed authored body binding"))
    }

    fn push_binding(
        &mut self,
        cursor: &SourceNodeCursor,
        name: &str,
        type_cursor: Option<SourceNodeCursor>,
        initializer: Option<(SilExpr<'m>, SourceNodeCursor)>,
    ) -> Result<()> {
        let site = self.binding_site(&cursor.child(ChildEdge::BindingName))?;
        let type_site = type_cursor.as_ref().map(|cursor| self.binding_site(cursor)).transpose()?;
        self.authored_bindings.push(AuthoredBodyBinding { name: name.to_string(), site, type_site, initializer });
        Ok(())
    }

    fn collect_entry_bindings(&mut self, cursor: &SourceNodeCursor, statements: &[AuthoredEntryStatement<'m>]) -> Result<()> {
        for (index, statement) in statements.iter().enumerate() {
            self.collect_entry_binding(&cursor.child(ChildEdge::Statement(index)), statement)?;
        }
        Ok(())
    }

    fn collect_entry_binding(&mut self, cursor: &SourceNodeCursor, statement: &AuthoredEntryStatement<'m>) -> Result<()> {
        match statement {
            AuthoredEntryStatement::Block { statements, .. } => self.collect_entry_bindings(cursor, statements)?,
            AuthoredEntryStatement::If { then_branch, else_branch, .. } => {
                self.collect_entry_binding(&cursor.child(ChildEdge::ThenBranch), then_branch)?;
                if let Some(else_branch) = else_branch {
                    self.collect_entry_binding(&cursor.child(ChildEdge::ElseBranch), else_branch)?;
                }
            }
            AuthoredEntryStatement::Sil(statement) => self.collect_sil_bindings(cursor, statement)?,
            AuthoredEntryStatement::Become { routes, .. } | AuthoredEntryStatement::ForeignBecome { routes, .. } => {
                let foreign = matches!(statement, AuthoredEntryStatement::ForeignBecome { .. });
                for (index, route) in routes.iter().enumerate() {
                    if let crate::compiler::syntax::AuthoredSuccessor::Constructed { state, many, .. } = &route.successor {
                        self.authored_validation_routes.push((
                            route.id,
                            route.output.segments.join("::"),
                            if *many { RouteArity::Many } else { RouteArity::One },
                            foreign,
                        ));
                        self.authored_routes
                            .insert(route.id, ((**state).clone(), cursor.child(ChildEdge::Route(index)).child(ChildEdge::RouteState)));
                    }
                }
            }
        }
        Ok(())
    }

    fn collect_sil_bindings(&mut self, cursor: &SourceNodeCursor, statement: &SilStatement<'m>) -> Result<()> {
        match statement {
            SilStatement::VariableDefinition { name, expr, .. } => {
                self.push_binding(
                    cursor,
                    name,
                    Some(cursor.child(ChildEdge::TypeUse)),
                    expr.as_ref().map(|expr| (expr.clone(), cursor.child(ChildEdge::Expression))),
                )?;
            }
            SilStatement::TupleAssignment { left_name, right_name, .. } => {
                for (edge, name) in [(ChildEdge::ExpressionLeft, left_name), (ChildEdge::ExpressionRight, right_name)] {
                    let binding = cursor.child(edge);
                    self.push_binding(&binding, name, Some(binding.child(ChildEdge::TypeUse)), None)?;
                }
            }
            SilStatement::FunctionCallAssign { bindings, .. } => {
                for (index, binding) in bindings.iter().enumerate() {
                    let cursor = cursor.child(ChildEdge::Element(index));
                    self.push_binding(&cursor, &binding.name, Some(cursor.child(ChildEdge::TypeUse)), None)?;
                }
            }
            SilStatement::StateFunctionCallAssign { bindings, .. } | SilStatement::StructDestructure { bindings, .. } => {
                for (index, binding) in bindings.iter().enumerate() {
                    let cursor = cursor.child(ChildEdge::Field(index));
                    self.push_binding(&cursor, &binding.name, Some(cursor.child(ChildEdge::TypeUse)), None)?;
                }
            }
            SilStatement::For { ident, body, .. } => {
                let site = self.binding_site(&cursor.child(ChildEdge::LoopBinding))?;
                self.authored_bindings.push(AuthoredBodyBinding { name: ident.clone(), site, type_site: None, initializer: None });
                for (index, statement) in body.iter().enumerate() {
                    self.collect_sil_bindings(&cursor.child(ChildEdge::Statement(index)), statement)?;
                }
            }
            SilStatement::Block { body, .. } => {
                for (index, statement) in body.iter().enumerate() {
                    self.collect_sil_bindings(&cursor.child(ChildEdge::Statement(index)), statement)?;
                }
            }
            SilStatement::If { then_branch, else_branch, .. } => {
                for (index, statement) in then_branch.iter().enumerate() {
                    self.collect_sil_bindings(&cursor.child(ChildEdge::ThenBranch).child(ChildEdge::Statement(index)), statement)?;
                }
                if let Some(else_branch) = else_branch {
                    for (index, statement) in else_branch.iter().enumerate() {
                        self.collect_sil_bindings(&cursor.child(ChildEdge::ElseBranch).child(ChildEdge::Statement(index)), statement)?;
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn seed(&mut self) -> Result<()> {
        for (index, param) in self.entry.params.iter().enumerate() {
            let ty =
                self.model.types.entry_params.get(&(self.id.actor, self.id.index, index)).ok_or_else(|| {
                    ArgentError::new(format!("entry `{}` has an unplanned parameter `{}`", self.entry.name, param.name))
                })?;
            let value = ActorValuePlan::type_value(self.model, ty)?;
            if let Some(value) = value {
                let id = self.program.bindings(self.id.actor).parameter_ids[&(RootSlot::Entry(self.id.index), index)];
                self.local_values.insert(id, value);
            }
        }
        Ok(())
    }

    fn declared_body_value(&self, type_site: Option<NodeId>) -> Result<Option<PlannedStateValue>> {
        let Some(type_site) = type_site else { return Ok(None) };
        let ty = self
            .program
            .type_use(type_site)
            .ok_or_else(|| ArgentError::at(self.program.declaration_path(self.id.actor), "missing indexed authored state type"))?;
        if ty.actor_state.is_some() {
            return Ok(None);
        }
        let Some(state) = self.model.types.node_state_uses.get(&type_site) else { return Ok(None) };
        let source = self.model.source_state_id_by_decl(*state)?;
        Ok(PlannedStateValue::from_bound_type(source, &ty.ty, None))
    }

    fn record_binding(&mut self, name: &str) -> Result<()> {
        let authored = self
            .authored_bindings
            .get(self.next_binding)
            .ok_or_else(|| ArgentError::new(format!("entry `{}` has an unmatched authored binding `{name}`", self.entry.name)))?;
        if authored.name != name {
            return Err(ArgentError::new(format!(
                "entry `{}` has a rewritten binding that differs from its authored identity",
                self.entry.name
            )));
        }
        let declared = self.declared_body_value(authored.type_site)?;
        if let Some((expr, cursor)) = &authored.initializer
            && let SilExprKind::Call { name, args, .. } = &expr.kind
            && let Some(target) = self.program.nodes().find(&cursor.child(ChildEdge::CallTarget).address)
        {
            let callable = match self.program.bindings(self.id.actor).sites.get(&target) {
                Some(Binding::Source(ResolvedName::Declaration(id))) if id.kind() == SymbolKind::Function => {
                    Some(CallableId { owner: *id, member: None })
                }
                Some(Binding::ActorHelper(member)) => Some(CallableId { owner: self.id.actor, member: Some(*member) }),
                _ => None,
            };
            if let Some(signature) = callable.and_then(|id| self.values.signature_ids.get(&id)) {
                for (index, arg) in args.iter().enumerate() {
                    let Some(expected) = signature.param(index) else { continue };
                    if let Some(actual) = self.infer_authored(arg, &cursor.child(ChildEdge::Argument(index)))
                        && actual.is_proven_incompatible_with(expected)
                    {
                        return Err(ArgentError::new(format!(
                            "call to `{name}` passes an authored state value with incompatible identity or array shape at argument {}",
                            index + 1
                        )));
                    }
                }
            }
        }
        let inferred = authored.initializer.as_ref().and_then(|(expr, cursor)| self.infer_authored(expr, cursor));
        let provenance_matches = declared.as_ref().is_some_and(|declared| {
            let is_authored_constructor = authored
                .initializer
                .as_ref()
                .is_some_and(|(expr, _)| matches!(&expr.kind, SilExprKind::StructLiteral { name, .. } if name.is_empty()));
            is_authored_constructor || inferred.as_ref().is_some_and(|value| !value.is_proven_incompatible_with(declared))
        });
        let value = if matches!(
            declared.as_ref().map(PlannedStateValue::shape),
            Some(StateValueShape::FixedArray(FixedArrayLength::Unresolved))
        ) {
            inferred
                .filter(|inferred| {
                    declared.as_ref().is_some_and(|declared| inferred.source() == declared.source())
                        && matches!(inferred.shape(), StateValueShape::FixedArray(_))
                })
                .or_else(|| (authored.initializer.is_none() || provenance_matches).then_some(declared).flatten())
        } else if authored.initializer.is_none() || provenance_matches {
            declared
        } else {
            None
        };
        let site = authored.site;
        self.next_binding += 1;
        let Some(Binding::Local(id)) = self.program.bindings(self.id.actor).sites.get(&site) else {
            return Err(ArgentError::new(format!("entry `{}` has an unbound authored local", self.entry.name)));
        };
        if let Some(value) = &value {
            self.local_values.insert(*id, value.clone());
        }
        if let Some(value) = &value {
            self.planned_nodes.insert(site, value.clone());
        }
        Ok(())
    }

    fn validate_current_route(&self, route: RouteId, output: &str, arity: RouteArity) -> Result<()> {
        let actor = self.model.actor_by_decl(self.id.actor)?;
        let entry_model = self.model.entry_model_by_id(self.id)?;
        let Some((expr, cursor)) = self.authored_routes.get(&route) else {
            return Ok(());
        };
        let resolved = entry_model
            .route(route)
            .ok_or_else(|| ArgentError::new(format!("entry `{}` has an unresolved route", self.entry.name)))?;
        let target_ids = self.model.route_target_ids_by_id(self.id, resolved)?;
        let expected = target_ids.iter().map(|id| self.model.static_actor_source_state(id)).collect::<Result<BTreeSet<_>>>()?;
        if expected.len() != 1 {
            return Err(ArgentError::new(format!(
                "entry `{}::{}` route `{}` has no single authored state identity",
                actor.name, self.entry.name, output
            )));
        }
        let restates_active = matches!(&expr.kind,
                    SilExprKind::FieldAccess { source, field, .. }
                        if field == "state" && matches!(&source.kind, SilExprKind::Identifier(name) if name == "self"))
            || matches!(&expr.kind,
                        SilExprKind::Call { name, args, .. }
                            if name == "state" && matches!(args.as_slice(), [arg] if matches!(&arg.kind, SilExprKind::Identifier(root) if root == "self")));
        if restates_active && target_ids.as_slice() == [super::StaticActorId::InApp(self.id.actor)] {
            return Err(ArgentError::new(format!(
                "route state `{}` restates the active input; use `{} <- self` for an exact successor",
                expr.span.as_str().trim(),
                output
            )));
        }
        let inferred = self.infer_authored(expr, cursor);
        if !inferred.as_ref().is_some_and(|value| {
            let correct_shape = match arity {
                RouteArity::One => value.shape().is_scalar(),
                RouteArity::Many => !value.shape().is_scalar(),
            };
            correct_shape && expected.contains(value.source())
        }) {
            let state = expected.iter().next().ok_or_else(|| ArgentError::new("route has no target state"))?;
            let rendered = expr.span.as_str().trim();
            if matches!(expr.kind, SilExprKind::Identifier(_)) {
                return Err(ArgentError::new(format!(
                    "route state `{rendered}` is not an authored `{}` value; construct `{}` explicitly",
                    state.as_str(),
                    state.as_str()
                )));
            }
            return Err(ArgentError::new(format!(
                "route state `{rendered}` is not a proven authored `{}` value; bind or construct an authored value explicitly",
                state.as_str()
            )));
        }
        Ok(())
    }

    fn validate_foreign_route(&self, route: RouteId, arity: RouteArity) -> Result<()> {
        let actor = self.model.actor_by_decl(self.id.actor)?;
        let entry_model = self.model.entry_model_by_id(self.id)?;
        let Some((expr, cursor)) = self.authored_routes.get(&route) else {
            return Err(ArgentError::new(format!("entry `{}` has an unbound foreign route state", self.entry.name)));
        };
        let (group_id, output_id) = entry_model
            .route_output(route)
            .ok_or_else(|| ArgentError::new(format!("entry `{}` has an unresolved foreign route output", self.entry.name)))?;
        let group = entry_model.group(group_id).ok_or_else(|| ArgentError::new("foreign route has no bound covenant group"))?;
        let output = group
            .outputs()
            .iter()
            .find(|candidate| candidate.id() == output_id)
            .ok_or_else(|| ArgentError::new("foreign route has no bound output"))?;
        if output.cardinality().is_range() {
            return Ok(());
        }
        let expected = if let Some(target) = output.target().single_static_actor() {
            Some(self.model.static_actor_source_state(target)?)
        } else {
            match (group.observe(), group.spawn(), output.source()) {
                (Some(observe), None, InteractionSource::ObserveOutput(declaration)) => {
                    observed_open_state_for_decl(self.id, actor, self.entry, observe, declaration, self.model)?
                }
                (None, Some(_), InteractionSource::SpawnOutput(declaration)) => {
                    spawn_target_state(self.id, output.id(), output.target(), &declaration.actor, actor, self.entry, self.model)?
                }
                _ => return Err(ArgentError::new("foreign route output has no source declaration")),
            }
        }
        .ok_or_else(|| ArgentError::new("foreign route has no authored state target"))?;
        let inferred = self.infer_authored(expr, cursor);
        if !inferred.as_ref().is_some_and(|value| {
            let correct_shape = match arity {
                RouteArity::One => value.shape().is_scalar(),
                RouteArity::Many => !value.shape().is_scalar(),
            };
            correct_shape && value.source() == &expected
        }) {
            return Err(ArgentError::new(format!(
                "foreign route state `{}` is not a proven authored `{}` value",
                expr.span.as_str().trim(),
                expected.as_str()
            )));
        }
        Ok(())
    }

    fn infer_authored(&self, expr: &SilExpr<'_>, cursor: &SourceNodeCursor) -> Option<PlannedStateValue> {
        let site = self.program.nodes().find(&cursor.address);
        let binding = site.and_then(|site| self.program.bindings(self.id.actor).sites.get(&site));
        match &expr.kind {
            SilExprKind::Identifier(name) => match binding {
                Some(Binding::Local(id)) => self.local_values.get(id).cloned(),
                Some(Binding::ActorField) => {
                    let state = self.model.types.actor_states.get(&self.id.actor)?;
                    let source = self.model.source_state_id_by_decl(*state).ok()?;
                    self.values.field_values.get(&SourceFieldId::new(source, name)).cloned()
                }
                Some(Binding::Source(ResolvedName::Declaration(id))) if id.kind() == SymbolKind::Const => {
                    self.values.constant_ids.get(id).cloned()
                }
                _ => None,
            },
            SilExprKind::Call { name, args, .. } => {
                if name == word::STATE
                    && let [reference] = args.as_slice()
                {
                    let actor = self.model.actor_by_decl(self.id.actor).ok()?;
                    let entry_model = self.model.entry_model_by_id(self.id).ok()?;
                    let bindings = self.program.bindings(self.id.actor);
                    let reference_cursor = cursor.child(ChildEdge::Argument(0));
                    let current_input_state = |id: LocalId| {
                        bindings
                            .entry_consumes
                            .iter()
                            .find_map(|((entry, index), binding)| {
                                (*entry == self.id.index && *binding == id)
                                    .then(|| entry_model.current().inputs().get(*index))
                                    .flatten()
                            })
                            .and_then(|input| input.target().single_static_actor())
                            .and_then(|id| self.model.static_actor_source_state(id).ok())
                    };
                    let current = match &reference.kind {
                        SilExprKind::Identifier(handle)
                            if handle == word::SELF
                                && self.program.nodes().find(&reference_cursor.address).and_then(|site| bindings.sites.get(&site))
                                    == Some(&Binding::RuntimeRoot) =>
                        {
                            self.model
                                .types
                                .actor_states
                                .get(&self.id.actor)
                                .and_then(|state| self.model.source_state_id_by_decl(*state).ok())
                        }
                        SilExprKind::Identifier(_) => {
                            self.program.nodes().find(&reference_cursor.address).and_then(|site| bindings.sites.get(&site)).and_then(
                                |binding| match binding {
                                    Binding::Local(id) => current_input_state(*id),
                                    _ => None,
                                },
                            )
                        }
                        SilExprKind::ArrayIndex { source, .. } if matches!(&source.kind, SilExprKind::Identifier(_)) => self
                            .program
                            .nodes()
                            .find(&reference_cursor.child(ChildEdge::ExpressionSource).address)
                            .and_then(|site| bindings.sites.get(&site))
                            .and_then(|binding| match binding {
                                Binding::Local(id) => current_input_state(*id),
                                _ => None,
                            }),
                        _ => None,
                    };
                    let observed = if let SilExprKind::FieldAccess { source, field: handle, .. } = &reference.kind
                        && let SilExprKind::FieldAccess { source, field: inputs, .. } = &source.kind
                        && inputs == "inputs"
                        && matches!(&source.kind, SilExprKind::Identifier(_))
                        && let Some(root_site) = self
                            .program
                            .nodes()
                            .find(&reference_cursor.child(ChildEdge::ExpressionSource).child(ChildEdge::ExpressionSource).address)
                        && let Some(Binding::Local(root)) = bindings.sites.get(&root_site)
                    {
                        self.entry
                            .observes
                            .iter()
                            .enumerate()
                            .find(|(index, _)| bindings.entry_observes.get(&(self.id.index, *index)) == Some(root))
                            .and_then(|(_, observe)| {
                                observe.inputs.iter().find(|input| input.name == *handle).and_then(|input| {
                                    observed_open_state_for_decl(self.id, actor, self.entry, observe, input, self.model)
                                        .ok()
                                        .flatten()
                                        .or_else(|| {
                                            self.model
                                                .static_observed_actor_target(self.id, actor, self.entry, observe, input)
                                                .ok()
                                                .flatten()
                                                .and_then(|target| self.model.static_actor_source_state(&target.id()).ok())
                                        })
                                })
                            })
                    } else {
                        None
                    };
                    return Some(PlannedStateValue { source: current.or(observed)?, shape: StateValueShape::Scalar });
                }
                let target = self.program.nodes().find(&cursor.child(ChildEdge::CallTarget).address)?;
                match self.program.bindings(self.id.actor).sites.get(&target) {
                    Some(Binding::Source(ResolvedName::Declaration(id))) if id.kind() == SymbolKind::Function => {
                        self.values.signature_ids.get(&CallableId { owner: *id, member: None })?.result().cloned()
                    }
                    Some(Binding::ActorHelper(member)) => {
                        self.values.signature_ids.get(&CallableId { owner: self.id.actor, member: Some(*member) })?.result().cloned()
                    }
                    _ => None,
                }
            }
            SilExprKind::Array { type_ref, values, .. } => {
                let type_site = self.program.nodes().find(&cursor.child(ChildEdge::TypeUse).address)?;
                let state = match self.program.bindings(self.id.actor).sites.get(&type_site) {
                    Some(Binding::Source(ResolvedName::Declaration(state))) if state.kind() == SymbolKind::State => state,
                    _ => return None,
                };
                let source = self.model.source_state_id_by_decl(*state).ok()?;
                PlannedStateValue::from_bound_type(source, type_ref, Some(values.len()))
            }
            SilExprKind::StructLiteral { .. } | SilExprKind::New { .. } => {
                let type_site = self.program.nodes().find(&cursor.child(ChildEdge::TypeUse).address)?;
                let state = match self.program.bindings(self.id.actor).sites.get(&type_site) {
                    Some(Binding::Source(ResolvedName::Declaration(state))) if state.kind() == SymbolKind::State => state,
                    _ => return None,
                };
                Some(PlannedStateValue { source: self.model.source_state_id_by_decl(*state).ok()?, shape: StateValueShape::Scalar })
            }
            SilExprKind::FieldAccess { source, field, .. } if matches!(&source.kind, SilExprKind::Identifier(root) if root == "self") =>
            {
                let root = self.program.nodes().find(&cursor.child(ChildEdge::ExpressionSource).address)?;
                if !matches!(self.program.bindings(self.id.actor).sites.get(&root), Some(Binding::RuntimeRoot)) {
                    return None;
                }
                let state = self.model.types.actor_states.get(&self.id.actor)?;
                let source = self.model.source_state_id_by_decl(*state).ok()?;
                self.values.field_values.get(&SourceFieldId::new(source, field)).cloned()
            }
            SilExprKind::Append { source, args, .. } => {
                self.infer_authored(source, &cursor.child(ChildEdge::ExpressionSource))?.appended(args.len())
            }
            SilExprKind::ArrayIndex { source, .. } => {
                self.infer_authored(source, &cursor.child(ChildEdge::ExpressionSource))?.element()
            }
            _ => None,
        }
    }
}

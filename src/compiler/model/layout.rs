//! Typed source, storage, and physical state layout plans.

use std::collections::{BTreeMap, BTreeSet};

use crate::compiler::naming::to_snake;
use crate::compiler::syntax::lexer::RESERVED_GENERATED_PREFIX;
use crate::compiler::syntax::word;
use crate::compiler::syntax::{ArrayDim, TypeRef};
use crate::error::{ArgentError, Result};

use super::{InteractionSource, Model, RouteFamily, RouteRootLeaf, observed_open_state_for_decl, spawn_target_state};

#[cfg(test)]
mod tests;

/// Nominal identity of one Argent source state declaration.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct SourceStateId(String);

impl SourceStateId {
    pub(crate) fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// Identity of one storage payload declaration.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct StorageStateId(String);

impl StorageStateId {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// Canonical actor identity across local and linked source spellings.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct CompiledActorId {
    app: String,
    actor: String,
}

impl CompiledActorId {
    pub(crate) fn app(&self) -> &str {
        &self.app
    }

    pub(crate) fn actor(&self) -> &str {
        &self.actor
    }
}

/// Stable identity of a source field.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct SourceFieldId {
    state: SourceStateId,
    field: String,
}

impl SourceFieldId {
    pub(crate) fn state(&self) -> &SourceStateId {
        &self.state
    }

    pub(crate) fn field(&self) -> &str {
        &self.field
    }
}

/// Stable identity of a storage payload field.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct StorageFieldId {
    state: StorageStateId,
    field: String,
}

impl StorageFieldId {
    pub(crate) fn state(&self) -> &StorageStateId {
        &self.state
    }

    pub(crate) fn field(&self) -> &str {
        &self.field
    }
}

/// Compiler-owned meaning carried by one generated physical field.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum GeneratedFieldId {
    Template(CompiledActorId),
    RouteFamilyDigest { app: String, family: String },
    RouteFamilyTable { app: String, family: String, actors: Vec<CompiledActorId> },
}

/// Stable identity of one field in a physical state layout.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum PhysicalFieldId {
    Generated(GeneratedFieldId),
    Storage(StorageFieldId),
}

/// SIL field encoding and its fixed physical width.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LayoutField {
    id: PhysicalFieldId,
    sil_name: String,
    ty: TypeRef,
    sil_type: String,
    packed_len: usize,
}

impl LayoutField {
    pub(crate) fn id(&self) -> &PhysicalFieldId {
        &self.id
    }

    pub(crate) fn sil_name(&self) -> &str {
        &self.sil_name
    }

    pub(crate) fn ty(&self) -> &TypeRef {
        &self.ty
    }

    pub(crate) fn sil_type(&self) -> &str {
        &self.sil_type
    }

    pub(crate) fn packed_len(&self) -> usize {
        self.packed_len
    }
}

/// Ordered physical fields accepted by a SIL state builtin.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PhysicalStateLayout {
    fields: Vec<LayoutField>,
}

impl PhysicalStateLayout {
    pub(crate) fn fields(&self) -> &[LayoutField] {
        &self.fields
    }

    pub(crate) fn field(&self, id: &PhysicalFieldId) -> Option<&LayoutField> {
        self.fields.iter().find(|field| field.id() == id)
    }

    /// Compare field meaning and SIL encoding, not just byte widths or names.
    pub(crate) fn is_sil_compatible_with(&self, other: &Self) -> bool {
        self.fields.len() == other.fields.len()
            && self
                .fields
                .iter()
                .zip(&other.fields)
                .all(|(left, right)| left.id == right.id && left.sil_type == right.sil_type && left.packed_len == right.packed_len)
    }
}

/// Authored fields belonging to one nominal source state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SourceStateLayout {
    id: SourceStateId,
    fields: Vec<(SourceFieldId, TypeRef)>,
}

impl SourceStateLayout {
    pub(crate) fn id(&self) -> &SourceStateId {
        &self.id
    }

    pub(crate) fn fields(&self) -> &[(SourceFieldId, TypeRef)] {
        &self.fields
    }

    pub(crate) fn field_id(&self, name: &str) -> Option<&SourceFieldId> {
        self.fields.iter().find_map(|(id, _)| (id.field() == name).then_some(id))
    }
}

/// Fixed-width fields stored for one source state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StorageStateLayout {
    id: StorageStateId,
    fields: Vec<(StorageFieldId, TypeRef, usize)>,
}

impl StorageStateLayout {
    pub(crate) fn id(&self) -> &StorageStateId {
        &self.id
    }

    pub(crate) fn fields(&self) -> &[(StorageFieldId, TypeRef, usize)] {
        &self.fields
    }

    pub(crate) fn field_id(&self, name: &str) -> Option<&StorageFieldId> {
        self.fields.iter().find_map(|(id, _, _)| (id.field() == name).then_some(id))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SourceFieldLowering {
    Identity { source: SourceFieldId, storage: StorageFieldId },
    Digest { source: SourceFieldId, storage: StorageFieldId, expanded_state: SourceStateId },
}

/// Typed source-to-storage relation for one nominal source state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SourceStorageRelation {
    Identity { fields: Vec<SourceFieldLowering> },
    Expanded { fields: Vec<SourceFieldLowering> },
}

impl SourceStorageRelation {
    pub(crate) fn is_identity(&self) -> bool {
        matches!(self, Self::Identity { .. })
    }

    pub(crate) fn storage_field(&self, source: &SourceFieldId) -> Option<&StorageFieldId> {
        let fields = match self {
            Self::Identity { fields } | Self::Expanded { fields } => fields,
        };
        fields.iter().find_map(|field| match field {
            SourceFieldLowering::Identity { source: candidate, storage }
            | SourceFieldLowering::Digest { source: candidate, storage, .. }
                if candidate == source =>
            {
                Some(storage)
            }
            SourceFieldLowering::Identity { .. } | SourceFieldLowering::Digest { .. } => None,
        })
    }

    fn fields(&self) -> &[SourceFieldLowering] {
        match self {
            Self::Identity { fields } | Self::Expanded { fields } => fields,
        }
    }
}

impl SourceFieldLowering {
    fn source(&self) -> &SourceFieldId {
        match self {
            Self::Identity { source, .. } | Self::Digest { source, .. } => source,
        }
    }

    fn storage(&self) -> &StorageFieldId {
        match self {
            Self::Identity { storage, .. } | Self::Digest { storage, .. } => storage,
        }
    }

    fn is_identity(&self) -> bool {
        matches!(self, Self::Identity { .. })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StoragePhysicalField {
    storage: StorageFieldId,
    physical: PhysicalFieldId,
}

/// Typed storage-to-physical relation for one actor-context target.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum StoragePhysicalRelation {
    Identity { storage_to_physical: Vec<StoragePhysicalField> },
    Augmented { generated_fields: Vec<GeneratedFieldId>, storage_to_physical: Vec<StoragePhysicalField> },
}

impl StoragePhysicalRelation {
    pub(crate) fn is_identity(&self) -> bool {
        matches!(self, Self::Identity { .. })
    }

    pub(crate) fn generated_fields(&self) -> &[GeneratedFieldId] {
        match self {
            Self::Identity { .. } => &[],
            Self::Augmented { generated_fields, .. } => generated_fields,
        }
    }

    pub(crate) fn physical_field(&self, storage: &StorageFieldId) -> Option<&PhysicalFieldId> {
        let fields = match self {
            Self::Identity { storage_to_physical } | Self::Augmented { storage_to_physical, .. } => storage_to_physical,
        };
        fields.iter().find_map(|field| (&field.storage == storage).then_some(&field.physical))
    }
}

/// Stable lookup key for a concrete actor or dynamic target capability.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum PhysicalTargetId {
    Actor(CompiledActorId),
    OpenState(SourceStateId),
    ActorDomain { state: SourceStateId, actors: Vec<CompiledActorId> },
}

/// Contract-local SIL spelling category selected for a state value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SilStateType {
    State,
    Source(SourceStateId),
    StoragePhysical(SourceStateId),
    TargetPhysical(PhysicalTargetId),
}

/// One authored field's stable projection into a target physical layout.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SourcePhysicalField {
    source: SourceFieldId,
    physical: PhysicalFieldId,
    sil_name: String,
    identity: bool,
}

impl SourcePhysicalField {
    pub(crate) fn source(&self) -> &SourceFieldId {
        &self.source
    }

    pub(crate) fn physical(&self) -> &PhysicalFieldId {
        &self.physical
    }

    pub(crate) fn sil_name(&self) -> &str {
        &self.sil_name
    }

    pub(crate) fn is_identity(&self) -> bool {
        self.identity
    }
}

/// One contract-local authored representation decision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SourceRepresentationPlan {
    source: SourceStateId,
    source_to_storage: SourceStorageRelation,
    sil_type: SilStateType,
    active_state_eligible: bool,
}

impl SourceRepresentationPlan {
    pub(crate) fn source(&self) -> &SourceStateId {
        &self.source
    }

    pub(crate) fn source_to_storage(&self) -> &SourceStorageRelation {
        &self.source_to_storage
    }

    pub(crate) fn sil_type(&self) -> &SilStateType {
        &self.sil_type
    }

    /// Eligibility is recorded now; selection of `State` is deliberately later.
    pub(crate) fn active_state_eligible(&self) -> bool {
        self.active_state_eligible
    }
}

/// One target's physical layout and conservative contract-local SIL type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TargetPhysicalPlan {
    id: PhysicalTargetId,
    source: SourceStateId,
    source_to_storage: SourceStorageRelation,
    storage_to_physical: StoragePhysicalRelation,
    physical: PhysicalStateLayout,
    sil_type: SilStateType,
    active_compatible: bool,
}

impl TargetPhysicalPlan {
    pub(crate) fn id(&self) -> &PhysicalTargetId {
        &self.id
    }

    pub(crate) fn source(&self) -> &SourceStateId {
        &self.source
    }

    pub(crate) fn source_to_storage(&self) -> &SourceStorageRelation {
        &self.source_to_storage
    }

    pub(crate) fn storage_to_physical(&self) -> &StoragePhysicalRelation {
        &self.storage_to_physical
    }

    pub(crate) fn physical(&self) -> &PhysicalStateLayout {
        &self.physical
    }

    pub(crate) fn sil_type(&self) -> &SilStateType {
        &self.sil_type
    }

    pub(crate) fn active_compatible(&self) -> bool {
        self.active_compatible
    }

    /// Nominal identity remains separate from physical compatibility.
    pub(crate) fn has_source_identity(&self, requested: &SourceStateId) -> bool {
        &self.source == requested
    }

    /// Resolve authored fields through both layout relations without positions.
    pub(crate) fn source_fields(&self) -> Result<Vec<SourcePhysicalField>> {
        self.source_to_storage
            .fields()
            .iter()
            .map(|field| {
                let physical = self.storage_to_physical.physical_field(field.storage()).ok_or_else(|| {
                    ArgentError::new(format!(
                        "source field `{}.{}` has no physical target mapping",
                        field.source().state().as_str(),
                        field.source().field()
                    ))
                })?;
                let layout_field = self.physical.field(physical).ok_or_else(|| {
                    ArgentError::new(format!(
                        "source field `{}.{}` maps outside its physical target layout",
                        field.source().state().as_str(),
                        field.source().field()
                    ))
                })?;
                Ok(SourcePhysicalField {
                    source: field.source().clone(),
                    physical: physical.clone(),
                    sil_name: layout_field.sil_name().to_string(),
                    identity: field.is_identity(),
                })
            })
            .collect()
    }
}

/// Active source, storage, and physical layouts for one emitted contract.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ContractStateLayout {
    actor: CompiledActorId,
    source: SourceStateLayout,
    storage: StorageStateLayout,
    physical: PhysicalStateLayout,
    source_to_storage: SourceStorageRelation,
    storage_to_physical: StoragePhysicalRelation,
}

impl ContractStateLayout {
    pub(crate) fn actor(&self) -> &CompiledActorId {
        &self.actor
    }

    pub(crate) fn physical(&self) -> &PhysicalStateLayout {
        &self.physical
    }

    pub(crate) fn source(&self) -> &SourceStateLayout {
        &self.source
    }

    pub(crate) fn storage(&self) -> &StorageStateLayout {
        &self.storage
    }

    pub(crate) fn source_to_storage(&self) -> &SourceStorageRelation {
        &self.source_to_storage
    }

    pub(crate) fn storage_to_physical(&self) -> &StoragePhysicalRelation {
        &self.storage_to_physical
    }
}

/// Immutable state lowering environment owned by one emitted contract.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ContractStateLowering {
    active: ContractStateLayout,
    source_representations: BTreeMap<SourceStateId, SourceRepresentationPlan>,
    target_physical: BTreeMap<PhysicalTargetId, TargetPhysicalPlan>,
    actor_targets: BTreeMap<String, PhysicalTargetId>,
}

impl ContractStateLowering {
    pub(crate) fn active(&self) -> &ContractStateLayout {
        &self.active
    }

    pub(crate) fn source_representation(&self, source: &SourceStateId) -> Option<&SourceRepresentationPlan> {
        self.source_representations.get(source)
    }

    pub(crate) fn source_representations(&self) -> &BTreeMap<SourceStateId, SourceRepresentationPlan> {
        &self.source_representations
    }

    pub(crate) fn target(&self, id: &PhysicalTargetId) -> Option<&TargetPhysicalPlan> {
        self.target_physical.get(id)
    }

    pub(crate) fn targets(&self) -> &BTreeMap<PhysicalTargetId, TargetPhysicalPlan> {
        &self.target_physical
    }

    pub(crate) fn target_for_actor(&self, actor: &str) -> Option<&TargetPhysicalPlan> {
        self.actor_targets.get(actor).and_then(|id| self.target(id))
    }

    pub(crate) fn open_state_target(&self, state: &SourceStateId) -> Option<&TargetPhysicalPlan> {
        self.target(&PhysicalTargetId::OpenState(state.clone()))
    }
}

/// Build every emitted contract's plan from validated state and route facts.
pub(crate) fn build_contract_state_lowerings(model: &Model<'_>) -> Result<BTreeMap<String, ContractStateLowering>> {
    model
        .actors
        .iter()
        .map(|actor| build_contract_state_lowering(actor.name.as_str(), model).map(|plan| (actor.name.clone(), plan)))
        .collect()
}

fn build_contract_state_lowering(active_actor: &str, model: &Model<'_>) -> Result<ContractStateLowering> {
    let active_id = compiled_actor_id(active_actor, model)?;
    let active_source = source_state_id(&model.actor(active_actor)?.state);
    let (active_source_layout, active_storage, active_source_to_storage) = state_layouts(&active_source, model)?;
    let (active_physical, active_storage_to_physical) = actor_physical_layout(active_actor, &active_storage, model)?;
    let active = ContractStateLayout {
        actor: active_id,
        source: active_source_layout,
        storage: active_storage,
        physical: active_physical.clone(),
        source_to_storage: active_source_to_storage,
        storage_to_physical: active_storage_to_physical,
    };

    let mut source_representations = BTreeMap::new();
    for state in model.all_states() {
        let source = source_state_id(&state.name);
        if source_representations.contains_key(&source) {
            continue;
        }
        let (_, _, source_to_storage) = state_layouts(&source, model)?;
        let active_state_eligible =
            source == active_source && source_to_storage.is_identity() && active.storage_to_physical.is_identity();
        source_representations.insert(
            source.clone(),
            SourceRepresentationPlan {
                source: source.clone(),
                source_to_storage,
                sil_type: SilStateType::Source(source),
                active_state_eligible,
            },
        );
    }

    let mut target_physical = BTreeMap::new();
    let mut actor_targets = BTreeMap::new();
    for actor in model.app_actors.iter().chain(model.linked_actors.keys()) {
        let plan = concrete_target_plan(actor, active_actor, &active_physical, model)?;
        actor_targets.insert(actor.clone(), plan.id.clone());
        target_physical.insert(plan.id.clone(), plan);
    }

    for source in dynamic_source_states(active_actor, model)? {
        let plan = open_state_target_plan(source, &active_physical, model)?;
        target_physical.insert(plan.id.clone(), plan);
    }

    let actor = model.actor(active_actor)?;
    for entry in &actor.entries {
        for selector in model.entry_model(actor, entry)?.template_selectors().values() {
            let variant_ids = selector
                .variants
                .iter()
                .map(|variant| {
                    actor_targets
                        .get(variant)
                        .cloned()
                        .ok_or_else(|| ArgentError::new(format!("selector target `{variant}` has no physical state plan")))
                })
                .collect::<Result<Vec<_>>>()?;
            let id = PhysicalTargetId::ActorDomain { state: source_state_id(&selector.state), actors: actor_ids(&variant_ids)? };
            if target_physical.contains_key(&id) {
                continue;
            }
            let plan = canonical_domain_plan(&id, &variant_ids, &active_physical, &target_physical).map_err(|err| {
                ArgentError::new(format!(
                    "entry `{}::{}` actor selector `{}` has incompatible target layouts: {err}",
                    actor.name, entry.name, selector.name
                ))
            })?;
            target_physical.insert(id, plan);
        }
    }

    Ok(ContractStateLowering { active, source_representations, target_physical, actor_targets })
}

fn source_state_id(state: &str) -> SourceStateId {
    SourceStateId::new(state)
}

fn storage_state_id(state: &str) -> StorageStateId {
    StorageStateId(state.to_string())
}

fn compiled_actor_id(reference: &str, model: &Model<'_>) -> Result<CompiledActorId> {
    if let Some(linked) = model.linked_actor(reference) {
        Ok(CompiledActorId { app: linked.app.clone(), actor: linked.actor.clone() })
    } else {
        model.actor(reference)?;
        Ok(CompiledActorId { app: model.app_name.clone(), actor: reference.to_string() })
    }
}

fn actor_ids(targets: &[PhysicalTargetId]) -> Result<Vec<CompiledActorId>> {
    targets
        .iter()
        .map(|target| match target {
            PhysicalTargetId::Actor(actor) => Ok(actor.clone()),
            PhysicalTargetId::OpenState(_) | PhysicalTargetId::ActorDomain { .. } => {
                Err(ArgentError::new("actor domain contains a non-actor target"))
            }
        })
        .collect()
}

fn state_layouts(source: &SourceStateId, model: &Model<'_>) -> Result<(SourceStateLayout, StorageStateLayout, SourceStorageRelation)> {
    let state = model.state(source.as_str())?;
    let storage = model.storage_state(source.as_str())?;
    let storage_id = storage_state_id(&storage.name);
    let digest_states = state
        .expansion
        .as_ref()
        .map(|expansion| {
            expansion.digests.iter().map(|digest| (digest.field.as_str(), digest.state.as_str())).collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();

    let mut source_fields = Vec::new();
    let mut storage_fields = Vec::new();
    let mut lowering = Vec::new();
    for field in &storage.fields {
        let source_field = SourceFieldId { state: source.clone(), field: field.name.clone() };
        let storage_field = StorageFieldId { state: storage_id.clone(), field: field.name.clone() };
        let source_ty =
            digest_states.get(field.name.as_str()).map_or_else(|| field.ty.clone(), |expanded| TypeRef::new((*expanded).to_string()));
        source_fields.push((source_field.clone(), source_ty));
        storage_fields.push((storage_field.clone(), field.ty.clone(), packed_layout_field_len(&field.ty, model)?));
        lowering.push(match digest_states.get(field.name.as_str()) {
            Some(expanded) => {
                SourceFieldLowering::Digest { source: source_field, storage: storage_field, expanded_state: source_state_id(expanded) }
            }
            None => SourceFieldLowering::Identity { source: source_field, storage: storage_field },
        });
    }

    let relation = if state.expansion.is_some() {
        SourceStorageRelation::Expanded { fields: lowering }
    } else {
        SourceStorageRelation::Identity { fields: lowering }
    };
    Ok((
        SourceStateLayout { id: source.clone(), fields: source_fields },
        StorageStateLayout { id: storage_id, fields: storage_fields },
        relation,
    ))
}

fn actor_physical_layout(
    actor: &str,
    storage: &StorageStateLayout,
    model: &Model<'_>,
) -> Result<(PhysicalStateLayout, StoragePhysicalRelation)> {
    physical_layout(storage, generated_fields_for_actor(actor, model)?, model)
}

fn physical_layout(
    storage: &StorageStateLayout,
    generated: Vec<(GeneratedFieldId, String, TypeRef)>,
    model: &Model<'_>,
) -> Result<(PhysicalStateLayout, StoragePhysicalRelation)> {
    let generated_ids = generated.iter().map(|(id, _, _)| id.clone()).collect::<Vec<_>>();
    let mut fields = generated
        .into_iter()
        .map(|(id, sil_name, ty)| {
            Ok(LayoutField {
                id: PhysicalFieldId::Generated(id),
                sil_name,
                sil_type: lower_layout_type(&ty, model),
                packed_len: packed_field_len(&ty)?,
                ty,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let mut storage_to_physical = Vec::new();
    for (storage_id, ty, packed_len) in &storage.fields {
        let physical_id = PhysicalFieldId::Storage(storage_id.clone());
        fields.push(LayoutField {
            id: physical_id.clone(),
            sil_name: storage_id.field.clone(),
            ty: ty.clone(),
            sil_type: lower_layout_type(ty, model),
            packed_len: *packed_len,
        });
        storage_to_physical.push(StoragePhysicalField { storage: storage_id.clone(), physical: physical_id });
    }
    let relation = if generated_ids.is_empty() {
        StoragePhysicalRelation::Identity { storage_to_physical }
    } else {
        StoragePhysicalRelation::Augmented { generated_fields: generated_ids, storage_to_physical }
    };
    Ok((PhysicalStateLayout { fields }, relation))
}

fn concrete_target_plan(
    actor: &str,
    active_actor: &str,
    active_physical: &PhysicalStateLayout,
    model: &Model<'_>,
) -> Result<TargetPhysicalPlan> {
    let actor_id = compiled_actor_id(actor, model)?;
    let source = source_state_id(&model.actor(actor)?.state);
    let (_, storage, source_to_storage) = state_layouts(&source, model)?;
    let (physical, storage_to_physical) = actor_physical_layout(actor, &storage, model)?;
    let id = PhysicalTargetId::Actor(actor_id);
    let sil_type = if actor == active_actor {
        SilStateType::State
    } else {
        conservative_named_type(&id, &source, &source_to_storage, &storage_to_physical)
    };
    let active_compatible = physical.is_sil_compatible_with(active_physical);
    Ok(TargetPhysicalPlan { id, source, source_to_storage, storage_to_physical, physical, sil_type, active_compatible })
}

fn open_state_target_plan(
    source: SourceStateId,
    active_physical: &PhysicalStateLayout,
    model: &Model<'_>,
) -> Result<TargetPhysicalPlan> {
    let (_, storage, source_to_storage) = state_layouts(&source, model)?;
    let (physical, storage_to_physical) = physical_layout(&storage, Vec::new(), model)?;
    let id = PhysicalTargetId::OpenState(source.clone());
    let sil_type = conservative_named_type(&id, &source, &source_to_storage, &storage_to_physical);
    let active_compatible = physical.is_sil_compatible_with(active_physical);
    Ok(TargetPhysicalPlan { id, source, source_to_storage, storage_to_physical, physical, sil_type, active_compatible })
}

fn canonical_domain_plan(
    id: &PhysicalTargetId,
    variants: &[PhysicalTargetId],
    active_physical: &PhysicalStateLayout,
    plans: &BTreeMap<PhysicalTargetId, TargetPhysicalPlan>,
) -> Result<TargetPhysicalPlan> {
    let first_id = variants.first().ok_or_else(|| ArgentError::new("actor domain has no variants"))?;
    let first = plans.get(first_id).ok_or_else(|| ArgentError::new("actor domain variant has no physical plan"))?;
    for variant in &variants[1..] {
        let candidate = plans.get(variant).ok_or_else(|| ArgentError::new("actor domain variant has no physical plan"))?;
        if candidate.source != first.source {
            return Err(ArgentError::new(format!(
                "source state `{}` differs from `{}`",
                candidate.source.as_str(),
                first.source.as_str()
            )));
        }
        if !candidate.physical.is_sil_compatible_with(&first.physical) {
            return Err(ArgentError::new("variants do not share one semantic physical layout"));
        }
    }
    let sil_type = conservative_named_type(id, &first.source, &first.source_to_storage, &first.storage_to_physical);
    Ok(TargetPhysicalPlan {
        id: id.clone(),
        source: first.source.clone(),
        source_to_storage: first.source_to_storage.clone(),
        storage_to_physical: first.storage_to_physical.clone(),
        physical: first.physical.clone(),
        sil_type,
        active_compatible: first.physical.is_sil_compatible_with(active_physical),
    })
}

fn conservative_named_type(
    id: &PhysicalTargetId,
    source: &SourceStateId,
    source_to_storage: &SourceStorageRelation,
    storage_to_physical: &StoragePhysicalRelation,
) -> SilStateType {
    if !storage_to_physical.is_identity() {
        SilStateType::TargetPhysical(id.clone())
    } else if !source_to_storage.is_identity() {
        SilStateType::StoragePhysical(source.clone())
    } else {
        SilStateType::Source(source.clone())
    }
}

fn dynamic_source_states(active_actor: &str, model: &Model<'_>) -> Result<BTreeSet<SourceStateId>> {
    let actor = model.actor(active_actor)?;
    let mut states = BTreeSet::new();
    for field in &model.storage_state(&actor.state)?.fields {
        if let Some(state) = &field.ty.actor_state {
            states.insert(source_state_id(state));
        }
    }
    for entry in &actor.entries {
        for param in &entry.params {
            if let Some(state) = &param.ty.actor_state {
                states.insert(source_state_id(state));
            }
        }
        for declaration in entry.body.local_declarations() {
            if let Some(state) = &declaration.binding.actor_type_state {
                states.insert(source_state_id(state));
            }
        }
        for observe in &entry.observes {
            for observed in observe.inputs.iter().chain(&observe.outputs) {
                if model.static_actor_target(&observed.actor).is_none()
                    && let Some(state) = observed_open_state_for_decl(actor, entry, observe, observed, model)?
                {
                    states.insert(source_state_id(&state));
                }
            }
        }
        for group in model.entry_model(actor, entry)?.genesis_groups() {
            for interaction in group.outputs() {
                if model.resolve_static_actor_target(interaction.target()).is_some() {
                    continue;
                }
                let InteractionSource::SpawnOutput(output) = interaction.source() else {
                    unreachable!("genesis output retains its spawn declaration");
                };
                if let Some(state) = spawn_target_state(interaction.target(), &output.actor, actor, entry, model)? {
                    states.insert(source_state_id(&state));
                }
            }
        }
    }
    Ok(states)
}

fn generated_fields_for_actor(actor: &str, model: &Model<'_>) -> Result<Vec<(GeneratedFieldId, String, TypeRef)>> {
    if model.linked_actor(actor).is_some() {
        return Ok(Vec::new());
    }
    let leaves =
        model.route_leaves_by_actor.get(actor).ok_or_else(|| ArgentError::new(format!("actor `{actor}` has no planned route cut")))?;
    let families = model.route_family_for_actor(actor).into_iter().collect::<Vec<_>>();
    let mut fields = Vec::new();
    if families.is_empty() {
        for actor in leaves.iter().filter_map(|leaf| match leaf {
            RouteRootLeaf::Actor(actor) => Some(actor),
            RouteRootLeaf::Family(_) => None,
        }) {
            fields.push(template_field(actor, model)?);
        }
        for family in leaves.iter().filter_map(|leaf| match leaf {
            RouteRootLeaf::Family(family) => Some(family),
            RouteRootLeaf::Actor(_) => None,
        }) {
            let family = model
                .route_family(family)
                .ok_or_else(|| ArgentError::new(format!("route cut references unknown family `{family}`")))?;
            fields.push(route_digest_field(family, model));
        }
        return Ok(fields);
    }

    let family_actors = families.iter().flat_map(|family| family.actors.iter().map(String::as_str)).collect::<BTreeSet<_>>();
    let own_families = families.iter().map(|family| family.id.as_str()).collect::<BTreeSet<_>>();
    for actor in leaves.iter().filter_map(|leaf| match leaf {
        RouteRootLeaf::Actor(actor) if !family_actors.contains(actor.as_str()) => Some(actor),
        RouteRootLeaf::Actor(_) | RouteRootLeaf::Family(_) => None,
    }) {
        fields.push(template_field(actor, model)?);
    }
    for family in leaves.iter().filter_map(|leaf| match leaf {
        RouteRootLeaf::Family(family) if !own_families.contains(family.as_str()) => Some(family),
        RouteRootLeaf::Actor(_) | RouteRootLeaf::Family(_) => None,
    }) {
        let family =
            model.route_family(family).ok_or_else(|| ArgentError::new(format!("route cut references unknown family `{family}`")))?;
        fields.push(route_digest_field(family, model));
    }
    for family in families {
        for actor in family.direct_template_actors() {
            fields.push(template_field(actor, model)?);
        }
        fields.push(route_table_field(family, model)?);
    }
    Ok(fields)
}

fn template_field(actor: &str, model: &Model<'_>) -> Result<(GeneratedFieldId, String, TypeRef)> {
    Ok((GeneratedFieldId::Template(compiled_actor_id(actor, model)?), hidden_template_name(actor), TypeRef::array("byte", 32)))
}

fn route_digest_field(family: &RouteFamily, model: &Model<'_>) -> (GeneratedFieldId, String, TypeRef) {
    (
        GeneratedFieldId::RouteFamilyDigest { app: model.app_name.clone(), family: family.id.clone() },
        hidden_route_family_commitment_name(family),
        TypeRef::array("byte", 32),
    )
}

fn route_table_field(family: &RouteFamily, model: &Model<'_>) -> Result<(GeneratedFieldId, String, TypeRef)> {
    let actors = family.table_actors().iter().map(|actor| compiled_actor_id(actor, model)).collect::<Result<Vec<_>>>()?;
    Ok((
        GeneratedFieldId::RouteFamilyTable { app: model.app_name.clone(), family: family.id.clone(), actors },
        hidden_route_family_table_name(family),
        TypeRef::array("byte", family.table_byte_len()),
    ))
}

fn hidden_actor_suffix(actor: &str) -> String {
    to_snake(&actor.replace(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_', "_"))
}

fn hidden_template_name(actor: &str) -> String {
    format!("{RESERVED_GENERATED_PREFIX}{}_template", hidden_actor_suffix(actor))
}

fn route_family_suffix(family: &str) -> String {
    let hub = family.strip_prefix("route_family/").and_then(|rest| rest.rsplit('/').next()).unwrap_or(family);
    to_snake(hub)
}

fn hidden_route_family_commitment_name(family: &RouteFamily) -> String {
    format!("{RESERVED_GENERATED_PREFIX}{}_routes_digest", route_family_suffix(&family.id))
}

fn hidden_route_family_table_name(family: &RouteFamily) -> String {
    format!("{RESERVED_GENERATED_PREFIX}{}_routes", route_family_suffix(&family.id))
}

/// Return the SIL spelling used by layout compatibility checks.
pub(crate) fn lower_layout_type(ty: &TypeRef, model: &Model<'_>) -> String {
    if model.is_actor_enum_type(ty) {
        "int".to_string()
    } else if ty.name == word::COVENANT_ID && ty.array.is_none() {
        "byte[32]".to_string()
    } else {
        ty.to_sil()
    }
}

pub(crate) fn packed_layout_field_len(ty: &TypeRef, model: &Model<'_>) -> Result<usize> {
    packed_layout_field_len_inner(ty, model, &mut BTreeSet::new())
}

fn packed_layout_field_len_inner(ty: &TypeRef, model: &Model<'_>, visiting: &mut BTreeSet<String>) -> Result<usize> {
    if let Ok(len) = packed_field_len(ty) {
        return Ok(len);
    }
    if ty.array.is_some() || !model.has_state(&ty.name) {
        return packed_field_len(ty);
    }
    if !visiting.insert(ty.name.clone()) {
        return Err(ArgentError::new(format!("recursive state field type `{}` has no fixed packed width", ty.name)));
    }
    let state = model.storage_state(&ty.name)?;
    let len = state.fields.iter().try_fold(0usize, |sum, field| {
        packed_layout_field_len_inner(&field.ty, model, visiting).and_then(|len| {
            sum.checked_add(len).ok_or_else(|| ArgentError::new(format!("state `{}` packed width overflows", ty.name)))
        })
    });
    visiting.remove(&ty.name);
    len
}

/// Return the packed byte width of a supported state field.
pub(crate) fn packed_field_len(ty: &TypeRef) -> Result<usize> {
    if ty.is_actor_type() {
        return Ok(32);
    }
    match (ty.name.as_str(), ty.array) {
        ("int" | "temporal", None) => Ok(8),
        ("bool", None) | ("byte", None) => Ok(1),
        ("byte", Some(ArrayDim::Fixed(len))) => Ok(len),
        ("pubkey", None) | (word::COVENANT_ID, None) => Ok(32),
        ("sig", None) => Ok(65),
        ("datasig", None) => Ok(64),
        ("bytes", None) | ("string", None) | (_, Some(_)) => Err(ArgentError::new("only fixed-width scalar fields are supported")),
        (name, None) => Err(ArgentError::new(format!("unsupported type `{name}`"))),
    }
}

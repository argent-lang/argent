//! Plans authored state values for one emitted contract.

use std::collections::{BTreeMap, BTreeSet};

use silverscript_lang::ast::{
    ArrayDim as SilArrayDim, TypeBase as SilTypeBase, TypeRef as SilTypeRef, parse_type_ref as parse_sil_type_ref,
};

use crate::compiler::model::{Model, ResolvedSuccessor, SilStateType, SourceStateId};
use crate::compiler::syntax::{ActorDecl, ArrayDim, TypeRef};
use crate::error::{ArgentError, Result};

/// One authored state identity and its scalar or array shape.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::compiler::codegen) struct PlannedStateValue {
    source: SourceStateId,
    shape: StateValueShape,
}

/// Cardinality of an authored state value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::compiler::codegen) enum StateValueShape {
    Scalar,
    FixedArray(FixedArrayLength),
    DynamicArray,
}

/// What Argent can prove about one authored fixed-array length.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::compiler::codegen) enum FixedArrayLength {
    Known(usize),
    Unresolved,
}

/// State-valued positions in one contract-local callable signature.
pub(in crate::compiler::codegen) struct CallableSignaturePlan {
    params: Vec<Option<PlannedStateValue>>,
    result: Option<PlannedStateValue>,
}

/// Authored state values and callable signatures for one emitted contract.
pub(in crate::compiler::codegen) struct ContractStateValuePlan {
    authored_sil_types: BTreeMap<SourceStateId, String>,
    /// Source declarations reached by this contract's authored value sites.
    required_source_declarations: BTreeSet<SourceStateId>,
    signatures: BTreeMap<String, CallableSignaturePlan>,
}

impl PlannedStateValue {
    pub(in crate::compiler::codegen) fn source(&self) -> &SourceStateId {
        &self.source
    }

    pub(in crate::compiler::codegen) fn shape(&self) -> StateValueShape {
        self.shape
    }

    pub(in crate::compiler::codegen) fn element(&self) -> Option<Self> {
        (!self.shape.is_scalar()).then(|| Self { source: self.source.clone(), shape: StateValueShape::Scalar })
    }

    pub(in crate::compiler::codegen) fn appended(&self, count: usize) -> Option<Self> {
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

    pub(in crate::compiler::codegen) fn is_proven_incompatible_with(&self, expected: &Self) -> bool {
        self.source != expected.source || self.shape.is_proven_incompatible_with(expected.shape)
    }
}

impl StateValueShape {
    pub(in crate::compiler::codegen) fn is_scalar(self) -> bool {
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

    fn render(self, element_type: &str) -> String {
        match self {
            Self::Scalar => element_type.to_string(),
            Self::FixedArray(FixedArrayLength::Known(len)) => format!("{element_type}[{len}]"),
            Self::FixedArray(FixedArrayLength::Unresolved) => format!("{element_type}[_]"),
            Self::DynamicArray => format!("{element_type}[]"),
        }
    }
}

impl ContractStateValuePlan {
    pub(in crate::compiler::codegen) fn new(actor: &ActorDecl, model: &Model<'_>) -> Result<Self> {
        let lowering = model.state_lowering(&actor.name)?;
        let authored_sil_types = lowering
            .source_representations()
            .iter()
            .map(|(source, representation)| {
                let sil_type = match representation.sil_type() {
                    SilStateType::Source(planned) if planned == source => source.as_str().to_string(),
                    SilStateType::State => "State".to_string(),
                    SilStateType::Source(planned) => {
                        return Err(ArgentError::new(format!(
                            "source state `{}` uses unrelated authored SIL type `{}` in actor `{}`",
                            source.as_str(),
                            planned.as_str(),
                            actor.name
                        )));
                    }
                    SilStateType::StoragePhysical(_) | SilStateType::TargetPhysical(_) => {
                        return Err(ArgentError::new(format!(
                            "source state `{}` uses a physical SIL type at an authored state-value boundary in actor `{}`",
                            source.as_str(),
                            actor.name
                        )));
                    }
                };
                Ok((source.clone(), sil_type))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        let signatures = model
            .functions
            .iter()
            .copied()
            .chain(actor.functions.iter())
            .map(|function| {
                let params = function.params.iter().map(|param| plan_type_ref(&param.ty, &authored_sil_types)).collect();
                let result = function.return_ty.as_ref().and_then(|ty| plan_type_ref(ty, &authored_sil_types));
                (function.name.clone(), CallableSignaturePlan { params, result })
            })
            .collect::<BTreeMap<_, _>>();
        let mut required_source_declarations = model.states.keys().map(SourceStateId::new).collect::<BTreeSet<_>>();
        for ct in &model.consts {
            collect_type_ref_source(&ct.ty, &authored_sil_types, &mut required_source_declarations);
        }
        for signature in signatures.values() {
            required_source_declarations.extend(signature.params.iter().flatten().map(|value| value.source().clone()));
            required_source_declarations.extend(signature.result.iter().map(|value| value.source().clone()));
        }
        for entry in &actor.entries {
            for param in &entry.params {
                collect_type_ref_source(&param.ty, &authored_sil_types, &mut required_source_declarations);
            }
            for source_type in entry.body.declared_value_types() {
                collect_sil_type_source(source_type, &authored_sil_types, &mut required_source_declarations);
            }
            let entry_model = model.entry_model(actor, entry)?;
            for route in entry_model.routes() {
                let ResolvedSuccessor::Constructed { actor: target, .. } = &route.successor else {
                    continue;
                };
                let source = if let Some(selector) = entry_model.template_selectors().get(target) {
                    selector.state.as_str()
                } else {
                    model.actor(target)?.state.as_str()
                };
                collect_sil_type_source(source, &authored_sil_types, &mut required_source_declarations);
            }
            for group in entry_model.groups() {
                for interaction in group.inputs() {
                    for target in interaction.target().static_actors() {
                        collect_sil_type_source(&model.actor(target)?.state, &authored_sil_types, &mut required_source_declarations);
                    }
                }
            }
            for group in entry_model.existing_groups().chain(entry_model.genesis_groups()) {
                for interaction in group.outputs() {
                    for target in interaction.target().static_actors() {
                        collect_sil_type_source(&model.actor(target)?.state, &authored_sil_types, &mut required_source_declarations);
                    }
                }
            }
        }
        collect_required_source_dependencies(model, &authored_sil_types, &mut required_source_declarations)?;
        Ok(Self { authored_sil_types, required_source_declarations, signatures })
    }

    #[cfg(test)]
    pub(super) fn with_authored_sil_types(authored_sil_types: BTreeMap<SourceStateId, String>) -> Self {
        let required_source_declarations = authored_sil_types.keys().cloned().collect();
        Self { authored_sil_types, required_source_declarations, signatures: BTreeMap::new() }
    }

    pub(in crate::compiler::codegen) fn signature(&self, name: &str) -> Option<&CallableSignaturePlan> {
        self.signatures.get(name)
    }

    pub(in crate::compiler::codegen) fn plan_type_ref(&self, ty: &TypeRef) -> Option<PlannedStateValue> {
        plan_type_ref(ty, &self.authored_sil_types)
    }

    pub(in crate::compiler::codegen) fn plan_sil_type(&self, ty: &str) -> Option<PlannedStateValue> {
        let ty = parse_sil_type_ref(ty).ok()?;
        self.plan_ast_type_ref(&ty, None)
    }

    pub(in crate::compiler::codegen) fn plan_source_sil_type(&self, ty: &str) -> Option<PlannedStateValue> {
        let ty = parse_sil_type_ref(ty).ok()?;
        let SilTypeBase::Custom(name) = &ty.base else {
            return None;
        };
        let source = SourceStateId::new(name);
        if !self.authored_sil_types.contains_key(&source) {
            return None;
        }
        state_value_shape(&ty, None).map(|shape| PlannedStateValue { source, shape })
    }

    pub(in crate::compiler::codegen) fn plan_ast_type_ref(
        &self,
        ty: &SilTypeRef,
        inferred_len: Option<usize>,
    ) -> Option<PlannedStateValue> {
        let SilTypeBase::Custom(name) = &ty.base else {
            return None;
        };
        let shape = state_value_shape(ty, inferred_len)?;
        let named_source = SourceStateId::new(name.clone());
        let source = if self.authored_sil_types.contains_key(&named_source) {
            named_source
        } else {
            self.authored_sil_types.iter().find_map(|(source, sil_type)| (sil_type == name).then(|| source.clone()))?
        };
        Some(PlannedStateValue { source, shape })
    }

    pub(in crate::compiler::codegen) fn sil_type(&self, value: &PlannedStateValue) -> String {
        let element_type = self.authored_sil_types.get(value.source()).expect("planned state value belongs to this contract");
        value.shape().render(element_type)
    }

    pub(in crate::compiler::codegen) fn sil_type_for_type_ref(&self, ty: &TypeRef) -> Option<String> {
        self.plan_type_ref(ty).map(|value| self.sil_type(&value))
    }

    pub(in crate::compiler::codegen) fn sil_type_for_sil_type(&self, ty: &str) -> Option<String> {
        let mut parsed = parse_sil_type_ref(ty).ok()?;
        let value = self.plan_ast_type_ref(&parsed, None)?;
        parsed.base = SilTypeBase::Custom(self.authored_sil_types.get(value.source())?.clone());
        Some(parsed.type_name())
    }

    pub(in crate::compiler::codegen) fn authored_sil_type(&self, source: &SourceStateId) -> Option<&str> {
        self.authored_sil_types.get(source).map(String::as_str)
    }

    pub(in crate::compiler::codegen) fn authored_sil_type_for_name(&self, source: &str) -> Option<&str> {
        self.authored_sil_type(&SourceStateId::new(source))
    }

    pub(in crate::compiler::codegen) fn equivalent_state_sources(&self) -> impl Iterator<Item = &SourceStateId> {
        self.authored_sil_types.iter().filter_map(|(source, sil_type)| (sil_type == "State").then_some(source))
    }

    pub(in crate::compiler::codegen) fn required_named_source_declarations(&self) -> impl Iterator<Item = &SourceStateId> {
        self.required_source_declarations
            .iter()
            .filter(|source| self.authored_sil_types.get(*source).is_some_and(|sil_type| sil_type != "State"))
    }

    pub(in crate::compiler::codegen) fn has_equivalent_state_sources(&self) -> bool {
        self.equivalent_state_sources().next().is_some()
    }

    pub(in crate::compiler::codegen) fn plan_initialized_sil_type(
        &self,
        declared_ty: &str,
        initializer: Option<&PlannedStateValue>,
    ) -> Option<PlannedStateValue> {
        let ty = parse_sil_type_ref(declared_ty).ok()?;
        let declared = self.plan_source_sil_type(declared_ty)?;
        if !matches!(ty.array_dims.as_slice(), [SilArrayDim::Inferred]) {
            return Some(declared);
        }
        initializer
            .filter(|value| value.source() == declared.source() && matches!(value.shape(), StateValueShape::FixedArray(_)))
            .cloned()
            .or(Some(declared))
    }
}

fn collect_type_ref_source(
    ty: &TypeRef,
    authored_sil_types: &BTreeMap<SourceStateId, String>,
    required_sources: &mut BTreeSet<SourceStateId>,
) {
    if let Some(value) = plan_type_ref(ty, authored_sil_types) {
        required_sources.insert(value.source);
    }
}

fn collect_sil_type_source(
    ty: &str,
    authored_sil_types: &BTreeMap<SourceStateId, String>,
    required_sources: &mut BTreeSet<SourceStateId>,
) {
    let Ok(ty) = parse_sil_type_ref(ty) else {
        return;
    };
    let SilTypeBase::Custom(name) = ty.base else {
        return;
    };
    let source = SourceStateId::new(name);
    if authored_sil_types.contains_key(&source) {
        required_sources.insert(source);
    }
}

fn collect_required_source_dependencies(
    model: &Model<'_>,
    authored_sil_types: &BTreeMap<SourceStateId, String>,
    required_sources: &mut BTreeSet<SourceStateId>,
) -> Result<()> {
    let mut pending = required_sources.iter().cloned().collect::<Vec<_>>();
    let mut cursor = 0;
    while let Some(source) = pending.get(cursor) {
        let state = model.state(source.as_str())?;
        let storage = model.storage_state(source.as_str())?;
        for field in &storage.fields {
            let dependency = state
                .expansion
                .as_ref()
                .and_then(|expansion| expansion.digests.iter().find(|digest| digest.field == field.name))
                .map(|digest| SourceStateId::new(&digest.state))
                .or_else(|| plan_type_ref(&field.ty, authored_sil_types).map(|value| value.source));
            if let Some(dependency) = dependency
                && required_sources.insert(dependency.clone())
            {
                pending.push(dependency);
            }
        }
        cursor += 1;
    }
    Ok(())
}

fn state_value_shape(ty: &SilTypeRef, inferred_len: Option<usize>) -> Option<StateValueShape> {
    match ty.array_dims.as_slice() {
        [] => Some(StateValueShape::Scalar),
        [SilArrayDim::Fixed(len)] => Some(StateValueShape::FixedArray(FixedArrayLength::Known(*len))),
        [SilArrayDim::Dynamic] => Some(StateValueShape::DynamicArray),
        [SilArrayDim::Inferred] => {
            Some(StateValueShape::FixedArray(inferred_len.map_or(FixedArrayLength::Unresolved, FixedArrayLength::Known)))
        }
        [SilArrayDim::Constant(_)] => Some(StateValueShape::FixedArray(FixedArrayLength::Unresolved)),
        _ => None,
    }
}

impl CallableSignaturePlan {
    pub(in crate::compiler::codegen) fn has_state_param(&self) -> bool {
        self.params.iter().any(Option::is_some)
    }

    pub(in crate::compiler::codegen) fn param(&self, index: usize) -> Option<&PlannedStateValue> {
        self.params.get(index)?.as_ref()
    }

    pub(in crate::compiler::codegen) fn result(&self) -> Option<&PlannedStateValue> {
        self.result.as_ref()
    }
}

fn plan_type_ref(ty: &TypeRef, authored_sil_types: &BTreeMap<SourceStateId, String>) -> Option<PlannedStateValue> {
    let source = SourceStateId::new(&ty.name);
    let shape = match ty.array {
        None => StateValueShape::Scalar,
        Some(ArrayDim::Fixed(len)) => StateValueShape::FixedArray(FixedArrayLength::Known(len)),
        Some(ArrayDim::Dynamic) => StateValueShape::DynamicArray,
    };
    authored_sil_types.contains_key(&source).then_some(PlannedStateValue { source, shape })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_scalar_fixed_and_dynamic_state_values() {
        let source = SourceStateId::new("PeerState");
        let authored_sil_types = [(source.clone(), "PeerState".to_string())].into_iter().collect::<BTreeMap<_, _>>();

        assert_eq!(
            plan_type_ref(&TypeRef::new("PeerState"), &authored_sil_types),
            Some(PlannedStateValue { source: source.clone(), shape: StateValueShape::Scalar })
        );
        assert_eq!(
            plan_type_ref(&TypeRef::array("PeerState", 3), &authored_sil_types),
            Some(PlannedStateValue { source: source.clone(), shape: StateValueShape::FixedArray(FixedArrayLength::Known(3)) })
        );
        assert_eq!(
            plan_type_ref(&TypeRef::dynamic_array("PeerState"), &authored_sil_types),
            Some(PlannedStateValue { source, shape: StateValueShape::DynamicArray })
        );
        assert_eq!(plan_type_ref(&TypeRef::new("int"), &authored_sil_types), None);

        let plan = ContractStateValuePlan::with_authored_sil_types(authored_sil_types);
        let fixed = plan.plan_sil_type("PeerState[3]").expect("fixed body binding is planned");
        assert_eq!(fixed.shape(), StateValueShape::FixedArray(FixedArrayLength::Known(3)));
        assert_eq!(plan.sil_type(&fixed), "PeerState[3]");
        assert_eq!(fixed.element().expect("fixed array has elements").shape(), StateValueShape::Scalar);
        assert_eq!(
            fixed.appended(2).expect("fixed array can append").shape(),
            StateValueShape::FixedArray(FixedArrayLength::Known(5))
        );
        let dynamic = plan.plan_sil_type("PeerState[]").expect("dynamic body binding is planned");
        assert_eq!(dynamic.shape(), StateValueShape::DynamicArray);
        assert_eq!(plan.sil_type(&dynamic), "PeerState[]");
        assert_eq!(dynamic.appended(2).expect("dynamic array can append").shape(), StateValueShape::DynamicArray);

        let inferred = parse_sil_type_ref("PeerState[_]").expect("inferred Sil array type parses");
        assert_eq!(
            plan.plan_ast_type_ref(&inferred, Some(4)).expect("literal length resolves inferred shape").shape(),
            StateValueShape::FixedArray(FixedArrayLength::Known(4))
        );
        let constant = plan.plan_sil_type("PeerState[COUNT]").expect("constant length remains a state array");
        assert_eq!(constant.shape(), StateValueShape::FixedArray(FixedArrayLength::Unresolved));
        assert_eq!(plan.sil_type_for_sil_type("PeerState[COUNT]").as_deref(), Some("PeerState[COUNT]"));
        assert!(!constant.is_proven_incompatible_with(&fixed));
        assert!(constant.is_proven_incompatible_with(&dynamic));

        let inferred_local =
            plan.plan_initialized_sil_type("PeerState[_]", Some(&fixed)).expect("an inferred local uses its fixed initializer shape");
        assert_eq!(inferred_local, fixed);
    }

    #[test]
    fn renders_every_supported_authored_array_shape_with_selected_state() {
        let source = SourceStateId::new("PeerState");
        let plan = ContractStateValuePlan::with_authored_sil_types([(source, "State".to_string())].into_iter().collect());

        for (source_ty, expected) in [
            ("PeerState", "State"),
            ("PeerState[2]", "State[2]"),
            ("PeerState[_]", "State[_]"),
            ("PeerState[COUNT]", "State[COUNT]"),
            ("PeerState[]", "State[]"),
        ] {
            assert_eq!(plan.sil_type_for_sil_type(source_ty).as_deref(), Some(expected));
        }
    }
}

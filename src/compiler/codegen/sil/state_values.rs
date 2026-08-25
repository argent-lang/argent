//! Plans authored state values for one emitted contract.

use std::collections::BTreeMap;

use silverscript_lang::ast::{ArrayDim as SilArrayDim, TypeBase as SilTypeBase, parse_type_ref as parse_sil_type_ref};

use crate::compiler::model::{Model, SilStateType, SourceStateId};
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
    FixedArray(usize),
    DynamicArray,
}

/// State-valued positions in one contract-local callable signature.
pub(in crate::compiler::codegen) struct CallableSignaturePlan {
    params: Vec<Option<PlannedStateValue>>,
    result: Option<PlannedStateValue>,
}

/// Authored state values and callable signatures for one emitted contract.
pub(in crate::compiler::codegen) struct ContractStateValuePlan {
    authored_sil_types: BTreeMap<SourceStateId, String>,
    signatures: BTreeMap<String, CallableSignaturePlan>,
}

impl PlannedStateValue {
    pub(in crate::compiler::codegen) fn source(&self) -> &SourceStateId {
        &self.source
    }

    pub(in crate::compiler::codegen) fn shape(&self) -> StateValueShape {
        self.shape
    }
}

impl StateValueShape {
    pub(in crate::compiler::codegen) fn is_scalar(self) -> bool {
        self == Self::Scalar
    }

    fn render(self, element_type: &str) -> String {
        match self {
            Self::Scalar => element_type.to_string(),
            Self::FixedArray(len) => format!("{element_type}[{len}]"),
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
                    SilStateType::State => {
                        return Err(ArgentError::new(format!(
                            "source state `{}` selected equivalent `State` before the contract-wide optimization in actor `{}`",
                            source.as_str(),
                            actor.name
                        )));
                    }
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
            .collect();
        Ok(Self { authored_sil_types, signatures })
    }

    pub(in crate::compiler::codegen) fn signature(&self, name: &str) -> Option<&CallableSignaturePlan> {
        self.signatures.get(name)
    }

    pub(in crate::compiler::codegen) fn plan_type_ref(&self, ty: &TypeRef) -> Option<PlannedStateValue> {
        plan_type_ref(ty, &self.authored_sil_types)
    }

    pub(in crate::compiler::codegen) fn plan_sil_type(&self, ty: &str) -> Option<PlannedStateValue> {
        let ty = parse_sil_type_ref(ty).ok()?;
        let SilTypeBase::Custom(name) = ty.base else {
            return None;
        };
        let shape = match ty.array_dims.as_slice() {
            [] => StateValueShape::Scalar,
            [SilArrayDim::Fixed(len)] => StateValueShape::FixedArray(*len),
            [SilArrayDim::Dynamic] => StateValueShape::DynamicArray,
            _ => return None,
        };
        let source = SourceStateId::new(name);
        self.authored_sil_types.contains_key(&source).then_some(PlannedStateValue { source, shape })
    }

    pub(in crate::compiler::codegen) fn sil_type(&self, value: &PlannedStateValue) -> String {
        let element_type = self.authored_sil_types.get(value.source()).expect("planned state value belongs to this contract");
        value.shape().render(element_type)
    }

    pub(in crate::compiler::codegen) fn sil_type_for_type_ref(&self, ty: &TypeRef) -> Option<String> {
        self.plan_type_ref(ty).map(|value| self.sil_type(&value))
    }

    pub(in crate::compiler::codegen) fn sil_type_for_sil_type(&self, ty: &str) -> Option<String> {
        self.plan_sil_type(ty).map(|value| self.sil_type(&value))
    }
}

impl CallableSignaturePlan {
    pub(in crate::compiler::codegen) fn has_scalar_state_param(&self) -> bool {
        self.params.iter().flatten().any(|value| value.shape().is_scalar())
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
        Some(ArrayDim::Fixed(len)) => StateValueShape::FixedArray(len),
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
            Some(PlannedStateValue { source: source.clone(), shape: StateValueShape::FixedArray(3) })
        );
        assert_eq!(
            plan_type_ref(&TypeRef::dynamic_array("PeerState"), &authored_sil_types),
            Some(PlannedStateValue { source, shape: StateValueShape::DynamicArray })
        );
        assert_eq!(plan_type_ref(&TypeRef::new("int"), &authored_sil_types), None);

        let plan = ContractStateValuePlan { authored_sil_types, signatures: BTreeMap::new() };
        let fixed = plan.plan_sil_type("PeerState[3]").expect("fixed body binding is planned");
        assert_eq!(fixed.shape(), StateValueShape::FixedArray(3));
        assert_eq!(plan.sil_type(&fixed), "PeerState[3]");
        let dynamic = plan.plan_sil_type("PeerState[]").expect("dynamic body binding is planned");
        assert_eq!(dynamic.shape(), StateValueShape::DynamicArray);
        assert_eq!(plan.sil_type(&dynamic), "PeerState[]");
    }
}

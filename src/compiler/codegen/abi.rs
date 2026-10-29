//! Converts compiled actor contracts and semantic layouts to portable Sil ABI facts.

use std::collections::BTreeMap;

use crate::artifact::*;
use crate::compiler::model::{AppCompilationContext, GeneratedFieldId, PhysicalFieldId, SilStateType, SourceStateId};
use crate::compiler::syntax::node::DeclId;
use crate::compiler::syntax::{ActorDecl, ArrayDim, TypeRef, word};
use crate::error::{ArgentError, Result};
use silverscript_lang::ast::Expr as SilExpr;
use silverscript_lang::compiler::{COMPILER_VERSION, sil_abi_artifact_from_compiled};

use super::compile::CompiledActors;
use super::sil::render_sil_state_type;

#[cfg(test)]
mod tests;

#[derive(Clone, Debug, Eq, PartialEq)]
enum AbiStructIdentity {
    Source(SourceStateId),
    Physical { source: SourceStateId, fields: Vec<PhysicalFieldId> },
}

pub(super) fn sil_abi_artifact(model: &AppCompilationContext<'_>, compiled_actors: &CompiledActors<'_>) -> Result<SilAbiArtifact> {
    let mut combined = SilAbiArtifact {
        schema_version: SIL_ABI_SCHEMA_VERSION,
        compiler_version: COMPILER_VERSION.to_string(),
        structs: BTreeMap::new(),
        contracts: BTreeMap::new(),
    };
    let mut struct_sources = BTreeMap::new();

    for (actor_id, _) in model.app_actors.iter_with_ids() {
        let actor = model.actor_by_decl(actor_id)?;
        let args = constructor_args_for_actor(actor_id, actor, model)?;
        let compiled = compiled_actors.compile_base(actor_id, &args)?;
        let mut artifact = sil_abi_artifact_from_compiled(&compiled, &args)
            .map_err(|err| ArgentError::new(format!("cannot build Sil ABI for actor `{}`: {err}", actor.name)))?;
        if artifact.contract(&actor.name).is_none() {
            return Err(ArgentError::new(format!("generated Sil ABI has no contract for actor `{}`", actor.name)));
        }
        canonicalize_sil_abi_struct_state_refs(actor_id, actor, model, &mut artifact)?;
        let lowering = model.state_lowering_by_id(actor_id)?;
        let mut incoming_sources = BTreeMap::new();
        let mut record_identity = |name: String, identity: AbiStructIdentity| -> Result<()> {
            if artifact.structs.contains_key(&name)
                && let Some(existing) = incoming_sources.insert(name.clone(), identity.clone())
                && existing != identity
            {
                return Err(ArgentError::new(format!("Sil struct `{name}` has conflicting semantic identities")));
            }
            Ok(())
        };
        for (source, representation) in lowering.source_representations() {
            let name = match representation.sil_type() {
                SilStateType::State => source.as_str().to_string(),
                SilStateType::Source(planned) if planned == source => render_sil_state_type(representation.sil_type())?,
                SilStateType::Source(_) | SilStateType::StoragePhysical(_) | SilStateType::TargetPhysical(_) => {
                    return Err(ArgentError::new(format!("state `{}` has an invalid source representation", source.as_str())));
                }
            };
            record_identity(name, AbiStructIdentity::Source(source.clone()))?;
        }
        for selected in lowering
            .output_physical_types()
            .map(|output| {
                lowering
                    .target(output.canonical_target())
                    .map(|physical| (output.sil_type(), physical))
                    .ok_or_else(|| ArgentError::new("output ABI type has no canonical physical target"))
            })
            .chain(lowering.target_physical_plans().map(|physical| Ok((physical.sil_type(), physical))))
        {
            let (sil_type, physical) = selected?;
            match sil_type {
                SilStateType::State | SilStateType::Source(_) => continue,
                SilStateType::StoragePhysical(_) | SilStateType::TargetPhysical(_) => {}
            }
            let Ok(name) = render_sil_state_type(sil_type) else { continue };
            let identity = AbiStructIdentity::Physical {
                source: physical.source().clone(),
                fields: physical.physical().fields().iter().map(|field| field.id().clone()).collect(),
            };
            record_identity(name, identity)?;
        }
        combined = merge_sil_abi_artifacts(combined, artifact, &mut struct_sources, incoming_sources)?;
    }

    Ok(combined)
}

/// Remove contract-local `State` references from globally stored Sil structs.
///
/// Argent emits such a reference only when the actor's authored state and
/// physical `State` layouts are identical. Preserve that layout under its
/// stable Argent name before artifacts from several contracts are merged.
fn canonicalize_sil_abi_struct_state_refs(
    actor_id: DeclId,
    actor: &ActorDecl,
    model: &AppCompilationContext<'_>,
    artifact: &mut SilAbiArtifact,
) -> Result<()> {
    if !artifact.structs.values().any(struct_references_state) {
        return Ok(());
    }

    let source = model.source_state_id_by_decl(model.types.actor_states[&actor_id])?;
    let representation = model
        .state_lowering_by_id(actor_id)?
        .source_representation(&source)
        .ok_or_else(|| ArgentError::new(format!("actor `{}` has no authored state representation", actor.name)))?;
    if representation.sil_type() != &SilStateType::State {
        return Err(ArgentError::new(format!(
            "generated Sil ABI for actor `{}` contains a global struct that references physical `State`, but `{}` is not equivalent to `State`",
            actor.name, actor.state
        )));
    }

    let contract = artifact
        .contract(&actor.name)
        .ok_or_else(|| ArgentError::new(format!("generated Sil ABI has no contract for actor `{}`", actor.name)))?;
    let authored_state = StructArtifact {
        fields: contract
            .runtime_state
            .fields
            .iter()
            .map(|field| FieldArtifact { name: field.name.clone(), ty: field.ty.clone() })
            .collect(),
    };

    let canonical_name = source.as_str();
    if let Some(existing) = artifact.structs.get(canonical_name)
        && existing != &authored_state
    {
        return Err(ArgentError::new(format!(
            "cannot canonicalize Sil `State` as authored state `{}` for actor `{}`: their fields differ",
            canonical_name, actor.name
        )));
    }
    artifact.structs.insert(canonical_name.to_string(), authored_state);

    for structure in artifact.structs.values_mut() {
        for field in &mut structure.fields {
            replace_state_type_ref(&mut field.ty, canonical_name);
        }
    }
    Ok(())
}

fn struct_references_state(structure: &StructArtifact) -> bool {
    structure.fields.iter().any(|field| type_references_state(&field.ty))
}

fn type_references_state(ty: &TypeArtifact) -> bool {
    match ty {
        TypeArtifact::Struct { name } => name == "State",
        TypeArtifact::FixedArray { item, .. } | TypeArtifact::DynamicArray { item } => type_references_state(item),
        TypeArtifact::Int
        | TypeArtifact::Temporal
        | TypeArtifact::Bool
        | TypeArtifact::Byte
        | TypeArtifact::Bytes
        | TypeArtifact::Text
        | TypeArtifact::Pubkey
        | TypeArtifact::Sig
        | TypeArtifact::Datasig
        | TypeArtifact::FixedBytes { .. } => false,
    }
}

fn replace_state_type_ref(ty: &mut TypeArtifact, authored_state: &str) {
    match ty {
        TypeArtifact::Struct { name } if name == "State" => *name = authored_state.to_string(),
        TypeArtifact::FixedArray { item, .. } | TypeArtifact::DynamicArray { item } => {
            replace_state_type_ref(item, authored_state);
        }
        TypeArtifact::Int
        | TypeArtifact::Temporal
        | TypeArtifact::Bool
        | TypeArtifact::Byte
        | TypeArtifact::Bytes
        | TypeArtifact::Text
        | TypeArtifact::Pubkey
        | TypeArtifact::Sig
        | TypeArtifact::Datasig
        | TypeArtifact::FixedBytes { .. }
        | TypeArtifact::Struct { .. } => {}
    }
}

/// Merge two complete Sil ABI artifacts without changing their contracts or
/// struct definitions.
fn merge_sil_abi_artifacts(
    mut left: SilAbiArtifact,
    right: SilAbiArtifact,
    left_sources: &mut BTreeMap<String, AbiStructIdentity>,
    right_sources: BTreeMap<String, AbiStructIdentity>,
) -> Result<SilAbiArtifact> {
    if left.schema_version != right.schema_version {
        return Err(ArgentError::new(format!(
            "cannot merge Sil ABI schema versions {} and {}",
            left.schema_version, right.schema_version
        )));
    }
    if left.compiler_version != right.compiler_version {
        return Err(ArgentError::new(format!(
            "cannot merge Sil ABI compiler versions `{}` and `{}`",
            left.compiler_version, right.compiler_version
        )));
    }

    for (name, structure) in right.structs {
        if !right_sources.contains_key(&name) {
            return Err(ArgentError::new(format!("Sil struct `{name}` has no semantic identity")));
        }
        if let Some(existing) = left.structs.get(&name) {
            if existing != &structure {
                return Err(ArgentError::new(format!("cannot merge conflicting Sil struct `{name}`")));
            }
            if left_sources.get(&name) != right_sources.get(&name) {
                return Err(ArgentError::new(format!("cannot merge Sil struct `{name}` from distinct semantic identities")));
            }
        } else {
            left.structs.insert(name, structure);
        }
    }
    for (name, contract) in right.contracts {
        if left.contracts.insert(name.clone(), contract).is_some() {
            return Err(ArgentError::new(format!("cannot merge duplicate Sil contract `{name}`")));
        }
    }
    left_sources.extend(right_sources);

    Ok(left)
}

pub(super) fn constructor_args_for_actor<'i>(
    actor_id: DeclId,
    actor: &ActorDecl,
    model: &AppCompilationContext<'_>,
) -> Result<Vec<SilExpr<'i>>> {
    let state = model.storage_state_for_actor(actor_id)?;
    let lowering = model.state_lowering_by_id(actor_id)?;
    let generated_fields = lowering
        .active()
        .physical()
        .fields()
        .iter()
        .filter(|field| matches!(field.id(), PhysicalFieldId::Generated(_)))
        .collect::<Vec<_>>();
    let mut args = Vec::with_capacity(generated_fields.len() + state.fields.len());

    // These placeholders are valid because Argent-generated constructor
    // arguments are state initializers: hidden template commitments and source
    // state fields. If a constructor argument affects code shape outside the
    // compiled state span, the template hash changes and the contract must be
    // recompiled for that value.
    for field in generated_fields {
        args.push(placeholder_expr_for_type(field.ty()).map_err(|err| {
            ArgentError::new(format!(
                "cannot build placeholder constructor argument for actor `{}` generated field `{}`: {err}",
                actor.name,
                field.sil_name()
            ))
        })?);
    }
    for field in &state.fields {
        args.push(placeholder_expr_for_type(&field.ty).map_err(|err| {
            ArgentError::new(format!(
                "cannot build placeholder constructor argument for actor `{}` field `{}`: {err}",
                actor.name, field.name
            ))
        })?);
    }

    Ok(args)
}

pub(super) fn placeholder_expr_for_type<'i>(ty: &TypeRef) -> Result<SilExpr<'i>> {
    if ty.is_actor_type() {
        return Ok(zero_byte_array_expr(32));
    }
    match (&ty.name[..], ty.array) {
        ("byte", Some(ArrayDim::Fixed(len))) => Ok(zero_byte_array_expr(len)),
        (_, Some(ArrayDim::Fixed(len))) => {
            let item = TypeRef::new(ty.name.clone());
            let values = (0..len).map(|_| placeholder_expr_for_type(&item)).collect::<Result<Vec<_>>>()?;
            SilExpr::try_from(values).map_err(|err| ArgentError::new(err.to_string()))
        }
        (_, Some(ArrayDim::Dynamic)) => Err(ArgentError::new("dynamic arrays are not supported in actor state")),
        ("int", None) => Ok(SilExpr::int(0)),
        ("temporal", None) => Ok(SilExpr::temporal(0)),
        ("bool", None) => Ok(SilExpr::bool(false)),
        ("byte", None) => Ok(SilExpr::byte(0)),
        ("string", None) => Ok(SilExpr::string("")),
        ("pubkey", None) => Ok(zero_byte_array_expr(32)),
        (word::COVENANT_ID, None) => Ok(zero_byte_array_expr(32)),
        ("sig", None) => Ok(zero_byte_array_expr(65)),
        ("datasig", None) => Ok(zero_byte_array_expr(64)),
        (name, None) => Err(ArgentError::new(format!("unsupported constructor placeholder type `{name}`"))),
    }
}

fn zero_byte_array_expr<'i>(len: usize) -> SilExpr<'i> {
    SilExpr::bytes(vec![0; len])
}

pub(super) fn extract_sil_template(compiled: &CompiledContractArtifact) -> Result<ActorTemplateArtifact> {
    let (prefix, _, suffix) =
        compiled.script_parts(&compiled.bytecode).ok_or_else(|| ArgentError::new("compiled state span is outside its script"))?;
    Ok(ActorTemplateArtifact { prefix: prefix.to_vec(), suffix: suffix.to_vec(), hash: compiled.template_hash })
}

fn runtime_state_field_defs_for_actor(
    actor: DeclId,
    model: &AppCompilationContext<'_>,
) -> Result<Vec<(String, TypeArtifact, Option<RuntimeFieldRoleArtifact>)>> {
    model
        .state_lowering_by_id(actor)?
        .active()
        .physical()
        .fields()
        .iter()
        .map(|field| {
            let role = match field.id() {
                PhysicalFieldId::Storage(_) => None,
                PhysicalFieldId::Generated(GeneratedFieldId::Template(actor)) => {
                    Some(RuntimeFieldRoleArtifact::Template { contract: actor.actor().to_string() })
                }
                PhysicalFieldId::Generated(GeneratedFieldId::RouteFamilyDigest { family, .. }) => {
                    Some(RuntimeFieldRoleArtifact::TemplateDigest { id: family.clone() })
                }
                PhysicalFieldId::Generated(GeneratedFieldId::RouteFamilyTable { actors, .. }) => {
                    Some(RuntimeFieldRoleArtifact::TemplateTable {
                        contracts: actors.iter().map(|actor| actor.actor().to_string()).collect(),
                    })
                }
            };
            Ok((field.sil_name().to_string(), type_artifact(field.ty()), role))
        })
        .collect()
}

pub(super) fn runtime_state_fields_for_actor(actor: DeclId, model: &AppCompilationContext<'_>) -> Result<Vec<RuntimeFieldArtifact>> {
    Ok(runtime_state_field_defs_for_actor(actor, model)?
        .into_iter()
        .map(|(name, ty, _role)| RuntimeFieldArtifact { name, ty })
        .collect())
}

pub(super) fn runtime_state_plan_artifact(
    actor_id: DeclId,
    actor: &ActorDecl,
    model: &AppCompilationContext<'_>,
) -> Result<Option<RuntimeStatePlanArtifact>> {
    let field_roles = runtime_state_field_defs_for_actor(actor_id, model)?
        .into_iter()
        .filter_map(|(name, _ty, role)| role.map(|role| RuntimeFieldRolePlanArtifact { name, role }))
        .collect::<Vec<_>>();
    if field_roles.is_empty() {
        return Ok(None);
    }
    Ok(Some(RuntimeStatePlanArtifact {
        contract: actor.name.clone(),
        source: model.types.display_names[&model.types.actor_states[&actor_id]].clone(),
        field_roles,
    }))
}

pub(super) fn type_artifact(ty: &TypeRef) -> TypeArtifact {
    if ty.is_actor_type() {
        TypeArtifact::FixedBytes { len: 32 }
    } else if ty.name == word::COVENANT_ID {
        match ty.array {
            Some(ArrayDim::Fixed(len)) => TypeArtifact::FixedArray { item: Box::new(TypeArtifact::FixedBytes { len: 32 }), len },
            Some(ArrayDim::Dynamic) => TypeArtifact::dynamic_array(TypeArtifact::FixedBytes { len: 32 }),
            None => TypeArtifact::FixedBytes { len: 32 },
        }
    } else {
        match ty.array {
            Some(ArrayDim::Dynamic) if ty.name == "byte" => TypeArtifact::Bytes,
            Some(ArrayDim::Dynamic) => TypeArtifact::dynamic_array(TypeArtifact::from_parts(&ty.name, None)),
            Some(ArrayDim::Fixed(len)) => TypeArtifact::from_parts(&ty.name, Some(len)),
            None => TypeArtifact::from_parts(&ty.name, None),
        }
    }
}

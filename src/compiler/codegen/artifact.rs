//! Projects completed semantics and compiled actor instances to the portable artifact.

use std::collections::{BTreeMap, BTreeSet};

use crate::artifact::*;
use crate::compiler::loader::ResolvedModules;
use crate::compiler::model::{
    ActorTypeSourceWitnessProvider, AppCompilationContext, ClauseActorTypeRef, CovenantGroup, CovenantIdSource, EntryInteraction,
    EntryModel, InteractionLocation, InteractionSource, ObservedActorSide, ObservedActorWitnessSpec, ObservedOutputFieldWitnessSpec,
    ResolvedRoute, ResolvedSuccessor, SpawnActorWitnessSpec, StateExpansionWitnessSpec, StaticActorTarget, TemplateWitnessForm,
    WitnessAbiType, WitnessComponent, WitnessRole, observed_open_state_for_decl, spawn_target_state,
};
use crate::compiler::naming::to_snake;
use crate::compiler::resolve::ResolvedName;
use crate::compiler::syntax::lexer::RESERVED_GENERATED_PREFIX;
use crate::compiler::syntax::node::{DeclId, EntryId, RootSlot};
use crate::compiler::syntax::word;
use crate::compiler::syntax::*;
use crate::error::{ArgentError, Result};
use silverscript_lang::ast::Expr as SilExpr;
use silverscript_lang::compiler::CompiledContract;

use super::abi::{
    extract_sil_template, placeholder_expr_for_type, runtime_state_fields_for_actor, runtime_state_plan_artifact, type_artifact,
};
use super::manifest_path;
use super::sil::{
    clause_actor_type_witness_suffix, compact_expr, hidden_actor_suffix, hidden_route_family_table_name, hidden_template_name,
    observed_actor_side_label, observed_actor_spec_suffix, route_family_suffix_by_id, witness_role_name,
};

impl From<ObservedActorSide> for ObservedActorSideArtifact {
    fn from(side: ObservedActorSide) -> Self {
        match side {
            ObservedActorSide::Input => Self::Input,
            ObservedActorSide::Output => Self::Output,
        }
    }
}

pub(super) fn emit_artifact_compiled(
    program: &ResolvedModules,
    model: &AppCompilationContext<'_>,
    sil_abi: SilAbiArtifact,
    draft: TemplatePlanDraft,
    requests: &BTreeMap<DeclId, ContextRequest>,
    contexts: &BTreeMap<DeclId, CompiledContract<'_>>,
) -> Result<Artifact> {
    let templates = model.app_actors.iter().map(|actor| template_ref_artifact(actor)).collect::<Vec<_>>();
    let state_sources =
        model.states.keys().chain(model.linked_states.keys()).map(|name| model.source_state_id(name)).collect::<Result<Vec<_>>>()?;

    let argent_states = state_sources
        .iter()
        .map(|source| {
            let storage_source = model.storage_source_id(source);
            let storage = model.state_by_source(storage_source)?;
            let storage_id = model.state_decl_id_by_source(storage_source);
            let fields = storage
                .fields
                .iter()
                .map(|field| {
                    let mut ty = field.ty.clone();
                    if let Some(owner) = storage_id {
                        let bindings = model.resolution.bindings(owner);
                        if !ty.is_builtin()
                            && let Some(ResolvedName::Declaration(id)) = bindings.names.get(&ty.name)
                        {
                            ty.name = model.types.display_names[id].clone();
                        }
                        if let Some(state) = &mut ty.actor_state
                            && let Some(ResolvedName::Declaration(id)) = bindings.names.get(state)
                        {
                            *state = model.types.display_names[id].clone();
                        }
                    }
                    ArgentFieldArtifact {
                        name: field.name.clone(),
                        ty: type_artifact(&ty),
                        source_type: source_type_annotation(&ty),
                        virtual_slot: field.virtual_slot,
                    }
                })
                .collect();
            Ok(ArgentStateArtifact { name: source.as_str().to_string(), fields })
        })
        .collect::<Result<Vec<_>>>()?;
    let state_expansions = state_sources
        .iter()
        .map(|source| {
            let state = model.state_by_source(source)?;
            Ok(state.expansion.as_ref().map(|expansion| (source, expansion)))
        })
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .map(|(source, expansion)| {
            let owner = model.state_decl_id_by_source(source);
            let base = model.storage_source_id(source).as_str().to_string();
            let digests = expansion
                .digests
                .iter()
                .enumerate()
                .map(|(index, digest)| {
                    let state = if let Some(owner) = owner {
                        model.types.display_names[&model.bound_state_use(owner, RootSlot::DigestState(index))?].clone()
                    } else {
                        digest.state.clone()
                    };
                    Ok(StateDigestExpansionArtifact { field: digest.field.clone(), state })
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(StateExpansionArtifact { state: source.as_str().to_string(), base, digests })
        })
        .collect::<Result<Vec<_>>>()?;

    let actor_enums = model
        .actor_enums
        .values()
        .map(|actor_enum| ActorEnumArtifact {
            name: actor_enum.name.clone(),
            state: actor_enum.state.clone(),
            variants: actor_enum.variants.clone(),
        })
        .collect::<Vec<_>>();
    let argent_actors = model
        .app_actors
        .iter_with_ids()
        .map(|(id, _)| actor_artifact(id, model.actor_by_decl(id)?, model))
        .collect::<Result<Vec<_>>>()?;
    for (actor_id, _) in model.app_actors.iter_with_ids() {
        let actor = model.actor_by_decl(actor_id)?;
        let compiled = sil_abi
            .contract(&actor.name)
            .ok_or_else(|| ArgentError::new(format!("missing compiled Sil contract for actor `{}`", actor.name)))?;
        let projected = argent_actors
            .iter()
            .find(|candidate| candidate.name == actor.name)
            .ok_or_else(|| ArgentError::new(format!("missing artifact actor `{}`", actor.name)))?;
        for (index, entry) in actor.entries.iter().enumerate() {
            let roles = &model.witness_plan_by_id(EntryId { actor: actor_id, index })?.roles;
            let compiled_entry = compiled
                .entry(&entry.name)
                .ok_or_else(|| ArgentError::new(format!("missing compiled Sil entry `{}::{}`", actor.name, entry.name)))?;
            let projected_entry = projected
                .entries
                .iter()
                .find(|candidate| candidate.name == entry.name)
                .ok_or_else(|| ArgentError::new(format!("missing artifact entry `{}::{}`", actor.name, entry.name)))?;
            if compiled_entry.params.len() != entry.params.len() + roles.len() || projected_entry.hidden_params.len() != roles.len() {
                return Err(ArgentError::new(format!(
                    "entry `{}::{}` compiled ABI does not match its ordered witness roles",
                    actor.name, entry.name
                )));
            }
            for (compiled_param, planned_param) in
                compiled_entry.params[entry.params.len()..].iter().zip(&projected_entry.hidden_params)
            {
                if compiled_param.name != planned_param.name || compiled_param.ty != planned_param.ty {
                    return Err(ArgentError::new(format!(
                        "entry `{}::{}` compiled witness `{}` differs from its planned name or type",
                        actor.name, entry.name, planned_param.name
                    )));
                }
            }
        }
    }
    let template_plan = template_plan_artifact(model, &argent_actors, draft, requests, contexts)?;
    let interfaces = interface_set_artifact(model)?;

    let mut artifact = Artifact {
        schema_version: ARTIFACT_SCHEMA_VERSION,
        id: String::new(),
        generator: GeneratorArtifact { name: "argentc".to_string(), version: env!("CARGO_PKG_VERSION").to_string() },
        app: model.app_name.clone(),
        dependencies: model.app_dependencies.clone(),
        root: manifest_path(program.root_path()),
        modules: program.module_paths().map(manifest_path).collect(),
        argent: ArgentArtifact {
            templates,
            template_plan,
            interfaces,
            states: argent_states,
            state_expansions,
            actor_enums,
            actors: argent_actors,
        },
        sil_abi,
    };
    artifact.id =
        artifact.computed_id_hex().map_err(|err| ArgentError::new(format!("failed to compute generated artifact id: {err}")))?;
    artifact.check_consistency().map_err(|err| ArgentError::new(format!("invalid generated artifact: {err}")))?;
    Ok(artifact)
}

fn source_type_artifact(ty: &TypeRef) -> SourceTypeArtifact {
    SourceTypeArtifact {
        name: ty.name.clone(),
        array: ty.array.map(|array| match array {
            ArrayDim::Dynamic => SourceArrayArtifact::Dynamic,
            ArrayDim::Fixed(len) => SourceArrayArtifact::Fixed(len),
        }),
        actor_state: ty.actor_state.clone(),
    }
}

fn source_type_annotation(ty: &TypeRef) -> Option<SourceTypeArtifact> {
    (ty.name == word::COVENANT_ID || ty.is_actor_type()).then(|| source_type_artifact(ty))
}

fn interface_set_artifact(model: &AppCompilationContext<'_>) -> Result<InterfaceSetArtifact> {
    let exports = model.app_actors.iter_with_ids().map(|(id, _)| actor_interface_artifact(id, model)).collect::<Result<Vec<_>>>()?;

    let mut imports = BTreeMap::new();
    for (actor_id, _) in model.app_actors.iter_with_ids() {
        for id in model.contract_imported_templates(actor_id) {
            let linked = model.linked_actors.get(&id).ok_or_else(|| {
                ArgentError::new(format!("planned imported template `{}::{}` has no linked actor", id.app, id.actor))
            })?;
            imports.entry((linked.app.clone(), linked.actor.clone())).or_insert_with(|| linked.interface.clone());
        }
    }

    Ok(InterfaceSetArtifact { exports, imports: imports.into_values().collect() })
}

fn actor_interface_artifact(actor_id: DeclId, model: &AppCompilationContext<'_>) -> Result<ActorInterfaceArtifact> {
    let actor = model.actor_by_decl(actor_id)?;
    let state = &model.types.display_names[&model.types.actor_states[&actor_id]];
    let runtime_fields = runtime_state_fields_for_actor(actor_id, model)?;
    let fingerprint_hex = actor_interface_fingerprint_hex(&actor.name, state, &runtime_fields)
        .map_err(|err| ArgentError::new(format!("failed to compute actor interface fingerprint for `{}`: {err}", actor.name)))?;
    Ok(ActorInterfaceArtifact {
        id: actor_interface_id(&actor.name),
        app: model.app_name.clone(),
        actor: actor.name.clone(),
        state: state.clone(),
        fingerprint_hex,
    })
}

fn template_ref_artifact(actor: &str) -> TemplateRefArtifact {
    TemplateRefArtifact { id: template_receipt_id(actor), actor: actor.to_string(), symbol: hidden_template_name(actor) }
}

#[derive(Debug)]
struct TemplateReceiptDraft {
    actor_id: DeclId,
    id: String,
    actor: String,
    contract: String,
    symbol: String,
    source_state: String,
    sil_template_hash: [u8; 32],
    compiled_template: ActorTemplateArtifact,
}

#[derive(Debug)]
pub(super) struct TemplatePlanDraft {
    templates: Vec<TemplateReceiptDraft>,
    templates_by_id: BTreeMap<String, usize>,
    templates_by_actor: BTreeMap<String, usize>,
    runtime_states: Vec<RuntimeStatePlanArtifact>,
    route_tables: Vec<RouteTemplateTableArtifact>,
    route_proofs: Vec<RouteTemplateProofArtifact>,
    route_families: Vec<RouteTemplateFamilyArtifact>,
}

pub(super) struct ContextRequest {
    pub(super) args: Vec<SilExpr<'static>>,
    source_state: String,
    context_fields: Vec<String>,
    context_script: Vec<u8>,
    runtime_field_count: usize,
}

impl TemplateHashLookup for TemplatePlanDraft {
    fn template_hash_by_id(&self, id: &str) -> Option<TemplateHashRef<'_>> {
        self.templates_by_id.get(id).map(|index| &self.templates[*index]).map(|template| TemplateHashRef {
            id: &template.id,
            actor: &template.actor,
            hash: &template.sil_template_hash,
        })
    }

    fn template_hash_by_actor(&self, actor: &str) -> Option<TemplateHashRef<'_>> {
        self.templates_by_actor.get(actor).map(|index| &self.templates[*index]).map(|template| TemplateHashRef {
            id: &template.id,
            actor: &template.actor,
            hash: &template.sil_template_hash,
        })
    }
}

impl TemplatePlanLookup for TemplatePlanDraft {
    fn route_tables(&self) -> &[RouteTemplateTableArtifact] {
        &self.route_tables
    }

    fn route_proofs(&self) -> &[RouteTemplateProofArtifact] {
        &self.route_proofs
    }

    fn route_families(&self) -> &[RouteTemplateFamilyArtifact] {
        &self.route_families
    }
}

impl TemplatePlanDraft {
    pub(super) fn new(model: &AppCompilationContext<'_>, sil_contracts: &BTreeMap<String, SilContractArtifact>) -> Result<Self> {
        let templates = model
            .app_actors
            .iter_with_ids()
            .map(|(actor_id, actor_name)| {
                let template = template_ref_artifact(actor_name);
                let contract = sil_contracts
                    .get(&template.actor)
                    .ok_or_else(|| ArgentError::new(format!("missing Sil ABI contract for template actor `{}`", template.actor)))?;
                let state = model.source_state_id_by_decl(model.types.actor_states[&actor_id])?;
                let source_state = model.storage_source_id(&state).as_str();
                Ok(TemplateReceiptDraft {
                    actor_id,
                    id: template.id.clone(),
                    actor: template.actor.clone(),
                    contract: template.actor.clone(),
                    symbol: template.symbol.clone(),
                    source_state: source_state.to_string(),
                    sil_template_hash: contract.compiled.template_hash,
                    compiled_template: extract_sil_template(&contract.compiled)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        let mut runtime_states = Vec::new();
        for (actor_id, _) in model.app_actors.iter_with_ids() {
            if let Some(runtime_state) = runtime_state_plan_artifact(actor_id, model.actor_by_decl(actor_id)?, model)? {
                runtime_states.push(runtime_state);
            }
        }
        let route_tables = route_template_tables_artifact(&runtime_states, sil_contracts)?;
        let route_families = route_template_families_artifact(model);
        let templates_by_id = templates.iter().enumerate().map(|(index, template)| (template.id.clone(), index)).collect();
        let templates_by_actor = templates.iter().enumerate().map(|(index, template)| (template.actor.clone(), index)).collect();
        let mut draft = TemplatePlanDraft {
            templates,
            templates_by_id,
            templates_by_actor,
            runtime_states,
            route_tables,
            route_proofs: Vec::new(),
            route_families,
        };
        draft.route_proofs = route_template_proofs_artifact(&draft.route_tables, &draft)?;

        Ok(draft)
    }
}

fn template_plan_artifact(
    model: &AppCompilationContext<'_>,
    actors: &[ActorArtifact],
    draft: TemplatePlanDraft,
    requests: &BTreeMap<DeclId, ContextRequest>,
    contexts: &BTreeMap<DeclId, CompiledContract<'_>>,
) -> Result<TemplatePlanArtifact> {
    let mut seen = BTreeSet::new();
    let mut witness_recipes = Vec::new();
    for actor in actors {
        for entry in &actor.entries {
            for param in &entry.hidden_params {
                if seen.insert(param.recipe_id.clone()) {
                    witness_recipes.push(WitnessRecipeArtifact {
                        id: param.recipe_id.clone(),
                        template_id: match &param.subject {
                            HiddenParamSubjectArtifact::Actor { actor } if model.app_actors.contains(actor) => {
                                Some(template_receipt_id(actor))
                            }
                            HiddenParamSubjectArtifact::Actor { .. } => None,
                            HiddenParamSubjectArtifact::ObservedActor { .. } => None,
                            HiddenParamSubjectArtifact::SpawnActor { .. } => None,
                            HiddenParamSubjectArtifact::ObservedOutputField { .. } => None,
                            HiddenParamSubjectArtifact::RouteFamily { .. } => None,
                            HiddenParamSubjectArtifact::TemplateSelector { .. } => None,
                            HiddenParamSubjectArtifact::StateExpansion { .. } => None,
                        },
                        subject: param.subject.clone(),
                        param: param.name.clone(),
                        purpose: param.purpose,
                        route_proof_id: param.route_proof_id.clone(),
                    });
                }
            }
        }
    }

    let handles = draft
        .templates
        .iter()
        .map(|template| actor_type_handle_artifact(template, requests.get(&template.actor_id), contexts.get(&template.actor_id)))
        .collect::<Result<Vec<_>>>()?;
    let templates = draft
        .templates
        .into_iter()
        .zip(handles)
        .map(|(template, actor_type_handle)| TemplateReceiptArtifact {
            id: template.id,
            actor: template.actor,
            contract: template.contract,
            symbol: template.symbol,
            sil_template_hash: template.sil_template_hash,
            actor_type_handle,
        })
        .collect();
    Ok(TemplatePlanArtifact {
        templates,
        runtime_states: draft.runtime_states,
        route_tables: draft.route_tables,
        route_proofs: draft.route_proofs,
        route_families: draft.route_families,
        witness_recipes,
    })
}

impl TemplatePlanDraft {
    pub(super) fn context_requests(&self, model: &AppCompilationContext<'_>) -> Result<BTreeMap<DeclId, ContextRequest>> {
        let mut requests = BTreeMap::new();
        for template in &self.templates {
            let actor = model.actor_by_decl(template.actor_id)?;
            let source_state = template.source_state.as_str();
            let runtime_plan = self.runtime_states.iter().find(|runtime_state| runtime_state.contract == actor.name);
            let context_fields = runtime_plan
                .map(|runtime_state| runtime_state.field_roles.iter().map(|field| field.name.clone()).collect::<Vec<_>>())
                .unwrap_or_default();
            if context_fields.is_empty() {
                continue;
            }
            let context_values = runtime_plan
                .map(|runtime_state| {
                    runtime_state
                        .field_roles
                        .iter()
                        .map(|field| {
                            fixed_runtime_context_value(self, runtime_state, field)
                                .map_err(|err| ArgentError::new(format!("cannot derive fixed source-state context: {err}")))
                        })
                        .collect::<Result<Vec<_>>>()
                })
                .transpose()?
                .unwrap_or_default();
            let runtime_fields = runtime_state_fields_for_actor(template.actor_id, model)?;
            let context_state = RuntimeStateArtifact {
                source: template.source_state.clone(),
                fields: runtime_fields.iter().take(context_fields.len()).cloned().collect(),
            };
            let context_state_values = context_fields
                .iter()
                .cloned()
                .zip(context_values.iter().cloned().map(ArtifactValue::Bytes))
                .collect::<BTreeMap<_, _>>();
            // Fixed route context contains only byte leaves, so it needs no named
            // struct definitions from the contract ABI.
            let context_abi = SilAbiArtifact {
                schema_version: SIL_ABI_SCHEMA_VERSION,
                compiler_version: String::new(),
                structs: BTreeMap::new(),
                contracts: BTreeMap::new(),
            };
            let context_script = silverscript_abi::encode_runtime_state_script(&context_abi, &context_state, &context_state_values)
                .map_err(|err| ArgentError::new(format!("cannot encode actor_type<{source_state}> context: {err}")))?;

            let mut args = context_values.into_iter().map(SilExpr::from).collect::<Vec<_>>();
            for field in &model.storage_state_for_actor(template.actor_id)?.fields {
                args.push(placeholder_expr_for_type(&field.ty).map_err(|err| {
                    ArgentError::new(format!(
                        "cannot build actor_type<{}> placeholder for actor `{}` field `{}`: {err}",
                        source_state, actor.name, field.name
                    ))
                })?);
            }
            requests.insert(
                template.actor_id,
                ContextRequest {
                    args,
                    source_state: source_state.to_string(),
                    context_fields,
                    context_script,
                    runtime_field_count: runtime_fields.len(),
                },
            );
        }
        Ok(requests)
    }
}

fn actor_type_handle_artifact(
    template: &TemplateReceiptDraft,
    request: Option<&ContextRequest>,
    compiled: Option<&CompiledContract<'_>>,
) -> Result<ActorTypeHandleArtifact> {
    let Some(request) = request else {
        return Ok(ActorTypeHandleArtifact {
            state: template.source_state.clone(),
            context_fields: Vec::new(),
            template: template.compiled_template.clone(),
        });
    };
    let actor = &template.actor;
    let compiled = compiled.ok_or_else(|| ArgentError::new(format!("missing compiled source-state context for actor `{actor}`")))?;
    if compiled.template_hash() != template.sil_template_hash {
        return Err(ArgentError::new(format!("actor `{actor}` Sil template changed while resolving its capsule context")));
    }
    if compiled.ast.fields.len() != request.runtime_field_count {
        return Err(ArgentError::new(format!("actor `{actor}` compiled state fields do not match its runtime state layout")));
    }

    let state_start = compiled.state_layout.start;
    let context_end = state_start
        .checked_add(request.context_script.len())
        .ok_or_else(|| ArgentError::new(format!("actor `{actor}` capsule context offset overflow")))?;
    let state_end = state_start
        .checked_add(compiled.state_layout.len)
        .ok_or_else(|| ArgentError::new(format!("actor `{actor}` state offset overflow")))?;
    if context_end > state_end || state_end > compiled.bytecode.len() {
        return Err(ArgentError::new(format!("actor `{actor}` compiled capsule context exceeds its state span")));
    }
    if compiled.bytecode.get(state_start..context_end) != Some(request.context_script.as_slice()) {
        return Err(ArgentError::new(format!("actor `{actor}` compiled capsule context does not match its runtime state ABI")));
    }
    let prefix = &compiled.bytecode[..context_end];
    let suffix = &compiled.bytecode[state_end..];
    let hash = silverscript_lang::template::template_hash(prefix, suffix);
    Ok(ActorTypeHandleArtifact {
        state: request.source_state.clone(),
        context_fields: request.context_fields.clone(),
        template: ActorTemplateArtifact { prefix: prefix.to_vec(), suffix: suffix.to_vec(), hash },
    })
}

fn route_template_families_artifact(model: &AppCompilationContext<'_>) -> Vec<RouteTemplateFamilyArtifact> {
    model
        .route_families
        .iter()
        .map(|family| RouteTemplateFamilyArtifact {
            id: family.id.clone(),
            state: family.state.clone(),
            representative_actor: family.rep().to_string(),
            entry_actors: family.entry_actors.clone(),
            table_id: route_template_table_receipt_id(&family.state, &hidden_route_family_table_name(family)),
            actors: family.actors.clone(),
        })
        .collect()
}

fn route_template_tables_artifact(
    runtime_states: &[RuntimeStatePlanArtifact],
    sil_contracts: &BTreeMap<String, SilContractArtifact>,
) -> Result<Vec<RouteTemplateTableArtifact>> {
    let mut tables = BTreeMap::<String, RouteTemplateTableArtifact>::new();
    for runtime_state in runtime_states {
        let contract = sil_contracts
            .get(&runtime_state.contract)
            .ok_or_else(|| ArgentError::new(format!("missing Sil ABI contract for runtime state `{}`", runtime_state.contract)))?;
        for field in &runtime_state.field_roles {
            let sil_field = contract.runtime_state.fields.iter().find(|sil_field| sil_field.name == field.name).ok_or_else(|| {
                ArgentError::new(format!(
                    "runtime role for `{}::{}` points at a missing Sil ABI state field",
                    runtime_state.contract, field.name
                ))
            })?;
            let (leaves, expected_field_ty) = match &field.role {
                RuntimeFieldRoleArtifact::TemplateTable { contracts } => {
                    let leaves = contracts
                        .iter()
                        .map(|actor| RuntimeRouteLeafArtifact::Contract { contract: actor.clone() })
                        .collect::<Vec<_>>();
                    let expected_ty = TypeArtifact::FixedBytes { len: leaves.len() * 32 };
                    (leaves, expected_ty)
                }
                RuntimeFieldRoleArtifact::TemplateDigest { .. } => continue,
                RuntimeFieldRoleArtifact::TemplateRoot { leaves } => (leaves.clone(), TypeArtifact::FixedBytes { len: 32 }),
                RuntimeFieldRoleArtifact::Template { .. } => continue,
            };
            let id = route_template_table_receipt_id(&runtime_state.source, &field.name);
            let byte_len = leaves.len() * 32;
            let entries = leaves
                .iter()
                .enumerate()
                .map(|(index, leaf)| RouteTemplateTableEntryArtifact {
                    index,
                    offset: index * 32,
                    leaf: route_table_leaf_for_runtime_leaf(leaf),
                })
                .collect::<Vec<_>>();
            let table = RouteTemplateTableArtifact {
                id: id.clone(),
                state: runtime_state.source.clone(),
                field: field.name.clone(),
                byte_len,
                entries,
            };
            if sil_field.ty != expected_field_ty {
                return Err(ArgentError::new(format!("runtime route template table `{id}` field type does not match generated role")));
            }
            if let Some(existing) = tables.get(&id) {
                if existing != &table {
                    return Err(ArgentError::new(format!("runtime route template table `{id}` is emitted with conflicting layouts")));
                }
                continue;
            }
            tables.insert(id, table);
        }
    }
    Ok(tables.into_values().collect())
}

fn route_template_proofs_artifact(
    route_tables: &[RouteTemplateTableArtifact],
    templates: &(impl TemplateHashLookup + ?Sized),
) -> Result<Vec<RouteTemplateProofArtifact>> {
    let mut pending = route_tables.iter().collect::<Vec<_>>();
    let mut digest_roots = BTreeMap::<String, String>::new();
    let mut proofs = Vec::new();
    while !pending.is_empty() {
        let before = pending.len();
        let mut next_pending = Vec::new();
        for table in pending {
            let ready = table.entries.iter().all(|entry| match &entry.leaf {
                RouteTemplateLeafArtifact::Template { .. } => true,
                RouteTemplateLeafArtifact::RouteFamily { proof_id, .. } => digest_roots.contains_key(proof_id),
            });
            if !ready {
                next_pending.push(table);
                continue;
            }
            let proof =
                route_template_proof_from_table(table, templates, &digest_roots).map_err(|err| ArgentError::new(err.to_string()))?;
            digest_roots.insert(proof.id.clone(), proof.root_hex.clone());
            proofs.push(proof);
        }
        if next_pending.len() == before {
            return Err(ArgentError::new("route template tables contain an unresolved family digest dependency"));
        }
        pending = next_pending;
    }
    Ok(proofs)
}

fn actor_artifact(actor_id: DeclId, actor: &ActorDecl, model: &AppCompilationContext<'_>) -> Result<ActorArtifact> {
    let entries = actor
        .entries
        .iter()
        .enumerate()
        .map(|(index, entry)| entry_artifact(EntryId { actor: actor_id, index }, actor, entry, model))
        .collect::<Result<Vec<_>>>()?;
    let leader_for = model.leader_for(actor_id).to_vec();

    Ok(ActorArtifact {
        name: actor.name.clone(),
        state: model.types.display_names[&model.types.actor_states[&actor_id]].clone(),
        abi: ActorAbiRefArtifact { contract: actor.name.clone() },
        leader_for,
        entries,
    })
}

fn hidden_params_for_entry(
    entry_id: EntryId,
    actor: &ActorDecl,
    entry: &EntryDecl,
    model: &AppCompilationContext<'_>,
) -> Result<Vec<HiddenParamArtifact>> {
    let plan = model.witness_plan_by_id(entry_id)?;
    let mut hidden_params = Vec::with_capacity(plan.roles.len());
    for &role in &plan.roles {
        let (subject, purpose, recipe_id) = match role {
            WitnessRole::Template { index, component } => {
                let spec = &plan.templates[index];
                let purpose = match (spec.form, component) {
                    (TemplateWitnessForm::Bytes, WitnessComponent::Prefix) => HiddenParamPurposeArtifact::TemplatePrefixBytes,
                    (TemplateWitnessForm::Bytes, WitnessComponent::Suffix) => HiddenParamPurposeArtifact::TemplateSuffixBytes,
                    (TemplateWitnessForm::Len, WitnessComponent::Prefix) => HiddenParamPurposeArtifact::TemplatePrefixLen,
                    (TemplateWitnessForm::Len, WitnessComponent::Suffix) => HiddenParamPurposeArtifact::TemplateSuffixLen,
                    (_, WitnessComponent::TemplateHash) => unreachable!("template roles have no hash component"),
                };
                (
                    HiddenParamSubjectArtifact::Actor { actor: spec.actor.clone() },
                    purpose,
                    template_witness_recipe_id(&spec.actor, purpose),
                )
            }
            WitnessRole::RouteFamily { index } => {
                let spec = &plan.families[index];
                let purpose = HiddenParamPurposeArtifact::RouteFamilyTable;
                (
                    HiddenParamSubjectArtifact::RouteFamily { family_id: spec.family_id.clone() },
                    purpose,
                    route_family_witness_recipe_id(&spec.family_id, purpose),
                )
            }
            WitnessRole::Selector { index, component } => {
                let spec = &plan.selectors[index];
                let purpose = match component {
                    WitnessComponent::Prefix => HiddenParamPurposeArtifact::TemplatePrefixBytes,
                    WitnessComponent::Suffix => HiddenParamPurposeArtifact::TemplateSuffixBytes,
                    WitnessComponent::TemplateHash => unreachable!("selector roles have no hash component"),
                };
                (
                    HiddenParamSubjectArtifact::TemplateSelector { selector: spec.name.clone() },
                    purpose,
                    template_selector_witness_recipe_id(&spec.name, purpose),
                )
            }
            WitnessRole::Observed { index, component } => {
                let spec = &plan.observed_actors[index];
                let purpose = match (spec.side, component) {
                    (ObservedActorSide::Input, WitnessComponent::Prefix) => HiddenParamPurposeArtifact::TemplatePrefixLen,
                    (ObservedActorSide::Input, WitnessComponent::Suffix) => HiddenParamPurposeArtifact::TemplateSuffixLen,
                    (ObservedActorSide::Input, WitnessComponent::TemplateHash) => HiddenParamPurposeArtifact::TemplateHash,
                    (ObservedActorSide::Output, WitnessComponent::Prefix) => HiddenParamPurposeArtifact::TemplatePrefixBytes,
                    (ObservedActorSide::Output, WitnessComponent::Suffix) => HiddenParamPurposeArtifact::TemplateSuffixBytes,
                    (ObservedActorSide::Output, WitnessComponent::TemplateHash) => unreachable!("output roles have no hash component"),
                };
                (
                    HiddenParamSubjectArtifact::ObservedActor {
                        observe: spec.observe.clone(),
                        side: spec.side.into(),
                        handle: spec.handle.clone(),
                        actor: spec.actor.clone(),
                    },
                    purpose,
                    observed_actor_witness_recipe_id(spec, purpose),
                )
            }
            WitnessRole::SpawnIndex { index } => {
                let spec = &plan.spawn_outputs[index];
                let purpose = HiddenParamPurposeArtifact::SpawnOutputIndex;
                (
                    HiddenParamSubjectArtifact::SpawnActor {
                        spawn: spec.spawn.clone(),
                        handle: spec.handle.clone(),
                        actor: spec.actor.clone(),
                    },
                    purpose,
                    spawn_actor_witness_recipe_id(actor, entry, spec, purpose),
                )
            }
            WitnessRole::ActorType { index, component } => {
                let spec = &plan.actor_type_source_templates[index];
                let purpose = match (spec.form, component) {
                    (TemplateWitnessForm::Bytes, WitnessComponent::Prefix) => HiddenParamPurposeArtifact::TemplatePrefixBytes,
                    (TemplateWitnessForm::Bytes, WitnessComponent::Suffix) => HiddenParamPurposeArtifact::TemplateSuffixBytes,
                    (TemplateWitnessForm::Len, WitnessComponent::Prefix) => HiddenParamPurposeArtifact::TemplatePrefixLen,
                    (TemplateWitnessForm::Len, WitnessComponent::Suffix) => HiddenParamPurposeArtifact::TemplateSuffixLen,
                    (_, WitnessComponent::TemplateHash) => unreachable!("actor-type roles have no hash component"),
                };
                (
                    actor_type_source_witness_subject(&spec.provider),
                    purpose,
                    actor_type_source_witness_recipe_id(actor, entry, &spec.source, purpose),
                )
            }
            WitnessRole::StateExpansion { index } => {
                let spec = &plan.state_expansions[index];
                (
                    HiddenParamSubjectArtifact::StateExpansion {
                        state: spec.state.clone(),
                        field: spec.field.clone(),
                        memory_state: spec.memory_state.clone(),
                    },
                    HiddenParamPurposeArtifact::StateExpansionPreimage,
                    state_expansion_witness_recipe_id(spec),
                )
            }
            WitnessRole::ObservedOutputField { index } => {
                let spec = &plan.observed_output_fields[index];
                (
                    HiddenParamSubjectArtifact::ObservedOutputField {
                        observe: spec.observe.clone(),
                        handle: spec.handle.clone(),
                        state: spec.state.clone(),
                        field: spec.field.clone(),
                    },
                    HiddenParamPurposeArtifact::ObservedOutputFieldValue,
                    observed_output_field_witness_recipe_id(spec),
                )
            }
        };
        let ty = match plan.role_type(role) {
            WitnessAbiType::Bytes => TypeArtifact::Bytes,
            WitnessAbiType::Int => TypeArtifact::Int,
            WitnessAbiType::FixedBytes(len) => TypeArtifact::FixedBytes { len },
        };
        hidden_params.push(HiddenParamArtifact {
            recipe_id,
            name: witness_role_name(plan, role),
            ty,
            subject,
            purpose,
            route_proof_id: None,
        });
    }
    Ok(hidden_params)
}

fn entry_artifact(
    entry_id: EntryId,
    actor: &ActorDecl,
    entry: &EntryDecl,
    model: &AppCompilationContext<'_>,
) -> Result<EntryArtifact> {
    let hidden_params = hidden_params_for_entry(entry_id, actor, entry, model)?;
    let entry_model = model.entry_model_by_id(entry_id)?;
    let expanded_routes = model.expanded_routes_by_id(entry_id)?;
    let witnesses = hidden_params
        .iter()
        .map(|param| WitnessArtifact {
            recipe_id: param.recipe_id.clone(),
            param: param.name.clone(),
            subject: param.subject.clone(),
            purpose: param.purpose,
            route_proof_id: param.route_proof_id.clone(),
        })
        .collect::<Vec<_>>();
    Ok(EntryArtifact {
        name: entry.name.clone(),
        kind: match entry.kind {
            EntryKind::Leader => EntryKindArtifact::Leader,
            EntryKind::Delegate => EntryKindArtifact::Delegate,
        },
        abi: EntryAbiRefArtifact { contract: actor.name.clone(), entry: entry.name.clone() },
        route_plan: entry_route_plan_artifact(actor, entry_model, &witnesses, model)?,
        hidden_params,
        template_selectors: entry_model
            .template_selectors()
            .values()
            .map(|selector| TemplateSelectorArtifact {
                name: selector.name.clone(),
                actor_enum: selector.actor_enum.clone(),
                state: selector.state.clone(),
                variants: selector.variants.clone(),
                fixed_actor: selector.fixed_actor.clone(),
            })
            .collect(),
        observes: entry_model
            .existing_groups()
            .map(|group| observe_artifact(entry_id, actor, entry, model, group))
            .collect::<Result<Vec<_>>>()?,
        spawns: entry_model
            .genesis_groups()
            .map(|group| spawn_artifact(entry_id, actor, entry, model, group))
            .collect::<Result<Vec<_>>>()?,
        witnesses,
        consumes: entry_model
            .current()
            .inputs()
            .iter()
            .map(|interaction| {
                let InteractionSource::Consume(consume) = interaction.source() else {
                    unreachable!("current covenant inputs are consumes");
                };
                ConsumeArtifact {
                    name: interaction.handle().to_string(),
                    actor: consume.actor.clone(),
                    cardinality: cardinality_artifact(interaction),
                }
            })
            .collect(),
        emits: emit_spec_artifact(entry_model, model)?,
        routes: expanded_routes.iter().map(|route| route_artifact(route, entry, entry_id, model)).collect::<Result<Vec<_>>>()?,
    })
}

fn spawn_artifact(
    entry_id: EntryId,
    actor: &ActorDecl,
    entry: &EntryDecl,
    model: &AppCompilationContext<'_>,
    group: &CovenantGroup<'_>,
) -> Result<SpawnArtifact> {
    let spawn = group.spawn().expect("spawn artifact is built from a genesis covenant group");
    Ok(SpawnArtifact {
        name: spawn.name.clone(),
        covenant: spawn.covenant.clone(),
        outputs: group
            .outputs()
            .iter()
            .map(|interaction| {
                let InteractionSource::SpawnOutput(output) = interaction.source() else {
                    unreachable!("genesis covenant outputs are spawn outputs");
                };
                let state = spawn_target_state(entry_id, interaction.id(), interaction.target(), &output.actor, actor, entry, model)?
                    .ok_or_else(|| {
                        ArgentError::new(format!(
                            "spawn `{}.{}` target `{}` is not an actor_type value or a selected-app or linked actor",
                            spawn.name, output.name, output.actor
                        ))
                    })?;
                let target = match model.resolve_static_actor_target(interaction.target()) {
                    Some(StaticActorTarget::CrossApp(linked)) => {
                        Some(ActorTargetArtifact::StaticActor { app: linked.app.clone(), actor: linked.actor.clone() })
                    }
                    Some(StaticActorTarget::InApp(_)) | None => None,
                };
                Ok(SpawnOutputArtifact {
                    name: output.name.clone(),
                    actor: interaction
                        .target()
                        .artifact_references(model)?
                        .into_iter()
                        .next()
                        .ok_or_else(|| ArgentError::new("spawn output has no actor reference"))?,
                    state: state.as_str().to_string(),
                    group_index: output.group_index,
                    cardinality: cardinality_artifact(interaction),
                    target,
                })
            })
            .collect::<Result<Vec<_>>>()?,
    })
}

fn observe_artifact(
    entry_id: EntryId,
    actor: &ActorDecl,
    entry: &EntryDecl,
    model: &AppCompilationContext<'_>,
    group: &CovenantGroup<'_>,
) -> Result<ObserveArtifact> {
    let observe = group.observe().expect("observe artifact is built from an existing covenant group");
    let covenant_id_source = match model.input_plan_by_id(entry_id)?.observed_covenant_source(group.id())? {
        CovenantIdSource::StateField { field } => CovenantIdSourceArtifact::StateField { field: field.field().to_string() },
        CovenantIdSource::EntryArgument { index } => CovenantIdSourceArtifact::EntryArgument { index: *index },
    };
    Ok(ObserveArtifact {
        name: observe.name.clone(),
        covenant_expr: compact_expr(&observe.covenant_expr),
        covenant_id_source,
        inputs: group
            .inputs()
            .iter()
            .map(|interaction| {
                let InteractionSource::ObserveInput(observed) = interaction.source() else {
                    unreachable!("existing covenant inputs are observed inputs");
                };
                observed_actor_artifact(entry_id, actor, entry, model, observe, observed, interaction)
            })
            .collect::<Result<Vec<_>>>()?,
        outputs: group
            .outputs()
            .iter()
            .map(|interaction| {
                let InteractionSource::ObserveOutput(observed) = interaction.source() else {
                    unreachable!("existing covenant outputs are observed outputs");
                };
                observed_actor_artifact(entry_id, actor, entry, model, observe, observed, interaction)
            })
            .collect::<Result<Vec<_>>>()?,
    })
}

fn observed_actor_artifact(
    entry_id: EntryId,
    actor: &ActorDecl,
    entry: &EntryDecl,
    model: &AppCompilationContext<'_>,
    observe: &ObserveDecl,
    observed: &ObservedActorDecl,
    interaction: &EntryInteraction<'_>,
) -> Result<ObservedActorArtifact> {
    let target = if let Some(state) = observed_open_state_for_decl(entry_id, actor, entry, observe, observed, model)? {
        ObservedTargetArtifact::DynamicActor { state: state.as_str().to_string() }
    } else {
        match model.resolve_static_actor_target(interaction.target()) {
            Some(StaticActorTarget::InApp(id)) => {
                ObservedTargetArtifact::StaticActor { app: model.app_name.clone(), actor: model.actor_by_decl(id)?.name.clone() }
            }
            Some(StaticActorTarget::CrossApp(linked)) => {
                ObservedTargetArtifact::StaticActor { app: linked.app.clone(), actor: linked.actor.clone() }
            }
            None => return Err(ArgentError::new("observed actor has no bound artifact target")),
        }
    };
    Ok(ObservedActorArtifact { name: observed.name.clone(), target, cardinality: cardinality_artifact(interaction) })
}

fn entry_route_plan_artifact(
    actor: &ActorDecl,
    entry_model: &EntryModel<'_>,
    witnesses: &[WitnessArtifact],
    model: &AppCompilationContext<'_>,
) -> Result<EntryRoutePlanArtifact> {
    let entry = entry_model.source();
    let active_input = RouteInputArtifact {
        name: "self".to_string(),
        actor: actor.name.clone(),
        cov_index: matches!(entry.kind, EntryKind::Leader).then_some(0),
    };
    let consumes = entry_model
        .current()
        .inputs()
        .iter()
        .map(|interaction| {
            let InteractionSource::Consume(consume) = interaction.source() else {
                unreachable!("current entry inputs are consumes");
            };
            RouteInputArtifact {
                name: consume.name.clone(),
                actor: consume.actor.clone(),
                cov_index: fixed_interaction_index(interaction.location(), usize::from(entry.kind == EntryKind::Leader)),
            }
        })
        .collect::<Vec<_>>();
    let leader_input = match entry.kind {
        EntryKind::Leader => Some(active_input.clone()),
        EntryKind::Delegate => consumes.first().cloned(),
    };
    let outputs = route_output_handles(entry_model, model)?;
    Ok(EntryRoutePlanArtifact {
        active_input: Some(active_input),
        leader_input,
        consumes,
        outputs,
        witness_recipe_ids: witnesses.iter().map(|witness| witness.recipe_id.clone()).collect(),
    })
}

pub(super) fn fixed_interaction_index(location: InteractionLocation, section_offset: usize) -> Option<usize> {
    match location {
        InteractionLocation::FromStart(index) => Some(section_offset + index),
        InteractionLocation::Range { .. } | InteractionLocation::FromEnd(_) => None,
    }
}

fn route_output_handles(entry: &EntryModel<'_>, model: &AppCompilationContext<'_>) -> Result<Vec<RouteOutputHandleArtifact>> {
    entry
        .current()
        .outputs()
        .iter()
        .map(|output| {
            Ok(RouteOutputHandleArtifact {
                name: output.handle().to_string(),
                auth_index: fixed_interaction_index(output.location(), 0),
                actors: output.target().artifact_references(model)?,
            })
        })
        .collect()
}

fn emit_spec_artifact(entry: &EntryModel<'_>, model: &AppCompilationContext<'_>) -> Result<EmitArtifact> {
    match &entry.source().emits {
        EmitSpec::None => Ok(EmitArtifact::None),
        EmitSpec::Outputs(_) => Ok(EmitArtifact::Outputs {
            outputs: entry
                .current()
                .outputs()
                .iter()
                .map(|interaction| {
                    let InteractionSource::CurrentOutput(output) = interaction.source() else {
                        unreachable!("named emits output retains its source");
                    };
                    debug_assert_eq!(interaction.handle(), output.name);
                    Ok(EmitOutputArtifact {
                        name: interaction.handle().to_string(),
                        auth_index: fixed_interaction_index(interaction.location(), 0),
                        actors: interaction.target().artifact_references(model)?,
                        cardinality: cardinality_artifact(interaction),
                    })
                })
                .collect::<Result<Vec<_>>>()?,
        }),
    }
}

fn cardinality_artifact(interaction: &EntryInteraction<'_>) -> CardinalityArtifact {
    interaction
        .cardinality()
        .range_bounds()
        .map_or(CardinalityArtifact::One, |(minimum, maximum)| CardinalityArtifact::Range { minimum, maximum })
}

fn route_artifact(
    route: &ResolvedRoute,
    entry: &EntryDecl,
    entry_id: EntryId,
    model: &AppCompilationContext<'_>,
) -> Result<RouteArtifact> {
    let successor = match &route.successor {
        ResolvedSuccessor::ExactSelf => RouteSuccessorArtifact::ExactSelf,
        ResolvedSuccessor::Constructed { actor, .. } => {
            let (_, state) = model.resolution.route_texts(entry_id, route.id)?;
            let name = actor.display(entry, entry_id, route.id, model)?;
            RouteSuccessorArtifact::Constructed {
                actor: name.clone(),
                template_id: template_receipt_id(&name),
                state_expr: compact_expr(state),
            }
        }
    };
    Ok(RouteArtifact { output: route.output.clone(), successor })
}

fn actor_type_source_witness_subject(provider: &ActorTypeSourceWitnessProvider) -> HiddenParamSubjectArtifact {
    match provider {
        ActorTypeSourceWitnessProvider::Observed(spec) => HiddenParamSubjectArtifact::ObservedActor {
            observe: spec.observe.clone(),
            side: spec.side.into(),
            handle: spec.handle.clone(),
            actor: spec.actor.clone(),
        },
        ActorTypeSourceWitnessProvider::Spawn(spec) => HiddenParamSubjectArtifact::SpawnActor {
            spawn: spec.spawn.clone(),
            handle: spec.handle.clone(),
            actor: spec.actor.clone(),
        },
    }
}

fn hidden_template_root_name() -> String {
    format!("{RESERVED_GENERATED_PREFIX}template_root")
}

fn route_table_leaf_for_runtime_leaf(leaf: &RuntimeRouteLeafArtifact) -> RouteTemplateLeafArtifact {
    match leaf {
        RuntimeRouteLeafArtifact::Contract { contract } => {
            RouteTemplateLeafArtifact::Template { actor: contract.clone(), template_id: template_receipt_id(contract) }
        }
        RuntimeRouteLeafArtifact::Digest { id } => {
            RouteTemplateLeafArtifact::RouteFamily { family_id: id.clone(), proof_id: route_family_proof_id_from_id(id) }
        }
    }
}

fn route_family_proof_id_from_id(family_id: &str) -> String {
    let state = family_id.strip_prefix("route_family/").and_then(|rest| rest.split('/').next()).unwrap_or("");
    route_template_proof_receipt_id(state, &hidden_template_root_name())
}

fn template_receipt_id(actor: &str) -> String {
    format!("template/{}", hidden_actor_suffix(actor))
}

fn template_witness_recipe_id(actor: &str, purpose: HiddenParamPurposeArtifact) -> String {
    format!("witness/{}/{}", hidden_actor_suffix(actor), hidden_param_purpose_id(purpose))
}

fn route_family_witness_recipe_id(family_id: &str, purpose: HiddenParamPurposeArtifact) -> String {
    format!("witness/{}/{}", route_family_suffix_by_id(family_id), hidden_param_purpose_id(purpose))
}

fn template_selector_witness_recipe_id(selector: &str, purpose: HiddenParamPurposeArtifact) -> String {
    format!("witness/template_selector/{selector}/{}", hidden_param_purpose_id(purpose))
}

fn observed_actor_witness_recipe_id(spec: &ObservedActorWitnessSpec, purpose: HiddenParamPurposeArtifact) -> String {
    format!(
        "witness/observed/{}/{}/{}/{}",
        spec.observe,
        observed_actor_side_label(spec.side),
        observed_actor_spec_suffix(spec),
        hidden_param_purpose_id(purpose)
    )
}

fn spawn_actor_witness_recipe_id(
    actor: &ActorDecl,
    entry: &EntryDecl,
    spec: &SpawnActorWitnessSpec,
    purpose: HiddenParamPurposeArtifact,
) -> String {
    format!("witness/{}/{}/spawn/{}/{}/{}", actor.name, entry.name, spec.spawn, spec.handle, hidden_param_purpose_id(purpose))
}

fn actor_type_source_witness_recipe_id(
    actor: &ActorDecl,
    entry: &EntryDecl,
    source: &ClauseActorTypeRef,
    purpose: HiddenParamPurposeArtifact,
) -> String {
    format!(
        "witness/{}/{}/actor_type/{}/{}",
        actor.name,
        entry.name,
        clause_actor_type_witness_suffix(source),
        hidden_param_purpose_id(purpose)
    )
}

fn state_expansion_witness_recipe_id(spec: &StateExpansionWitnessSpec) -> String {
    format!(
        "witness/state_expansion/{}/{}/{}/{}",
        spec.state,
        spec.field,
        to_snake(&spec.memory_state),
        hidden_param_purpose_id(HiddenParamPurposeArtifact::StateExpansionPreimage)
    )
}

fn observed_output_field_witness_recipe_id(spec: &ObservedOutputFieldWitnessSpec) -> String {
    format!(
        "witness/observed/{}/output/{}/{}/{}/{}",
        spec.observe,
        spec.handle,
        to_snake(&spec.state),
        spec.field,
        hidden_param_purpose_id(HiddenParamPurposeArtifact::ObservedOutputFieldValue)
    )
}

fn hidden_param_purpose_id(purpose: HiddenParamPurposeArtifact) -> &'static str {
    match purpose {
        HiddenParamPurposeArtifact::SpawnOutputIndex => "spawn_output_index",
        HiddenParamPurposeArtifact::TemplatePrefixBytes => "template_prefix_bytes",
        HiddenParamPurposeArtifact::TemplateSuffixBytes => "template_suffix_bytes",
        HiddenParamPurposeArtifact::TemplatePrefixLen => "template_prefix_len",
        HiddenParamPurposeArtifact::TemplateSuffixLen => "template_suffix_len",
        HiddenParamPurposeArtifact::TemplateHash => "template_hash",
        HiddenParamPurposeArtifact::RouteTemplateLeaf => "route_template_leaf",
        HiddenParamPurposeArtifact::RouteTemplateProof => "route_template_proof",
        HiddenParamPurposeArtifact::RouteFamilyTable => "route_family_table",
        HiddenParamPurposeArtifact::RouteFamilyProof => "route_family_proof",
        HiddenParamPurposeArtifact::StateExpansionPreimage => "state_expansion_preimage",
        HiddenParamPurposeArtifact::ObservedOutputFieldValue => "observed_output_field_value",
    }
}

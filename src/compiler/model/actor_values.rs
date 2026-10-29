//! Statically linked actor handles used as authored expression values.

use std::collections::{BTreeMap, BTreeSet};

use crate::compiler::resolve::{Binding, ResolvedName};
use crate::compiler::syntax::node::{DeclId, NodeId, ReferenceRole, SymbolKind};
use crate::error::{ArgentError, Result};

use super::AppCompilationContext;
use super::link::LinkedActorId;

/// Expression identities and the imported constants needed by each emitted contract.
#[derive(Debug, Default)]
pub(crate) struct LinkedActorValuePlan {
    pub(crate) expressions: BTreeMap<NodeId, LinkedActorId>,
    imports_by_actor: BTreeMap<DeclId, BTreeSet<LinkedActorId>>,
}

impl LinkedActorValuePlan {
    pub(super) fn new(model: &AppCompilationContext<'_>) -> Result<Self> {
        let shared = model.consts.iter().map(|(id, _)| *id).chain(model.functions.iter().map(|(id, _)| *id));
        let mut uses_by_owner = BTreeMap::<DeclId, BTreeSet<LinkedActorId>>::new();
        let mut plan = Self::default();
        for owner in shared.clone().chain(model.app_actors.iter_with_ids().map(|(id, _)| id)) {
            for (site, binding) in &model.resolution.bindings(owner).sites {
                let Binding::Source(ResolvedName::AppMember(member)) = binding else { continue };
                // Clause targets and enum selectors have their own semantics. Only
                // an actor in an ordinary value position denotes a linked handle.
                if model.resolution.nodes().node(*site).reference_role != Some(ReferenceRole::Value)
                    || member.actor.kind() != SymbolKind::Actor
                {
                    continue;
                }
                let linked = LinkedActorId::from_member(*member, model.resolution);
                if !model.linked_actors.contains_key(&linked) {
                    return Err(ArgentError::new(format!("actor value `{}::{}` has no linked template", linked.app, linked.actor)));
                }
                uses_by_owner.entry(owner).or_default().insert(linked.clone());
                plan.expressions.insert(*site, linked);
            }
        }
        let shared_imports =
            shared.flat_map(|owner| uses_by_owner.get(&owner).into_iter().flatten().cloned()).collect::<BTreeSet<_>>();
        for (actor, _) in model.app_actors.iter_with_ids() {
            // Global helpers and constants are emitted into every actor contract;
            // actor helpers and entry bodies are emitted only into their owner.
            let mut imports = shared_imports.clone();
            imports.extend(uses_by_owner.remove(&actor).into_iter().flatten());
            plan.imports_by_actor.insert(actor, imports);
        }
        Ok(plan)
    }

    pub(super) fn imports(&self, actor: DeclId) -> impl Iterator<Item = &LinkedActorId> {
        self.imports_by_actor.get(&actor).into_iter().flatten()
    }
}

impl AppCompilationContext<'_> {
    /// Imported handles used by clauses or emitted expressions, without transaction witnesses.
    pub(crate) fn contract_imported_templates(&self, actor: DeclId) -> BTreeSet<LinkedActorId> {
        self.linked_actor_values
            .imports(actor)
            .cloned()
            .chain(
                self.witness_plans
                    .iter()
                    .filter(|(entry, _)| entry.actor == actor)
                    .flat_map(|(_, plan)| plan.imported_templates.iter().cloned()),
            )
            .collect()
    }
}

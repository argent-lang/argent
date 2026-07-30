//! Source-backed actor and entry lookup.

use std::collections::BTreeMap;

use crate::ast::ActorDecl;
use crate::error::{ArgentError, Result};

use super::{ActorEnumInfo, EntryModel};

/// An actor's entry models, indexed by name and iterated in source order.
#[derive(Debug)]
pub(crate) struct ActorModel<'a> {
    source: &'a ActorDecl,
    entries: BTreeMap<&'a str, EntryModel<'a>>,
}

impl<'a> ActorModel<'a> {
    /// Build the entry models for one source actor.
    pub(crate) fn build(source: &'a ActorDecl, actor_enums: &BTreeMap<String, ActorEnumInfo>) -> Result<Self> {
        let mut entries_by_name = BTreeMap::new();
        for entry in &source.entries {
            let model = EntryModel::build(source, entry, actor_enums)?;
            if entries_by_name.insert(entry.name.as_str(), model).is_some() {
                let name = &entry.name;
                return Err(ArgentError::new(format!("actor `{}` declares entry `{name}` more than once", source.name)));
            }
        }
        Ok(Self { source, entries: entries_by_name })
    }

    /// Return the source actor declaration.
    pub(crate) fn source(&self) -> &'a ActorDecl {
        self.source
    }

    /// Iterate entry models in source declaration order.
    pub(crate) fn entries(&self) -> impl Iterator<Item = &EntryModel<'a>> {
        self.source.entries.iter().map(|entry| self.entries.get(entry.name.as_str()).expect("source entry has an entry model"))
    }

    /// Look up an entry model by source name.
    pub(crate) fn entry(&self, name: &str) -> Option<&EntryModel<'a>> {
        self.entries.get(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{EmitSpec, EntryBody, EntryDecl, EntryKind};

    fn entry(name: &str) -> EntryDecl {
        EntryDecl {
            kind: EntryKind::Leader,
            name: name.to_string(),
            params: Vec::new(),
            consumes: Vec::new(),
            observes: Vec::new(),
            spawns: Vec::new(),
            emits: EmitSpec::None,
            body: EntryBody::default(),
            routes: Vec::new(),
            terminal_route_sets: Vec::new(),
        }
    }

    #[test]
    fn indexes_entries_without_changing_source_order() {
        let actor = ActorDecl { name: "Worker".to_string(), state: "WorkerState".to_string(), entries: vec![entry("z"), entry("a")] };
        let model = ActorModel::build(&actor, &BTreeMap::new()).expect("actor model");

        assert_eq!(model.entries().map(|entry| entry.source().name.as_str()).collect::<Vec<_>>(), ["z", "a"]);
        assert_eq!(model.entry("a").expect("indexed entry").source().name, "a");
    }

    #[test]
    fn rejects_duplicate_entry_names() {
        let actor =
            ActorDecl { name: "Worker".to_string(), state: "WorkerState".to_string(), entries: vec![entry("step"), entry("step")] };

        let err = ActorModel::build(&actor, &BTreeMap::new()).expect_err("duplicate entries must be rejected");

        assert_eq!(err.message, "actor `Worker` declares entry `step` more than once");
    }
}

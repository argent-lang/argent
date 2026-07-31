//! Source-backed actor and entry lookup.

use std::collections::BTreeMap;

use crate::compiler::syntax::ActorDecl;
use crate::error::{ArgentError, Result};

use super::{ActorEnumInfo, EntryModel};

#[cfg(test)]
mod tests;

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

//! Source-backed actor and entry lookup.

use std::collections::BTreeMap;

use crate::compiler::syntax::{ActorDecl, FunctionDecl};
use crate::error::{ArgentError, Result};

use super::{ActorEnumInfo, ConstResolver, EntryModel};

#[cfg(test)]
mod tests;

/// An actor's functions and entry models, indexed by name and iterated in source order.
#[derive(Debug)]
pub(crate) struct ActorModel<'a> {
    source: &'a ActorDecl,
    functions: BTreeMap<&'a str, &'a FunctionDecl>,
    entries: BTreeMap<&'a str, EntryModel<'a>>,
}

impl<'a> ActorModel<'a> {
    /// Build the function index and entry models for one source actor.
    pub(crate) fn build(
        source: &'a ActorDecl,
        actor_enums: &BTreeMap<String, ActorEnumInfo>,
        const_resolver: &ConstResolver<'_>,
    ) -> Result<Self> {
        let mut functions_by_name = BTreeMap::new();
        for function in &source.functions {
            if functions_by_name.insert(function.name.as_str(), function).is_some() {
                let name = &function.name;
                return Err(ArgentError::new(format!("actor `{}` declares function `{name}` more than once", source.name)));
            }
        }

        let mut entries_by_name = BTreeMap::new();
        for entry in &source.entries {
            if functions_by_name.contains_key(entry.name.as_str()) {
                return Err(ArgentError::new(format!(
                    "actor `{}` declares both a function and an entry named `{}`",
                    source.name, entry.name
                )));
            }
            let model = EntryModel::build(source, entry, actor_enums, const_resolver)?;
            if entries_by_name.insert(entry.name.as_str(), model).is_some() {
                let name = &entry.name;
                return Err(ArgentError::new(format!("actor `{}` declares entry `{name}` more than once", source.name)));
            }
        }
        Ok(Self { source, functions: functions_by_name, entries: entries_by_name })
    }

    /// Return the source actor declaration.
    pub(crate) fn source(&self) -> &'a ActorDecl {
        self.source
    }

    /// Iterate actor functions in source declaration order.
    pub(crate) fn functions(&self) -> impl Iterator<Item = &'a FunctionDecl> {
        self.source
            .functions
            .iter()
            .map(|function| *self.functions.get(function.name.as_str()).expect("source function has an indexed function"))
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

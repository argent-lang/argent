//! Source-backed compiler models shared by validation, planning, and code generation.

use std::collections::BTreeMap;

use crate::artifact::{AppDependencyArtifact, EntryRefArtifact};
use crate::ast::{ActorDecl, ConstDecl, FunctionDecl, StateDecl};
use crate::link::LinkedActor;

mod actor;
mod entry;

pub(crate) use actor::ActorModel;
pub(crate) use entry::{
    CovenantGroup, EntryModel, InteractionSource, TemplateSelector, actor_enum_variant_const_expr, parse_actor_enum_selector,
    parse_actor_enum_variant,
};

/// The selected application's compiler-wide source and routing model.
#[derive(Debug)]
pub(crate) struct Model<'a> {
    pub(crate) app_name: String,
    /// Direct artifacts used to link the selected app.
    pub(crate) app_dependencies: Vec<AppDependencyArtifact>,
    pub(crate) app_actors: Vec<String>,
    pub(crate) route_families: Vec<RouteFamily>,
    pub(crate) consts: Vec<&'a ConstDecl>,
    pub(crate) functions: Vec<&'a FunctionDecl>,
    pub(crate) states: BTreeMap<String, &'a StateDecl>,
    pub(crate) linked_states: BTreeMap<String, StateDecl>,
    pub(crate) actors_by_name: BTreeMap<String, &'a ActorDecl>,
    pub(crate) linked_actor_decls: BTreeMap<String, ActorDecl>,
    pub(crate) linked_actors: BTreeMap<String, LinkedActor>,
    pub(crate) actor_enums: BTreeMap<String, ActorEnumInfo>,
    pub(crate) actors: Vec<&'a ActorDecl>,
    pub(crate) actor_models: BTreeMap<&'a str, ActorModel<'a>>,
    /// Delegate entries that establish each actor as a leader actor.
    pub(crate) leader_for: BTreeMap<String, Vec<EntryRefArtifact>>,
    pub(crate) route_leaves_by_actor: BTreeMap<String, Vec<RouteRootLeaf>>,
    pub(crate) route_transitions: BTreeMap<(String, String), CompilerRouteTransition>,
    pub(crate) state_route_leaves: BTreeMap<String, Vec<RouteRootLeaf>>,
}

/// An actor enum resolved to one state domain and its ordered variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActorEnumInfo {
    pub(crate) name: String,
    pub(crate) state: String,
    pub(crate) variants: Vec<String>,
}

/// A state-local actor family represented by one ordered route table.
///
/// Entry actors remain direct while table actors are committed in table order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RouteFamily {
    pub(crate) id: String,
    pub(crate) state: String,
    pub(crate) rep: String,
    pub(crate) actors: Vec<String>,
    pub(crate) entry_actors: Vec<String>,
    pub(crate) table_actors: Vec<String>,
}

impl RouteFamily {
    /// Return the actor representing this family.
    pub(crate) fn rep(&self) -> &str {
        &self.rep
    }

    /// Return family actors whose templates remain direct.
    pub(crate) fn direct_template_actors(&self) -> &[String] {
        &self.entry_actors
    }

    /// Return family actors committed in the route table.
    pub(crate) fn table_actors(&self) -> &[String] {
        &self.table_actors
    }

    /// Return the serialized byte length of the route table.
    pub(crate) fn table_byte_len(&self) -> usize {
        self.table_actors().len() * 32
    }
}

/// One selected root in a state-carried route commitment.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum RouteRootLeaf {
    Actor(String),
    Family(String),
}

/// Operations that transform one actor's route cut into another's.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CompilerRouteTransition {
    pub(crate) families_to_open: Vec<String>,
    pub(crate) families_to_pack: Vec<String>,
}

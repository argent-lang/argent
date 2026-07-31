use super::*;
use crate::compiler::syntax::{EmitSpec, EntryBody, EntryDecl, EntryKind};

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

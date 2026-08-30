use super::*;
use crate::compiler::syntax::{EmitSpec, EntryBody, EntryDecl, EntryKind, FunctionDecl, TypeRef};

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

fn function(name: &str) -> FunctionDecl {
    FunctionDecl { name: name.to_string(), params: Vec::new(), return_ty: Some(TypeRef::new("int")), body: "return 0;".to_string() }
}

#[test]
fn indexes_entries_without_changing_source_order() {
    let actor = ActorDecl {
        name: "Worker".to_string(),
        state: "WorkerState".to_string(),
        functions: Vec::new(),
        entries: vec![entry("z"), entry("a")],
    };
    let model = ActorModel::build(&actor, &BTreeMap::new(), &ConstResolver::new(&[])).expect("actor model");

    assert_eq!(model.entries().map(|entry| entry.source().name.as_str()).collect::<Vec<_>>(), ["z", "a"]);
    assert_eq!(model.entry("a").expect("indexed entry").source().name, "a");
}

#[test]
fn rejects_duplicate_entry_names() {
    let actor = ActorDecl {
        name: "Worker".to_string(),
        state: "WorkerState".to_string(),
        functions: Vec::new(),
        entries: vec![entry("step"), entry("step")],
    };

    let err = ActorModel::build(&actor, &BTreeMap::new(), &ConstResolver::new(&[])).expect_err("duplicate entries must be rejected");

    assert_eq!(err.message, "actor `Worker` declares entry `step` more than once");
}

#[test]
fn indexes_functions_without_changing_source_order() {
    let actor = ActorDecl {
        name: "Worker".to_string(),
        state: "WorkerState".to_string(),
        functions: vec![function("z"), function("a")],
        entries: Vec::new(),
    };
    let model = ActorModel::build(&actor, &BTreeMap::new(), &ConstResolver::new(&[])).expect("actor model");

    assert_eq!(model.functions().map(|function| function.name.as_str()).collect::<Vec<_>>(), ["z", "a"]);
}

#[test]
fn rejects_duplicate_function_names() {
    let actor = ActorDecl {
        name: "Worker".to_string(),
        state: "WorkerState".to_string(),
        functions: vec![function("step"), function("step")],
        entries: Vec::new(),
    };

    let err = ActorModel::build(&actor, &BTreeMap::new(), &ConstResolver::new(&[])).expect_err("duplicate functions must be rejected");

    assert_eq!(err.message, "actor `Worker` declares function `step` more than once");
}

#[test]
fn rejects_function_and_entry_with_the_same_name() {
    let actor = ActorDecl {
        name: "Worker".to_string(),
        state: "WorkerState".to_string(),
        functions: vec![function("step")],
        entries: vec![entry("step")],
    };

    let err =
        ActorModel::build(&actor, &BTreeMap::new(), &ConstResolver::new(&[])).expect_err("function and entry names must not collide");

    assert_eq!(err.message, "actor `Worker` declares both a function and an entry named `step`");
}

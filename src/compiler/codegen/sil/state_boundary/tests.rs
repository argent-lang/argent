use std::path::PathBuf;

use crate::compiler::model::Model;
use crate::compiler::syntax::parser::parse_module;
use crate::compiler::syntax::{ActorDecl, EntryDecl, Program};

use super::*;

fn program(source: &str) -> Program {
    let path = PathBuf::from("state-boundary-test.ag");
    let module = parse_module(path.clone(), source.to_string()).expect("test source parses");
    Program { root: path, modules: vec![module] }
}

fn actor_entry<'a>(model: &'a Model<'a>, actor: &str, entry: &str) -> (&'a ActorDecl, &'a EntryDecl) {
    let actor = model.actor(actor).expect("actor exists");
    let entry = actor.entries.iter().find(|candidate| candidate.name == entry).expect("entry exists");
    (actor, entry)
}

#[test]
fn active_input_state_remains_physical_and_uses_the_covenant_domain_proof() {
    let program = program(include_str!("../../../../../tests/fixtures/emit/single_actor_self_consume/app.ag"));
    let model = Model::from_program(&program).expect("self-consume fixture plans");
    let (actor, entry) = actor_entry(&model, "Counter", "merge");
    let plan = plan_entry_input_states(actor, entry, &model).expect("input states plan");
    let input = plan.consumed("other").expect("self input exists");

    assert!(input.uses_covenant_domain_proof());
    assert_eq!(input.physical_type(), "State");
    assert!(matches!(input.access(), SourceStateAccess::Projected(_)));
    assert_eq!(
        input.access().require_authored_value(8).expect("identity projection materializes").into_sil(),
        "CounterState {\n            // :: user declared fields\n            count: other.count,\n        }"
    );
}

#[test]
fn named_identity_input_is_already_an_authored_source_value() {
    let program = program(include_str!("../../../../../tests/fixtures/emit/input_template_route_reuse/app.ag"));
    let model = Model::from_program(&program).expect("peer input fixture plans");
    let (actor, entry) = actor_entry(&model, "Controller", "step");
    let plan = plan_entry_input_states(actor, entry, &model).expect("input states plan");
    let input = plan.consumed("peer").expect("peer input exists");

    assert!(!input.uses_covenant_domain_proof());
    assert_eq!(input.physical_type(), "PeerState");
    assert!(matches!(input.access(), SourceStateAccess::Authored { .. }));
    let authored = input.access().require_authored_value(8).expect("named input is authored");
    assert_eq!(authored.source().as_str(), "PeerState");
    assert_eq!(authored.into_sil(), "peer");
}

#[test]
fn augmented_input_projects_only_user_fields_from_its_actor_keyed_type() {
    let program = program(
        r#"
            state BoxState { int units; }

            actor Left owns BoxState {
                entry shift() consumes { peer: Right, } emits { left: Left, peer: Right, } {
                    BoxState next_left = { units: units - 1, };
                    BoxState next_peer = { units: peer.units + 1, };
                    unrestricted(left.value);
                    unrestricted(peer.value);
                    become { left <- Left(next_left), peer <- Right(next_peer), };
                }
            }

            actor Right owns BoxState {
                delegate accept() consumes { leader: Left, } {}
            }

            app Test { actor Left; actor Right; }
        "#,
    );
    let model = Model::from_program(&program).expect("paired actors plan");
    let (actor, entry) = actor_entry(&model, "Left", "shift");
    let plan = plan_entry_input_states(actor, entry, &model).expect("input states plan");
    let input = plan.consumed("peer").expect("peer input exists");
    let authored = input.access().require_authored_value(8).expect("identity user fields materialize").into_sil();

    assert_eq!(input.physical_type(), "Gen__RightState");
    assert!(matches!(input.access(), SourceStateAccess::Projected(_)));
    assert!(authored.contains("units: peer.units"), "{authored}");
    assert!(!authored.contains("gen__"), "generated route fields must not enter authored state: {authored}");
}

#[test]
fn expanded_input_requires_a_validated_preimage_for_authored_access() {
    let program = program(
        r#"
            state Capsule { int nonce; virtual detail; }
            state Details { int count; }
            state Expanded expands Capsule { detail: Details; }

            actor Vault owns Expanded {
                entry hold() emits none { require(nonce >= 0); }
            }

            state ReaderState { int nonce; }
            actor Reader owns ReaderState {
                entry inspect() consumes { vault: Vault, } emits next: Reader {
                    require(vault.nonce >= 0);
                    unrestricted(next.value);
                    become next <- Reader(self.state);
                }
            }

            app Test { actor Vault; actor Reader; }
        "#,
    );
    let model = Model::from_program(&program).expect("expanded input plans");
    let (actor, entry) = actor_entry(&model, "Reader", "inspect");
    let plan = plan_entry_input_states(actor, entry, &model).expect("input states plan");
    let input = plan.consumed("vault").expect("vault input exists");

    assert_eq!(input.physical_type(), "Gen__PhysicalExpanded");
    plan.reject_unavailable_field_refs("vault.nonce >= 0").expect("ordinary field projects");
    let projection_err = plan.reject_unavailable_field_refs("vault.detail.count >= 0").expect_err("digest field does not project");
    assert!(projection_err.to_string().contains("validated preimage"), "unexpected error: {projection_err}");
    let authored_err = input.access().require_authored_value(8).expect_err("expanded value needs its preimage");
    assert!(authored_err.to_string().contains("field `detail`"), "unexpected error: {authored_err}");
}

#[test]
fn observed_input_binding_owns_its_source_to_physical_reference() {
    let program = program(include_str!("../../../../../tests/fixtures/emit/observed_template_witnesses/app.ag"));
    let model = Model::from_program(&program).expect("observed input fixture plans");
    let (actor, entry) = actor_entry(&model, "Local", "step");
    let plan = plan_entry_input_states(actor, entry, &model).expect("input states plan");
    let input = plan.observed("asset", "src").expect("observed input exists");

    assert_eq!(input.source_ref(), "asset.inputs.src.state");
    assert_eq!(input.access().source_type(), "ForeignState");
    assert_eq!(input.access().physical_expr(), "gen__asset_src_state");
    assert!(matches!(input.access(), SourceStateAccess::Authored { .. }));
}

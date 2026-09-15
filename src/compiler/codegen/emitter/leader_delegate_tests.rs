//! CoffeeMachine, Cup, and Coffee are actors in the same application.
//! CoffeeMachine stores available water, Cup stores capacity and Coffee stores drink volume.
//! A brew spends a machine and a cup from the same covenant group.
//! The machine is the leader, Cup::fill delegates to that machine.
//! The machine checks the cup capacity and uses 200 ml of water.
//! It authorizes an updated machine and a Coffee output representing the filled cup.
//! The cup authorizes no outputs of its own.

use std::path::PathBuf;

use kaspa_consensus_core::{
    Hash,
    tx::{CovenantBinding, TransactionId, TransactionOutpoint},
};

use super::*;
use crate::builder::{BuilderError, EntryCall, TxBuilder, TxContext, args, state};

const COFFEE_SHOP: &str = r#"
const int ZERO = 0;
const int SERVING_ML = 200;
state MachineState { int water_ml; }
state CupState { int capacity_ml; }
state CoffeeState { int volume_ml; }

actor CoffeeMachine owns MachineState {
    entry brew() consumes { cup: Cup, } emits { machine: CoffeeMachine, drink: Coffee, } {
        require(water_ml >= SERVING_ML);
        require(cup.capacity_ml >= SERVING_ML);
        unrestricted(machine.value);
        unrestricted(drink.value);
        become {
            machine <- CoffeeMachine(MachineState { water_ml: water_ml - SERVING_ML, }),
            drink <- Coffee(CoffeeState { volume_ml: SERVING_ML, }),
        };
    }
    entry brew_batch() consumes { cups: Cup[ZERO..=2], }
    emits { machine: CoffeeMachine, drinks: Coffee[ZERO..=2], } {
        require(water_ml >= SERVING_ML * cups.length);
        CoffeeState[] servings;
        for (i, 0, cups.length, 2) {
            require(cups[i].capacity_ml >= SERVING_ML);
            servings = servings.append(CoffeeState { volume_ml: SERVING_ML, });
            unrestricted(drinks[i].value);
        }
        unrestricted(machine.value);
        become {
            machine <- CoffeeMachine(MachineState { water_ml: water_ml - SERVING_ML * cups.length, }),
            drinks <- Coffee[](servings),
        };
    }
    entry retire() consumes { cup: Cup, } emits none {}
    entry takeaway() consumes { cup: Cup, }
    spawns order by order_id { outputs { drink: Coffee, } }
    emits none {
        // retire the machine after its last takeaway.
        require(water_ml >= SERVING_ML);
        require(cup.capacity_ml >= SERVING_ML);
        unrestricted(order.outputs.drink.value);
        require order.outputs become { drink <- Coffee(CoffeeState { volume_ml: SERVING_ML, }), };
    }
    entry service() emits next: CoffeeMachine {
        unrestricted(next.value);
        become next <- self;
    }
}

actor Cup owns CupState {
    delegate fill() consumes { machine: CoffeeMachine, } {}
    entry discard() emits none {}
    entry wash() emits next: Cup {
        unrestricted(next.value);
        become next <- self;
    }
    entry replace(int count) emits next: Cup[ZERO..=2] {
        CupState[] replacements;
        for (i, 0, count, 2) {
            replacements = replacements.append(CupState { capacity_ml: capacity_ml, });
            unrestricted(next[i].value);
        }
        become next <- Cup[](replacements);
    }
    entry replace_required() emits next: Cup[1..=2] {
        CupState[] replacements;
        replacements = replacements.append(CupState { capacity_ml: capacity_ml, });
        unrestricted(next[0].value);
        become next <- Cup[](replacements);
    }
    entry wash_with_spares() emits { first: Cup, rest: Cup[ZERO..=2], } {
        CupState[] spares;
        unrestricted(first.value);
        for (i, 0, rest.length, 2) {
            unrestricted(rest[i].value);
        }
        become { first <- self, rest <- Cup[](spares), };
    }
    entry takeaway()
    spawns order by order_id { outputs { drink: Coffee, } }
    emits none {
        require(capacity_ml >= SERVING_ML);
        unrestricted(order.outputs.drink.value);
        require order.outputs become { drink <- Coffee(CoffeeState { volume_ml: SERVING_ML, }), };
    }
}

actor Coffee owns CoffeeState {
    entry discard() emits none { require(volume_ml >= 0); }
    entry keep_warm() emits next: Coffee {
        unrestricted(next.value);
        become next <- self;
    }
}

app CoffeeShop { actor CoffeeMachine; actor Cup; actor Coffee; }
"#;

fn compile_coffee_shop(source: &str) -> (BTreeMap<String, String>, Artifact) {
    let program = crate::compiler::loader::load_inline_program(PathBuf::from("coffee_shop.ag"), source.to_owned())
        .expect("coffee shop source resolves");
    let source = crate::compiler::model::ModelSource::new(&program, None).expect("model source adapts");
    let model = Model::from_source(&source).expect("coffee shop model validates");
    let sil = model.actors.iter().map(|actor| (actor.name.clone(), emit_actor(actor, &model).expect("actor emits"))).collect();
    let artifact = emit_artifact(&program, &model, &sil).expect("coffee shop contracts compile");
    (sil, artifact)
}

fn entry_sil<'a>(sil: &'a str, entry: &str) -> &'a str {
    sil.split_once(&format!("    entry {entry}(")).expect("entry exists").1.split_once("\n    }\n").expect("entry ends").0
}

#[test]
fn rule_5_emits_closure_for_every_coordinated_entry_only() {
    let closure = "require(OpCovOutputCount(gen__cov_id) == OpAuthOutputCount(this.activeInputIndex));";
    for source in [COFFEE_SHOP.to_owned(), COFFEE_SHOP.replace("delegate fill() consumes { machine: CoffeeMachine, } {}", "")] {
        let (sil, _) = compile_coffee_shop(&source);
        for entry in ["brew", "brew_batch", "retire", "takeaway"] {
            let body = entry_sil(&sil["CoffeeMachine"], entry);
            assert_eq!(body.matches(closure).count(), 1, "{entry}: {body}");
            assert!(body.find("// :: auth outputs").unwrap() < body.find(closure).unwrap(), "{body}");
            assert_eq!(body.matches("byte[32] gen__cov_id =").count(), 1, "{body}");
        }
        assert!(!entry_sil(&sil["CoffeeMachine"], "service").contains(closure));
        assert!(!sil["Cup"].contains(closure));
        assert!(!sil["Coffee"].contains(closure));
    }
}

#[test]
fn rule_6_uses_resolved_current_output_minima_and_actor_roles() {
    let (sil, _) = compile_coffee_shop(COFFEE_SHOP);
    let position = "require(OpCovInputIdx(gen__cov_id, 0) == this.activeInputIndex);";
    for entry in ["discard", "replace", "takeaway"] {
        let body = entry_sil(&sil["Cup"], entry);
        assert_eq!(body.matches(position).count(), 1, "{entry}: {body}");
        assert_eq!(body.matches("byte[32] gen__cov_id =").count(), 1, "{body}");
        assert!(!body.contains("OpCovInputCount"), "{body}");
    }
    for entry in ["wash", "replace_required", "wash_with_spares"] {
        let body = entry_sil(&sil["Cup"], entry);
        assert!(!body.contains("OpCovInputIdx"), "{entry}: {body}");
        assert!(!body.contains("OpCovInputCount"), "{body}");
    }
    assert!(!entry_sil(&sil["Coffee"], "discard").contains("OpCovInputIdx"));
    let delegate = entry_sil(&sil["Cup"], "fill");
    assert!(delegate.contains("require(OpCovInputIdx(gen__cov_id, 0) != this.activeInputIndex);"));
    assert!(delegate.contains("require(OpAuthOutputCount(this.activeInputIndex) == 0);"));
    assert!(!delegate.contains(position), "{delegate}");

    // Cup now has both roles: Coffee trusts Cup, and Cup trusts CoffeeMachine.
    let source = COFFEE_SHOP
        .replace("actor Coffee owns CoffeeState {", "actor Coffee owns CoffeeState { delegate serve() consumes { cup: Cup, } {}");
    let (sil, _) = compile_coffee_shop(&source);
    for entry in ["discard", "replace", "takeaway"] {
        let body = entry_sil(&sil["Cup"], entry);
        assert!(body.contains("require(OpCovInputCount(gen__cov_id) == 1);"), "{body}");
        assert!(!body.contains(position), "{body}");
    }
}

#[test]
fn rule_5_rejects_parallel_continuations_in_compiled_scripts() {
    let (_, artifact) = compile_coffee_shop(COFFEE_SHOP);
    let builder = TxBuilder::new(&artifact).expect("artifact verifies");
    let covenant_id = Hash::from_bytes([51; 32]);
    let machine = state! { water_ml: 1_000 };
    let brewed_machine = state! { water_ml: 800 };
    let cup = state! { capacity_ml: 250 };
    let coffee = state! { volume_ml: 200 };
    let machine_utxo = builder.covenant_utxo("CoffeeMachine", machine.clone(), 10_000, 0, false, Some(covenant_id)).unwrap();
    let cup_utxo = builder.covenant_utxo("Cup", cup.clone(), 10_000, 0, false, Some(covenant_id)).unwrap();

    for machine_entry in ["brew", "brew_batch", "retire", "takeaway"] {
        for parallel in [false, true] {
            let mut context = TxContext::new()
                .actor_input(
                    "CoffeeMachine",
                    machine.clone(),
                    machine_entry,
                    TransactionOutpoint::new(TransactionId::from_bytes([52; 32]), 0),
                    machine_utxo.clone(),
                    0,
                )
                .actor_input(
                    "Cup",
                    cup.clone(),
                    if parallel { "wash" } else { "fill" },
                    TransactionOutpoint::new(TransactionId::from_bytes([53; 32]), 0),
                    cup_utxo.clone(),
                    0,
                );
            if matches!(machine_entry, "brew" | "brew_batch") {
                context = context
                    .actor_output("CoffeeMachine", brewed_machine.clone(), CovenantBinding::new(0, covenant_id), 5_000)
                    .actor_output("Coffee", coffee.clone(), CovenantBinding::new(0, covenant_id), 5_000);
            }
            if machine_entry == "takeaway" {
                context = context.actor_genesis_output(0, "spawn::order", "Coffee", coffee.clone(), 5_000);
            }
            if parallel {
                context = context.actor_output("Cup", cup.clone(), CovenantBinding::new(1, covenant_id), 5_000);
            }
            let result = builder.build(&context);
            if parallel {
                assert!(matches!(result, Err(BuilderError::InputScript { input_index: 0, .. })), "{machine_entry}: {result:?}");
            } else {
                result.unwrap_or_else(|err| panic!("{machine_entry} with a delegate must execute: {err}"));
            }
        }
    }

    let context = TxContext::new()
        .actor_input(
            "CoffeeMachine",
            machine.clone(),
            "brew_batch",
            TransactionOutpoint::new(TransactionId::from_bytes([54; 32]), 0),
            machine_utxo,
            0,
        )
        .actor_output("CoffeeMachine", machine, CovenantBinding::new(0, covenant_id), 5_000);
    builder.build(&context).expect("an empty brew batch preserves the machine's water and produces no coffee");
}

#[test]
fn rule_5_keeps_interleaved_covenant_groups_independent() {
    let (_, artifact) = compile_coffee_shop(COFFEE_SHOP);
    let builder = TxBuilder::new(&artifact).expect("artifact verifies");
    let machine = state! { water_ml: 1_000 };
    let brewed_machine = state! { water_ml: 800 };
    let cup = state! { capacity_ml: 250 };
    let coffee = state! { volume_ml: 200 };
    let ids = [Hash::from_bytes([81; 32]), Hash::from_bytes([82; 32])];
    let mut context = TxContext::new();
    // The leaders occupy tx[0] and tx[1]; their delegates occupy tx[2] and tx[3].
    for (index, (actor, entry, covenant_id, state)) in [
        ("CoffeeMachine", "brew", ids[0], &machine),
        ("CoffeeMachine", "brew", ids[1], &machine),
        ("Cup", "fill", ids[0], &cup),
        ("Cup", "fill", ids[1], &cup),
    ]
    .into_iter()
    .enumerate()
    {
        let utxo = builder.covenant_utxo(actor, state.clone(), 10_000, 0, false, Some(covenant_id)).unwrap();
        context = context.actor_input(
            actor,
            state.clone(),
            entry,
            TransactionOutpoint::new(TransactionId::from_bytes([83; 32]), index as u32),
            utxo,
            0,
        );
    }
    context = context
        .actor_output("CoffeeMachine", brewed_machine.clone(), CovenantBinding::new(0, ids[0]), 5_000)
        .actor_output("CoffeeMachine", brewed_machine, CovenantBinding::new(1, ids[1]), 5_000)
        .actor_output("Coffee", coffee.clone(), CovenantBinding::new(0, ids[0]), 5_000)
        .actor_output("Coffee", coffee, CovenantBinding::new(1, ids[1]), 5_000);
    builder.build(&context).expect("each covenant group has its own leader and continuation closure");
}

#[test]
fn rule_6_rejects_outputless_ordinary_entries_at_delegate_positions() {
    let (_, artifact) = compile_coffee_shop(COFFEE_SHOP);
    let builder = TxBuilder::new(&artifact).expect("artifact verifies");
    let covenant_id = Hash::from_bytes([61; 32]);
    let machine = state! { water_ml: 1_000 };
    let brewed_machine = state! { water_ml: 800 };
    let cup = state! { capacity_ml: 250 };
    let coffee = state! { volume_ml: 200 };
    let machine_utxo = builder.covenant_utxo("CoffeeMachine", machine.clone(), 10_000, 0, false, Some(covenant_id)).unwrap();
    let cup_utxo = builder.covenant_utxo("Cup", cup.clone(), 10_000, 0, false, Some(covenant_id)).unwrap();
    for cup_entry in ["discard", "replace", "takeaway"] {
        let call = if cup_entry == "replace" { EntryCall::new(cup_entry).args(args![0]) } else { EntryCall::new(cup_entry) };
        let mut context = TxContext::new()
            .actor_input(
                "CoffeeMachine",
                machine.clone(),
                "brew",
                TransactionOutpoint::new(TransactionId::from_bytes([62; 32]), 0),
                machine_utxo.clone(),
                0,
            )
            .actor_input(
                "Cup",
                cup.clone(),
                call,
                TransactionOutpoint::new(TransactionId::from_bytes([63; 32]), 0),
                cup_utxo.clone(),
                0,
            )
            .actor_output("CoffeeMachine", brewed_machine.clone(), CovenantBinding::new(0, covenant_id), 5_000)
            .actor_output("Coffee", coffee.clone(), CovenantBinding::new(0, covenant_id), 5_000);
        if cup_entry == "takeaway" {
            context = context.actor_genesis_output(1, "spawn::order", "Coffee", coffee.clone(), 5_000);
        }
        let result = builder.build(&context);
        assert!(matches!(result, Err(BuilderError::InputScript { input_index: 1, .. })), "{cup_entry}: {result:?}");
    }
}

#[test]
fn rule_6_preserves_independent_batches_and_uses_covenant_group_indices() {
    let (_, artifact) = compile_coffee_shop(COFFEE_SHOP);
    let builder = TxBuilder::new(&artifact).expect("artifact verifies");
    let covenant_id = Hash::from_bytes([71; 32]);
    let other_id = Hash::from_bytes([72; 32]);
    let cup = state! { capacity_ml: 250 };
    let coffee = state! { volume_ml: 200 };
    let cup_utxo = builder.covenant_utxo("Cup", cup.clone(), 10_000, 0, false, Some(covenant_id)).unwrap();
    let coffee_utxo = builder.covenant_utxo("Coffee", coffee.clone(), 10_000, 0, false, Some(covenant_id)).unwrap();
    let other_utxo = builder.covenant_utxo("Coffee", coffee.clone(), 10_000, 0, false, Some(other_id)).unwrap();

    for (entry, output_count, restricted) in [
        ("discard", 0, true),
        ("replace", 0, true),
        ("replace", 1, true),
        ("takeaway", 0, true),
        ("wash", 1, false),
        ("replace_required", 1, false),
        ("wash_with_spares", 1, false),
    ] {
        for cup_first in [true, false] {
            let call = if entry == "replace" { EntryCall::new(entry).args(args![output_count]) } else { EntryCall::new(entry) };
            // Another covenant occupies tx[0]. The tested covenant begins at tx[1].
            let mut context = TxContext::new().actor_input(
                "Coffee",
                coffee.clone(),
                "keep_warm",
                TransactionOutpoint::new(TransactionId::from_bytes([73; 32]), 0),
                other_utxo.clone(),
                0,
            );
            let cup_outpoint = TransactionOutpoint::new(TransactionId::from_bytes([74; 32]), 0);
            let coffee_outpoint = TransactionOutpoint::new(TransactionId::from_bytes([75; 32]), 0);
            if cup_first {
                context = context.actor_input("Cup", cup.clone(), call, cup_outpoint, cup_utxo.clone(), 0).actor_input(
                    "Coffee",
                    coffee.clone(),
                    "keep_warm",
                    coffee_outpoint,
                    coffee_utxo.clone(),
                    0,
                );
            } else {
                context = context
                    .actor_input("Coffee", coffee.clone(), "keep_warm", coffee_outpoint, coffee_utxo.clone(), 0)
                    .actor_input("Cup", cup.clone(), call, cup_outpoint, cup_utxo.clone(), 0);
            }
            let cup_index = if cup_first { 1 } else { 2 };
            let coffee_index = if cup_first { 2 } else { 1 };
            context = context.actor_output("Coffee", coffee.clone(), CovenantBinding::new(0, other_id), 5_000).actor_output(
                "Coffee",
                coffee.clone(),
                CovenantBinding::new(coffee_index, covenant_id),
                5_000,
            );
            if output_count > 0 {
                context = context.actor_output("Cup", cup.clone(), CovenantBinding::new(cup_index, covenant_id), 5_000);
            }
            if entry == "takeaway" {
                context = context.actor_genesis_output(cup_index, "spawn::order", "Coffee", coffee.clone(), 5_000);
            }
            let result = builder.build(&context);
            if restricted && !cup_first {
                assert!(matches!(result, Err(BuilderError::InputScript { input_index: 2, .. })), "{entry}/{output_count}: {result:?}");
            } else {
                result.unwrap_or_else(|err| panic!("{entry}/{output_count}, cup first={cup_first}: {err}"));
            }
        }
    }

    let context = TxContext::new()
        .actor_input(
            "Coffee",
            coffee.clone(),
            "discard",
            TransactionOutpoint::new(TransactionId::from_bytes([76; 32]), 0),
            coffee_utxo.clone(),
            0,
        )
        .actor_input("Coffee", coffee, "discard", TransactionOutpoint::new(TransactionId::from_bytes([77; 32]), 0), coffee_utxo, 0);
    builder.build(&context).expect("actors without delegates can batch zero-output entries at any group position");
}

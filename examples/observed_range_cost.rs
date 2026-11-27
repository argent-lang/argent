//! Report observer bytecode size and VM charges for ranged input reads.
//! Run with `cargo run --locked --example observed_range_cost`.

use argent::compile_inline;
use argent_runtime::{EntryCall, TxBuilder, TxContext, args, covenant_engine_flags, state};
use kaspa_consensus_core::{
    Hash,
    hashing::sighash::SigHashReusedValuesUnsync,
    tx::{PopulatedTransaction, TransactionId, TransactionOutpoint},
};
use kaspa_txscript::{EngineCtx, TxScriptEngine, caches::Cache, covenants::CovenantsContext};

const SOURCE: &str = r#"
    state ObserverState { cov_id remote_id; }
    state AssetState { int amount; }

    actor Observer owns ObserverState {
        entry inspect(int last)
        observes remote by self.remote_id {
            inputs { assets: Asset[1..=MAX_ASSETS], }
        }
        emits none {
            require(remote.inputs.assets[last].amount == last);
        }
    }

    actor Asset owns AssetState {
        entry close() emits none { require(amount >= 0); }
    }

    app RangeCost { actor Observer; actor Asset; }
"#;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let remote_id = Hash::from_bytes([1; 32]);
    let observer_id = Hash::from_bytes([2; 32]);
    let outpoint = |index| TransactionOutpoint::new(TransactionId::from_bytes([3; 32]), index);
    let counts = [1, 4, 8, 16, 32, 64];

    println!("maximum,actual,observer_bytes,observer_script_units");
    for maximum in [4, 8, 16, 32, 64] {
        let artifact = compile_inline("observed_range_cost.ag", format!("const int MAX_ASSETS = {maximum};\n{SOURCE}"))?;
        let observer_bytes = artifact.sil_abi.contract("Observer").expect("Observer contract").compiled.bytecode.len();
        let builder = TxBuilder::new(&artifact)?;
        let observer_state = state! { remote_id: remote_id };
        let observer_utxo = builder.covenant_utxo("Observer", observer_state.clone(), 1_000, 0, false, Some(observer_id))?;

        for actual in counts.into_iter().take_while(|count| *count <= maximum) {
            let mut context = TxContext::new().actor_input(
                "Observer",
                observer_state.clone(),
                EntryCall::new("inspect").args(args![i64::from(actual - 1)]),
                outpoint(0),
                observer_utxo.clone(),
                0,
            );
            let mut entries = vec![observer_utxo.clone()];
            for index in 0..actual {
                let state = state! { amount: i64::from(index) };
                let utxo = builder.covenant_utxo("Asset", state.clone(), 1_000, 0, false, Some(remote_id))?;
                context = context.actor_input("Asset", state, "close", outpoint(index + 1), utxo.clone(), 0);
                entries.push(utxo);
            }

            // Measure only the observer input; the other scripts stay out of the result.
            let tx = builder.build(&context)?;
            let populated = PopulatedTransaction::new(&tx, entries);
            let covenants = CovenantsContext::from_tx(&populated)?;
            let cache = Cache::new(100);
            let reused = SigHashReusedValuesUnsync::new();
            let mut vm = TxScriptEngine::from_transaction_input(
                &populated,
                &tx.inputs[0],
                0,
                &populated.entries[0],
                EngineCtx::new(&cache).with_reused(&reused).with_covenants_ctx(&covenants),
                covenant_engine_flags(),
            );
            vm.execute()?;
            println!("{maximum},{actual},{observer_bytes},{}", vm.used_script_units().0);
        }
    }
    Ok(())
}

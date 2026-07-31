use std::{fs, path::PathBuf};

use argent::builder::{Artifact, EntryCall, TxBuilder, TxContext, actor, args, state};
use kaspa_consensus_core::{
    Hash,
    tx::{CovenantBinding, TransactionId, TransactionOutpoint},
};

const APP_SOURCE: &str = r#"
state CounterState {
    int count;
    cov_id guard;
}

state ShadowState {
    bool marker;
}

actor Counter owns CounterState {
    entry bump(bool choose, int iterations) emits next: Counter {
        unrestricted(next.value);

        __BODY__
    }
}

actor Shadow owns ShadowState {
    entry inspect() emits none {
        require(marker);
    }
}

app ScopeMatrix {
    actor Counter;
    actor Shadow;
}
"#;

const SELECTOR_APP_SOURCE: &str = r#"
state BoardState {
    int count;
}

actor enum NextActor {
    Alpha;
    Beta;
}

actor Router owns BoardState {
    entry choose(NextActor target, bool first_branch) emits next: NextActor {
        unrestricted(next.value);
        BoardState next_state = {
            count: count + 1,
        };

        if (first_branch) {
            become next <- target(next_state);
        } else {
            become next <- target(next_state);
        }
    }
}

actor Alpha owns BoardState {
    entry inspect() emits none {
        require(count >= 0);
    }
}

actor Beta owns BoardState {
    entry inspect() emits none {
        require(count >= 0);
    }
}

app SelectorScope {
    actor Router;
    actor Alpha;
    actor Beta;
}
"#;

#[test]
fn state_local_scope_survives_a_standalone_block() {
    execute_scope_flows(
        "state-standalone-block",
        r#"
        CounterState next_state = {
            count: count + 1,
            guard: guard,
        };
        {
            ShadowState next_state = {
                marker: true,
            };
            require(next_state.marker);
        }

        become next <- Counter(next_state);
        "#,
        &[(true, 1)],
    );
}

#[test]
fn state_local_scope_survives_both_if_paths() {
    execute_scope_flows(
        "state-if-branch",
        r#"
        CounterState next_state = {
            count: count + 1,
            guard: guard,
        };
        if (choose) {
            ShadowState next_state = {
                marker: true,
            };
            require(next_state.marker);
        }

        become next <- Counter(next_state);
        "#,
        &[(false, 1), (true, 1)],
    );
}

#[test]
fn state_local_scope_survives_zero_and_nonzero_loop_iterations() {
    execute_scope_flows(
        "state-for-body",
        r#"
        CounterState next_state = {
            count: count + 1,
            guard: guard,
        };
        for (i, 0, iterations, 8) {
            ShadowState next_state = {
                marker: i >= 0,
            };
            require(next_state.marker);
        }

        become next <- Counter(next_state);
        "#,
        &[(true, 0), (true, 3)],
    );
}

#[test]
fn cov_id_local_scope_survives_a_standalone_block() {
    execute_scope_flows(
        "cov-id-standalone-block",
        r#"
        cov_id relevant = guard;
        {
            int relevant = 1;
            require(relevant == 1);
        }
        require(relevant.co_spent());

        CounterState next_state = {
            count: count + 1,
            guard: guard,
        };
        become next <- Counter(next_state);
        "#,
        &[(true, 1)],
    );
}

#[test]
fn cov_id_local_scope_survives_both_if_paths() {
    execute_scope_flows(
        "cov-id-if-branch",
        r#"
        cov_id relevant = guard;
        if (choose) {
            int relevant = 1;
            require(relevant == 1);
        }
        require(relevant.co_spent());

        CounterState next_state = {
            count: count + 1,
            guard: guard,
        };
        become next <- Counter(next_state);
        "#,
        &[(false, 1), (true, 1)],
    );
}

#[test]
fn cov_id_local_scope_survives_zero_and_nonzero_loop_iterations() {
    execute_scope_flows(
        "cov-id-for-body",
        r#"
        cov_id relevant = guard;
        for (i, 0, iterations, 8) {
            int relevant = i;
            require(relevant >= 0);
        }
        require(relevant.co_spent());

        CounterState next_state = {
            count: count + 1,
            guard: guard,
        };
        become next <- Counter(next_state);
        "#,
        &[(true, 0), (true, 3)],
    );
}

#[test]
fn selector_materialization_is_independent_between_if_branches() {
    let artifact = compile_app("selector-if-branches", SELECTOR_APP_SOURCE);
    let builder = TxBuilder::new(&artifact).expect("builder accepts selector scope fixture");
    let covenant_id = Hash::from_bytes([0x61; 32]);
    let input_value = 1_000;

    for (index, (target, first_branch)) in [("Alpha", false), ("Alpha", true), ("Beta", false), ("Beta", true)].into_iter().enumerate()
    {
        let initial_state = state! { count: 4 };
        let input_utxo = builder
            .covenant_utxo("Router", initial_state.clone(), input_value, 0, false, Some(covenant_id))
            .expect("Router UTXO builds");
        let context = TxContext::new()
            .actor_input(
                "Router",
                initial_state,
                EntryCall::new("choose").args(args![actor(target), first_branch]),
                TransactionOutpoint::new(TransactionId::from_bytes([0x70 + index as u8; 32]), 0),
                input_utxo,
                0,
            )
            .actor_output(target, state! { count: 5 }, CovenantBinding::new(0, covenant_id), input_value);

        builder
            .build(&context)
            .unwrap_or_else(|err| panic!("selector scope flow target={target}, first_branch={first_branch} failed: {err}"));
    }
}

fn execute_scope_flows(name: &str, body: &str, flows: &[(bool, i64)]) {
    let artifact = compile_scope_app(name, body);
    let builder = TxBuilder::new(&artifact).expect("builder accepts scope fixture");
    let covenant_id = Hash::from_bytes([0x41; 32]);
    let input_value = 1_000;

    for (index, &(choose, iterations)) in flows.iter().enumerate() {
        let initial_state = state! {
            count: 4,
            guard: covenant_id,
        };
        let input_utxo = builder
            .covenant_utxo("Counter", initial_state.clone(), input_value, 0, false, Some(covenant_id))
            .expect("Counter UTXO builds");
        let context = TxContext::new()
            .actor_input(
                "Counter",
                initial_state,
                EntryCall::new("bump").args(args![choose, iterations]),
                TransactionOutpoint::new(TransactionId::from_bytes([0x50 + index as u8; 32]), 0),
                input_utxo,
                0,
            )
            .actor_output(
                "Counter",
                state! {
                    count: 5,
                    guard: covenant_id,
                },
                CovenantBinding::new(0, covenant_id),
                input_value,
            );

        builder.build(&context).unwrap_or_else(|err| panic!("scope flow choose={choose}, iterations={iterations} failed: {err}"));
    }
}

fn compile_scope_app(name: &str, body: &str) -> Artifact {
    let source = APP_SOURCE.replace("__BODY__", body);
    compile_app(name, &source)
}

fn compile_app(name: &str, source: &str) -> Artifact {
    let out_dir = std::env::temp_dir().join(format!("argent-{name}-scope-test-{}", std::process::id()));
    if out_dir.exists() {
        fs::remove_dir_all(&out_dir).expect("old scope test output is removed");
    }

    let result = argent::build_inline(PathBuf::from(format!("{name}.ag")), source, &out_dir);
    if out_dir.exists() {
        fs::remove_dir_all(&out_dir).expect("scope test output is removed");
    }
    result.unwrap_or_else(|err| panic!("scope fixture `{name}` must compile: {err}"))
}

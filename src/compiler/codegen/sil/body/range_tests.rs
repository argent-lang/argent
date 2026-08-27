//! Focused regressions for ranged entry-body lowering.

use std::path::PathBuf;

use super::*;

fn test_program(source: &str) -> Program {
    let path = PathBuf::from("body-lowering.ag");
    let module = crate::compiler::syntax::parser::parse_module(path.clone(), source.to_string()).expect("source parses");
    Program { root: path, modules: vec![module] }
}

fn lower_test_body(source: &str, actor_name: &str) -> String {
    lower_test_body_result(source, actor_name).expect("body lowers")
}

fn lower_test_body_result(source: &str, actor_name: &str) -> Result<String> {
    let program = test_program(source);
    let model = Model::from_program(&program).expect("model builds");
    let actor = model.actor(actor_name).expect("actor exists");
    let entry = actor.entries.first().expect("actor has an entry");
    let input_references =
        super::super::state_boundary::plan_entry_input_references(actor, entry, &model).expect("input references plan");
    let state_values = ContractStateValuePlan::new(actor, &model).expect("state values plan");
    lower_entry_body(actor, entry, &model, &input_references, &state_values).map(|body| body.sil)
}

#[test]
fn lowers_singleton_and_ranged_input_values_by_lexical_binding() {
    let sil = lower_test_body(
        r#"
            const int MAX_ACCOUNTS = 3;

            state BatchState {}

            state AccountState {
                int balance;
            }

            actor Batch owns BatchState {
                entry inspect(int[] positions)
                consumes {
                    account: Account,
                    accounts: Account[1..=MAX_ACCOUNTS],
                }
                emits none {
                    require(account.value > 0);
                    require(accounts[accounts.length].value > 0);
                    require(accounts[positions[accounts.length - 1]].value > 0);
                    require(accounts[positions[/* accounts[0].value */ 0]].value > 0);
                    require(accounts[/* guaranteed */ 0].value >= 0);
                    require(accounts[1].value >= 0);
                    require(accounts[0].balance >= 0);
                    require(accounts[0].value_note >= 0);
                    require(parent.accounts[0].value >= 0);
                    require("accounts[0].value" == "accounts[0].value");
                }
            }

            actor Account owns AccountState {}

            app Test {
                actor Batch;
                actor Account;
            }
        "#,
        "Batch",
    );

    assert!(sil.contains("tx.inputs[gen__account_input_idx].value"), "singleton input value was not lowered:\n{sil}");
    assert!(
        sil.contains(
            "tx.inputs[OpCovInputIdx(gen__cov_id, 2 + (gen__checked_range_index(accounts.length, gen__accounts_count)))].value"
        ),
        "a one-past-end input index omitted its runtime check:\n{sil}"
    );
    assert!(
        sil.contains(
            "tx.inputs[OpCovInputIdx(gen__cov_id, 2 + (gen__checked_range_index(positions[accounts.length - 1], gen__accounts_count)))].value"
        ),
        "nested range index was not lowered:\n{sil}"
    );
    assert!(
        sil.contains(
            "tx.inputs[OpCovInputIdx(gen__cov_id, 2 + (gen__checked_range_index(positions[/* accounts[0].value */ 0], gen__accounts_count)))].value"
        ),
        "comments in the index expression were not preserved:\n{sil}"
    );
    assert!(
        sil.contains("tx.inputs[OpCovInputIdx(gen__cov_id, 2 + (/* guaranteed */ 0))].value"),
        "a guaranteed literal index retained a runtime check:\n{sil}"
    );
    assert!(
        sil.contains("tx.inputs[OpCovInputIdx(gen__cov_id, 2 + (gen__checked_range_index(1, gen__accounts_count)))].value"),
        "an optional literal index omitted its runtime check:\n{sil}"
    );
    assert!(sil.contains("accounts[0].balance"), "ordinary ranged state access changed:\n{sil}");
    assert!(sil.contains("accounts[0].value_note"), "a longer member name was rewritten:\n{sil}");
    assert!(sil.contains("parent.accounts[0].value"), "a non-rooted input name was rewritten:\n{sil}");
    assert!(sil.contains(r#""accounts[0].value" == "accounts[0].value""#), "string contents were rewritten:\n{sil}");
}

#[test]
fn lowers_ranged_output_values_and_named_state_array_routes() {
    let sil = lower_test_body(
        r#"
            state BatchState {}
            state AccountState {}

            actor Batch owns BatchState {
                entry inspect(int[] positions)
                emits {
                    next: Account[1..=3],
                } {
                    AccountState[] next_states;
                    AccountState first = AccountState {};
                    next_states = next_states.append(first);

                    {
                        int[] positions_copy = positions;
                        require(positions_copy.length >= 0);
                    }
                    require(next.length == next_states.length);
                    require(next[next.length].value > 0);
                    require(next[positions[next_states.length - 1]].value > 0);
                    require(next[1].value > 0);
                    require(next[/* next[0].value */ 0].value >= 0);
                    require(next[0].balance >= 0);
                    require(next.length_note >= 0);
                    require(parent.next.length >= 0);
                    require(parent.next[0].value >= 0);
                    require("next[0].value" == "next[0].value");
                    become next <- Account[](next_states);
                }
            }

            actor Account owns AccountState {}
            app Test { actor Batch; actor Account; }
        "#,
        "Batch",
    );

    assert!(sil.contains("require(positions_copy.length >= 0);"), "an unrelated local was rewritten as the output range:\n{sil}");
    assert!(sil.contains("require(gen__next_output_count == next_states.length);"), "range output length was not lowered:\n{sil}");
    assert!(
        sil.contains(
            "tx.outputs[OpAuthOutputIdx(this.activeInputIndex, gen__checked_range_index(gen__next_output_count, gen__next_output_count))].value"
        ),
        "a one-past-end output index omitted its runtime check:\n{sil}"
    );
    assert!(
        sil.contains(
            "tx.outputs[OpAuthOutputIdx(this.activeInputIndex, gen__checked_range_index(positions[next_states.length - 1], gen__next_output_count))].value"
        ),
        "nested range output index was not lowered:\n{sil}"
    );
    assert!(
        sil.contains("tx.outputs[OpAuthOutputIdx(this.activeInputIndex, gen__checked_range_index(1, gen__next_output_count))].value"),
        "an optional output index omitted its runtime check:\n{sil}"
    );
    assert!(
        sil.contains("tx.outputs[OpAuthOutputIdx(this.activeInputIndex, /* next[0].value */ 0)].value"),
        "comments in the output index were not preserved:\n{sil}"
    );
    assert!(sil.contains("next[0].balance"), "ordinary output-handle syntax changed:\n{sil}");
    assert!(sil.contains("next.length_note"), "a longer output member name was rewritten:\n{sil}");
    assert!(sil.contains("parent.next.length"), "a non-rooted output length was rewritten:\n{sil}");
    assert!(sil.contains("parent.next[0].value"), "a non-rooted output name was rewritten:\n{sil}");
    assert!(sil.contains(r#""next[0].value" == "next[0].value""#), "string contents were rewritten:\n{sil}");
    assert!(
        sil.contains("require(next_states.length == gen__next_output_count);"),
        "state-array length was not tied to the range:\n{sil}"
    );
    assert!(
        sil.contains("for (gen__next_output_position, 0, gen__next_output_count, 3)"),
        "the bounded output route loop was not emitted:\n{sil}"
    );
    assert!(sil.contains("next_states[gen__next_output_position]"), "the state array was not indexed inside the route loop:\n{sil}");
}

#[test]
fn permanently_empty_output_range_has_no_value_policy() {
    let source = r#"
        state BatchState {}
        state AccountState {
            int balance;
        }

        actor Batch owns BatchState {
            entry finish(AccountState[] next_states)
            emits {
                next: Account[0..=0],
            } {
                require(next.length == 0);
                become next <- Account[](next_states);
            }
        }

        actor Account owns AccountState {
            entry hold() emits none {}
        }
        app Test { actor Batch; actor Account; }
    "#;
    let sil = lower_test_body(source, "Batch");

    assert!(sil.contains("require(gen__next_output_count == 0);"), "empty range length was not lowered:\n{sil}");
    assert!(
        sil.contains("require(next_states.length == gen__next_output_count);"),
        "empty successor array was not tied to the range:\n{sil}"
    );
    crate::compile_inline("empty-output-range.ag", source).expect("a statically empty output range compiles");
}

#[test]
fn rejects_literal_range_value_indices_that_can_never_exist() {
    for (handle, index) in [("accounts", "-1"), ("accounts", "3"), ("next", "-1"), ("next", "3")] {
        let source = format!(
            r#"
                state BatchState {{}}
                state AccountState {{}}

                actor Batch owns BatchState {{
                    entry inspect(AccountState[] next_states)
                    consumes {{
                        accounts: Account[1..=3],
                    }}
                    emits {{
                        next: Account[1..=3],
                    }} {{
                        require({handle}[{index}].value >= 0);
                        unrestricted(next[0].value);
                        become next <- Account[](next_states);
                    }}
                }}

                actor Account owns AccountState {{}}
                app Test {{ actor Batch; actor Account; }}
            "#
        );
        let err = lower_test_body_result(&source, "Batch").expect_err("an impossible literal index must be rejected");
        assert!(
            err.to_string().contains(&format!("range `{handle}` index `{index}` is outside its declared positions `0..3`")),
            "unexpected error: {err}"
        );
    }
}

#[test]
fn records_ranged_current_input_state_arrays() {
    let program = test_program(
        r#"
            const int MAX_ACCOUNTS = 3;

            state BatchState {
                cov_id source_id;
            }

            state AccountState {
                int balance;
            }

            actor Batch owns BatchState {
                entry inspect()
                consumes {
                    accounts: Account[1..=MAX_ACCOUNTS],
                }
                observes source by self.source_id {
                    inputs {
                        previous: Account[1..=MAX_ACCOUNTS],
                    }
                    outputs {}
                }
                emits none {}
            }

            actor Account owns AccountState {}

            app Test {
                actor Batch;
                actor Account;
            }
        "#,
    );
    let model = Model::from_program(&program).expect("model builds");
    let actor = model.actor("Batch").expect("actor exists");
    let entry = actor.entries.first().expect("actor has an entry");
    let input_references =
        super::super::state_boundary::plan_entry_input_references(actor, entry, &model).expect("input references plan");
    let state_values = ContractStateValuePlan::new(actor, &model).expect("state values plan");
    let lowerer = BodyLowerer::new(actor, entry, &model, EntryInputReferenceView::Complete(&input_references), &state_values)
        .expect("body lowerer builds");

    assert_eq!(lowerer.bindings.source_type("accounts"), Some("AccountState[]"));
    assert_eq!(lowerer.bindings.lowered_type("accounts"), Some("AccountState[]"));
}

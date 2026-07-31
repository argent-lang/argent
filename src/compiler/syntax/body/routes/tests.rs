use super::collect_routes;

#[test]
fn extracts_atomic_named_routes() {
    let routes = collect_routes(
        r#"
            become {
                player_a_out <- Player(next_player_a),
                player_b_out <- Player(next_player_b),
            };
            "#,
    )
    .expect("routes parse");

    assert_eq!(routes.len(), 2);
    assert_eq!(routes[0].output, "player_a_out");
    assert_eq!(routes[0].actor, "Player");
    assert_eq!(routes[0].state, "next_player_a");
    assert_eq!(routes[1].output, "player_b_out");
    assert_eq!(routes[1].actor, "Player");
    assert_eq!(routes[1].state, "next_player_b");
}

#[test]
fn rejects_semicolons_in_become_route_lists() {
    let err = collect_routes(
        r#"
            become {
                player_a_out <- Player(next_player_a);
                player_b_out <- Player(next_player_b);
            };
            "#,
    )
    .expect_err("semicolon-separated routes must not parse");

    assert!(err.to_string().contains("expected `,` or `}`"), "unexpected error: {err}");
}

#[test]
fn extracts_inline_named_single_output_route() {
    let routes = collect_routes(
        r#"
            become next <- Done({
                final_value: next_value,
            });
            "#,
    )
    .expect("routes parse");

    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].output, "next");
    assert_eq!(routes[0].actor, "Done");
    assert!(routes[0].state.contains("final_value"));
}

#[test]
fn rejects_unnamed_single_output_route() {
    let err = collect_routes("become Done(next);").expect_err("unnamed routes must not parse");

    assert!(err.to_string().contains("must name its output"), "unexpected error: {err}");
}

#[test]
fn rejects_become_with_parent_fallthrough() {
    let err = collect_routes(
        r#"
            if (done) {
                become next <- Done(ticket);
            }
            become next <- Live(state);
            "#,
    )
    .expect_err("fallthrough after conditional become must be rejected");

    assert!(err.to_string().contains("must be terminal"), "unexpected error: {err}");
}

#[test]
fn rejects_one_sided_conditional_become() {
    let err = collect_routes(
        r#"
            if (done) {
                become next <- Done(ticket);
            }
            "#,
    )
    .expect_err("one-sided conditional become must be rejected");

    assert!(err.to_string().contains("explicit `else`"), "unexpected error: {err}");
}

#[test]
fn rejects_become_nested_in_for_loop() {
    let err = collect_routes(
        r#"
            for (i, 0, count, MAX_COUNT) {
                become next <- Done(states[i]);
            }
            "#,
    )
    .expect_err("a loop cannot provide one terminal route for the entry");

    assert!(err.to_string().contains("cannot be nested in a `for` loop"), "unexpected error: {err}");
}

#[test]
fn accepts_terminal_if_else_becomes() {
    let routes = collect_routes(
        r#"
            if (done) {
                become next <- Done(ticket);
            } else {
                become next <- Live(state);
            }
            "#,
    )
    .expect("terminal if/else becomes parse");

    assert_eq!(routes.len(), 2);
    assert_eq!(routes[0].actor, "Done");
    assert_eq!(routes[1].actor, "Live");
}

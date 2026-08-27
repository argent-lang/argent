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
    assert_eq!(routes[0].actor.as_deref(), Some("Player"));
    assert_eq!(routes[0].state.as_deref(), Some("next_player_a"));
    assert_eq!(routes[1].output, "player_b_out");
    assert_eq!(routes[1].actor.as_deref(), Some("Player"));
    assert_eq!(routes[1].state.as_deref(), Some("next_player_b"));
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
    assert_eq!(routes[0].actor.as_deref(), Some("Done"));
    assert!(routes[0].state.as_deref().is_some_and(|state| state.contains("final_value")));
}

#[test]
fn rejects_unnamed_single_output_route() {
    let err = collect_routes("become Done(next);").expect_err("unnamed routes must not parse");

    assert!(err.to_string().contains("must name its output with `output <- successor`"), "unexpected error: {err}");
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
    assert_eq!(routes[0].actor.as_deref(), Some("Done"));
    assert_eq!(routes[1].actor.as_deref(), Some("Live"));
}

#[test]
fn parses_exact_self_successor_forms() {
    let named = collect_routes("become next <- self;").expect("named exact successor parses");
    assert_eq!(named[0].output, "next");
    assert!(named[0].exact_self);

    let mixed = collect_routes("become { next <- self, peer <- Peer(next_peer) };").expect("mixed successor block parses");
    assert!(mixed[0].exact_self);
    assert_eq!(mixed[1].actor.as_deref(), Some("Peer"));

    let err = collect_routes("become self;").expect_err("exact successors must name an output like constructed successors");
    assert!(err.to_string().contains("every `become` route must name its output"), "unexpected error: {err}");
}

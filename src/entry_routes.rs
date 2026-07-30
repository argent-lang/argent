//! Extracts body routes and verifies that every `become` is terminal.

use crate::ast::{EntryBody, RouteCall};
use crate::entry_body::{EntryRoute, EntryStatement};
use crate::error::Result;

#[derive(Debug, Clone)]
pub struct RouteAnalysis {
    pub routes: Vec<RouteCall>,
    pub terminal_route_sets: Vec<Vec<RouteCall>>,
}

pub fn collect_routes(body: &str) -> Result<Vec<RouteCall>> {
    analyze_routes(body).map(|analysis| analysis.routes)
}

pub fn analyze_routes(body: &str) -> Result<RouteAnalysis> {
    analyze_entry_routes(&EntryBody::new(body)?)
}

pub(crate) fn analyze_entry_routes(body: &EntryBody) -> Result<RouteAnalysis> {
    let info = analyze_sequence(body, body.statements(), body.text().len())?;
    let routes = info.terminal_route_sets.iter().flatten().cloned().collect();
    Ok(RouteAnalysis { routes, terminal_route_sets: info.terminal_route_sets })
}

#[derive(Debug, Clone, Copy)]
struct TerminalInfo {
    contains_become: bool,
    all_paths_terminal: bool,
}

impl TerminalInfo {
    fn empty() -> Self {
        Self { contains_become: false, all_paths_terminal: false }
    }

    fn terminal(routes: Vec<RouteCall>) -> TerminalResult {
        TerminalResult { info: Self { contains_become: true, all_paths_terminal: true }, terminal_route_sets: vec![routes] }
    }
}

#[derive(Debug, Clone)]
struct TerminalResult {
    info: TerminalInfo,
    terminal_route_sets: Vec<Vec<RouteCall>>,
}

impl TerminalResult {
    fn empty() -> Self {
        Self { info: TerminalInfo::empty(), terminal_route_sets: Vec::new() }
    }
}

fn analyze_sequence(body: &EntryBody, statements: &[EntryStatement], end_offset: usize) -> Result<TerminalResult> {
    let mut result = TerminalResult::empty();
    for (index, statement) in statements.iter().enumerate() {
        let statement_result = analyze_statement(body, statement)?;
        result.info.contains_become |= statement_result.info.contains_become;
        result.terminal_route_sets.extend(statement_result.terminal_route_sets);

        if statement_result.info.all_paths_terminal {
            if let Some(next) = statements.get(index + 1) {
                return Err(body_error(
                    body,
                    next.span().start,
                    "`become` must be terminal; move following code into an explicit `else` branch",
                ));
            }
            result.info.all_paths_terminal = true;
            break;
        }
        if statement_result.info.contains_become {
            let next_offset = statements.get(index + 1).map_or(end_offset, |next| next.span().start);
            return Err(body_error(
                body,
                next_offset,
                "conditional `become` must be terminal on every branch; add an explicit `else` branch",
            ));
        }
    }
    Ok(result)
}

fn analyze_statement(body: &EntryBody, statement: &EntryStatement) -> Result<TerminalResult> {
    match statement {
        EntryStatement::If { condition: _, then_branch, else_branch, .. } => {
            let then_result = analyze_statement(body, then_branch)?;
            let else_result =
                if let Some(else_branch) = else_branch { analyze_statement(body, else_branch)? } else { TerminalResult::empty() };
            let contains_become = then_result.info.contains_become || else_result.info.contains_become;
            let all_paths_terminal = then_result.info.all_paths_terminal && else_result.info.all_paths_terminal;
            let mut terminal_route_sets = then_result.terminal_route_sets;
            terminal_route_sets.extend(else_result.terminal_route_sets);

            Ok(TerminalResult { info: TerminalInfo { contains_become, all_paths_terminal }, terminal_route_sets })
        }
        EntryStatement::Become { routes, .. } => {
            Ok(TerminalInfo::terminal(routes.iter().map(|route| route_call(body, route)).collect()))
        }
        EntryStatement::Block { statements, span } => analyze_sequence(body, statements, span.end.saturating_sub(1)),
        EntryStatement::Plain { .. } => Ok(TerminalResult::empty()),
    }
}

fn route_call(body: &EntryBody, route: &EntryRoute) -> RouteCall {
    RouteCall { output: route.output.clone(), actor: route.actor.clone(), state: body.span_text(route.state).trim().to_string() }
}

fn body_error(body: &EntryBody, byte_offset: usize, message: &str) -> crate::error::ArgentError {
    let preview = body.text()[byte_offset..].lines().next().unwrap_or("").trim().chars().take(80).collect::<String>();
    crate::error::ArgentError::new(format!("{message} at body byte {byte_offset} near `{preview}`"))
}

#[cfg(test)]
mod tests {
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
}

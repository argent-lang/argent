//! Extracts body routes and verifies that every `become` is terminal.

use super::{EntryBody, EntryRoute, EntryStatement};
use crate::compiler::syntax::RouteCall;
use crate::error::Result;

#[derive(Debug, Clone)]
pub struct RouteAnalysis {
    pub routes: Vec<RouteCall>,
    pub terminal_route_sets: Vec<Vec<RouteCall>>,
}

#[cfg(test)]
pub fn collect_routes(body: &str) -> Result<Vec<RouteCall>> {
    analyze_routes(body).map(|analysis| analysis.routes)
}

#[cfg(test)]
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
        EntryStatement::For { body: loop_body, .. } => {
            let result = analyze_statement(body, loop_body)?;
            if result.info.contains_become {
                return Err(body_error(body, loop_body.span().start, "`become` cannot be nested in a `for` loop"));
            }
            Ok(TerminalResult::empty())
        }
        EntryStatement::Block { statements, span } => analyze_sequence(body, statements, span.end.saturating_sub(1)),
        EntryStatement::ValidateOutputsBecome { .. } | EntryStatement::Plain { .. } => Ok(TerminalResult::empty()),
    }
}

fn route_call(body: &EntryBody, route: &EntryRoute) -> RouteCall {
    RouteCall {
        output: route.output.clone(),
        actor: body.span_text(route.actor).trim().to_string(),
        state: body.span_text(route.state).trim().to_string(),
    }
}

fn body_error(body: &EntryBody, byte_offset: usize, message: &str) -> crate::error::ArgentError {
    let preview = body.text()[byte_offset..].lines().next().unwrap_or("").trim().chars().take(80).collect::<String>();
    crate::error::ArgentError::new(format!("{message} at body byte {byte_offset} near `{preview}`"))
}

#[cfg(test)]
mod tests;

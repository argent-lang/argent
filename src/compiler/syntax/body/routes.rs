//! Extracts body routes and verifies that every `become` is terminal.

#[cfg(test)]
use super::EntrySuccessor;
use super::{EntryBody, EntryRoute, EntryStatement, RouteId};
use crate::error::Result;

#[derive(Debug, Clone)]
pub struct RouteAnalysis {
    pub routes: Vec<EntryRoute>,
    pub terminal_route_sets: Vec<Vec<RouteId>>,
}

#[cfg(test)]
#[derive(Debug)]
pub struct CollectedRoute {
    pub output: String,
    pub actor: Option<String>,
    pub state: Option<String>,
    pub exact_self: bool,
}

#[cfg(test)]
pub fn collect_routes(source: &str) -> Result<Vec<CollectedRoute>> {
    let body = EntryBody::new(source)?;
    analyze_entry_routes(&body).map(|analysis| {
        analysis
            .routes
            .into_iter()
            .map(|route| {
                let (actor, state, exact_self) = match route.successor {
                    EntrySuccessor::ExactSelf { .. } => (None, None, true),
                    EntrySuccessor::Constructed { actor, state } => {
                        (Some(body.span_text(actor).trim().to_string()), Some(body.span_text(state).trim().to_string()), false)
                    }
                };
                CollectedRoute { output: route.output, actor, state, exact_self }
            })
            .collect()
    })
}

pub(crate) fn analyze_entry_routes(body: &EntryBody) -> Result<RouteAnalysis> {
    let info = analyze_sequence(body, body.statements(), body.text().len())?;
    let routes = info.terminal_route_sets.iter().flatten().cloned().collect();
    let terminal_route_sets = info.terminal_route_sets.iter().map(|routes| routes.iter().map(|route| route.id).collect()).collect();
    Ok(RouteAnalysis { routes, terminal_route_sets })
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

    fn terminal(routes: Vec<EntryRoute>) -> TerminalResult {
        TerminalResult { info: Self { contains_become: true, all_paths_terminal: true }, terminal_route_sets: vec![routes] }
    }
}

#[derive(Debug, Clone)]
struct TerminalResult {
    info: TerminalInfo,
    terminal_route_sets: Vec<Vec<EntryRoute>>,
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
        EntryStatement::Become { routes, .. } => Ok(TerminalInfo::terminal(routes.clone())),
        EntryStatement::For { body: loop_body, .. } => {
            let result = analyze_statement(body, loop_body)?;
            if result.info.contains_become {
                return Err(body_error(body, loop_body.span().start, "`become` cannot be nested in a `for` loop"));
            }
            Ok(TerminalResult::empty())
        }
        EntryStatement::Block { statements, span } => analyze_sequence(body, statements, span.end.saturating_sub(1)),
        EntryStatement::ValidateOutputsBecome { .. } | EntryStatement::Local { .. } | EntryStatement::Plain { .. } => {
            Ok(TerminalResult::empty())
        }
    }
}

fn body_error(body: &EntryBody, byte_offset: usize, message: &str) -> crate::error::ArgentError {
    let preview = body.text()[byte_offset..].lines().next().unwrap_or("").trim().chars().take(80).collect::<String>();
    crate::error::ArgentError::new(format!("{message} at body byte {byte_offset} near `{preview}`"))
}

#[cfg(test)]
mod tests;

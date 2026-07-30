//! Extracts body routes and verifies that every `become` is terminal.

use crate::ast::{EntryBody, RouteCall};
use crate::entry_body::EntryBodyCursor;
use crate::error::{ArgentError, Result};
use crate::language::word;
use crate::lexer::TokenKind;

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
    let mut parser = TerminalParser { cursor: body.cursor() };
    let info = parser.parse_sequence(None)?;
    let routes = info.terminal_route_sets.iter().flatten().cloned().collect();
    Ok(RouteAnalysis { routes, terminal_route_sets: info.terminal_route_sets })
}

struct TerminalParser<'a> {
    cursor: EntryBodyCursor<'a>,
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

impl TerminalParser<'_> {
    fn parse_sequence(&mut self, end: Option<char>) -> Result<TerminalResult> {
        let mut result = TerminalResult::empty();
        while !self.cursor.is_eof() && !end.is_some_and(|symbol| self.cursor.check_symbol(symbol)) {
            let stmt = self.parse_statement()?;
            result.info.contains_become |= stmt.info.contains_become;
            result.terminal_route_sets.extend(stmt.terminal_route_sets);

            if stmt.info.all_paths_terminal {
                while self.cursor.consume_symbol(';') {}
                if self.cursor.is_eof() || end.is_some_and(|symbol| self.cursor.check_symbol(symbol)) {
                    result.info.all_paths_terminal = true;
                    break;
                }
                return Err(self.error("`become` must be terminal; move following code into an explicit `else` branch"));
            }
            if stmt.info.contains_become {
                return Err(self.error("conditional `become` must be terminal on every branch; add an explicit `else` branch"));
            }
        }
        Ok(result)
    }

    fn parse_statement(&mut self) -> Result<TerminalResult> {
        if self.cursor.consume_ident(word::IF) {
            self.expect_symbol('(')?;
            self.skip_balanced_after_open('(', ')')?;
            let then_info = self.parse_block_or_statement()?;
            let else_info =
                if self.cursor.consume_ident(word::ELSE) { self.parse_block_or_statement()? } else { TerminalResult::empty() };
            let contains_become = then_info.info.contains_become || else_info.info.contains_become;
            let all_paths_terminal = then_info.info.all_paths_terminal && else_info.info.all_paths_terminal;
            let mut terminal_route_sets = then_info.terminal_route_sets;
            terminal_route_sets.extend(else_info.terminal_route_sets);

            Ok(TerminalResult { info: TerminalInfo { contains_become, all_paths_terminal }, terminal_route_sets })
        } else if self.cursor.consume_ident(word::BECOME) {
            let routes = self.parse_become_tail()?;
            Ok(TerminalInfo::terminal(routes))
        } else if self.cursor.consume_symbol('{') {
            let result = self.parse_sequence(Some('}'))?;
            self.expect_symbol('}')?;
            Ok(result)
        } else {
            self.skip_statement()?;
            Ok(TerminalResult::empty())
        }
    }

    fn parse_block_or_statement(&mut self) -> Result<TerminalResult> {
        if self.cursor.consume_symbol('{') {
            let result = self.parse_sequence(Some('}'))?;
            self.expect_symbol('}')?;
            Ok(result)
        } else {
            self.parse_statement()
        }
    }

    fn parse_become_tail(&mut self) -> Result<Vec<RouteCall>> {
        if self.cursor.consume_symbol('{') {
            let mut routes = Vec::new();
            while !self.cursor.check_symbol('}') && !self.cursor.is_eof() {
                if self.cursor.check_ident(word::BECOME) {
                    return Err(self.error("nested `become` blocks are not supported yet"));
                }
                routes.push(self.parse_route()?);
                self.expect_list_separator_or_end('}')?;
            }
            self.expect_symbol('}')?;
            self.cursor.consume_symbol(';');
            return Ok(routes);
        }

        let route = self.parse_route()?;
        self.cursor.consume_symbol(';');
        Ok(vec![route])
    }

    fn parse_route(&mut self) -> Result<RouteCall> {
        let output = self.expect_any_ident()?;
        if !self.consume_left_arrow() {
            return Err(self.error("every `become` route must name its output with `output <- Actor(state)`"));
        }
        let actor = self.parse_actor_name()?;

        self.expect_symbol('(')?;
        let state_span =
            self.cursor.take_balanced_after_open('(', ')').ok_or_else(|| self.error("unterminated route state expression"))?;
        let state = self.cursor.span_text(state_span).trim().to_string();
        Ok(RouteCall { output, actor, state })
    }

    fn parse_actor_name(&mut self) -> Result<String> {
        let first = self.expect_any_ident()?;
        self.parse_qualified_tail(first)
    }

    fn parse_qualified_tail(&mut self, first: String) -> Result<String> {
        if !self.cursor.consume_symbol(':') {
            return Ok(first);
        }
        self.expect_symbol(':')?;
        Ok(format!("{first}::{}", self.expect_any_ident()?))
    }

    fn consume_left_arrow(&mut self) -> bool {
        match self.cursor.current().kind {
            TokenKind::LeftArrow => {
                self.cursor.advance();
                true
            }
            TokenKind::Symbol('<') if matches!(self.cursor.peek_kind(1), Some(TokenKind::Symbol('-'))) => {
                self.cursor.advance();
                self.cursor.advance();
                true
            }
            _ => false,
        }
    }

    fn expect_any_ident(&mut self) -> Result<String> {
        match self.cursor.current().kind.clone() {
            TokenKind::Ident(name) => {
                self.cursor.advance();
                Ok(name)
            }
            _ => Err(self.error("expected identifier in `become` route")),
        }
    }

    fn skip_statement(&mut self) -> Result<()> {
        self.skip_until_statement_end()
    }

    fn skip_until_statement_end(&mut self) -> Result<()> {
        while !self.cursor.is_eof() {
            match self.cursor.current().kind {
                TokenKind::Symbol(';') => {
                    self.cursor.advance();
                    return Ok(());
                }
                TokenKind::Symbol('{') => {
                    self.cursor.advance();
                    self.skip_balanced_after_open('{', '}')?;
                }
                TokenKind::Symbol('(') => {
                    self.cursor.advance();
                    self.skip_balanced_after_open('(', ')')?;
                }
                TokenKind::Symbol('[') => {
                    self.cursor.advance();
                    self.skip_balanced_after_open('[', ']')?;
                }
                TokenKind::Symbol('}') => return Ok(()),
                _ => self.cursor.advance(),
            }
        }
        Ok(())
    }

    fn skip_balanced_after_open(&mut self, open: char, close: char) -> Result<()> {
        if self.cursor.take_balanced_after_open(open, close).is_some() {
            Ok(())
        } else {
            Err(self.error(format!("unterminated `{open}` group")))
        }
    }

    fn expect_symbol(&mut self, expected: char) -> Result<()> {
        if self.cursor.consume_symbol(expected) { Ok(()) } else { Err(self.error(format!("expected `{expected}`"))) }
    }

    fn expect_list_separator_or_end(&mut self, end: char) -> Result<()> {
        if self.cursor.consume_symbol(',') || self.cursor.check_symbol(end) {
            Ok(())
        } else {
            Err(self.error(format!("expected `,` or `{end}`")))
        }
    }

    fn error(&self, message: impl Into<String>) -> ArgentError {
        let preview = self.cursor.remaining_text().lines().next().unwrap_or("").trim().chars().take(80).collect::<String>();
        ArgentError::new(format!("{} at body byte {} near `{}`", message.into(), self.cursor.byte_offset(), preview))
    }
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

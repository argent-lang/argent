//! Resolves global-function bindings from Sil's structured function AST.

use std::collections::BTreeSet;
use std::ops::Range;

use silverscript_lang::ast::parse_function_ast;
use silverscript_lang::ast::visit::{AstVisitorMut, NameKind, visit_function_mut};
use silverscript_lang::span::Span;

use crate::error::{ArgentError, Result};

#[derive(Debug)]
struct NameOccurrence {
    name: String,
    kind: NameKind,
    span: Range<usize>,
}

#[derive(Default)]
struct NameCollector {
    occurrences: Vec<NameOccurrence>,
}

impl<'i> AstVisitorMut<'i> for NameCollector {
    fn visit_name(&mut self, name: &mut String, kind: NameKind, span: Span<'i>) {
        self.occurrences.push(NameOccurrence { name: name.clone(), kind, span: span.start()..span.end() });
    }
}

pub(super) fn prefix_ranges(
    source: &str,
    body_span: Range<usize>,
    constants: &BTreeSet<String>,
    actor_functions: &BTreeSet<String>,
    function_name: &str,
) -> Result<Vec<Range<usize>>> {
    let mut function = parse_function_ast(source)
        .map_err(|err| ArgentError::new(format!("global function `{function_name}` could not be parsed as Silverscript: {err}")))?;
    let mut collector = NameCollector::default();
    visit_function_mut(&mut collector, &mut function);

    let bindings = collector
        .occurrences
        .iter()
        .filter(|occurrence| is_binding(occurrence.kind))
        .map(|occurrence| occurrence.name.clone())
        .collect::<BTreeSet<_>>();
    if let Some(name) = bindings.intersection(constants).next() {
        return Err(ArgentError::new(format!(
            "global function `{function_name}` binding `{name}` shadows a shared constant with the same name"
        )));
    }

    let mut ranges = Vec::new();
    for occurrence in collector.occurrences {
        let rewrite = match occurrence.kind {
            NameKind::LocalBinding | NameKind::LoopBinding | NameKind::StateBinding => true,
            NameKind::AssignmentTarget if bindings.contains(&occurrence.name) => true,
            NameKind::IdentifierExpr if bindings.contains(&occurrence.name) => true,
            NameKind::IdentifierExpr if constants.contains(&occurrence.name) => false,
            NameKind::IdentifierExpr => {
                return Err(ArgentError::new(format!(
                    "global function `{function_name}` cannot access unresolved identifier `{}`; pass it as a parameter, declare it locally, or use a shared constant",
                    occurrence.name
                )));
            }
            NameKind::AssignmentTarget => {
                return Err(ArgentError::new(format!(
                    "global function `{function_name}` assigns unresolved identifier `{}`; declare it locally before assignment",
                    occurrence.name
                )));
            }
            NameKind::CallTarget if actor_functions.contains(&occurrence.name) => {
                return Err(ArgentError::new(format!(
                    "global function `{function_name}` cannot call actor function `{}`",
                    occurrence.name
                )));
            }
            // Sil classifies call targets, field names, and contextual runtime
            // expressions independently from variable references.
            NameKind::Function
            | NameKind::Parameter
            | NameKind::AttributePathSegment
            | NameKind::AttributeArg
            | NameKind::CallTarget
            | NameKind::StateField
            | NameKind::Contract
            | NameKind::ContractField
            | NameKind::Constant => false,
        };
        if rewrite && occurrence.span.start >= body_span.start && occurrence.span.end <= body_span.end {
            ranges.push((occurrence.span.start - body_span.start)..(occurrence.span.end - body_span.start));
        }
    }
    ranges.sort_by_key(|range| range.start);
    ranges.dedup();
    debug_assert!(ranges.windows(2).all(|pair| pair[0].end <= pair[1].start));
    Ok(ranges)
}

fn is_binding(kind: NameKind) -> bool {
    matches!(kind, NameKind::Parameter | NameKind::LocalBinding | NameKind::LoopBinding | NameKind::StateBinding)
}

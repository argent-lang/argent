//! Lowers Argent functions into collision-free Sil function source.
//!
//! Identifier discovery is isolated behind `token_scan` so the current lexer
//! can later be replaced without changing the lowering or emitter boundary.

use std::collections::BTreeSet;

use crate::compiler::model::Model;
use crate::compiler::syntax::lexer::Span;
use crate::compiler::syntax::{FunctionDecl, TypeRef};
use crate::error::{ArgentError, Result};

use self::token_scan::variable_occurrences;

mod token_scan;

const VARIABLE_PREFIX: &str = "gen__glob_";

pub(in crate::compiler::codegen) struct GlobalFunctionLowerer {
    names: FunctionNames,
}

pub(in crate::compiler::codegen) struct LoweredFunction<'a> {
    pub(in crate::compiler::codegen) name: &'a str,
    pub(in crate::compiler::codegen) params: Vec<LoweredParam<'a>>,
    pub(in crate::compiler::codegen) return_ty: Option<&'a TypeRef>,
    pub(in crate::compiler::codegen) body: String,
}

pub(in crate::compiler::codegen) struct LoweredParam<'a> {
    pub(in crate::compiler::codegen) ty: &'a TypeRef,
    pub(in crate::compiler::codegen) name: String,
}

#[derive(Default)]
struct FunctionNames {
    constants: BTreeSet<String>,
    types: BTreeSet<String>,
}

impl GlobalFunctionLowerer {
    pub(in crate::compiler::codegen) fn new(model: &Model<'_>) -> Self {
        let constants = model.consts.iter().map(|ct| ct.name.clone()).collect();
        let types = model.states.keys().chain(model.linked_states.keys()).chain(model.actor_enums.keys()).cloned().collect();
        Self { names: FunctionNames { constants, types } }
    }

    pub(in crate::compiler::codegen) fn lower<'a>(&self, function: &'a FunctionDecl) -> Result<LoweredFunction<'a>> {
        lower_global_function(function, &self.names)
    }
}

fn lower_global_function<'a>(function: &'a FunctionDecl, names: &FunctionNames) -> Result<LoweredFunction<'a>> {
    if let Some(param) = function.params.iter().find(|param| names.constants.contains(&param.name)) {
        return Err(ArgentError::new(format!(
            "global function `{}` parameter `{}` shadows a constant with the same name",
            function.name, param.name
        )));
    }

    let params = function.params.iter().map(|param| LoweredParam { ty: &param.ty, name: prefixed(&param.name) }).collect();
    let occurrences = variable_occurrences(&function.body, names)?;
    let body = apply_prefix(&function.body, &occurrences);
    Ok(LoweredFunction { name: &function.name, params, return_ty: function.return_ty.as_ref(), body })
}

fn apply_prefix(body: &str, occurrences: &[Span]) -> String {
    let mut out = String::with_capacity(body.len() + occurrences.len() * VARIABLE_PREFIX.len());
    let mut cursor = 0;
    for span in occurrences {
        debug_assert!(cursor <= span.start && span.start <= span.end);
        out.push_str(&body[cursor..span.start]);
        out.push_str(VARIABLE_PREFIX);
        out.push_str(&body[span.start..span.end]);
        cursor = span.end;
    }
    out.push_str(&body[cursor..]);
    out
}

fn prefixed(name: &str) -> String {
    format!("{VARIABLE_PREFIX}{name}")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::compiler::syntax::parser::parse_module;

    use super::*;

    #[test]
    fn prefixes_only_global_function_variable_identifiers() {
        let source = r#"
            const int LIMIT = 4;

            state Turn {
                int cycles;
            }

            fn summarize(Turn turn, int index) -> int {
                /** result and values remain unchanged in documentation. */
                int result = helper(turn.cycles, LIMIT);
                Turn snapshot = Turn { cycles: result };
                result = values[index] + snapshot.cycles;
                int Turn = result;
                result = Turn;
                string note = "turn, result, values"; // values[index]
                for (i, 0, values.length, LIMIT) {
                    result = result + i;
                }
                return result + tx.inputs[index].value;
            }
        "#;
        let module = parse_module(PathBuf::from("test.ag"), source.to_string()).expect("module parses");
        let function = &module.functions[0];
        let names = FunctionNames {
            constants: ["LIMIT".to_string()].into_iter().collect(),
            types: ["Turn".to_string()].into_iter().collect(),
        };
        let lowered = lower_global_function(function, &names).expect("variables prefix");

        assert_eq!(lowered.params[0].name, "gen__glob_turn");
        assert_eq!(lowered.params[1].name, "gen__glob_index");
        assert_eq!(
            lowered.body,
            r#"
                /** result and values remain unchanged in documentation. */
                int gen__glob_result = helper(gen__glob_turn.cycles, LIMIT);
                Turn gen__glob_snapshot = Turn { cycles: gen__glob_result };
                gen__glob_result = gen__glob_values[gen__glob_index] + gen__glob_snapshot.cycles;
                int gen__glob_Turn = gen__glob_result;
                gen__glob_result = gen__glob_Turn;
                string gen__glob_note = "turn, result, values"; // values[index]
                for (gen__glob_i, 0, gen__glob_values.length, LIMIT) {
                    gen__glob_result = gen__glob_result + gen__glob_i;
                }
                return gen__glob_result + tx.inputs[gen__glob_index].value;
            "#
        );
    }
}

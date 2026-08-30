//! Owns function-specific namespace lowering and actor capture validation.
//!
//! Sil classifies names and their source spans; Argent resolves which names
//! belong to the isolated global-function namespace.

use std::collections::BTreeSet;
use std::ops::Range;

use crate::compiler::model::Model;
use crate::compiler::syntax::{ActorDecl, FunctionDecl, TypeRef};
use crate::error::Result;

use self::bindings::{prefix_ranges, reject_expanded_field_captures};

mod bindings;

const VARIABLE_PREFIX: &str = "gen__glob_";

pub(in crate::compiler::codegen) fn validate_actor_function_captures(actor: &ActorDecl, model: &Model<'_>) -> Result<()> {
    let Some(expansion) = model.state(&actor.state)?.expansion.as_ref() else {
        return Ok(());
    };
    let expanded_fields = expansion.digests.iter().map(|digest| digest.field.clone()).collect::<BTreeSet<_>>();
    for function in &actor.functions {
        let (source, body_span) = standalone_sil_function(function);
        reject_expanded_field_captures(&source, body_span, &expanded_fields, &actor.name, &function.name)?;
    }
    Ok(())
}

pub(in crate::compiler::codegen) struct GlobalFunctionLowerer {
    constants: BTreeSet<String>,
    actor_functions: BTreeSet<String>,
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

impl GlobalFunctionLowerer {
    pub(in crate::compiler::codegen) fn new(model: &Model<'_>) -> Self {
        let constants = model.consts.iter().map(|ct| ct.name.clone()).collect();
        let actor_functions =
            model.actor_models.values().flat_map(|actor| actor.functions()).map(|function| function.name.clone()).collect();
        Self { constants, actor_functions }
    }

    pub(in crate::compiler::codegen) fn lower<'a>(&self, function: &'a FunctionDecl) -> Result<LoweredFunction<'a>> {
        lower_global_function(function, &self.constants, &self.actor_functions)
    }
}

fn lower_global_function<'a>(
    function: &'a FunctionDecl,
    constants: &BTreeSet<String>,
    actor_functions: &BTreeSet<String>,
) -> Result<LoweredFunction<'a>> {
    let (source, body_span) = standalone_sil_function(function);
    let ranges = prefix_ranges(&source, body_span, constants, actor_functions, &function.name)?;
    let params = function.params.iter().map(|param| LoweredParam { ty: &param.ty, name: prefixed(&param.name) }).collect();
    let body = apply_prefix(&function.body, &ranges);
    Ok(LoweredFunction { name: &function.name, params, return_ty: function.return_ty.as_ref(), body })
}

pub(super) fn standalone_sil_function(function: &FunctionDecl) -> (String, Range<usize>) {
    let params = function.params.iter().map(|param| format!("{} {}", param.ty.to_sil(), param.name)).collect::<Vec<_>>().join(", ");
    let return_type = function.return_ty.as_ref().map(|ty| format!(" : {}", ty.to_sil())).unwrap_or_default();
    let mut source = format!("function {}({params}){return_type} {{", function.name);
    let body_start = source.len();
    source.push_str(&function.body);
    let body_end = source.len();
    source.push('}');
    (source, body_start..body_end)
}

fn apply_prefix(body: &str, ranges: &[Range<usize>]) -> String {
    let mut out = String::with_capacity(body.len() + ranges.len() * VARIABLE_PREFIX.len());
    let mut cursor = 0;
    for range in ranges {
        debug_assert!(cursor <= range.start && range.start <= range.end);
        out.push_str(&body[cursor..range.start]);
        out.push_str(VARIABLE_PREFIX);
        out.push_str(&body[range.clone()]);
        cursor = range.end;
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

            fn summarize(Turn turn, int index, int[] values, int seconds, int tx) -> int {
                /** result and values remain unchanged in documentation. */
                int result = helper(turn.cycles, LIMIT);
                Turn snapshot = Turn { cycles: result };
                byte[2] marker = byte[_](0xaabb);
                int grouped = 1_000;
                temporal delay = 5 seconds;
                int left, int right = pair();
                (int total) = sum(left, right);
                Turn { cycles: int copied } = snapshot;
                result = values[index] + snapshot.cycles;
                result = result + seconds + tx + total + copied;
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
        let constants = ["LIMIT".to_string()].into_iter().collect();
        let lowered = lower_global_function(function, &constants, &BTreeSet::new()).expect("variables prefix");

        assert_eq!(lowered.params[0].name, "gen__glob_turn");
        assert_eq!(lowered.params[1].name, "gen__glob_index");
        assert_eq!(lowered.params[2].name, "gen__glob_values");
        assert_eq!(lowered.params[3].name, "gen__glob_seconds");
        assert_eq!(lowered.params[4].name, "gen__glob_tx");
        assert_eq!(
            lowered.body,
            r#"
                /** result and values remain unchanged in documentation. */
                int gen__glob_result = helper(gen__glob_turn.cycles, LIMIT);
                Turn gen__glob_snapshot = Turn { cycles: gen__glob_result };
                byte[2] gen__glob_marker = byte[_](0xaabb);
                int gen__glob_grouped = 1_000;
                temporal gen__glob_delay = 5 seconds;
                int gen__glob_left, int gen__glob_right = pair();
                (int gen__glob_total) = sum(gen__glob_left, gen__glob_right);
                Turn { cycles: int gen__glob_copied } = gen__glob_snapshot;
                gen__glob_result = gen__glob_values[gen__glob_index] + gen__glob_snapshot.cycles;
                gen__glob_result = gen__glob_result + gen__glob_seconds + gen__glob_tx + gen__glob_total + gen__glob_copied;
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

    #[test]
    fn rejects_bindings_that_shadow_shared_constants() {
        let source = r#"
            fn invalid_param(int LIMIT) -> int {
                return LIMIT;
            }

            fn invalid_local() -> int {
                int LIMIT = 2;
                return LIMIT;
            }
        "#;
        let module = parse_module(PathBuf::from("test.ag"), source.to_string()).expect("module parses");
        let constants = ["LIMIT".to_string()].into_iter().collect();

        for function in &module.functions {
            let err =
                lower_global_function(function, &constants, &BTreeSet::new()).err().expect("constant shadowing must be rejected");
            assert!(
                err.to_string().contains(&format!("global function `{}` binding `LIMIT` shadows a shared constant", function.name)),
                "unexpected error: {err}"
            );
        }
    }
}

//! Sil-specific lowering used by the current code generator.
//!
//! Structured body lowering and token-aware source rewrites live here.

mod body;
mod functions;
mod state_boundary;
mod state_values;
mod token_refs;

pub(super) use body::{lower_entry_body, lower_entry_expr};
pub(super) use functions::{GlobalFunctionLowerer, validate_actor_function_captures};
pub(super) use state_boundary::{EntryInputStatePlan, plan_entry_input_states};
pub(super) use state_values::ContractStateValuePlan;

//! Sil-specific lowering used by the current code generator.
//!
//! Structured body lowering and token-aware source rewrites live here.

mod body;
mod functions;
mod state_boundary;
mod state_values;
mod token_refs;

pub(super) use body::{lower_entry_body, lower_entry_expr, reject_function_physical_state_constructors};
pub(super) use functions::{GlobalFunctionLowerer, validate_actor_function_captures};
pub(super) use state_boundary::{
    EntryInputBindingView, EntryInputStatePlan, plan_actor_output_state, plan_entry_input_states, plan_open_output_state,
    plan_selector_output_state,
};
pub(super) use state_values::ContractStateValuePlan;

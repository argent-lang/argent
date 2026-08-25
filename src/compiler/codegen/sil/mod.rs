//! Sil-specific lowering used by the current code generator.
//!
//! Structured body lowering and token-aware source rewrites live here.

mod body;
mod functions;
mod state_boundary;
mod token_refs;

pub(super) use body::{lower_entry_body, lower_entry_expr};
pub(super) use functions::{ContractFunctionPlan, GlobalFunctionLowerer};
pub(super) use state_boundary::{EntryInputStatePlan, plan_entry_input_states};

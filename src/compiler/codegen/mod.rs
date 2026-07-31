//! Generates Sil contracts and portable build products from a validated model.
//!
//! The emitter is the current source-text backend for this subsystem.

mod emitter;
mod sil;

#[cfg(test)]
pub(crate) use emitter::emit_build_app;
pub(crate) use emitter::{emit_build, emit_build_app_linked};

mod emitter;
mod sil;

#[cfg(test)]
pub(crate) use emitter::emit_build_app;
pub(crate) use emitter::{emit_build, emit_build_app_linked};

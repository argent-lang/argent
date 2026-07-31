mod generator;

#[cfg(test)]
pub(crate) use generator::emit_build_app;
pub(crate) use generator::{emit_build, emit_build_app_linked};

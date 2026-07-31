//! Internal compiler subsystems from source syntax through generated products.
//!
//! Each child module owns one stage or shared representation of compilation.

pub(crate) mod codegen;
pub(crate) mod loader;
pub(crate) mod model;
pub(crate) mod naming;
pub(crate) mod syntax;

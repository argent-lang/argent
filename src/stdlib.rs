use std::path::PathBuf;

use crate::ast::Module;
use crate::error::{ArgentError, Result};
use crate::parser::parse_module;

pub const CORE_MODULE: &str = "std::core";

pub fn is_standard_module(path: &str) -> bool {
    path.starts_with("std::")
}

pub fn load_standard_module(path: &str) -> Result<Module> {
    let source = match path {
        CORE_MODULE => include_str!("../std/core.ag"),
        _ => return Err(ArgentError::new(format!("unknown Argent standard module `{path}`"))),
    };
    parse_module(PathBuf::from(path), source.to_string())
}

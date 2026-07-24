use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::ast::{Import, Program};
use crate::error::{ArgentError, Result};
use crate::parser::parse_module;
use crate::stdlib::{is_standard_module, load_standard_module};

pub fn load_program(root: impl AsRef<Path>) -> Result<Program> {
    let root = root.as_ref().to_path_buf();
    let canonical_root = fs::canonicalize(&root).map_err(|err| ArgentError::at(&root, err.to_string()))?;
    let mut loader = Loader::default();
    loader.load_module(&canonical_root)?;
    Ok(Program { root: canonical_root, modules: loader.modules })
}

pub fn load_inline_program(root: PathBuf, source: String) -> Result<Program> {
    let module = parse_module(root.clone(), source)?;
    let imports = module.imports.clone();
    let mut loader = Loader::default();
    loader.modules.push(module);
    loader.load_inline_imports(imports)?;
    Ok(Program { root, modules: loader.modules })
}

#[derive(Default)]
struct Loader {
    visited: BTreeSet<PathBuf>,
    visited_standard: BTreeSet<String>,
    modules: Vec<crate::ast::Module>,
}

impl Loader {
    fn load_module(&mut self, path: &Path) -> Result<()> {
        let canonical = fs::canonicalize(path).map_err(|err| ArgentError::at(path, err.to_string()))?;
        if !self.visited.insert(canonical.clone()) {
            return Ok(());
        }

        let source = fs::read_to_string(&canonical).map_err(|err| ArgentError::at(&canonical, err.to_string()))?;
        let module = parse_module(canonical.clone(), source)?;
        let base = canonical.parent().ok_or_else(|| ArgentError::at(&canonical, "module path has no parent"))?.to_path_buf();
        let imports = module.imports.clone();
        self.modules.push(module);

        for import in imports {
            self.load_import(&base, import)?;
        }

        Ok(())
    }

    fn load_inline_imports(&mut self, imports: Vec<Import>) -> Result<()> {
        for import in imports {
            if let Import::Module { path } = import
                && is_standard_module(&path)
            {
                self.load_standard_module(&path)?;
            }
        }
        Ok(())
    }

    fn load_import(&mut self, base: &Path, import: Import) -> Result<()> {
        match import {
            Import::Module { path } if is_standard_module(&path) => self.load_standard_module(&path),
            Import::Module { path } | Import::Actor { path, .. } => self.load_module(&base.join(path)),
        }
    }

    fn load_standard_module(&mut self, path: &str) -> Result<()> {
        if !self.visited_standard.insert(path.to_string()) {
            return Ok(());
        }
        let module = load_standard_module(path)?;
        let imports = module.imports.clone();
        self.modules.push(module);
        for import in imports {
            match import {
                Import::Module { path } if is_standard_module(&path) => self.load_standard_module(&path)?,
                Import::Module { path } | Import::Actor { path, .. } => {
                    return Err(ArgentError::new(format!("Argent standard module `{path}` cannot import a filesystem module")));
                }
            }
        }
        Ok(())
    }
}

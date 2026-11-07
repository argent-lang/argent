//! Reads published app artifacts for pinned artifact imports.
//!
//! `import "./asset/artifact.json" id "<artifact id>";` makes a published app available to the
//! importing module without its source. The artifact is checked against its pin and then described
//! as declaration-only Argent source: its states, actor enums, exported actors (no entries) and its
//! app. That source goes through the same lexer, parser and name resolution as every other module,
//! so references into the artifact resolve like references into a source import. The bundle builder
//! links the artifact itself and never compiles the app again.

use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use crate::artifact::Artifact;
use crate::compiler::model::link::linked_field_decl;
use crate::compiler::naming::is_identifier;
use crate::compiler::syntax::FieldDecl;
use crate::error::{ArgentError, Result};

/// An import whose path ends in `.json` names a published app artifact.
pub(crate) fn is_artifact_path(path: &Path) -> bool {
    path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
}

/// Reads a published app artifact and checks that it is self-consistent.
///
/// Self-consistency only proves that the artifact id is the hash of its content. Anyone can produce
/// a consistent artifact with the same app name and interfaces but other templates, so the importer
/// must pin the id it reviewed; see [check_pin].
pub(super) fn read_artifact(path: &Path) -> Result<Artifact> {
    let invalid = |err: String| ArgentError::at(path, format!("invalid app artifact: {err}"));
    let json = fs::read_to_string(path).map_err(|err| ArgentError::at(path, err.to_string()))?;
    let artifact: Artifact = serde_json::from_str(&json).map_err(|err| invalid(err.to_string()))?;
    artifact.check_consistency().map_err(|err| invalid(err.to_string()))?;
    Ok(artifact)
}

/// An artifact import must name the artifact id it trusts, and the artifact must have that id.
pub(super) fn check_pin(path: &Path, artifact: &Artifact, pin: Option<&str>) -> Result<()> {
    match pin {
        None => Err(ArgentError::at(
            path,
            format!(
                "artifact import of app `{}` has no pinned id; after reviewing the artifact, write `import \"...\" id \"{}\";`",
                artifact.app, artifact.id
            ),
        )),
        Some(pin) if pin != artifact.id => {
            Err(ArgentError::at(path, format!("artifact of app `{}` has id {}, the import pins {pin}", artifact.app, artifact.id)))
        }
        Some(_) => Ok(()),
    }
}

/// Describes an artifact as declaration-only Argent source, in artifact order.
///
/// The declaration order is the identity `artifact_origins` of the model linker reports, so the importing app and the
/// linked artifact agree on which states and actor enums are shared.
pub(super) fn declaration_source(path: &Path, artifact: &Artifact) -> Result<String> {
    let invalid = |message: String| ArgentError::at(path, format!("invalid app artifact: {message}"));
    // The text below is parsed as Argent source: every name must be a plain (possibly qualified) identifier.
    let name = |name: &str| -> Result<String> {
        if name.is_empty() || !name.split("::").all(is_identifier) {
            return Err(invalid(format!("`{name}` is not a source name")));
        }
        Ok(name.to_string())
    };
    let argent = &artifact.argent;
    let mut source = String::new();

    for state in &argent.states {
        let expansion = argent.state_expansions.iter().find(|item| item.state == state.name);
        match expansion {
            Some(expansion) => {
                writeln!(source, "state {} expands {} {{", name(&state.name)?, name(&expansion.base)?).unwrap();
                for digest in &expansion.digests {
                    writeln!(source, "    {}: {};", name(&digest.field)?, name(&digest.state)?).unwrap();
                }
            }
            None => {
                writeln!(source, "state {} {{", name(&state.name)?).unwrap();
                for field in &state.fields {
                    let FieldDecl { ty, name: field_name, virtual_slot } = linked_field_decl(field)?;
                    let field_name = name(&field_name)?;
                    if virtual_slot {
                        writeln!(source, "    virtual {field_name};").unwrap();
                    } else {
                        writeln!(source, "    {} {field_name};", type_source(&ty, &name)?).unwrap();
                    }
                }
            }
        }
        source.push_str("}\n");
    }

    for item in &argent.actor_enums {
        writeln!(source, "actor enum {} {{", name(&item.name)?).unwrap();
        for variant in &item.variants {
            writeln!(source, "    {};", name(variant)?).unwrap();
        }
        source.push_str("}\n");
    }

    let mut exported = Vec::new();
    for interface in &argent.interfaces.exports {
        if interface.app != artifact.app {
            return Err(invalid(format!("app `{}` exports actor `{}` for app `{}`", artifact.app, interface.actor, interface.app)));
        }
        let actor = argent
            .actors
            .iter()
            .find(|actor| actor.name == interface.actor)
            .ok_or_else(|| invalid(format!("app `{}` does not describe exported actor `{}`", artifact.app, interface.actor)))?;
        writeln!(source, "actor {} owns {} {{}}", name(&actor.name)?, name(&actor.state)?).unwrap();
        exported.push(actor.name.as_str());
    }

    writeln!(source, "app {} {{", name(&artifact.app)?).unwrap();
    for actor in exported {
        writeln!(source, "    actor {actor};").unwrap();
    }
    source.push_str("}\n");
    Ok(source)
}

fn type_source(ty: &crate::compiler::syntax::TypeRef, name: &impl Fn(&str) -> Result<String>) -> Result<String> {
    name(&ty.name)?;
    if let Some(state) = &ty.actor_state {
        name(state)?;
    }
    Ok(ty.to_source())
}

//! Command-line entry point for Argent applications and genesis proofs.
//!
//! CLI commands delegate to the public library operations.

mod cli;

use std::path::PathBuf;

use argent::inspect::{inspect_path, render_report};
use argent::{Result, build_file, build_file_app_bundle};
use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "argentc",
    version,
    about = "Build and inspect Argent applications and genesis proofs",
    arg_required_else_help = true
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Build an Argent application and its generated contracts.
    Build(BuildArgs),
    /// Inspect a build directory or artifact JSON file.
    Inspect(InspectArgs),
    /// Compose or verify a covenant genesis proof.
    #[command(subcommand)]
    Genesis(cli::genesis::GenesisCommand),
}

#[derive(Debug, Args)]
struct BuildArgs {
    /// Argent source file to compile.
    #[arg(value_name = "APP.AG")]
    input: PathBuf,
    /// Select the app to build.
    #[arg(long, value_name = "NAME")]
    app: Option<String>,
    /// Directory for generated build files.
    #[arg(long, value_name = "DIR", default_value = "build/argent")]
    out: PathBuf,
}

#[derive(Debug, Args)]
struct InspectArgs {
    /// Build directory or artifact JSON file to inspect.
    #[arg(value_name = "BUILD-DIR|ARTIFACT.JSON")]
    input: PathBuf,
}

fn main() {
    #[cfg(windows)]
    let _ = colored::control::set_virtual_terminal(true);

    if let Err(err) = run(Cli::parse()) {
        eprintln!("argentc: {err}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Build(args) => build(args),
        Command::Inspect(args) => inspect(args),
        Command::Genesis(command) => cli::genesis::run(command),
    }
}

fn build(args: BuildArgs) -> Result<()> {
    if let Some(app_name) = args.app {
        build_file_app_bundle(&args.input, &app_name, &args.out)?;
    } else {
        build_file(&args.input, &args.out)?;
    }
    println!("wrote {}", args.out.display());
    Ok(())
}

fn inspect(args: InspectArgs) -> Result<()> {
    let report = inspect_path(args.input)?;
    print!("{}", render_report(&report));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{CommandFactory, error::ErrorKind};

    #[test]
    fn cli_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn build_defaults_match_existing_cli() {
        let cli = Cli::try_parse_from(["argentc", "build", "tickets.ag"]).expect("build parses");
        let Command::Build(args) = cli.command else {
            panic!("expected build command");
        };
        assert_eq!(args.input, PathBuf::from("tickets.ag"));
        assert_eq!(args.app, None);
        assert_eq!(args.out, PathBuf::from("build/argent"));
    }

    #[test]
    fn build_accepts_options_before_or_after_input() {
        for argv in [
            vec!["argentc", "build", "tickets.ag", "--app", "Tickets", "--out", "build/tickets"],
            vec!["argentc", "build", "--out", "build/tickets", "--app", "Tickets", "tickets.ag"],
            vec!["argentc", "build", "tickets.ag", "--app=Tickets", "--out=build/tickets"],
        ] {
            let cli = Cli::try_parse_from(argv).expect("build options parse");
            let Command::Build(args) = cli.command else {
                panic!("expected build command");
            };
            assert_eq!(args.input, PathBuf::from("tickets.ag"));
            assert_eq!(args.app.as_deref(), Some("Tickets"));
            assert_eq!(args.out, PathBuf::from("build/tickets"));
        }
    }

    #[test]
    fn inspect_accepts_directory_or_artifact_path() {
        for input in ["build/tickets", "build/tickets/artifact.json"] {
            let cli = Cli::try_parse_from(["argentc", "inspect", input]).expect("inspect parses");
            let Command::Inspect(args) = cli.command else {
                panic!("expected inspect command");
            };
            assert_eq!(args.input, PathBuf::from(input));
        }
    }

    #[test]
    fn cli_rejects_missing_required_arguments() {
        for argv in [
            vec!["argentc", "build"],
            vec!["argentc", "inspect"],
            vec!["argentc", "build", "tickets.ag", "--app"],
            vec!["argentc", "build", "tickets.ag", "--out"],
        ] {
            let err = Cli::try_parse_from(argv).expect_err("missing argument is rejected");
            assert_eq!(err.exit_code(), 2);
        }
    }

    #[test]
    fn cli_rejects_unknown_extra_and_duplicate_arguments() {
        for argv in [
            vec!["argentc", "unknown"],
            vec!["argentc", "build", "tickets.ag", "extra.ag"],
            vec!["argentc", "inspect", "build/tickets", "extra.json"],
            vec!["argentc", "build", "tickets.ag", "--unknown"],
            vec!["argentc", "build", "tickets.ag", "--app", "Tickets", "--app", "Other"],
            vec!["argentc", "build", "tickets.ag", "--out", "first", "--out", "second"],
        ] {
            let err = Cli::try_parse_from(argv).expect_err("invalid argument is rejected");
            assert_eq!(err.exit_code(), 2);
        }
    }

    #[test]
    fn cli_provides_help_and_version() {
        for argv in [vec!["argentc", "--help"], vec!["argentc", "build", "--help"], vec!["argentc", "inspect", "--help"]] {
            let err = Cli::try_parse_from(argv).expect_err("help skips execution");
            assert_eq!(err.kind(), ErrorKind::DisplayHelp);
            assert_eq!(err.exit_code(), 0);
        }
        let version = Cli::try_parse_from(["argentc", "--version"]).expect_err("version skips execution");
        assert_eq!(version.kind(), ErrorKind::DisplayVersion);
        assert!(version.to_string().contains(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn no_arguments_show_generated_help() {
        let err = Cli::try_parse_from(["argentc"]).expect_err("a command is required");
        assert_eq!(err.kind(), ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand);
        let help = err.to_string();
        assert!(help.contains("build"));
        assert!(help.contains("inspect"));
    }
}

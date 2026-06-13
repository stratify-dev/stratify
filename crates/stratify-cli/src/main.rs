mod churn;
mod lsp;
mod mcp;
mod run;

use clap::{Parser, Subcommand, ValueEnum};
use run::Format;
use std::path::PathBuf;
use std::process::ExitCode;
use stratify_core::Severity;

#[derive(Parser)]
#[command(name = "stratify", version, about = "Polyglot codebase intelligence")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Analyze a repository and report findings.
    Check {
        /// Path to the repository root.
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Output format.
        #[arg(long, value_enum, default_value_t = FormatArg::Human)]
        format: FormatArg,
        /// Exit with code 1 if any finding meets or exceeds this severity.
        #[arg(long, value_enum, default_value_t = FailOn::Never)]
        fail_on: FailOn,
    },
    /// Run an MCP server over stdio for coding agents.
    Mcp,
    /// Run a Language Server over stdio for editor diagnostics.
    Lsp,
}

#[derive(Clone, Copy, ValueEnum)]
enum FormatArg {
    Human,
    Json,
    Sarif,
}

impl From<FormatArg> for Format {
    fn from(f: FormatArg) -> Self {
        match f {
            FormatArg::Human => Format::Human,
            FormatArg::Json => Format::Json,
            FormatArg::Sarif => Format::Sarif,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum FailOn {
    Never,
    Info,
    Warning,
    Error,
}

impl FailOn {
    fn threshold(self) -> Option<Severity> {
        match self {
            FailOn::Never => None,
            FailOn::Info => Some(Severity::Info),
            FailOn::Warning => Some(Severity::Warning),
            FailOn::Error => Some(Severity::Error),
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Check {
            path,
            format,
            fail_on,
        } => {
            let report = match run::analyze_repo(&path) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("stratify: {e}");
                    return ExitCode::FAILURE;
                }
            };

            let rendered = match Format::from(format) {
                Format::Human => stratify_report::human::render(&report),
                Format::Json => stratify_report::json::render(&report),
                Format::Sarif => stratify_report::sarif::render(&report),
            };
            print!("{rendered}");

            if let Some(threshold) = fail_on.threshold() {
                if run::gate(&report, threshold) {
                    return ExitCode::FAILURE;
                }
            }

            ExitCode::SUCCESS
        }
        Command::Mcp => match mcp::run_stdio() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("stratify: {e}");
                ExitCode::FAILURE
            }
        },
        Command::Lsp => match lsp::run_stdio() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("stratify: {e}");
                ExitCode::FAILURE
            }
        },
    }
}

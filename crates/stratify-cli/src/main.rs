mod run;

use std::path::PathBuf;
use std::process::ExitCode;
use clap::{Parser, Subcommand, ValueEnum};
use run::Format;

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
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum FormatArg {
    Human,
    Json,
}

impl From<FormatArg> for Format {
    fn from(f: FormatArg) -> Self {
        match f {
            FormatArg::Human => Format::Human,
            FormatArg::Json => Format::Json,
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Check { path, format } => match run::run(&path, format.into()) {
            Ok(out) => {
                print!("{out}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("stratify: {e}");
                ExitCode::FAILURE
            }
        },
    }
}

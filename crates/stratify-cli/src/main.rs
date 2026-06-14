mod churn;
mod gitmeta;
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
        /// OTLP endpoint base URL. Overrides OTEL_EXPORTER_OTLP_ENDPOINT.
        #[arg(long)]
        otlp_endpoint: Option<String>,
        /// Project name (service.name). Overrides OTEL_SERVICE_NAME.
        #[arg(long)]
        project: Option<String>,
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
            otlp_endpoint,
            project,
        } => {
            let (report, stats) = match run::analyze_repo_with_stats(&path) {
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

            maybe_emit_telemetry(&path, &report, &stats, otlp_endpoint, project);

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

fn maybe_emit_telemetry(
    path: &std::path::Path,
    report: &stratify_core::Report,
    stats: &run::ScanStats,
    otlp_endpoint: Option<String>,
    project: Option<String>,
) {
    let endpoint = otlp_endpoint
        .or_else(|| std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").ok())
        .filter(|s| !s.trim().is_empty());
    let Some(endpoint) = endpoint else {
        return; // no endpoint configured -> silent no-op
    };

    let git = gitmeta::git_meta(path);
    let (namespace, git_basename) = match git.remote_url.as_deref() {
        Some(url) => gitmeta::parse_remote_url(url),
        None => (None, None),
    };
    let env_service = std::env::var("OTEL_SERVICE_NAME").ok();
    let dir_name = path
        .canonicalize()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
        .unwrap_or_else(|| "stratify".to_string());
    let service_name = stratify_telemetry::resolve_service_name(
        project.as_deref(),
        env_service.as_deref(),
        git_basename.as_deref(),
        &dir_name,
    );
    let headers = std::env::var("OTEL_EXPORTER_OTLP_HEADERS")
        .ok()
        .map(|h| stratify_telemetry::parse_headers(&h))
        .unwrap_or_default();

    let metrics = stratify_telemetry::report_to_metrics(
        report,
        stats.files_scanned,
        stats.functions,
        stats.complexity_max,
        stats.complexity_mean,
        stats.duration_ms,
    );
    let event = stratify_telemetry::report_to_event(
        report,
        &git,
        &service_name,
        stats.duration_ms,
        &stats.languages,
    );
    let config = stratify_telemetry::TelemetryConfig {
        endpoint,
        headers,
        service_name,
        namespace,
        version: env!("CARGO_PKG_VERSION").to_string(),
    };
    if let Err(e) = stratify_telemetry::emit(&metrics, &event, &config) {
        eprintln!("warning: telemetry export failed: {e}");
    }
}

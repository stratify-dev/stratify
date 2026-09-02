use ignore::WalkBuilder;
use std::collections::BTreeSet;
use std::path::Path;
use stratify_analysis::deadcode;
use stratify_analysis::duplication;
use stratify_core::{IrGraph, Report, Severity, SymbolKind};
use stratify_lang::LanguageAdapter;
use stratify_lang_java::JavaAdapter;

/// Default minimum identical normalized-token run length to count as a clone.
/// Tunable per repo via `[duplication] min_tokens` in stratify.toml. Set high
/// enough that short structurally-parallel fragments (the language adapters
/// share shape) stop firing, while real clones remain. PMD CPD defaults to a
/// similar 75-100 token range.
const DUP_MIN_TOKENS_DEFAULT: usize = 100;

/// Cyclomatic complexity above this is reported.
const COMPLEXITY_THRESHOLD: u32 = 10;

/// complexity x churn above this is reported as a hotspot.
const HOTSPOT_THRESHOLD: u32 = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Human,
    Json,
    Sarif,
}

/// Repo-wide aggregate metrics computed from the IR, for telemetry.
#[derive(Debug, Clone)]
pub struct ScanStats {
    pub files_scanned: u64,
    pub functions: u64,
    pub complexity_max: u32,
    pub complexity_mean: f64,
    pub languages: BTreeSet<String>,
    pub duration_ms: u64,
}

/// Map a file path to its Stratify language name by extension.
fn lang_of_file(path: &str) -> Option<&'static str> {
    let ext = path.rsplit('.').next()?;
    match ext {
        "java" => Some("java"),
        "rb" => Some("ruby"),
        "ts" | "tsx" | "mts" | "cts" => Some("typescript"),
        "py" | "pyi" => Some("python"),
        "go" => Some("go"),
        "rs" => Some("rust"),
        _ => None,
    }
}

/// Compute repo-wide stats from the merged IR plus the measured wall time.
pub fn scan_stats(graph: &IrGraph, duration_ms: u64) -> ScanStats {
    let mut files_scanned = 0u64;
    let mut functions = 0u64;
    let mut languages = BTreeSet::new();
    for s in graph.symbols() {
        match s.kind {
            SymbolKind::File => {
                files_scanned += 1;
                if let Some(lang) = lang_of_file(&s.span.file) {
                    languages.insert(lang.to_string());
                }
            }
            SymbolKind::Function => functions += 1,
            _ => {}
        }
    }
    let complexities: Vec<u32> = graph.complexities().iter().map(|(_, c)| *c).collect();
    let complexity_max = complexities.iter().copied().max().unwrap_or(0);
    let complexity_mean = if complexities.is_empty() {
        0.0
    } else {
        complexities.iter().map(|c| *c as f64).sum::<f64>() / complexities.len() as f64
    };
    ScanStats {
        files_scanned,
        functions,
        complexity_max,
        complexity_mean,
        languages,
        duration_ms,
    }
}

/// Walk `root`, parse every file a registered adapter handles, merge into one
/// IrGraph, run dead-code, and return the Report along with ScanStats.
pub fn analyze_repo_with_stats(root: &Path) -> std::io::Result<(Report, ScanStats)> {
    let start = std::time::Instant::now();
    if !root.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("path not found: {}", root.display()),
        ));
    }

    let adapters: Vec<Box<dyn LanguageAdapter>> = vec![
        Box::new(JavaAdapter),
        Box::new(stratify_lang_ruby::RubyAdapter),
        Box::new(stratify_lang_ts::TsAdapter),
        Box::new(stratify_lang_py::PyAdapter),
        Box::new(stratify_lang_go::GoAdapter),
        Box::new(stratify_lang_rust::RustAdapter),
    ];

    let ignore_globs = load_ignore_globs(root);
    let mut graph = IrGraph::new();
    for entry in WalkBuilder::new(root).build() {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let path = entry.path();
        let ext = match path.extension().and_then(|e| e.to_str()) {
            Some(e) => e,
            None => continue,
        };
        let adapter = match adapters.iter().find(|a| a.handles_extension(ext)) {
            Some(a) => a,
            None => continue,
        };
        let source = std::fs::read_to_string(path)?;
        let rel = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();
        if ignore_globs.is_match(&rel) {
            continue;
        }
        if let Ok(file_graph) = adapter.parse_file(&rel, &source) {
            graph.merge(file_graph);
        }
    }

    stratify_analysis::resolve::cross_file_calls(&mut graph);
    stratify_analysis::resolve::go_imports(&mut graph);
    stratify_analysis::resolve::promote_intra_file_calls(&mut graph);

    let mut findings = deadcode::analyze(&graph, load_dead_code_mode(root));
    findings.extend(duplication::analyze(&graph, load_dup_min_tokens(root)));
    findings.extend(stratify_analysis::complexity::analyze(
        &graph,
        COMPLEXITY_THRESHOLD,
    ));
    let churn = crate::churn::git_churn(root);
    findings.extend(stratify_analysis::hotspot::analyze(
        &graph,
        &churn,
        HOTSPOT_THRESHOLD,
        COMPLEXITY_THRESHOLD,
    ));
    findings.extend(stratify_analysis::cycles::analyze(&graph));
    let boundary_config = load_boundary_config(root);
    findings.extend(stratify_analysis::boundaries::analyze(
        &graph,
        &boundary_config,
    ));
    let stats = scan_stats(&graph, start.elapsed().as_millis() as u64);
    Ok((Report::new(findings), stats))
}

/// Convenience wrapper for callers that only need the Report (mcp, lsp).
pub fn analyze_repo(root: &Path) -> std::io::Result<Report> {
    Ok(analyze_repo_with_stats(root)?.0)
}

/// Load `[ignore] paths` globs from `stratify.toml` at the scan root.
fn load_ignore_globs(root: &Path) -> globset::GlobSet {
    match std::fs::read_to_string(root.join("stratify.toml")) {
        Ok(text) => {
            let cfg: stratify_analysis::ignore::IgnoreToml =
                toml::from_str(&text).unwrap_or_default();
            stratify_analysis::ignore::ignore_globset(&cfg.ignore.paths)
        }
        Err(_) => globset::GlobSet::empty(),
    }
}

/// Resolve `[dead_code] mode` from stratify.toml, defaulting to
/// `DeadCodeMode::Library` (the historical behavior) when absent or unreadable.
fn load_dead_code_mode(root: &Path) -> deadcode::DeadCodeMode {
    let explicit = match std::fs::read_to_string(root.join("stratify.toml")) {
        Ok(text) => {
            let cfg: deadcode::DeadCodeToml = toml::from_str(&text).unwrap_or_default();
            cfg.dead_code.mode
        }
        Err(_) => None,
    };
    explicit.unwrap_or_else(|| autodetect_dead_code_mode(root))
}

/// With no explicit `[dead_code] mode`, guess Application from a project-
/// layout signal that specifically means "not meant to be imported by code
/// outside this repo" - never guess Application from an absence of evidence,
/// only from a real, standard opt-out-of-distribution marker. Defaults to
/// Library (today's historical behavior) when no such marker exists, which
/// is deliberately the common case: Go, Java, and Python have no equally
/// explicit, single-file "this isn't a library" signal yet, so they always
/// fall through to Library here rather than a guess that could be wrong.
fn autodetect_dead_code_mode(root: &Path) -> deadcode::DeadCodeMode {
    use deadcode::DeadCodeMode;

    // Rust: `publish = false` is Cargo's own standard opt-out of
    // distribution. A crate with no `src/lib.rs` has nothing importable
    // either way, regardless of `publish`. Both only apply to a real
    // `[package]` manifest - a bare `[workspace]` root (this repo's own
    // Cargo.toml is shaped exactly like this) has no single crate's
    // publish/bin status to read, so it must not trigger this at all.
    if let Ok(text) = std::fs::read_to_string(root.join("Cargo.toml")) {
        if let Ok(doc) = text.parse::<toml::Value>() {
            if let Some(package) = doc.get("package") {
                let publish_false =
                    matches!(package.get("publish"), Some(toml::Value::Boolean(false)));
                let bin_only =
                    !root.join("src/lib.rs").is_file() && root.join("src/main.rs").is_file();
                if publish_false || bin_only {
                    return DeadCodeMode::Application;
                }
            }
        }
    }

    // Node/TypeScript: `"private": true` is npm's own standard opt-out of
    // `npm publish`.
    if let Ok(text) = std::fs::read_to_string(root.join("package.json")) {
        if let Ok(doc) = serde_json::from_str::<serde_json::Value>(&text) {
            if doc.get("private") == Some(&serde_json::Value::Bool(true)) {
                return DeadCodeMode::Application;
            }
        }
    }

    // Ruby: reuse the same Rails markers the boundary preset already
    // auto-detects with (see `autodetect_preset` below). A Rails app is a
    // deployed service, never a published gem, so the same signal that
    // picks the `rails` boundary preset is trustworthy here too.
    if root.join("app/controllers").is_dir() || root.join("config/routes.rb").is_file() {
        return DeadCodeMode::Application;
    }

    DeadCodeMode::Library
}

/// Resolve the duplication `min_tokens` from `[duplication]` in stratify.toml,
/// falling back to `DUP_MIN_TOKENS_DEFAULT` when absent or unreadable.
fn load_dup_min_tokens(root: &Path) -> usize {
    match std::fs::read_to_string(root.join("stratify.toml")) {
        Ok(text) => {
            let cfg: stratify_analysis::duplication::DuplicationToml =
                toml::from_str(&text).unwrap_or_default();
            cfg.duplication.min_tokens.unwrap_or(DUP_MIN_TOKENS_DEFAULT)
        }
        Err(_) => DUP_MIN_TOKENS_DEFAULT,
    }
}

fn load_boundary_config(root: &Path) -> stratify_analysis::boundaries::BoundaryConfig {
    use stratify_analysis::boundaries::resolve;
    let path = root.join("stratify.toml");
    match std::fs::read_to_string(&path) {
        Ok(text) => resolve(toml::from_str(&text).unwrap_or_default()),
        Err(_) => resolve(autodetect_preset(root)),
    }
}

/// With no stratify.toml, guess a preset from the project layout. Returns an
/// empty config (no boundary checks) when nothing matches.
fn autodetect_preset(root: &Path) -> stratify_analysis::boundaries::BoundaryConfig {
    use stratify_analysis::boundaries::BoundaryConfig;
    let preset = if root.join("app/controllers").is_dir() || root.join("config/routes.rb").is_file()
    {
        Some("rails".to_string())
    } else if root.join("pom.xml").is_file() || root.join("build.gradle").is_file() {
        Some("layered".to_string())
    } else {
        None
    };
    BoundaryConfig {
        preset,
        ..Default::default()
    }
}

/// Returns true if any finding in `report` has severity >= `threshold`.
pub fn gate(report: &Report, threshold: Severity) -> bool {
    report.findings.iter().any(|f| f.severity >= threshold)
}

#[cfg(test)]
mod tests {
    use super::*;
    use stratify_core::confidence::Confidence;
    use stratify_core::ir::Span;
    use stratify_core::{Finding, Severity};

    #[test]
    fn run_flags_unused_method_in_temp_repo() {
        let dir = std::env::temp_dir().join("stratify-cli-test-1");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("App.java"),
            "class App {\n  public static void main(String[] a) {}\n  void unusedHelper() {}\n}\n",
        )
        .unwrap();

        let report = analyze_repo(&dir).unwrap();

        let out = stratify_report::human::render(&report);
        assert!(out.contains("unusedHelper"), "got: {out}");

        let json = stratify_report::json::render(&report);
        assert!(json.contains("\"schema_version\": 1"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn make_report(severity: Severity) -> Report {
        Report::new(vec![Finding {
            rule: "dead_code".into(),
            severity,
            message: "unused".into(),
            span: Span {
                file: "A.java".into(),
                start_byte: 0,
                end_byte: 1,
                start_line: 1,
            },
            confidence: Confidence::Certain,
        }])
    }

    #[test]
    fn gate_trips_at_threshold_and_below() {
        let report = make_report(Severity::Warning);
        assert!(
            gate(&report, Severity::Warning),
            "warning report should trip at Warning threshold"
        );
        assert!(
            gate(&report, Severity::Info),
            "warning report should trip at Info threshold"
        );
        assert!(
            !gate(&report, Severity::Error),
            "warning report should NOT trip at Error threshold"
        );
    }

    #[test]
    fn gate_empty_report_never_trips() {
        let report = Report::new(vec![]);
        assert!(!gate(&report, Severity::Info));
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("stratify-autodetect-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn cargo_publish_false_detects_application_mode() {
        let dir = temp_dir("cargo-publish-false");
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"x\"\nversion = \"0.1.0\"\npublish = false\n",
        )
        .unwrap();
        assert_eq!(
            autodetect_dead_code_mode(&dir),
            deadcode::DeadCodeMode::Application
        );
    }

    #[test]
    fn cargo_bin_only_no_lib_detects_application_mode() {
        let dir = temp_dir("cargo-bin-only");
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"x\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();
        assert_eq!(
            autodetect_dead_code_mode(&dir),
            deadcode::DeadCodeMode::Application
        );
    }

    #[test]
    fn cargo_lib_crate_stays_library_mode() {
        let dir = temp_dir("cargo-lib-crate");
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"x\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/lib.rs"), "pub fn f() {}\n").unwrap();
        assert_eq!(
            autodetect_dead_code_mode(&dir),
            deadcode::DeadCodeMode::Library
        );
    }

    #[test]
    fn cargo_workspace_root_with_no_package_table_stays_library_mode() {
        // A pure `[workspace]` manifest (this repo's own root Cargo.toml is
        // shaped exactly like this) has no single crate's publish/bin status
        // to read - must not guess Application from that.
        let dir = temp_dir("cargo-workspace-root");
        std::fs::write(
            dir.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\n",
        )
        .unwrap();
        assert_eq!(
            autodetect_dead_code_mode(&dir),
            deadcode::DeadCodeMode::Library
        );
    }

    #[test]
    fn package_json_private_true_detects_application_mode() {
        let dir = temp_dir("package-json-private");
        std::fs::write(dir.join("package.json"), "{\"private\": true}\n").unwrap();
        assert_eq!(
            autodetect_dead_code_mode(&dir),
            deadcode::DeadCodeMode::Application
        );
    }

    #[test]
    fn package_json_without_private_stays_library_mode() {
        let dir = temp_dir("package-json-public");
        std::fs::write(
            dir.join("package.json"),
            "{\"name\": \"x\", \"version\": \"1.0.0\"}\n",
        )
        .unwrap();
        assert_eq!(
            autodetect_dead_code_mode(&dir),
            deadcode::DeadCodeMode::Library
        );
    }

    #[test]
    fn rails_markers_detect_application_mode() {
        let dir = temp_dir("rails-markers");
        std::fs::create_dir_all(dir.join("app/controllers")).unwrap();
        assert_eq!(
            autodetect_dead_code_mode(&dir),
            deadcode::DeadCodeMode::Application
        );
    }

    #[test]
    fn no_markers_defaults_to_library_mode() {
        let dir = temp_dir("no-markers");
        assert_eq!(
            autodetect_dead_code_mode(&dir),
            deadcode::DeadCodeMode::Library
        );
    }
}

#[cfg(test)]
mod stats_tests {
    use super::*;
    use stratify_core::ir::{Span, Symbol, SymbolId, Visibility};
    use stratify_core::{Confidence, IrGraph, SymbolKind};

    fn sym(g: &mut IrGraph, kind: SymbolKind, file: &str) -> SymbolId {
        g.add_symbol(Symbol {
            id: SymbolId(0),
            kind,
            name: file.into(),
            fqn: file.into(),
            span: Span {
                file: file.into(),
                start_byte: 0,
                end_byte: 1,
                start_line: 1,
            },
            visibility: Visibility::Unknown,
            confidence: Confidence::Certain,
        })
    }

    #[test]
    fn scan_stats_counts_and_aggregates() {
        let mut g = IrGraph::new();
        sym(&mut g, SymbolKind::File, "a.rb");
        sym(&mut g, SymbolKind::File, "b.go");
        let f1 = sym(&mut g, SymbolKind::Function, "a.rb");
        let f2 = sym(&mut g, SymbolKind::Function, "b.go");
        g.set_complexity(f1, 4);
        g.set_complexity(f2, 10);

        let stats = scan_stats(&g, 123);
        assert_eq!(stats.files_scanned, 2);
        assert_eq!(stats.functions, 2);
        assert_eq!(stats.complexity_max, 10);
        assert_eq!(stats.complexity_mean, 7.0);
        assert_eq!(stats.duration_ms, 123);
        assert!(stats.languages.contains("ruby"));
        assert!(stats.languages.contains("go"));
    }

    #[test]
    fn scan_stats_empty_graph_is_zero() {
        let g = IrGraph::new();
        let stats = scan_stats(&g, 0);
        assert_eq!(stats.files_scanned, 0);
        assert_eq!(stats.functions, 0);
        assert_eq!(stats.complexity_max, 0);
        assert_eq!(stats.complexity_mean, 0.0);
        assert!(stats.languages.is_empty());
    }
}

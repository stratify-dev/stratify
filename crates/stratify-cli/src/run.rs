use std::path::Path;
use ignore::WalkBuilder;
use stratify_analysis::deadcode;
use stratify_core::{IrGraph, Report, Severity};
use stratify_lang::LanguageAdapter;
use stratify_lang_java::JavaAdapter;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Human,
    Json,
}

/// Walk `root`, parse every file a registered adapter handles, merge into one
/// IrGraph, run dead-code, and return the Report.
pub fn analyze_repo(root: &Path) -> std::io::Result<Report> {
    let adapters: Vec<Box<dyn LanguageAdapter>> = vec![Box::new(JavaAdapter)];

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
        let rel = path.strip_prefix(root).unwrap_or(path).to_string_lossy().to_string();
        if let Ok(file_graph) = adapter.parse_file(&rel, &source) {
            graph.merge(file_graph);
        }
    }

    let findings = deadcode::analyze(&graph);
    Ok(Report::new(findings))
}

/// Returns true if any finding in `report` has severity >= `threshold`.
pub fn gate(report: &Report, threshold: Severity) -> bool {
    report.findings.iter().any(|f| f.severity >= threshold)
}

#[cfg(test)]
mod tests {
    use super::*;
    use stratify_core::{Finding, Severity};
    use stratify_core::confidence::Confidence;
    use stratify_core::ir::Span;

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
            span: Span { file: "A.java".into(), start_byte: 0, end_byte: 1, start_line: 1 },
            confidence: Confidence::Certain,
        }])
    }

    #[test]
    fn gate_trips_at_threshold_and_below() {
        let report = make_report(Severity::Warning);
        assert!(gate(&report, Severity::Warning), "warning report should trip at Warning threshold");
        assert!(gate(&report, Severity::Info), "warning report should trip at Info threshold");
        assert!(!gate(&report, Severity::Error), "warning report should NOT trip at Error threshold");
    }

    #[test]
    fn gate_empty_report_never_trips() {
        let report = Report::new(vec![]);
        assert!(!gate(&report, Severity::Info));
    }
}

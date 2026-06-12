use std::path::Path;
use ignore::WalkBuilder;
use stratify_analysis::deadcode;
use stratify_core::{IrGraph, Report};
use stratify_lang::LanguageAdapter;
use stratify_lang_java::JavaAdapter;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Human,
    Json,
}

/// Walk `root`, parse every file a registered adapter handles, merge into one
/// IrGraph, run dead-code, and return the rendered string.
pub fn run(root: &Path, format: Format) -> std::io::Result<String> {
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
    let report = Report::new(findings);

    Ok(match format {
        Format::Human => stratify_report::human::render(&report),
        Format::Json => stratify_report::json::render(&report),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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

        let out = run(&dir, Format::Human).unwrap();
        assert!(out.contains("unusedHelper"), "got: {out}");

        let json = run(&dir, Format::Json).unwrap();
        assert!(json.contains("\"schema_version\": 1"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

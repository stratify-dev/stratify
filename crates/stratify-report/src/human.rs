use stratify_core::{Report, Severity};

/// Render a report as human-readable lines: `severity file:line  message`.
pub fn render(report: &Report) -> String {
    if report.findings.is_empty() {
        return "No findings.\n".to_string();
    }
    let mut out = String::new();
    for f in &report.findings {
        let sev = match f.severity {
            Severity::Error => "error",
            Severity::Warning => "warn",
            Severity::Info => "info",
        };
        out.push_str(&format!(
            "{sev:<5} {}:{}  {}\n",
            f.span.file, f.span.start_line, f.message
        ));
    }
    out.push_str(&format!("\n{} finding(s).\n", report.findings.len()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::sample_finding;

    #[test]
    fn empty_report_says_no_findings() {
        let r = Report::new(vec![]);
        assert_eq!(render(&r), "No findings.\n");
    }

    #[test]
    fn formats_a_finding_line() {
        let r = Report::new(vec![sample_finding()]);
        let out = render(&r);
        assert!(out.contains("warn  T.java:5  unused function `orphan`"));
        assert!(out.contains("1 finding(s)."));
    }
}

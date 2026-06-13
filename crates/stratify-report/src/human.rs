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
    use stratify_core::ir::Span;
    use stratify_core::{Confidence, Finding, Severity};

    #[test]
    fn empty_report_says_no_findings() {
        let r = Report::new(vec![]);
        assert_eq!(render(&r), "No findings.\n");
    }

    #[test]
    fn formats_a_finding_line() {
        let r = Report::new(vec![Finding {
            rule: "dead_code".into(),
            severity: Severity::Warning,
            message: "unused function `orphan`".into(),
            span: Span {
                file: "T.java".into(),
                start_byte: 0,
                end_byte: 1,
                start_line: 5,
            },
            confidence: Confidence::Certain,
        }]);
        let out = render(&r);
        assert!(out.contains("warn  T.java:5  unused function `orphan`"));
        assert!(out.contains("1 finding(s)."));
    }
}

use stratify_core::Report;

/// Render a report as pretty JSON. This is the machine contract.
pub fn render(report: &Report) -> String {
    serde_json::to_string_pretty(report).expect("report serializes")
}

#[cfg(test)]
mod tests {
    use super::*;
    use stratify_core::ir::Span;
    use stratify_core::{Confidence, Finding, Severity};

    #[test]
    fn renders_schema_version_and_finding() {
        let report = Report::new(vec![Finding {
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
        let out = render(&report);
        assert!(out.contains("\"schema_version\": 1"));
        assert!(out.contains("\"rule\": \"dead_code\""));
    }
}

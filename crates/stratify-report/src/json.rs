use stratify_core::Report;

/// Render a report as pretty JSON. This is the machine contract.
pub fn render(report: &Report) -> String {
    serde_json::to_string_pretty(report).expect("report serializes")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::sample_finding;

    #[test]
    fn renders_schema_version_and_finding() {
        let report = Report::new(vec![sample_finding()]);
        let out = render(&report);
        assert!(out.contains("\"schema_version\": 1"));
        assert!(out.contains("\"rule\": \"dead_code\""));
    }
}

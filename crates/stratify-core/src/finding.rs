use crate::confidence::Confidence;
use crate::ir::Span;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warning,
    Error,
}

/// One reported problem. `rule` identifies the analysis, e.g. "dead_code".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub rule: String,
    pub severity: Severity,
    pub message: String,
    pub span: Span,
    pub confidence: Confidence,
}

/// The top-level machine output. Versioned so downstream consumers can rely on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub schema_version: u32,
    pub findings: Vec<Finding>,
}

impl Report {
    pub const SCHEMA_VERSION: u32 = 1;

    pub fn new(findings: Vec<Finding>) -> Self {
        Report {
            schema_version: Self::SCHEMA_VERSION,
            findings,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::Span;

    #[test]
    fn severity_ordering_info_lt_warning_lt_error() {
        assert!(Severity::Info < Severity::Warning);
        assert!(Severity::Warning < Severity::Error);
    }

    #[test]
    fn report_serializes_with_schema_version() {
        let report = Report::new(vec![Finding {
            rule: "dead_code".into(),
            severity: Severity::Warning,
            message: "unused method bar".into(),
            span: Span {
                file: "Foo.java".into(),
                start_byte: 0,
                end_byte: 1,
                start_line: 3,
            },
            confidence: Confidence::Certain,
        }]);
        let v: serde_json::Value = serde_json::to_value(&report).unwrap();
        assert_eq!(v["schema_version"], 1);
        assert_eq!(v["findings"][0]["rule"], "dead_code");
    }
}

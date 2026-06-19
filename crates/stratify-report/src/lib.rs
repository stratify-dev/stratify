pub mod human;
pub mod json;
pub mod sarif;

#[cfg(test)]
pub(crate) mod test_support {
    use stratify_core::ir::Span;
    use stratify_core::{Confidence, Finding, Severity};

    /// A single warning finding shared by the renderer tests.
    pub(crate) fn sample_finding() -> Finding {
        Finding {
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
        }
    }
}

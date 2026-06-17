use stratify_core::IrGraph;

pub mod walk;

#[derive(Debug)]
pub enum AdapterError {
    Parse(String),
}

impl std::fmt::Display for AdapterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AdapterError::Parse(m) => write!(f, "parse error: {m}"),
        }
    }
}

impl std::error::Error for AdapterError {}

/// Turns source files of one language into IR. The only language-aware code
/// in the system. Analyses never see this; they read the merged IrGraph.
pub trait LanguageAdapter {
    /// Lowercase language id, e.g. "java".
    fn language(&self) -> &'static str;

    /// True if this adapter handles the given file extension (no dot), e.g. "java".
    fn handles_extension(&self, ext: &str) -> bool;

    /// Parse one file's `source` (already read from `path`) into a per-file IrGraph.
    fn parse_file(&self, path: &str, source: &str) -> Result<IrGraph, AdapterError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Noop;
    impl LanguageAdapter for Noop {
        fn language(&self) -> &'static str {
            "noop"
        }
        fn handles_extension(&self, ext: &str) -> bool {
            ext == "noop"
        }
        fn parse_file(&self, _path: &str, _source: &str) -> Result<IrGraph, AdapterError> {
            Ok(IrGraph::new())
        }
    }

    #[test]
    fn adapter_contract_holds() {
        let a = Noop;
        assert_eq!(a.language(), "noop");
        assert!(a.handles_extension("noop"));
        assert!(!a.handles_extension("java"));
        assert_eq!(a.parse_file("x.noop", "").unwrap().symbols().len(), 0);
    }
}

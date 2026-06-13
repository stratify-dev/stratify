mod extract;

use stratify_core::IrGraph;
use stratify_lang::{AdapterError, LanguageAdapter};

pub struct TsAdapter;

impl LanguageAdapter for TsAdapter {
    fn language(&self) -> &'static str {
        "typescript"
    }

    fn handles_extension(&self, ext: &str) -> bool {
        matches!(ext, "ts" | "tsx" | "mts" | "cts")
    }

    fn parse_file(&self, path: &str, source: &str) -> Result<IrGraph, AdapterError> {
        Ok(extract::extract(path, source))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_parses_a_function() {
        let a = TsAdapter;
        assert!(a.handles_extension("ts"));
        assert!(a.handles_extension("tsx"));
        let g = a.parse_file("a.ts", "function hi() {}").unwrap();
        assert!(g.symbols().iter().any(|s| s.name == "hi"));
    }
}

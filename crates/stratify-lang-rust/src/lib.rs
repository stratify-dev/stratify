mod extract;

use stratify_core::IrGraph;
use stratify_lang::{AdapterError, LanguageAdapter};

pub struct RustAdapter;

impl LanguageAdapter for RustAdapter {
    fn language(&self) -> &'static str {
        "rust"
    }

    fn handles_extension(&self, ext: &str) -> bool {
        ext == "rs"
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
        let a = RustAdapter;
        assert!(a.handles_extension("rs"));
        let g = a.parse_file("a.rs", "fn hi() {}\n").unwrap();
        assert!(g.symbols().iter().any(|s| s.name == "hi"));
    }
}

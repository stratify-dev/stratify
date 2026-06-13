mod extract;

use stratify_core::IrGraph;
use stratify_lang::{AdapterError, LanguageAdapter};

pub struct GoAdapter;

impl LanguageAdapter for GoAdapter {
    fn language(&self) -> &'static str {
        "go"
    }

    fn handles_extension(&self, ext: &str) -> bool {
        ext == "go"
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
        let a = GoAdapter;
        assert!(a.handles_extension("go"));
        let g = a.parse_file("a.go", "package main\nfunc hi() {}\n").unwrap();
        assert!(g.symbols().iter().any(|s| s.name == "hi"));
    }
}

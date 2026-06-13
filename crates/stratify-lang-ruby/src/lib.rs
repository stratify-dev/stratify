mod extract;

use stratify_core::IrGraph;
use stratify_lang::{AdapterError, LanguageAdapter};

pub struct RubyAdapter;

impl LanguageAdapter for RubyAdapter {
    fn language(&self) -> &'static str {
        "ruby"
    }

    fn handles_extension(&self, ext: &str) -> bool {
        ext == "rb"
    }

    fn parse_file(&self, path: &str, source: &str) -> Result<IrGraph, AdapterError> {
        Ok(extract::extract(path, source))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_parses_a_method() {
        let a = RubyAdapter;
        assert!(a.handles_extension("rb"));
        let g = a.parse_file("a.rb", "def hi\nend\n").unwrap();
        assert!(g.symbols().iter().any(|s| s.name == "hi"));
    }
}

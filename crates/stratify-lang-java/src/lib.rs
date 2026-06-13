mod extract;

use stratify_core::IrGraph;
use stratify_lang::{AdapterError, LanguageAdapter};

pub struct JavaAdapter;

impl LanguageAdapter for JavaAdapter {
    fn language(&self) -> &'static str {
        "java"
    }

    fn handles_extension(&self, ext: &str) -> bool {
        ext == "java"
    }

    fn parse_file(&self, path: &str, source: &str) -> Result<IrGraph, AdapterError> {
        Ok(extract::extract(path, source))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_parses_a_class() {
        let a = JavaAdapter;
        assert!(a.handles_extension("java"));
        let g = a
            .parse_file("Foo.java", "class Foo { void bar() {} }")
            .unwrap();
        assert!(g.symbols().iter().any(|s| s.name == "bar"));
    }
}

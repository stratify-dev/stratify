mod extract;

use stratify_core::IrGraph;
use stratify_lang::{AdapterError, LanguageAdapter};

pub struct PyAdapter;

impl LanguageAdapter for PyAdapter {
    fn language(&self) -> &'static str {
        "python"
    }

    fn handles_extension(&self, ext: &str) -> bool {
        matches!(ext, "py" | "pyi")
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
        let a = PyAdapter;
        assert!(a.handles_extension("py"));
        let g = a.parse_file("a.py", "def hi():\n    pass\n").unwrap();
        assert!(g.symbols().iter().any(|s| s.name == "hi"));
    }
}

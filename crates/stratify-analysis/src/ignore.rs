use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use serde::Deserialize;

/// The `[ignore]` table of `stratify.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct IgnoreSection {
    /// Glob patterns (relative to the scan root) of files to skip.
    #[serde(default)]
    pub paths: Vec<String>,
}

/// Wrapper to deserialize just the `[ignore]` table from `stratify.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct IgnoreToml {
    #[serde(default)]
    pub ignore: IgnoreSection,
}

/// Compile ignore globs into a GlobSet. `*` does not cross `/`, `**` does.
/// Bad patterns are skipped. An empty pattern list yields a set that matches
/// nothing.
pub fn ignore_globset(paths: &[String]) -> GlobSet {
    let mut b = GlobSetBuilder::new();
    for p in paths {
        if let Ok(g) = GlobBuilder::new(p).literal_separator(true).build() {
            b.add(g);
        }
    }
    b.build().unwrap_or_else(|_| GlobSet::empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_nested_globs() {
        let set = ignore_globset(&["tests/**".to_string(), "**/vendor/**".to_string()]);
        assert!(set.is_match("tests/sample/a.rb"));
        assert!(set.is_match("crates/x/vendor/y.go"));
        assert!(!set.is_match("src/main.rs"));
    }

    #[test]
    fn empty_matches_nothing() {
        let set = ignore_globset(&[]);
        assert!(!set.is_match("anything.rb"));
    }
}

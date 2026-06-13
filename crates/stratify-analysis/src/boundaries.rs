use std::collections::HashSet;
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use serde::Deserialize;
use stratify_core::ir::Span;
use stratify_core::{Confidence, Finding, IrGraph, Severity};

/// Layer-boundary configuration, parsed from `stratify.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct BoundaryConfig {
    /// Layer name -> glob patterns matching files in that layer.
    #[serde(default)]
    pub layers: std::collections::BTreeMap<String, Vec<String>>,
    /// Forbidden import rules.
    #[serde(default)]
    pub forbid: Vec<ForbidRule>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ForbidRule {
    pub from: String,
    pub to: String,
}

/// Compile each layer's globs into a GlobSet (path separators are literal, so
/// `*` does not cross `/` but `**` does). Bad patterns are skipped.
fn compile_layers(config: &BoundaryConfig) -> Vec<(String, GlobSet)> {
    let mut out = Vec::new();
    for (layer, patterns) in &config.layers {
        let mut b = GlobSetBuilder::new();
        for p in patterns {
            if let Ok(g) = GlobBuilder::new(p).literal_separator(true).build() {
                b.add(g);
            }
        }
        if let Ok(set) = b.build() {
            out.push((layer.clone(), set));
        }
    }
    out
}

/// Classify a file path into the first matching layer (config order).
fn classify<'a>(file: &str, layers: &'a [(String, GlobSet)]) -> Option<&'a str> {
    layers
        .iter()
        .find(|(_, set)| set.is_match(file))
        .map(|(name, _)| name.as_str())
}

/// Report import edges that cross a forbidden layer boundary.
pub fn analyze(graph: &IrGraph, config: &BoundaryConfig) -> Vec<Finding> {
    if config.forbid.is_empty() {
        return Vec::new();
    }
    let layers = compile_layers(config);
    let forbidden: HashSet<(String, String)> = config
        .forbid
        .iter()
        .map(|r| (r.from.clone(), r.to.clone()))
        .collect();

    let adj = crate::imports::file_import_graph(graph);
    let span_of = crate::imports::file_spans(graph);

    let mut findings = Vec::new();
    for (src, targets) in &adj {
        let Some(src_layer) = classify(src, &layers) else {
            continue;
        };
        for tgt in targets {
            let Some(tgt_layer) = classify(tgt, &layers) else {
                continue;
            };
            if forbidden.contains(&(src_layer.to_string(), tgt_layer.to_string())) {
                let span = span_of.get(src).cloned().unwrap_or(Span {
                    file: src.clone(),
                    start_byte: 0,
                    end_byte: 0,
                    start_line: 1,
                });
                findings.push(Finding {
                    rule: "boundary".into(),
                    severity: Severity::Warning,
                    message: format!(
                        "layer `{src_layer}` must not import `{tgt_layer}` ({src} -> {tgt})"
                    ),
                    span,
                    confidence: Confidence::Certain,
                });
            }
        }
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use stratify_core::ir::{Reference, Symbol, SymbolId, Visibility};
    use stratify_core::{RefKind, SymbolKind};

    fn file_sym(g: &mut IrGraph, path: &str) -> SymbolId {
        g.add_symbol(Symbol {
            id: SymbolId(0),
            kind: SymbolKind::File,
            name: path.into(),
            fqn: path.into(),
            span: Span { file: path.into(), start_byte: 0, end_byte: 1, start_line: 1 },
            visibility: Visibility::Unknown,
            confidence: Confidence::Certain,
        })
    }

    fn import(g: &mut IrGraph, from: SymbolId, key: &str) {
        let d = g.add_symbol(Symbol {
            id: SymbolId(0),
            kind: SymbolKind::Dependency,
            name: key.into(),
            fqn: key.into(),
            span: Span { file: "x".into(), start_byte: 0, end_byte: 1, start_line: 1 },
            visibility: Visibility::Unknown,
            confidence: Confidence::Certain,
        });
        g.add_reference(Reference {
            from,
            to: d,
            kind: RefKind::Imports,
            span: Span { file: "x".into(), start_byte: 0, end_byte: 1, start_line: 1 },
            confidence: Confidence::Certain,
        });
    }

    fn config() -> BoundaryConfig {
        let mut layers = std::collections::BTreeMap::new();
        layers.insert("models".to_string(), vec!["models/**".to_string()]);
        layers.insert("controllers".to_string(), vec!["controllers/**".to_string()]);
        BoundaryConfig {
            layers,
            forbid: vec![ForbidRule { from: "models".into(), to: "controllers".into() }],
        }
    }

    #[test]
    fn glob_classifies_nested_file() {
        let layers = compile_layers(&config());
        assert_eq!(classify("models/user.rb", &layers), Some("models"));
        assert_eq!(classify("controllers/users.rb", &layers), Some("controllers"));
        assert_eq!(classify("lib/util.rb", &layers), None);
    }

    #[test]
    fn flags_forbidden_edge() {
        let mut g = IrGraph::new();
        let m = file_sym(&mut g, "models/user.rb");
        file_sym(&mut g, "controllers/users.rb");
        import(&mut g, m, "controllers/users.rb"); // models -> controllers (forbidden)
        let findings = analyze(&g, &config());
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].rule, "boundary");
        assert!(findings[0].message.contains("models"));
        assert!(findings[0].message.contains("controllers"));
    }

    #[test]
    fn allows_reverse_direction() {
        let mut g = IrGraph::new();
        file_sym(&mut g, "models/user.rb");
        let c = file_sym(&mut g, "controllers/users.rb");
        import(&mut g, c, "models/user.rb"); // controllers -> models (allowed)
        assert!(analyze(&g, &config()).is_empty());
    }

    #[test]
    fn no_config_no_findings() {
        let mut g = IrGraph::new();
        let m = file_sym(&mut g, "models/user.rb");
        file_sym(&mut g, "controllers/users.rb");
        import(&mut g, m, "controllers/users.rb");
        assert!(analyze(&g, &BoundaryConfig::default()).is_empty());
    }
}

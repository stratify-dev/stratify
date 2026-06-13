use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use serde::Deserialize;
use std::collections::HashSet;
use stratify_core::ir::Span;
use stratify_core::{Confidence, Finding, IrGraph, Severity};

/// Layer-boundary configuration, parsed from `stratify.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct BoundaryConfig {
    /// Optional built-in preset name (e.g. "rails", "layered").
    #[serde(default)]
    pub preset: Option<String>,
    /// Layer name -> glob patterns matching files in that layer.
    #[serde(default)]
    pub layers: std::collections::BTreeMap<String, Vec<String>>,
    /// Forbidden import rules.
    #[serde(default)]
    pub forbid: Vec<ForbidRule>,
}

fn layer(name: &str, globs: &[&str]) -> (String, Vec<String>) {
    (
        name.to_string(),
        globs.iter().map(|s| s.to_string()).collect(),
    )
}

fn forbid_rule(from: &str, to: &str) -> ForbidRule {
    ForbidRule {
        from: from.to_string(),
        to: to.to_string(),
    }
}

/// Return the layers + forbid rules for a built-in preset, or None if unknown.
pub fn builtin_preset(name: &str) -> Option<BoundaryConfig> {
    match name {
        "rails" => Some(BoundaryConfig {
            preset: None,
            layers: [
                layer("controllers", &["app/controllers/**"]),
                layer("models", &["app/models/**"]),
                layer("views", &["app/views/**"]),
                layer("mailers", &["app/mailers/**"]),
                layer("jobs", &["app/jobs/**"]),
            ]
            .into_iter()
            .collect(),
            // Domain models must not depend on the web/delivery layers.
            forbid: vec![
                forbid_rule("models", "controllers"),
                forbid_rule("models", "views"),
                forbid_rule("models", "mailers"),
            ],
        }),
        "layered" => Some(BoundaryConfig {
            preset: None,
            layers: [
                layer("controller", &["**/controller/**", "**/controllers/**"]),
                layer("service", &["**/service/**", "**/services/**"]),
                layer(
                    "repository",
                    &["**/repository/**", "**/repositories/**", "**/dao/**"],
                ),
                layer(
                    "domain",
                    &[
                        "**/domain/**",
                        "**/model/**",
                        "**/models/**",
                        "**/entity/**",
                    ],
                ),
            ]
            .into_iter()
            .collect(),
            // Lower layers must not import higher ones; domain is innermost.
            forbid: vec![
                forbid_rule("repository", "controller"),
                forbid_rule("repository", "service"),
                forbid_rule("domain", "controller"),
                forbid_rule("domain", "service"),
                forbid_rule("domain", "repository"),
            ],
        }),
        _ => None,
    }
}

/// Resolve a config: if it names a known preset, start from the preset's
/// layers + forbid, then layer the user's own entries on top (user layer keys
/// override preset keys; user forbid rules are appended). Unknown or absent
/// preset -> the config is returned unchanged.
pub fn resolve(config: BoundaryConfig) -> BoundaryConfig {
    let Some(base) = config.preset.as_deref().and_then(builtin_preset) else {
        return config;
    };
    let mut layers = base.layers;
    for (k, v) in config.layers {
        layers.insert(k, v); // user overrides preset for same-named layer
    }
    let mut forbid = base.forbid;
    forbid.extend(config.forbid); // user rules appended
    BoundaryConfig {
        preset: config.preset,
        layers,
        forbid,
    }
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
            span: Span {
                file: path.into(),
                start_byte: 0,
                end_byte: 1,
                start_line: 1,
            },
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
            span: Span {
                file: "x".into(),
                start_byte: 0,
                end_byte: 1,
                start_line: 1,
            },
            visibility: Visibility::Unknown,
            confidence: Confidence::Certain,
        });
        g.add_reference(Reference {
            from,
            to: d,
            kind: RefKind::Imports,
            span: Span {
                file: "x".into(),
                start_byte: 0,
                end_byte: 1,
                start_line: 1,
            },
            confidence: Confidence::Certain,
        });
    }

    fn config() -> BoundaryConfig {
        let mut layers = std::collections::BTreeMap::new();
        layers.insert("models".to_string(), vec!["models/**".to_string()]);
        layers.insert(
            "controllers".to_string(),
            vec!["controllers/**".to_string()],
        );
        BoundaryConfig {
            preset: None,
            layers,
            forbid: vec![ForbidRule {
                from: "models".into(),
                to: "controllers".into(),
            }],
        }
    }

    #[test]
    fn glob_classifies_nested_file() {
        let layers = compile_layers(&config());
        assert_eq!(classify("models/user.rb", &layers), Some("models"));
        assert_eq!(
            classify("controllers/users.rb", &layers),
            Some("controllers")
        );
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

    #[test]
    fn rails_preset_has_expected_layers_and_rules() {
        let c = builtin_preset("rails").unwrap();
        assert!(c.layers.contains_key("models"));
        assert!(c.layers.contains_key("controllers"));
        assert!(c
            .forbid
            .iter()
            .any(|r| r.from == "models" && r.to == "controllers"));
    }

    #[test]
    fn resolve_expands_named_preset() {
        let c = resolve(BoundaryConfig {
            preset: Some("rails".into()),
            ..Default::default()
        });
        assert!(c.layers.contains_key("models"));
        assert!(!c.forbid.is_empty());
    }

    #[test]
    fn resolve_unknown_preset_is_noop() {
        let c = resolve(BoundaryConfig {
            preset: Some("nope".into()),
            ..Default::default()
        });
        assert!(c.layers.is_empty());
        assert!(c.forbid.is_empty());
    }

    #[test]
    fn user_entries_extend_preset() {
        let mut layers = std::collections::BTreeMap::new();
        layers.insert("models".to_string(), vec!["lib/models/**".to_string()]); // override
        layers.insert("custom".to_string(), vec!["lib/custom/**".to_string()]); // add
        let c = resolve(BoundaryConfig {
            preset: Some("rails".into()),
            layers,
            forbid: vec![ForbidRule {
                from: "custom".into(),
                to: "controllers".into(),
            }],
        });
        assert_eq!(
            c.layers.get("models").unwrap(),
            &vec!["lib/models/**".to_string()]
        ); // overridden
        assert!(c.layers.contains_key("custom")); // added
        assert!(c.layers.contains_key("controllers")); // from preset
        assert!(c
            .forbid
            .iter()
            .any(|r| r.from == "custom" && r.to == "controllers")); // user rule appended
        assert!(c
            .forbid
            .iter()
            .any(|r| r.from == "models" && r.to == "controllers")); // preset rule kept
    }

    #[test]
    fn analyze_works_through_a_resolved_preset() {
        // models/user.rb importing controllers/x.rb violates the rails preset.
        use stratify_core::ir::{Reference, Span, Symbol, SymbolId, Visibility};
        use stratify_core::{Confidence, RefKind, SymbolKind};
        let mut g = IrGraph::new();
        let m = g.add_symbol(Symbol {
            id: SymbolId(0),
            kind: SymbolKind::File,
            name: "app/models/user.rb".into(),
            fqn: "app/models/user.rb".into(),
            span: Span {
                file: "app/models/user.rb".into(),
                start_byte: 0,
                end_byte: 1,
                start_line: 1,
            },
            visibility: Visibility::Unknown,
            confidence: Confidence::Certain,
        });
        g.add_symbol(Symbol {
            id: SymbolId(0),
            kind: SymbolKind::File,
            name: "app/controllers/x.rb".into(),
            fqn: "app/controllers/x.rb".into(),
            span: Span {
                file: "app/controllers/x.rb".into(),
                start_byte: 0,
                end_byte: 1,
                start_line: 1,
            },
            visibility: Visibility::Unknown,
            confidence: Confidence::Certain,
        });
        let dep = g.add_symbol(Symbol {
            id: SymbolId(0),
            kind: SymbolKind::Dependency,
            name: "app/controllers/x.rb".into(),
            fqn: "app/controllers/x.rb".into(),
            span: Span {
                file: "x".into(),
                start_byte: 0,
                end_byte: 1,
                start_line: 1,
            },
            visibility: Visibility::Unknown,
            confidence: Confidence::Certain,
        });
        g.add_reference(Reference {
            from: m,
            to: dep,
            kind: RefKind::Imports,
            span: Span {
                file: "app/models/user.rb".into(),
                start_byte: 0,
                end_byte: 1,
                start_line: 1,
            },
            confidence: Confidence::Certain,
        });
        let config = resolve(BoundaryConfig {
            preset: Some("rails".into()),
            ..Default::default()
        });
        let findings = analyze(&g, &config);
        assert!(findings.iter().any(|f| f.rule == "boundary"
            && f.message.contains("models")
            && f.message.contains("controllers")));
    }
}

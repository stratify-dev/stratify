use serde::Deserialize;
use std::collections::HashSet;
use stratify_core::ir::SymbolId;
use stratify_core::{Confidence, Finding, IrGraph, RefKind, Severity, SymbolKind, Visibility};

/// How an unreached `Visibility::Public` symbol should be treated. Doesn't
/// affect private/unknown-visibility symbols, which are always fully
/// evaluated regardless of mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeadCodeMode {
    /// A published library: an unreached public symbol might still be
    /// consumed by a caller outside this scan, so it's reported at reduced
    /// confidence (Info/Likely) rather than a flat Warning. The default -
    /// preserves the historical behavior for every existing caller.
    #[default]
    Library,
    /// A self-contained application: nothing outside this repo can call its
    /// exports, so an unreached public symbol is exactly as dead as a
    /// private one and gets the same full-strength Warning/Certain.
    Application,
}

/// The `[dead_code]` table of `stratify.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DeadCodeSection {
    #[serde(default)]
    pub mode: Option<DeadCodeMode>,
}

/// Wrapper to deserialize just the `[dead_code]` table from `stratify.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DeadCodeToml {
    #[serde(default)]
    pub dead_code: DeadCodeSection,
}

/// Find functions that no entrypoint can reach via Calls/Defines edges.
/// A function reachable only through a low-confidence edge is reported as
/// "possibly unused" (Info) rather than "dead" (Warning) - so is an
/// unreached `Visibility::Public` symbol in `DeadCodeMode::Library`, on the
/// same reasoning: not certain, not a Warning.
pub fn analyze(graph: &IrGraph, mode: DeadCodeMode) -> Vec<Finding> {
    let roots: Vec<SymbolId> = graph.entrypoints().to_vec();

    // BFS reachability. Track the weakest edge confidence used to reach a node.
    let mut reached_certain: HashSet<SymbolId> = HashSet::new();
    let mut reached_any: HashSet<SymbolId> = HashSet::new();
    let mut queue: Vec<(SymbolId, bool)> = roots.iter().map(|r| (*r, true)).collect();
    for r in &roots {
        reached_certain.insert(*r);
        reached_any.insert(*r);
    }

    while let Some((node, path_certain)) = queue.pop() {
        for r in graph.references() {
            if r.from != node {
                continue;
            }
            if !matches!(r.kind, RefKind::Calls | RefKind::Inherits) {
                continue;
            }
            let edge_certain = path_certain && r.confidence == Confidence::Certain;
            let newly_certain = edge_certain && reached_certain.insert(r.to);
            let newly_any = reached_any.insert(r.to);
            if newly_certain || newly_any {
                queue.push((r.to, edge_certain));
            }
        }
    }

    let mut findings = Vec::new();
    for s in graph.symbols() {
        if !matches!(s.kind, SymbolKind::Function) {
            continue;
        }
        if reached_certain.contains(&s.id) {
            continue; // definitely used
        }
        let low_confidence = reached_any.contains(&s.id)
            || (s.visibility == Visibility::Public && mode == DeadCodeMode::Library);
        if low_confidence {
            findings.push(Finding {
                rule: "dead_code".into(),
                severity: Severity::Info,
                message: format!("possibly unused function `{}`", s.name),
                span: s.span.clone(),
                confidence: Confidence::Likely,
            });
        } else {
            findings.push(Finding {
                rule: "dead_code".into(),
                severity: Severity::Warning,
                message: format!("unused function `{}`", s.name),
                span: s.span.clone(),
                confidence: Confidence::Certain,
            });
        }
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use stratify_core::ir::{Reference, Span, Symbol, SymbolId, Visibility};

    fn func(g: &mut IrGraph, name: &str) -> SymbolId {
        sym_with_visibility(g, name, Visibility::Unknown)
    }

    fn public_func(g: &mut IrGraph, name: &str) -> SymbolId {
        sym_with_visibility(g, name, Visibility::Public)
    }

    fn sym_with_visibility(g: &mut IrGraph, name: &str, visibility: Visibility) -> SymbolId {
        g.add_symbol(Symbol {
            id: SymbolId(0),
            kind: SymbolKind::Function,
            name: name.into(),
            fqn: name.into(),
            span: Span {
                file: "T.java".into(),
                start_byte: 0,
                end_byte: 1,
                start_line: 1,
            },
            visibility,
            confidence: Confidence::Certain,
        })
    }

    fn edge(g: &mut IrGraph, from: SymbolId, to: SymbolId, conf: Confidence) {
        g.add_reference(Reference {
            from,
            to,
            kind: RefKind::Calls,
            span: Span {
                file: "T.java".into(),
                start_byte: 0,
                end_byte: 1,
                start_line: 1,
            },
            confidence: conf,
        });
    }

    #[test]
    fn unreached_function_is_dead() {
        let mut g = IrGraph::new();
        let main = func(&mut g, "main");
        g.mark_entrypoint(main);
        let _orphan = func(&mut g, "orphan");
        let findings = analyze(&g, DeadCodeMode::Library);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Warning);
        assert!(findings[0].message.contains("orphan"));
    }

    #[test]
    fn reached_via_certain_edge_is_not_reported() {
        let mut g = IrGraph::new();
        let main = func(&mut g, "main");
        g.mark_entrypoint(main);
        let used = func(&mut g, "used");
        edge(&mut g, main, used, Confidence::Certain);
        assert!(analyze(&g, DeadCodeMode::Library).is_empty());
    }

    #[test]
    fn reached_only_via_likely_edge_is_possibly_unused() {
        let mut g = IrGraph::new();
        let main = func(&mut g, "main");
        g.mark_entrypoint(main);
        let maybe = func(&mut g, "maybe");
        edge(&mut g, main, maybe, Confidence::Likely);
        let findings = analyze(&g, DeadCodeMode::Library);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Info);
        assert!(findings[0].message.contains("possibly unused"));
    }

    #[test]
    fn unreached_public_symbol_is_possibly_unused_in_library_mode() {
        // A public/exported symbol nothing in this scan calls might still be
        // consumed by an unseen external caller - a published library's
        // public surface. Library mode (the default) downgrades this case to
        // Info/Likely rather than a flat Warning, matching the confidence
        // language already used for low-confidence-edge reachability.
        let mut g = IrGraph::new();
        let main = func(&mut g, "main");
        g.mark_entrypoint(main);
        let public_orphan = public_func(&mut g, "publicOrphan");
        let findings = analyze(&g, DeadCodeMode::Library);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Info);
        assert_eq!(findings[0].confidence, Confidence::Likely);
        assert!(findings[0].message.contains("publicOrphan"));
        let _ = public_orphan;
    }

    #[test]
    fn unreached_public_symbol_is_a_full_warning_in_application_mode() {
        // An application isn't a library: nothing outside this repo can call
        // its exports, so an unreached public symbol is exactly as dead as a
        // private one.
        let mut g = IrGraph::new();
        let main = func(&mut g, "main");
        g.mark_entrypoint(main);
        let public_orphan = public_func(&mut g, "publicOrphan");
        let findings = analyze(&g, DeadCodeMode::Application);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Warning);
        assert_eq!(findings[0].confidence, Confidence::Certain);
        let _ = public_orphan;
    }

    #[test]
    fn unreached_private_symbol_is_a_full_warning_in_either_mode() {
        // Visibility only changes the outcome for Public symbols; a private
        // orphan is unambiguously dead regardless of mode.
        for mode in [DeadCodeMode::Library, DeadCodeMode::Application] {
            let mut g = IrGraph::new();
            let main = func(&mut g, "main");
            g.mark_entrypoint(main);
            let _orphan = func(&mut g, "orphan");
            let findings = analyze(&g, mode);
            assert_eq!(findings.len(), 1, "mode: {mode:?}");
            assert_eq!(findings[0].severity, Severity::Warning, "mode: {mode:?}");
        }
    }

    #[test]
    fn file_defines_does_not_make_methods_reachable() {
        // Regression: File entrypoint + Defines traversal used to mark every
        // method reachable, so nothing was ever flagged. Defines is structural
        // containment, not a use-edge, and must not confer reachability.
        use crate::test_support::{add_ref, add_sym};

        let mut g = IrGraph::new();
        let file_id = add_sym(&mut g, SymbolKind::File, "Foo.java", "Foo.java", "Foo.java");
        let orphan = add_sym(&mut g, SymbolKind::Function, "orphan", "orphan", "Foo.java");
        add_ref(&mut g, file_id, orphan, RefKind::Defines, "Foo.java");

        let findings = analyze(&g, DeadCodeMode::Library);
        assert_eq!(
            findings.len(),
            1,
            "File --Defines--> orphan must not make orphan reachable"
        );
        assert_eq!(findings[0].severity, Severity::Warning);
        assert!(findings[0].message.contains("orphan"));
    }
}

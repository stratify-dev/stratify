use std::collections::HashSet;
use stratify_core::ir::SymbolId;
use stratify_core::{Confidence, Finding, IrGraph, RefKind, Severity, SymbolKind};

/// A symbol is an entrypoint if it is a function named `main`. For M1 (Java),
/// `main` methods are the only roots. Framework and file-based roots come in a
/// later milestone.
fn is_entrypoint(name: &str, kind: SymbolKind) -> bool {
    matches!(kind, SymbolKind::Function) && name == "main"
}

/// Find functions that no entrypoint can reach via Calls/Defines edges.
/// A function reachable only through a low-confidence edge is reported as
/// "possibly unused" (Info) rather than "dead" (Warning).
pub fn analyze(graph: &IrGraph) -> Vec<Finding> {
    // Build adjacency: from -> [(to, confidence)].
    let mut roots: Vec<SymbolId> = Vec::new();
    for s in graph.symbols() {
        if is_entrypoint(&s.name, s.kind) {
            roots.push(s.id);
        }
    }

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
        if reached_any.contains(&s.id) {
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
        g.add_symbol(Symbol {
            id: SymbolId(0),
            kind: SymbolKind::Function,
            name: name.into(),
            fqn: name.into(),
            span: Span { file: "T.java".into(), start_byte: 0, end_byte: 1, start_line: 1 },
            visibility: Visibility::Unknown,
            confidence: Confidence::Certain,
        })
    }

    fn edge(g: &mut IrGraph, from: SymbolId, to: SymbolId, conf: Confidence) {
        g.add_reference(Reference {
            from, to, kind: RefKind::Calls,
            span: Span { file: "T.java".into(), start_byte: 0, end_byte: 1, start_line: 1 },
            confidence: conf,
        });
    }

    #[test]
    fn unreached_function_is_dead() {
        let mut g = IrGraph::new();
        let _main = func(&mut g, "main");
        let _orphan = func(&mut g, "orphan");
        let findings = analyze(&g);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Warning);
        assert!(findings[0].message.contains("orphan"));
    }

    #[test]
    fn reached_via_certain_edge_is_not_reported() {
        let mut g = IrGraph::new();
        let main = func(&mut g, "main");
        let used = func(&mut g, "used");
        edge(&mut g, main, used, Confidence::Certain);
        assert!(analyze(&g).is_empty());
    }

    #[test]
    fn reached_only_via_likely_edge_is_possibly_unused() {
        let mut g = IrGraph::new();
        let main = func(&mut g, "main");
        let maybe = func(&mut g, "maybe");
        edge(&mut g, main, maybe, Confidence::Likely);
        let findings = analyze(&g);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Info);
        assert!(findings[0].message.contains("possibly unused"));
    }

    #[test]
    fn file_defines_does_not_make_methods_reachable() {
        // Regression: File entrypoint + Defines traversal used to mark every
        // method reachable, so nothing was ever flagged. Defines is structural
        // containment, not a use-edge, and must not confer reachability.
        use stratify_core::ir::{Reference, Span, Symbol, Visibility};

        let mut g = IrGraph::new();
        let file_id = g.add_symbol(Symbol {
            id: SymbolId(0),
            kind: SymbolKind::File,
            name: "Foo.java".into(),
            fqn: "Foo.java".into(),
            span: Span { file: "Foo.java".into(), start_byte: 0, end_byte: 100, start_line: 1 },
            visibility: Visibility::Unknown,
            confidence: Confidence::Certain,
        });
        let orphan = g.add_symbol(Symbol {
            id: SymbolId(0),
            kind: SymbolKind::Function,
            name: "orphan".into(),
            fqn: "orphan".into(),
            span: Span { file: "Foo.java".into(), start_byte: 0, end_byte: 10, start_line: 2 },
            visibility: Visibility::Unknown,
            confidence: Confidence::Certain,
        });
        g.add_reference(Reference {
            from: file_id,
            to: orphan,
            kind: RefKind::Defines,
            span: Span { file: "Foo.java".into(), start_byte: 0, end_byte: 10, start_line: 2 },
            confidence: Confidence::Certain,
        });

        let findings = analyze(&g);
        assert_eq!(findings.len(), 1, "File --Defines--> orphan must not make orphan reachable");
        assert_eq!(findings[0].severity, Severity::Warning);
        assert!(findings[0].message.contains("orphan"));
    }
}

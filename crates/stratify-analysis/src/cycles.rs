use std::collections::{BTreeMap, BTreeSet, HashMap};
use stratify_core::ir::Span;
use stratify_core::{Confidence, Finding, IrGraph, RefKind, Severity, SymbolKind};

/// Detect circular dependencies in the cross-file import graph. An `Imports`
/// edge (File -> Dependency) resolves to a file-to-file edge when the
/// Dependency's name (import key) equals some File/Class/Module symbol's fqn
/// (export key). Cycles are found by DFS back-edge detection.
pub fn analyze(graph: &IrGraph) -> Vec<Finding> {
    // export key -> file path. Built from importable symbols only.
    let mut export: HashMap<&str, String> = HashMap::new();
    for s in graph.symbols() {
        if matches!(s.kind, SymbolKind::File | SymbolKind::Class | SymbolKind::Module) {
            export.entry(s.fqn.as_str()).or_insert_with(|| s.span.file.clone());
        }
    }

    // file -> sorted set of files it imports.
    let mut adj: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut span_of: HashMap<String, Span> = HashMap::new();
    for s in graph.symbols() {
        if matches!(s.kind, SymbolKind::File) {
            adj.entry(s.span.file.clone()).or_default();
            span_of.entry(s.span.file.clone()).or_insert_with(|| s.span.clone());
        }
    }
    for r in graph.references() {
        if !matches!(r.kind, RefKind::Imports) {
            continue;
        }
        let (Some(from), Some(to)) = (graph.symbol(r.from), graph.symbol(r.to)) else {
            continue;
        };
        let src_file = &from.span.file;
        if let Some(target_file) = export.get(to.name.as_str()) {
            if target_file != src_file {
                adj.entry(src_file.clone())
                    .or_default()
                    .insert(target_file.clone());
            }
        }
    }

    // DFS back-edge detection. Colors: 0 = white, 1 = gray (on stack), 2 = black.
    let mut color: HashMap<String, u8> = HashMap::new();
    let mut path: Vec<String> = Vec::new();
    let mut reported: BTreeSet<Vec<String>> = BTreeSet::new();
    let nodes: Vec<String> = adj.keys().cloned().collect();
    for start in &nodes {
        if color.get(start).copied().unwrap_or(0) == 0 {
            dfs(start, &adj, &mut color, &mut path, &mut reported);
        }
    }

    // Emit one finding per distinct cycle (canonicalized to its lexicographically
    // smallest rotation so A->B->A and B->A->B are the same cycle).
    let mut findings = Vec::new();
    for cycle in reported {
        let file = &cycle[0];
        let span = span_of
            .get(file)
            .cloned()
            .unwrap_or(Span { file: file.clone(), start_byte: 0, end_byte: 0, start_line: 1 });
        findings.push(Finding {
            rule: "cycle".into(),
            severity: Severity::Warning,
            message: format!("circular dependency: {}", cycle.join(" -> ")),
            span,
            confidence: Confidence::Certain,
        });
    }
    findings
}

fn dfs(
    node: &str,
    adj: &BTreeMap<String, BTreeSet<String>>,
    color: &mut HashMap<String, u8>,
    path: &mut Vec<String>,
    reported: &mut BTreeSet<Vec<String>>,
) {
    color.insert(node.to_string(), 1);
    path.push(node.to_string());
    if let Some(neighbors) = adj.get(node) {
        for next in neighbors {
            match color.get(next).copied().unwrap_or(0) {
                0 => dfs(next, adj, color, path, reported),
                1 => {
                    // Back edge: cycle is path[pos..] where path[pos] == next.
                    if let Some(pos) = path.iter().position(|n| n == next) {
                        let cycle = canonical_cycle(&path[pos..]);
                        reported.insert(cycle);
                    }
                }
                _ => {}
            }
        }
    }
    path.pop();
    color.insert(node.to_string(), 2);
}

/// Rotate a cycle so it starts at its lexicographically smallest node, so the
/// same cycle discovered from different entry points dedupes.
fn canonical_cycle(nodes: &[String]) -> Vec<String> {
    let min_pos = nodes
        .iter()
        .enumerate()
        .min_by_key(|(_, n)| (*n).clone())
        .map(|(i, _)| i)
        .unwrap_or(0);
    let mut out: Vec<String> = nodes[min_pos..].to_vec();
    out.extend_from_slice(&nodes[..min_pos]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use stratify_core::ir::{Reference, Symbol, SymbolId, Visibility};

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

    fn dep(g: &mut IrGraph, from: SymbolId, key: &str) {
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

    #[test]
    fn detects_two_file_cycle() {
        let mut g = IrGraph::new();
        let a = file_sym(&mut g, "a.rb"); // exports key "a.rb"
        let b = file_sym(&mut g, "b.rb"); // exports key "b.rb"
        dep(&mut g, a, "b.rb"); // a imports b
        dep(&mut g, b, "a.rb"); // b imports a
        let findings = analyze(&g);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].rule, "cycle");
        assert!(findings[0].message.contains("a.rb"));
        assert!(findings[0].message.contains("b.rb"));
    }

    #[test]
    fn no_cycle_for_dag() {
        let mut g = IrGraph::new();
        let a = file_sym(&mut g, "a.rb");
        let _b = file_sym(&mut g, "b.rb");
        dep(&mut g, a, "b.rb"); // a -> b only
        assert!(analyze(&g).is_empty());
    }

    #[test]
    fn unresolved_import_is_ignored() {
        let mut g = IrGraph::new();
        let a = file_sym(&mut g, "a.rb");
        dep(&mut g, a, "nonexistent.rb"); // no matching export
        assert!(analyze(&g).is_empty());
    }

    #[test]
    fn detects_three_file_cycle_once() {
        let mut g = IrGraph::new();
        let a = file_sym(&mut g, "a.rb");
        let b = file_sym(&mut g, "b.rb");
        let c = file_sym(&mut g, "c.rb");
        dep(&mut g, a, "b.rb");
        dep(&mut g, b, "c.rb");
        dep(&mut g, c, "a.rb");
        let findings = analyze(&g);
        assert_eq!(findings.len(), 1, "one cycle, not one per entry point");
        assert!(findings[0].message.contains("a.rb -> b.rb -> c.rb"));
    }
}

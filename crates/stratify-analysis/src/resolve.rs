use std::collections::HashMap;
use stratify_core::ir::{Reference, Span, SymbolId};
use stratify_core::{Confidence, IrGraph, RefKind, SymbolKind};

/// Resolve cross-file calls: for each recorded unresolved call, add a `Calls`
/// edge to every repo-wide Function whose name matches the callee, when that
/// function lives in a DIFFERENT file than the caller. Edges are `Likely`
/// (bare-name matching is a heuristic, so this only ever downgrades a "dead"
/// verdict, never falsely clears one). Existing identical edges are not
/// duplicated.
pub fn cross_file_calls(graph: &mut IrGraph) {
    // Repo-wide function name -> (symbol id, file).
    let mut by_name: HashMap<String, Vec<(SymbolId, String)>> = HashMap::new();
    for s in graph.symbols() {
        if matches!(s.kind, SymbolKind::Function) {
            by_name
                .entry(s.name.clone())
                .or_default()
                .push((s.id, s.span.file.clone()));
        }
    }

    // Existing Calls edges, to avoid duplicates.
    let mut existing: std::collections::HashSet<(SymbolId, SymbolId)> = graph
        .references()
        .iter()
        .filter(|r| matches!(r.kind, RefKind::Calls))
        .map(|r| (r.from, r.to))
        .collect();

    // Caller id -> caller file, for the cross-file check.
    let caller_file: HashMap<SymbolId, String> = graph
        .symbols()
        .iter()
        .map(|s| (s.id, s.span.file.clone()))
        .collect();

    let mut to_add: Vec<Reference> = Vec::new();
    for (from, name) in graph.unresolved_calls() {
        let Some(candidates) = by_name.get(name) else {
            continue; // not a repo function (stdlib/builtin/external) — skip
        };
        let from_file = caller_file.get(from);
        for (to, to_file) in candidates {
            // cross-file only: intra-file calls were already resolved by adapters
            if from_file.map(|f| f == to_file).unwrap_or(false) {
                continue;
            }
            if to == from {
                continue;
            }
            if existing.insert((*from, *to)) {
                to_add.push(Reference {
                    from: *from,
                    to: *to,
                    kind: RefKind::Calls,
                    span: Span {
                        file: "<resolved>".into(),
                        start_byte: 0,
                        end_byte: 0,
                        start_line: 0,
                    },
                    confidence: Confidence::Likely,
                });
            }
        }
    }
    for r in to_add {
        graph.add_reference(r);
    }
}

/// Resolve Go imports: rewrite each Dependency reached by an `Imports` edge
/// from a `.go` file so its name becomes the longest in-repo Go package dir
/// that is a suffix of the raw import path. External imports (no matching
/// package dir) are left unchanged (they then match no fqn and form no edge).
pub fn go_imports(graph: &mut IrGraph) {
    // Known Go package dirs = fqns of File symbols whose file ends in ".go".
    let mut pkgs: Vec<String> = graph
        .symbols()
        .iter()
        .filter(|s| matches!(s.kind, SymbolKind::File) && s.span.file.ends_with(".go"))
        .map(|s| s.fqn.clone())
        .collect();
    pkgs.sort();
    pkgs.dedup();
    if pkgs.is_empty() {
        return;
    }

    // Collect (dependency id, new name) for Go import edges.
    let mut renames: Vec<(stratify_core::ir::SymbolId, String)> = Vec::new();
    for r in graph.references() {
        if !matches!(r.kind, RefKind::Imports) {
            continue;
        }
        let (Some(from), Some(to)) = (graph.symbol(r.from), graph.symbol(r.to)) else {
            continue;
        };
        if !from.span.file.ends_with(".go") {
            continue;
        }
        let path = to.name.as_str();
        // longest package dir that equals the path or is a trailing path segment of it
        let best = pkgs
            .iter()
            .filter(|d| path == d.as_str() || path.ends_with(&format!("/{d}")))
            .max_by_key(|d| d.len());
        if let Some(dir) = best {
            if dir.as_str() != path {
                renames.push((to.id, dir.clone()));
            }
        }
    }
    for (id, name) in renames {
        graph.set_symbol_name(id, name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stratify_core::ir::{Symbol, Visibility};

    fn func(g: &mut IrGraph, name: &str, file: &str) -> SymbolId {
        g.add_symbol(Symbol {
            id: SymbolId(0),
            kind: SymbolKind::Function,
            name: name.into(),
            fqn: name.into(),
            span: Span {
                file: file.into(),
                start_byte: 0,
                end_byte: 1,
                start_line: 1,
            },
            visibility: Visibility::Unknown,
            confidence: Confidence::Certain,
        })
    }

    #[test]
    fn resolves_cross_file_call() {
        let mut g = IrGraph::new();
        let caller = func(&mut g, "caller", "a.rb");
        let target = func(&mut g, "target", "b.rb");
        g.add_unresolved_call(caller, "target".into());
        cross_file_calls(&mut g);
        assert!(g
            .references()
            .iter()
            .any(|r| matches!(r.kind, RefKind::Calls)
                && r.from == caller
                && r.to == target
                && r.confidence == Confidence::Likely));
    }

    #[test]
    fn ignores_unknown_callee() {
        let mut g = IrGraph::new();
        let caller = func(&mut g, "caller", "a.rb");
        g.add_unresolved_call(caller, "println".into()); // no repo function named this
        cross_file_calls(&mut g);
        assert!(g
            .references()
            .iter()
            .all(|r| !matches!(r.kind, RefKind::Calls)));
    }

    #[test]
    fn does_not_resolve_same_file() {
        // An unresolved call whose only match is in the caller's own file is not
        // re-added here (intra-file resolution is the adapter's job).
        let mut g = IrGraph::new();
        let caller = func(&mut g, "caller", "a.rb");
        let _same = func(&mut g, "target", "a.rb");
        g.add_unresolved_call(caller, "target".into());
        cross_file_calls(&mut g);
        assert!(g
            .references()
            .iter()
            .all(|r| !matches!(r.kind, RefKind::Calls)));
    }

    #[test]
    fn dedupes_against_existing_edge() {
        let mut g = IrGraph::new();
        let caller = func(&mut g, "caller", "a.rb");
        let target = func(&mut g, "target", "b.rb");
        g.add_reference(Reference {
            from: caller,
            to: target,
            kind: RefKind::Calls,
            span: Span {
                file: "a.rb".into(),
                start_byte: 0,
                end_byte: 1,
                start_line: 1,
            },
            confidence: Confidence::Likely,
        });
        g.add_unresolved_call(caller, "target".into());
        cross_file_calls(&mut g);
        let count = g
            .references()
            .iter()
            .filter(|r| matches!(r.kind, RefKind::Calls) && r.from == caller && r.to == target)
            .count();
        assert_eq!(count, 1, "should not duplicate the existing edge");
    }

    #[test]
    fn go_imports_resolves_by_suffix() {
        use stratify_core::ir::{Reference, Span, Symbol, Visibility};
        let mut g = IrGraph::new();
        // package "b" exists (b/b.go), file in package "a" imports example.com/m/b
        let a_file = g.add_symbol(Symbol {
            id: SymbolId(0), kind: SymbolKind::File, name: "a/a.go".into(), fqn: "a".into(),
            span: Span { file: "a/a.go".into(), start_byte: 0, end_byte: 1, start_line: 1 },
            visibility: Visibility::Unknown, confidence: Confidence::Certain });
        g.add_symbol(Symbol {
            id: SymbolId(0), kind: SymbolKind::File, name: "b/b.go".into(), fqn: "b".into(),
            span: Span { file: "b/b.go".into(), start_byte: 0, end_byte: 1, start_line: 1 },
            visibility: Visibility::Unknown, confidence: Confidence::Certain });
        let dep = g.add_symbol(Symbol {
            id: SymbolId(0), kind: SymbolKind::Dependency, name: "example.com/m/b".into(), fqn: "example.com/m/b".into(),
            span: Span { file: "x".into(), start_byte: 0, end_byte: 1, start_line: 1 },
            visibility: Visibility::Unknown, confidence: Confidence::Certain });
        g.add_reference(Reference { from: a_file, to: dep, kind: RefKind::Imports,
            span: Span { file: "a/a.go".into(), start_byte: 0, end_byte: 1, start_line: 1 }, confidence: Confidence::Certain });
        go_imports(&mut g);
        assert_eq!(g.symbol(dep).unwrap().name, "b");
    }

    #[test]
    fn go_imports_leaves_external_unchanged() {
        use stratify_core::ir::{Reference, Span, Symbol, Visibility};
        let mut g = IrGraph::new();
        let a_file = g.add_symbol(Symbol {
            id: SymbolId(0), kind: SymbolKind::File, name: "a/a.go".into(), fqn: "a".into(),
            span: Span { file: "a/a.go".into(), start_byte: 0, end_byte: 1, start_line: 1 },
            visibility: Visibility::Unknown, confidence: Confidence::Certain });
        let dep = g.add_symbol(Symbol {
            id: SymbolId(0), kind: SymbolKind::Dependency, name: "fmt".into(), fqn: "fmt".into(),
            span: Span { file: "x".into(), start_byte: 0, end_byte: 1, start_line: 1 },
            visibility: Visibility::Unknown, confidence: Confidence::Certain });
        g.add_reference(Reference { from: a_file, to: dep, kind: RefKind::Imports,
            span: Span { file: "a/a.go".into(), start_byte: 0, end_byte: 1, start_line: 1 }, confidence: Confidence::Certain });
        go_imports(&mut g);
        assert_eq!(g.symbol(dep).unwrap().name, "fmt"); // no matching package dir -> unchanged
    }
}

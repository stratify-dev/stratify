use std::collections::{BTreeMap, BTreeSet, HashMap};
use stratify_core::ir::Span;
use stratify_core::{IrGraph, RefKind, SymbolKind};

/// Build the file-level import graph: each file maps to the set of files it
/// imports. An `Imports` edge (File -> Dependency) resolves to a file edge when
/// the Dependency's name (import key) equals some File/Class/Module fqn (export
/// key). Every File symbol appears as a key (possibly with an empty set).
/// Self-edges are excluded.
pub fn file_import_graph(graph: &IrGraph) -> BTreeMap<String, BTreeSet<String>> {
    let mut export: HashMap<&str, String> = HashMap::new();
    for s in graph.symbols() {
        if matches!(s.kind, SymbolKind::File | SymbolKind::Class | SymbolKind::Module) {
            export.entry(s.fqn.as_str()).or_insert_with(|| s.span.file.clone());
        }
    }

    let mut adj: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for s in graph.symbols() {
        if matches!(s.kind, SymbolKind::File) {
            adj.entry(s.span.file.clone()).or_default();
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
    adj
}

/// Map each file to a representative span (its File symbol's span).
pub fn file_spans(graph: &IrGraph) -> HashMap<String, Span> {
    let mut spans = HashMap::new();
    for s in graph.symbols() {
        if matches!(s.kind, SymbolKind::File) {
            spans.entry(s.span.file.clone()).or_insert_with(|| s.span.clone());
        }
    }
    spans
}

use std::collections::{BTreeMap, BTreeSet, HashMap};
use stratify_core::ir::{Span, SymbolId};
use stratify_core::{IrGraph, RefKind, SymbolKind};

/// Build the file-level import graph: each file maps to the set of files it
/// imports. An `Imports` edge (File -> Dependency) resolves to a file edge when
/// the Dependency's name (import key) equals some File/Class/Module fqn (export
/// key). Every File symbol appears as a key (possibly with an empty set).
/// Self-edges are excluded.
pub fn file_import_graph(graph: &IrGraph) -> BTreeMap<String, BTreeSet<String>> {
    let mut export: HashMap<&str, String> = HashMap::new();
    for s in graph.symbols() {
        if matches!(
            s.kind,
            SymbolKind::File | SymbolKind::Class | SymbolKind::Module
        ) {
            export
                .entry(s.fqn.as_str())
                .or_insert_with(|| s.span.file.clone());
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
            spans
                .entry(s.span.file.clone())
                .or_insert_with(|| s.span.clone());
        }
    }
    spans
}

/// Like `file_import_graph` but keyed by export-key (fqn) instead of file path.
/// Files sharing an fqn (e.g. Go package files) collapse into one node. For
/// languages where fqn is 1:1 with the file, this matches `file_import_graph`.
pub fn fqn_import_graph(graph: &IrGraph) -> BTreeMap<String, BTreeSet<String>> {
    // export key -> () to validate import targets.
    let mut export: HashMap<&str, ()> = HashMap::new();
    for s in graph.symbols() {
        if matches!(
            s.kind,
            SymbolKind::File | SymbolKind::Class | SymbolKind::Module
        ) {
            export.entry(s.fqn.as_str()).or_insert(());
        }
    }
    // file id -> owning file's fqn (the source node key).
    let file_fqn: HashMap<SymbolId, String> = graph
        .symbols()
        .iter()
        .filter(|s| matches!(s.kind, SymbolKind::File))
        .map(|s| (s.id, s.fqn.clone()))
        .collect();

    let mut adj: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for s in graph.symbols() {
        if matches!(s.kind, SymbolKind::File) {
            adj.entry(s.fqn.clone()).or_default();
        }
    }
    for r in graph.references() {
        if !matches!(r.kind, RefKind::Imports) {
            continue;
        }
        let (Some(from), Some(to)) = (graph.symbol(r.from), graph.symbol(r.to)) else {
            continue;
        };
        // The Imports edge `from` is a File symbol; skip if not.
        let Some(src_fqn) = file_fqn.get(&from.id) else {
            continue;
        };
        let tgt = to.name.as_str();
        if export.contains_key(tgt) && tgt != src_fqn.as_str() {
            adj.entry(src_fqn.clone())
                .or_default()
                .insert(tgt.to_string());
        }
    }
    adj
}

/// fqn -> a representative File span (first File with that fqn), for findings.
pub fn fqn_spans(graph: &IrGraph) -> HashMap<String, Span> {
    let mut spans = HashMap::new();
    for s in graph.symbols() {
        if matches!(s.kind, SymbolKind::File) {
            spans.entry(s.fqn.clone()).or_insert_with(|| s.span.clone());
        }
    }
    spans
}

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
    // file path -> that file's own fqn (its graph node key).
    let file_fqn_by_path: HashMap<&str, &str> = graph
        .symbols()
        .iter()
        .filter(|s| matches!(s.kind, SymbolKind::File))
        .map(|s| (s.span.file.as_str(), s.fqn.as_str()))
        .collect();

    // export key -> owning file's fqn. A File's own fqn owns itself; a Class or
    // Module's fqn is owned by whichever File shares its span's file. Resolving
    // through to the owning file matters for languages whose import syntax names
    // a class/module rather than a file path (Java's `import pkg.Class;`): the
    // node the DFS below can actually reach is a File fqn, so a class-fqn import
    // target has to land on that same file, not on the class's own name.
    let mut export: HashMap<&str, &str> = HashMap::new();
    for s in graph.symbols() {
        match s.kind {
            SymbolKind::File => {
                export.entry(s.fqn.as_str()).or_insert(s.fqn.as_str());
            }
            SymbolKind::Class | SymbolKind::Module => {
                if let Some(&owner) = file_fqn_by_path.get(s.span.file.as_str()) {
                    export.entry(s.fqn.as_str()).or_insert(owner);
                }
            }
            _ => {}
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
        if let Some(&owner_fqn) = export.get(to.name.as_str()) {
            if owner_fqn != src_fqn.as_str() {
                adj.entry(src_fqn.clone())
                    .or_default()
                    .insert(owner_fqn.to_string());
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{add_import, add_sym, file_sym};
    use stratify_core::SymbolKind;

    /// Java-shaped case: a File's own fqn is its raw path, but the import
    /// target names a Class whose fqn is `package.ClassName` — a different
    /// namespace. The edge must resolve through to the Class's *owning file*,
    /// not sit on the Class's own fqn (which is never a graph node, since
    /// nodes are seeded only from File symbols).
    #[test]
    fn class_fqn_import_resolves_to_owning_file() {
        let mut g = IrGraph::new();
        let a_file = file_sym(&mut g, "pkga/ClassA.java");
        let b_file = file_sym(&mut g, "pkgb/ClassB.java");
        add_sym(
            &mut g,
            SymbolKind::Class,
            "ClassA",
            "pkga.ClassA",
            "pkga/ClassA.java",
        );
        add_sym(
            &mut g,
            SymbolKind::Class,
            "ClassB",
            "pkgb.ClassB",
            "pkgb/ClassB.java",
        );
        add_import(&mut g, a_file, "pkgb.ClassB"); // ClassA.java imports ClassB
        add_import(&mut g, b_file, "pkga.ClassA"); // ClassB.java imports ClassA

        let adj = fqn_import_graph(&g);
        assert_eq!(
            adj.get("pkga/ClassA.java").map(|s| s.len()),
            Some(1),
            "adj: {adj:?}"
        );
        assert!(adj["pkga/ClassA.java"].contains("pkgb/ClassB.java"));
        assert!(adj["pkgb/ClassB.java"].contains("pkga/ClassA.java"));
    }

    #[test]
    fn self_consistent_fqn_still_matches_file_import_graph_shape() {
        // Go-shaped case: File fqn already equals what imports resolve
        // against, so this must keep working exactly as before.
        let mut g = IrGraph::new();
        let a = file_sym(&mut g, "a.go");
        let b = file_sym(&mut g, "b.go");
        let _ = (a, b);
        add_import(&mut g, a, "b.go");
        let adj = fqn_import_graph(&g);
        assert!(adj["a.go"].contains("b.go"));
    }
}

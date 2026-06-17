use stratify_core::ir::SymbolId;
use stratify_core::{Confidence, IrGraph, RefKind, Reference, Symbol, SymbolKind, Visibility};
use stratify_lang::walk::{self, ComplexityRules, NormalizeRules};
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, Parser, Query, QueryCursor};

/// Leaf-token normalization for Go duplication detection.
///
/// In tree-sitter-go an `interpreted_string_literal` / `raw_string_literal`
/// decomposes into quote (or backtick) delimiter leaves plus a content leaf, so
/// those parent kinds are never leaves themselves. Listing them in
/// `string_kinds` is therefore a no-op that documents intent; the delimiters
/// emit as literal tokens and the content emits as raw text, which is the
/// historical output. Only `rune_literal` is a true leaf that collapses to STR.
const NORMALIZE_RULES: NormalizeRules = NormalizeRules {
    identifier_kinds: &[
        "identifier",
        "field_identifier",
        "type_identifier",
        "package_identifier",
    ],
    number_kinds: &["int_literal", "float_literal", "imaginary_literal"],
    string_kinds: &[
        "interpreted_string_literal",
        "raw_string_literal",
        "rune_literal",
    ],
    atomic_string_kinds: &[],
};

/// Named decision-point kinds for cyclomatic complexity. `&&` / `||` are
/// unnamed operator leaves counted via `operator_texts`.
const COMPLEXITY_RULES: ComplexityRules = ComplexityRules {
    decision_kinds: &[
        "if_statement",
        "for_statement",
        "expression_case",
        "type_case",
        "communication_case",
    ],
    operator_texts: &["&&", "||"],
};

fn package_dir(path: &str) -> String {
    std::path::Path::new(path)
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default()
}

fn is_exported_go(name: &str) -> bool {
    name.chars()
        .next()
        .map(|c| c.is_uppercase())
        .unwrap_or(false)
}

/// Find the method that lexically encloses `node` by matching byte ranges against
/// known Function symbols in this file's graph.
fn enclosing_method_id(node: Node, g: &IrGraph, file: &str) -> Option<SymbolId> {
    let pos = node.start_byte();
    g.symbols()
        .iter()
        .filter(|s| matches!(s.kind, SymbolKind::Function) && s.span.file == file)
        .filter(|s| s.span.start_byte <= pos && pos < s.span.end_byte)
        // Innermost enclosing method = smallest span.
        .min_by_key(|s| s.span.end_byte - s.span.start_byte)
        .map(|s| s.id)
}

/// Add the File symbol. Its fqn is the package directory (parent dir of the path).
fn add_file_symbol(g: &mut IrGraph, file: &str, root: Node) -> SymbolId {
    g.add_symbol(Symbol {
        id: SymbolId(0),
        kind: SymbolKind::File,
        name: file.to_string(),
        fqn: package_dir(file),
        span: walk::span(root, file),
        visibility: Visibility::Unknown,
        confidence: Confidence::Certain,
    })
}

/// Emit a Dependency symbol per import path and an Imports edge from the file.
/// The query matches both single imports and grouped imports via import_spec.
fn extract_imports(g: &mut IrGraph, file: &str, src: &str, root: Node, file_id: SymbolId) {
    let lang: tree_sitter::Language = tree_sitter_go::LANGUAGE.into();
    let import_q = Query::new(
        &lang,
        r#"(import_spec path: (interpreted_string_literal) @path)"#,
    )
    .expect("go import query");
    let path_idx = import_q.capture_index_for_name("path").unwrap();
    let mut imp_cursor = QueryCursor::new();
    let mut imp_matches = imp_cursor.matches(&import_q, root, src.as_bytes());
    while let Some(m) = imp_matches.next() {
        for cap in m.captures {
            if cap.index != path_idx {
                continue;
            }
            let raw = walk::node_text(cap.node, src);
            // Strip surrounding double-quotes (or backticks for raw string imports).
            let import_path = raw
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .or_else(|| raw.strip_prefix('`').and_then(|s| s.strip_suffix('`')))
                .unwrap_or(raw);
            let dep_id = g.add_symbol(Symbol {
                id: SymbolId(0),
                kind: SymbolKind::Dependency,
                name: import_path.to_string(),
                fqn: import_path.to_string(),
                span: walk::span(cap.node, file),
                visibility: Visibility::Unknown,
                confidence: Confidence::Certain,
            });
            g.add_reference(Reference {
                from: file_id,
                to: dep_id,
                kind: RefKind::Imports,
                span: walk::span(cap.node, file),
                confidence: Confidence::Certain,
            });
        }
    }
}

/// Extract function/method/type definitions: add symbols, Defines edges (from
/// the file), entrypoint marks, and function complexity.
fn extract_definitions(g: &mut IrGraph, file: &str, src: &str, root: Node, file_id: SymbolId) {
    let lang: tree_sitter::Language = tree_sitter_go::LANGUAGE.into();
    let query = Query::new(
        &lang,
        r#"
        (function_declaration name: (identifier) @func.name) @func.node
        (method_declaration name: (field_identifier) @method.name) @method.node
        (type_spec name: (type_identifier) @type.name) @type.node
        "#,
    )
    .expect("go query");

    let func_name_idx = query.capture_index_for_name("func.name").unwrap();
    let func_node_idx = query.capture_index_for_name("func.node").unwrap();
    let method_name_idx = query.capture_index_for_name("method.name").unwrap();
    let method_node_idx = query.capture_index_for_name("method.node").unwrap();
    let type_name_idx = query.capture_index_for_name("type.name").unwrap();
    let type_node_idx = query.capture_index_for_name("type.node").unwrap();

    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, root, src.as_bytes());
    while let Some(m) = matches.next() {
        let mut name_node = None;
        let mut decl_node = None;
        let mut kind = SymbolKind::Function;

        for cap in m.captures {
            if cap.index == func_name_idx {
                name_node = Some(cap.node);
                kind = SymbolKind::Function;
            } else if cap.index == func_node_idx {
                decl_node = Some(cap.node);
            } else if cap.index == method_name_idx {
                name_node = Some(cap.node);
                kind = SymbolKind::Function;
            } else if cap.index == method_node_idx {
                decl_node = Some(cap.node);
            } else if cap.index == type_name_idx {
                name_node = Some(cap.node);
                kind = SymbolKind::Class;
            } else if cap.index == type_node_idx {
                decl_node = Some(cap.node);
            }
        }

        let (Some(name_node), Some(decl_node)) = (name_node, decl_node) else {
            continue;
        };
        add_definition(g, file, src, file_id, kind, name_node, decl_node);
    }
}

/// Add one definition symbol with its Defines edge plus, for functions, the
/// entrypoint mark (main/init/exported) and cyclomatic complexity.
fn add_definition(
    g: &mut IrGraph,
    file: &str,
    src: &str,
    file_id: SymbolId,
    kind: SymbolKind,
    name_node: Node,
    decl_node: Node,
) {
    let name = walk::node_text(name_node, src).to_string();
    let id = g.add_symbol(Symbol {
        id: SymbolId(0),
        kind,
        name: name.clone(),
        fqn: name.clone(),
        span: walk::span(decl_node, file),
        visibility: Visibility::Unknown,
        confidence: Confidence::Certain,
    });
    g.add_reference(Reference {
        from: file_id,
        to: id,
        kind: RefKind::Defines,
        span: walk::span(decl_node, file),
        confidence: Confidence::Certain,
    });
    if kind == SymbolKind::Function {
        // Entrypoints: main, init, or exported (capitalized).
        if name == "main" || name == "init" || is_exported_go(&name) {
            g.mark_entrypoint(id);
        }
        g.set_complexity(id, walk::cyclomatic(decl_node, src, &COMPLEXITY_RULES));
    }
}

/// Extract calls: intra-file Calls edges (resolved by function name) plus
/// unresolved cross-file calls. `from` is the enclosing method or the file.
fn extract_calls(g: &mut IrGraph, file: &str, src: &str, root: Node, file_id: SymbolId) {
    let lang: tree_sitter::Language = tree_sitter_go::LANGUAGE.into();
    // Build a map of Function name -> SymbolId.
    let name_to_id: std::collections::HashMap<String, SymbolId> = g
        .symbols()
        .iter()
        .filter(|s| matches!(s.kind, SymbolKind::Function))
        .map(|s| (s.name.clone(), s.id))
        .collect();

    let call_q = Query::new(
        &lang,
        r#"
        (call_expression function: (identifier) @callee) @call
        (call_expression function: (selector_expression field: (field_identifier) @callee)) @call
        "#,
    )
    .expect("go call query");

    let callee_idx = call_q.capture_index_for_name("callee").unwrap();
    let call_idx = call_q.capture_index_for_name("call").unwrap();

    let mut call_cursor = QueryCursor::new();
    let mut call_matches = call_cursor.matches(&call_q, root, src.as_bytes());
    let mut edges: Vec<(SymbolId, SymbolId)> = Vec::new();
    let mut unresolved: Vec<(SymbolId, String)> = Vec::new();
    while let Some(m) = call_matches.next() {
        let mut callee_name = None;
        let mut call_node = None;
        for cap in m.captures {
            if cap.index == callee_idx {
                callee_name = Some(walk::node_text(cap.node, src).to_string());
            } else if cap.index == call_idx {
                call_node = Some(cap.node);
            }
        }
        let (Some(callee_name), Some(call_node)) = (callee_name, call_node) else {
            continue;
        };
        let from = enclosing_method_id(call_node, g, file).unwrap_or(file_id);
        if let Some(&callee_id) = name_to_id.get(&callee_name) {
            edges.push((from, callee_id));
        } else {
            unresolved.push((from, callee_name));
        }
    }

    // Deduplicate and emit Calls edges.
    edges.sort_unstable();
    edges.dedup();
    for (from, to) in edges {
        g.add_reference(Reference {
            from,
            to,
            kind: RefKind::Calls,
            span: walk::span(root, file),
            confidence: Confidence::Likely,
        });
    }

    // Record unresolved (cross-file) calls.
    unresolved.sort_unstable();
    unresolved.dedup();
    for (from, name) in unresolved {
        g.add_unresolved_call(from, name);
    }
}

pub(crate) fn extract(file: &str, src: &str) -> IrGraph {
    let lang: tree_sitter::Language = tree_sitter_go::LANGUAGE.into();

    let mut parser = Parser::new();
    parser.set_language(&lang).expect("load go grammar");
    let tree = parser.parse(src, None).expect("parse go");
    let root = tree.root_node();

    let mut g = IrGraph::new();

    // File symbol — fqn is the package directory (parent dir of the path).
    let file_id = add_file_symbol(&mut g, file, root);

    walk::tokenize(&mut g, root, src, file, &NORMALIZE_RULES);

    extract_imports(&mut g, file, src, root, file_id);
    extract_definitions(&mut g, file, src, root, file_id);
    extract_calls(&mut g, file, src, root, file_id);

    g
}

#[cfg(test)]
mod tests {
    use super::*;
    use stratify_core::{RefKind, SymbolKind};

    #[test]
    fn extracts_func_method_type() {
        let src = "package main\n\ntype Foo struct{}\n\nfunc (f Foo) Bar() {}\n\nfunc baz() {}\n";
        let g = extract("foo.go", src);
        let names: Vec<_> = g
            .symbols()
            .iter()
            .map(|s| (s.kind, s.name.as_str()))
            .collect();
        assert!(names.contains(&(SymbolKind::File, "foo.go")));
        assert!(names.contains(&(SymbolKind::Class, "Foo")));
        assert!(names.contains(&(SymbolKind::Function, "Bar")));
        assert!(names.contains(&(SymbolKind::Function, "baz")));
    }

    #[test]
    fn emits_normalized_tokens() {
        let g = extract("a.go", "package main\nvar x = 5\n");
        let norms: Vec<&str> = g.tokens().iter().map(|t| t.norm.as_str()).collect();
        assert!(norms.contains(&"ID")); // x / package name
        assert!(norms.contains(&"NUM")); // 5
        assert!(norms.contains(&"package"));
    }

    #[test]
    fn main_init_and_exported_are_entrypoints() {
        let src =
            "package main\nfunc main() {}\nfunc init() {}\nfunc Exported() {}\nfunc helper() {}\n";
        let g = extract("m.go", src);
        let id = |name: &str| g.symbols().iter().find(|s| s.name == name).unwrap().id;
        let eps = g.entrypoints();
        assert!(eps.contains(&id("main")));
        assert!(eps.contains(&id("init")));
        assert!(eps.contains(&id("Exported")));
        assert!(
            !eps.contains(&id("helper")),
            "unexported helper is not an entrypoint"
        );
    }

    #[test]
    fn intra_file_call_edge() {
        let src = "package main\nfunc a() { b() }\nfunc b() {}\n";
        let g = extract("x.go", src);
        let a = g.symbols().iter().find(|s| s.name == "a").unwrap().id;
        let b = g.symbols().iter().find(|s| s.name == "b").unwrap().id;
        assert!(g
            .references()
            .iter()
            .any(|r| matches!(r.kind, RefKind::Calls) && r.from == a && r.to == b));
    }

    #[test]
    fn computes_complexity() {
        // base 1 + if + && + for = 4
        let src = "package main\nfunc m(x int) {\n  if x > 0 && x < 9 {\n  }\n  for {\n  }\n}\n";
        let g = extract("c.go", src);
        let m = g.symbols().iter().find(|s| s.name == "m").unwrap().id;
        assert_eq!(g.complexity_of(m), Some(4));
    }

    #[test]
    fn records_unresolved_cross_file_call() {
        // `external` is not defined in this file -> recorded as unresolved.
        let g = extract("a.go", "package main\nfunc m() { external() }\n");
        let m_id = g.symbols().iter().find(|s| s.name == "m").unwrap().id;
        assert!(
            g.unresolved_calls()
                .iter()
                .any(|(from, name)| *from == m_id && name == "external"),
            "expected unresolved call (m, external); got {:?}",
            g.unresolved_calls()
        );
    }

    #[test]
    fn file_fqn_is_package_dir() {
        let g = extract("internal/svc/a.go", "package svc\n");
        let f = g
            .symbols()
            .iter()
            .find(|s| s.kind == SymbolKind::File)
            .unwrap();
        assert_eq!(f.fqn, "internal/svc");
    }

    #[test]
    fn top_level_file_fqn_is_empty() {
        let g = extract("main.go", "package main\n");
        let f = g
            .symbols()
            .iter()
            .find(|s| s.kind == SymbolKind::File)
            .unwrap();
        assert_eq!(f.fqn, "");
    }

    #[test]
    fn emits_import_dependency_with_raw_path() {
        let g = extract("a/a.go", "package a\nimport \"example.com/m/b\"\n");
        let dep = g
            .symbols()
            .iter()
            .find(|s| s.kind == SymbolKind::Dependency && s.name == "example.com/m/b");
        assert!(
            dep.is_some(),
            "expected import Dependency for example.com/m/b"
        );
        let file_id = g
            .symbols()
            .iter()
            .find(|s| s.kind == SymbolKind::File)
            .unwrap()
            .id;
        assert!(g
            .references()
            .iter()
            .any(|r| matches!(r.kind, RefKind::Imports)
                && r.from == file_id
                && r.to == dep.unwrap().id));
    }

    #[test]
    fn emits_grouped_imports() {
        let g = extract("a/a.go", "package a\nimport (\n  \"x/y\"\n  \"p/q\"\n)\n");
        let names: Vec<&str> = g
            .symbols()
            .iter()
            .filter(|s| s.kind == SymbolKind::Dependency)
            .map(|s| s.name.as_str())
            .collect();
        assert!(names.contains(&"x/y"));
        assert!(names.contains(&"p/q"));
    }
}

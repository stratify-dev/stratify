use stratify_core::ir::{Span, SymbolId};
use stratify_core::{
    Confidence, IrGraph, RefKind, Reference, Symbol, SymbolKind, Token, Visibility,
};
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, Parser, Query, QueryCursor};

fn is_exported_go(name: &str) -> bool {
    name.chars()
        .next()
        .map(|c| c.is_uppercase())
        .unwrap_or(false)
}

fn count_decisions_go(node: Node) -> u32 {
    let mut count = 0u32;
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "if_statement" | "for_statement" | "expression_case" | "type_case"
            | "communication_case" | "&&" | "||" => {
                count += 1;
            }
            _ => {}
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }
    count
}

fn cyclomatic_go(node: Node) -> u32 {
    1 + count_decisions_go(node)
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

fn span(file: &str, node: Node) -> Span {
    Span {
        file: file.to_string(),
        start_byte: node.start_byte(),
        end_byte: node.end_byte(),
        start_line: node.start_position().row + 1,
    }
}

fn text<'a>(node: Node, src: &'a str) -> &'a str {
    node.utf8_text(src.as_bytes()).unwrap_or("")
}

fn normalize_go(kind: &str, text: &str) -> String {
    match kind {
        "identifier" | "field_identifier" | "type_identifier" | "package_identifier" => {
            "ID".to_string()
        }
        "int_literal" | "float_literal" | "imaginary_literal" => "NUM".to_string(),
        "interpreted_string_literal" | "raw_string_literal" | "rune_literal" => "STR".to_string(),
        _ => text.to_string(),
    }
}

fn collect_leaves<'a>(node: Node<'a>, out: &mut Vec<Node<'a>>) {
    if node.child_count() == 0 {
        out.push(node);
        return;
    }
    let mut c = node.walk();
    for child in node.children(&mut c) {
        collect_leaves(child, out);
    }
}

fn emit_tokens(g: &mut IrGraph, file: &str, src: &str, root: Node) {
    let mut leaves: Vec<Node> = Vec::new();
    collect_leaves(root, &mut leaves);
    for leaf in leaves {
        if leaf.start_byte() >= leaf.end_byte() {
            continue;
        }
        let t = text(leaf, src);
        if t.trim().is_empty() {
            continue;
        }
        let norm = normalize_go(leaf.kind(), t);
        g.add_token(Token {
            file: file.to_string(),
            start_byte: leaf.start_byte(),
            end_byte: leaf.end_byte(),
            start_line: leaf.start_position().row + 1,
            norm,
        });
    }
}

pub(crate) fn extract(file: &str, src: &str) -> IrGraph {
    let lang: tree_sitter::Language = tree_sitter_go::LANGUAGE.into();

    let mut parser = Parser::new();
    parser.set_language(&lang).expect("load go grammar");
    let tree = parser.parse(src, None).expect("parse go");
    let root = tree.root_node();

    let mut g = IrGraph::new();

    // File symbol — fqn is the path as-is (no import resolution needed).
    let file_id = g.add_symbol(Symbol {
        id: SymbolId(0),
        kind: SymbolKind::File,
        name: file.to_string(),
        fqn: file.to_string(),
        span: span(file, root),
        visibility: Visibility::Unknown,
        confidence: Confidence::Certain,
    });

    emit_tokens(&mut g, file, src, root);

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

        if let (Some(name_node), Some(decl_node)) = (name_node, decl_node) {
            let name = text(name_node, src).to_string();
            let id = g.add_symbol(Symbol {
                id: SymbolId(0),
                kind,
                name: name.clone(),
                fqn: name.clone(),
                span: span(file, decl_node),
                visibility: Visibility::Unknown,
                confidence: Confidence::Certain,
            });
            g.add_reference(Reference {
                from: file_id,
                to: id,
                kind: RefKind::Defines,
                span: span(file, decl_node),
                confidence: Confidence::Certain,
            });
            if kind == SymbolKind::Function {
                // Entrypoints: main, init, or exported (capitalized).
                if name == "main" || name == "init" || is_exported_go(&name) {
                    g.mark_entrypoint(id);
                }
                // Cyclomatic complexity.
                g.set_complexity(id, cyclomatic_go(decl_node));
            }
        }
    }

    // Second pass: intra-file calls. Build a map of Function name -> SymbolId.
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
                callee_name = Some(text(cap.node, src).to_string());
            } else if cap.index == call_idx {
                call_node = Some(cap.node);
            }
        }
        let (Some(callee_name), Some(call_node)) = (callee_name, call_node) else {
            continue;
        };
        let from = enclosing_method_id(call_node, &g, file).unwrap_or(file_id);
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
            span: span(file, root),
            confidence: Confidence::Likely,
        });
    }

    // Record unresolved (cross-file) calls.
    unresolved.sort_unstable();
    unresolved.dedup();
    for (from, name) in unresolved {
        g.add_unresolved_call(from, name);
    }

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
}

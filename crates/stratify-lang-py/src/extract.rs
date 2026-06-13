use stratify_core::ir::{Span, SymbolId};
use stratify_core::{
    Confidence, IrGraph, RefKind, Reference, Symbol, SymbolKind, Token, Visibility,
};
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, Parser, Query, QueryCursor};

/// Strip a trailing .py or .pyi extension from a file path.
fn strip_py_ext(path: &str) -> String {
    for ext in [".pyi", ".py"] {
        if let Some(stripped) = path.strip_suffix(ext) {
            return stripped.to_string();
        }
    }
    path.to_string()
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

fn normalize_py(kind: &str, text: &str) -> String {
    match kind {
        "identifier" => "ID".to_string(),
        "integer" | "float" => "NUM".to_string(),
        "string" | "string_content" | "concatenated_string" => "STR".to_string(),
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
        let norm = normalize_py(leaf.kind(), t);
        g.add_token(Token {
            file: file.to_string(),
            start_byte: leaf.start_byte(),
            end_byte: leaf.end_byte(),
            start_line: leaf.start_position().row + 1,
            norm,
        });
    }
}

/// Count decision points in a subtree for cyclomatic complexity.
fn count_decisions_py(node: Node) -> u32 {
    let mut count = 0u32;
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "if_statement" | "elif_clause" | "for_statement" | "while_statement"
            | "except_clause" | "conditional_expression" | "boolean_operator"
            | "case_clause" => {
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

fn cyclomatic_py(node: Node) -> u32 {
    1 + count_decisions_py(node)
}

/// Find the function that lexically encloses `node` by matching byte ranges against
/// known Function symbols in this file's graph.
fn enclosing_method_id(node: Node, g: &IrGraph, file: &str) -> Option<SymbolId> {
    let pos = node.start_byte();
    g.symbols()
        .iter()
        .filter(|s| matches!(s.kind, SymbolKind::Function) && s.span.file == file)
        .filter(|s| s.span.start_byte <= pos && pos < s.span.end_byte)
        // Innermost enclosing function = smallest span.
        .min_by_key(|s| s.span.end_byte - s.span.start_byte)
        .map(|s| s.id)
}

pub(crate) fn extract(file: &str, src: &str) -> IrGraph {
    let lang: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();

    let mut parser = Parser::new();
    parser.set_language(&lang).expect("load python grammar");
    let tree = parser.parse(src, None).expect("parse python");
    let root = tree.root_node();

    let mut g = IrGraph::new();

    // File symbol — fqn is the path with Python extension stripped.
    let file_id = g.add_symbol(Symbol {
        id: SymbolId(0),
        kind: SymbolKind::File,
        name: file.to_string(),
        fqn: strip_py_ext(file),
        span: span(file, root),
        visibility: Visibility::Unknown,
        confidence: Confidence::Certain,
    });

    // The file is always an entrypoint (Python has no exports; top-level code runs on import).
    g.mark_entrypoint(file_id);

    emit_tokens(&mut g, file, src, root);

    let query = Query::new(
        &lang,
        r#"
        (class_definition name: (identifier) @class.name) @class.node
        (function_definition name: (identifier) @func.name) @func.node
        "#,
    )
    .expect("py definition query");

    let class_name_idx = query.capture_index_for_name("class.name").unwrap();
    let class_node_idx = query.capture_index_for_name("class.node").unwrap();
    let func_name_idx = query.capture_index_for_name("func.name").unwrap();
    let func_node_idx = query.capture_index_for_name("func.node").unwrap();

    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, root, src.as_bytes());
    while let Some(m) = matches.next() {
        let mut name_node = None;
        let mut decl_node = None;
        let mut kind = SymbolKind::Function;

        for cap in m.captures {
            if cap.index == class_name_idx {
                name_node = Some(cap.node);
                kind = SymbolKind::Class;
            } else if cap.index == class_node_idx {
                decl_node = Some(cap.node);
            } else if cap.index == func_name_idx {
                name_node = Some(cap.node);
                kind = SymbolKind::Function;
            } else if cap.index == func_node_idx {
                decl_node = Some(cap.node);
            }
        }

        if let (Some(name_node), Some(decl_node)) = (name_node, decl_node) {
            let name = text(name_node, src).to_string();
            let id = g.add_symbol(Symbol {
                id: SymbolId(0),
                kind,
                name: name.clone(),
                fqn: name,
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
            // Set complexity for function definitions.
            if kind == SymbolKind::Function {
                g.set_complexity(id, cyclomatic_py(decl_node));
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
        (call function: (identifier) @callee) @call
        (call function: (attribute attribute: (identifier) @callee)) @call
        "#,
    )
    .expect("py call query");

    let callee_idx = call_q.capture_index_for_name("callee").unwrap();
    let call_idx = call_q.capture_index_for_name("call").unwrap();

    let mut call_cursor = QueryCursor::new();
    let mut call_matches = call_cursor.matches(&call_q, root, src.as_bytes());
    let mut edges: Vec<(SymbolId, SymbolId)> = Vec::new();
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
        let Some(&callee_id) = name_to_id.get(&callee_name) else {
            continue;
        };
        let from = enclosing_method_id(call_node, &g, file).unwrap_or(file_id);
        edges.push((from, callee_id));
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

    g
}

#[cfg(test)]
mod tests {
    use super::*;
    use stratify_core::SymbolKind;

    #[test]
    fn file_is_entrypoint() {
        let g = extract("m.py", "def a():\n    pass\n");
        let file = g.symbols().iter().find(|s| s.kind == SymbolKind::File).unwrap().id;
        assert_eq!(g.entrypoints(), &[file]);
    }

    #[test]
    fn intra_file_and_top_level_calls() {
        let src = "def a():\n    b()\n\ndef b():\n    pass\n\na()\n";
        let g = extract("x.py", src);
        let file = g.symbols().iter().find(|s| s.kind == SymbolKind::File).unwrap().id;
        let a = g.symbols().iter().find(|s| s.name == "a").unwrap().id;
        let b = g.symbols().iter().find(|s| s.name == "b").unwrap().id;
        // a calls b
        assert!(g.references().iter().any(|r| matches!(r.kind, RefKind::Calls) && r.from == a && r.to == b));
        // file (top-level) calls a
        assert!(g.references().iter().any(|r| matches!(r.kind, RefKind::Calls) && r.from == file && r.to == a));
    }

    #[test]
    fn computes_complexity() {
        // base 1 + if + `and` + for = 4
        let src = "def m(x):\n    if x > 0 and x < 9:\n        return 1\n    for i in range(3):\n        pass\n    return 0\n";
        let g = extract("c.py", src);
        let m = g.symbols().iter().find(|s| s.name == "m").unwrap().id;
        assert_eq!(g.complexity_of(m), Some(4));
    }

    #[test]
    fn extracts_class_function_method() {
        let src = "class Foo:\n    def bar(self):\n        pass\n\ndef baz():\n    pass\n";
        let g = extract("foo.py", src);
        let names: Vec<_> = g.symbols().iter().map(|s| (s.kind, s.name.as_str())).collect();
        assert!(names.contains(&(SymbolKind::File, "foo.py")));
        assert!(names.contains(&(SymbolKind::Class, "Foo")));
        assert!(names.contains(&(SymbolKind::Function, "bar")));
        assert!(names.contains(&(SymbolKind::Function, "baz")));
    }

    #[test]
    fn file_fqn_strips_extension() {
        let g = extract("pkg/mod.py", "def x():\n    pass\n");
        let f = g.symbols().iter().find(|s| s.kind == SymbolKind::File).unwrap();
        assert_eq!(f.fqn, "pkg/mod");
    }

    #[test]
    fn emits_normalized_tokens() {
        let g = extract("a.py", "x = 5\n");
        let norms: Vec<&str> = g.tokens().iter().map(|t| t.norm.as_str()).collect();
        assert!(norms.contains(&"ID"));   // x
        assert!(norms.contains(&"NUM"));  // 5
        assert!(norms.contains(&"="));
    }
}

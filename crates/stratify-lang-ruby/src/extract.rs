use stratify_core::ir::{Span, SymbolId};
use stratify_core::{
    Confidence, IrGraph, RefKind, Reference, Symbol, SymbolKind, Token, Visibility,
};
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, Parser, Query, QueryCursor};

pub(crate) fn parser() -> Parser {
    let mut p = Parser::new();
    p.set_language(&tree_sitter_ruby::LANGUAGE.into())
        .expect("load ruby grammar");
    p
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

fn normalize_ruby(kind: &str, text: &str) -> String {
    match kind {
        "identifier" | "constant" | "instance_variable" | "global_variable" | "class_variable" => {
            "ID".to_string()
        }
        "integer" | "float" => "NUM".to_string(),
        "string_content" | "string" | "simple_symbol" => "STR".to_string(),
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
        let text = text(leaf, src);
        if text.trim().is_empty() {
            continue;
        }
        let norm = normalize_ruby(leaf.kind(), text);
        g.add_token(Token {
            file: file.to_string(),
            start_byte: leaf.start_byte(),
            end_byte: leaf.end_byte(),
            start_line: leaf.start_position().row + 1,
            norm,
        });
    }
}

fn count_decisions_ruby(node: Node) -> u32 {
    let mut count = 0u32;
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if n.is_named() {
            match n.kind() {
                "if" | "elsif" | "unless" | "while" | "until" | "for" | "when"
                | "rescue" | "conditional" | "if_modifier" | "unless_modifier"
                | "while_modifier" | "until_modifier" => {
                    count += 1;
                }
                _ => {}
            }
        } else {
            match n.kind() {
                "&&" | "||" | "and" | "or" => {
                    count += 1;
                }
                _ => {}
            }
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }
    count
}

fn cyclomatic_ruby(node: Node) -> u32 {
    1 + count_decisions_ruby(node)
}

/// Extract modules, classes, and methods into a per-file graph. The file itself
/// becomes a `File` symbol; all top-level and nested definitions get `Defines` edges from it.
pub(crate) fn extract(file: &str, src: &str) -> IrGraph {
    let mut parser = parser();
    let tree = parser.parse(src, None).expect("parse ruby");
    let root = tree.root_node();

    let mut g = IrGraph::new();

    // File symbol.
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
        &tree_sitter_ruby::LANGUAGE.into(),
        r#"
        (method name: (identifier) @method.name) @method.node
        (class name: (constant) @class.name) @class.node
        (module name: (constant) @module.name) @module.node
        "#,
    )
    .expect("valid ruby query");

    let mut cursor = QueryCursor::new();
    let method_name_idx = query.capture_index_for_name("method.name").unwrap();
    let method_node_idx = query.capture_index_for_name("method.node").unwrap();
    let class_name_idx = query.capture_index_for_name("class.name").unwrap();
    let class_node_idx = query.capture_index_for_name("class.node").unwrap();
    let module_name_idx = query.capture_index_for_name("module.name").unwrap();
    let module_node_idx = query.capture_index_for_name("module.node").unwrap();

    let mut matches = cursor.matches(&query, root, src.as_bytes());
    while let Some(m) = matches.next() {
        let mut name_node = None;
        let mut decl_node = None;
        let mut kind = SymbolKind::Function;
        for cap in m.captures {
            if cap.index == method_name_idx {
                name_node = Some(cap.node);
                kind = SymbolKind::Function;
            } else if cap.index == method_node_idx {
                decl_node = Some(cap.node);
            } else if cap.index == class_name_idx {
                name_node = Some(cap.node);
                kind = SymbolKind::Class;
            } else if cap.index == class_node_idx {
                decl_node = Some(cap.node);
            } else if cap.index == module_name_idx {
                name_node = Some(cap.node);
                kind = SymbolKind::Module;
            } else if cap.index == module_node_idx {
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
            if kind == SymbolKind::Function {
                let cx = cyclomatic_ruby(decl_node);
                g.set_complexity(id, cx);
            }
        }
    }

    // Mark the file as an entrypoint (Ruby top-level code is the execution entry).
    g.mark_entrypoint(file_id);

    // Second pass: intra-file calls. Collect all Function symbols.
    let name_to_id: std::collections::HashMap<String, SymbolId> = g
        .symbols()
        .iter()
        .filter(|s| matches!(s.kind, SymbolKind::Function))
        .map(|s| (s.name.clone(), s.id))
        .collect();

    // Query A: explicit calls like foo(args) or recv.foo.
    let call_query = Query::new(
        &tree_sitter_ruby::LANGUAGE.into(),
        r#"(call method: (identifier) @callee)"#,
    )
    .expect("valid ruby call query");

    let callee_idx = call_query.capture_index_for_name("callee").unwrap();

    let mut call_cursor = QueryCursor::new();
    let mut call_matches = call_cursor.matches(&call_query, root, src.as_bytes());
    let mut edges: Vec<(SymbolId, SymbolId)> = Vec::new();
    while let Some(m) = call_matches.next() {
        for cap in m.captures {
            if cap.index == callee_idx {
                let callee_name = text(cap.node, src);
                if let Some(&callee_id) = name_to_id.get(callee_name) {
                    let from = enclosing_method_id(cap.node, &g, file).unwrap_or(file_id);
                    edges.push((from, callee_id));
                }
            }
        }
    }

    // Query B: bare identifier command calls like `greet` (no parens, no receiver).
    let ident_query = Query::new(&tree_sitter_ruby::LANGUAGE.into(), r#"(identifier) @ident"#)
        .expect("valid ruby ident query");

    let ident_idx = ident_query.capture_index_for_name("ident").unwrap();

    let mut ident_cursor = QueryCursor::new();
    let mut ident_matches = ident_cursor.matches(&ident_query, root, src.as_bytes());
    while let Some(m) = ident_matches.next() {
        for cap in m.captures {
            if cap.index == ident_idx {
                let callee_name = text(cap.node, src);
                // Only keep identifiers that match a known in-file method.
                if let Some(&callee_id) = name_to_id.get(callee_name) {
                    // Skip if this identifier is the method name in a `def` declaration
                    // or is a parameter definition node.
                    let parent_kind = cap.node.parent().map(|p| p.kind()).unwrap_or("");
                    if matches!(
                        parent_kind,
                        "method" | "method_parameters" | "block_parameter" | "keyword_parameter"
                    ) {
                        continue;
                    }
                    let from = enclosing_method_id(cap.node, &g, file).unwrap_or(file_id);
                    edges.push((from, callee_id));
                }
            }
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

    g
}

/// Find the method that lexically encloses `node` by matching byte ranges against
/// known method symbols in this file's graph.
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

#[cfg(test)]
mod tests {
    use super::*;
    use stratify_core::SymbolKind;

    #[test]
    fn emits_normalized_tokens() {
        let src = "def a\n  x = 5\nend\n";
        let g = extract("a.rb", src);
        let norms: Vec<&str> = g.tokens().iter().map(|t| t.norm.as_str()).collect();
        assert!(norms.contains(&"def"));
        assert!(norms.contains(&"ID")); // a / x
        assert!(norms.contains(&"NUM")); // 5
    }

    #[test]
    fn extracts_module_class_method() {
        let src = "module M\n  class Foo\n    def bar\n    end\n  end\nend\n";
        let g = extract("foo.rb", src);
        let kinds: Vec<_> = g
            .symbols()
            .iter()
            .map(|s| (s.kind, s.name.as_str()))
            .collect();
        assert!(kinds.contains(&(SymbolKind::File, "foo.rb")));
        assert!(kinds.contains(&(SymbolKind::Module, "M")));
        assert!(kinds.contains(&(SymbolKind::Class, "Foo")));
        assert!(kinds.contains(&(SymbolKind::Function, "bar")));
    }

    #[test]
    fn file_defines_each_member() {
        let src = "class A\n  def m\n  end\nend\n";
        let g = extract("a.rb", src);
        // Defines edges: class A, method m => 2.
        assert_eq!(
            g.references()
                .iter()
                .filter(|r| matches!(r.kind, RefKind::Defines))
                .count(),
            2
        );
    }

    #[test]
    fn marks_file_as_entrypoint() {
        let src = "def a\nend\n";
        let g = extract("x.rb", src);
        let file_id = g
            .symbols()
            .iter()
            .find(|s| s.kind == SymbolKind::File)
            .unwrap()
            .id;
        assert_eq!(g.entrypoints(), &[file_id]);
    }

    #[test]
    fn top_level_call_links_file_to_method() {
        // `greet` is defined and called at top level -> File --Calls--> greet.
        let src = "def greet\n  puts 'hi'\nend\n\ngreet\n";
        let g = extract("x.rb", src);
        let file_id = g
            .symbols()
            .iter()
            .find(|s| s.kind == SymbolKind::File)
            .unwrap()
            .id;
        let greet_id = g.symbols().iter().find(|s| s.name == "greet").unwrap().id;
        assert!(g
            .references()
            .iter()
            .any(|r| matches!(r.kind, RefKind::Calls) && r.from == file_id && r.to == greet_id));
    }

    #[test]
    fn computes_method_complexity() {
        // base 1 + if + elsif + while = 4
        let src = "def m(x)\n  if x > 0\n  elsif x < 9\n  end\n  while x > 0\n  end\nend\n";
        let g = extract("m.rb", src);
        let m = g.symbols().iter().find(|s| s.name == "m").unwrap().id;
        assert_eq!(g.complexity_of(m), Some(4));
    }

    #[test]
    fn intra_method_call_links_caller_to_callee() {
        let src = "def a\n  b\nend\n\ndef b\nend\n";
        let g = extract("x.rb", src);
        let a_id = g.symbols().iter().find(|s| s.name == "a").unwrap().id;
        let b_id = g.symbols().iter().find(|s| s.name == "b").unwrap().id;
        assert!(g
            .references()
            .iter()
            .any(|r| matches!(r.kind, RefKind::Calls) && r.from == a_id && r.to == b_id));
    }
}

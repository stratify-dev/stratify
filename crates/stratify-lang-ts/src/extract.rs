use stratify_core::ir::{Span, SymbolId};
use stratify_core::{
    Confidence, IrGraph, RefKind, Reference, Symbol, SymbolKind, Token, Visibility,
};
use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Node, Parser, Query, QueryCursor};

/// Strip a trailing TS/JS extension from a file path, returning the bare module key.
fn strip_ts_ext(path: &str) -> String {
    for ext in [".ts", ".tsx", ".mts", ".cts", ".js", ".jsx"] {
        if let Some(stripped) = path.strip_suffix(ext) {
            return stripped.to_string();
        }
    }
    path.to_string()
}

/// Select the tree-sitter Language for the given file path.
fn lang_for(file: &str) -> Language {
    if file.ends_with(".tsx") {
        tree_sitter_typescript::LANGUAGE_TSX.into()
    } else {
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
    }
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

fn normalize_ts(kind: &str, text: &str) -> String {
    match kind {
        "identifier"
        | "type_identifier"
        | "property_identifier"
        | "shorthand_property_identifier"
        | "shorthand_property_identifier_pattern" => "ID".to_string(),
        "number" => "NUM".to_string(),
        "string" | "template_string" | "string_fragment" => "STR".to_string(),
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
        let norm = normalize_ts(leaf.kind(), t);
        g.add_token(Token {
            file: file.to_string(),
            start_byte: leaf.start_byte(),
            end_byte: leaf.end_byte(),
            start_line: leaf.start_position().row + 1,
            norm,
        });
    }
}

/// Return true if `node` has an ancestor that is an `export_statement`.
fn is_exported(node: Node) -> bool {
    let mut cur = node.parent();
    while let Some(n) = cur {
        if n.kind() == "export_statement" {
            return true;
        }
        cur = n.parent();
    }
    false
}

/// Count decision points in a subtree for cyclomatic complexity.
fn count_decisions_ts(node: Node) -> u32 {
    let mut count = 0u32;
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "if_statement"
            | "for_statement"
            | "for_in_statement"
            | "while_statement"
            | "do_statement"
            | "switch_case"
            | "ternary_expression"
            | "catch_clause"
            | "&&"
            | "||"
            | "??" => {
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

fn cyclomatic_ts(node: Node) -> u32 {
    1 + count_decisions_ts(node)
}

/// Resolve a TypeScript import specifier to an extension-stripped module key.
/// Returns `None` for bare/package specifiers (not starting with `.`).
fn resolve_ts_import(importer_file: &str, spec: &str) -> Option<String> {
    if !spec.starts_with('.') {
        return None;
    }
    use std::path::{Component, Path};
    let dir = Path::new(importer_file)
        .parent()
        .unwrap_or_else(|| Path::new(""));
    let joined = dir.join(spec);
    let mut parts: Vec<String> = Vec::new();
    for comp in joined.components() {
        match comp {
            Component::Normal(s) => parts.push(s.to_string_lossy().to_string()),
            Component::ParentDir => {
                parts.pop();
            }
            _ => {}
        }
    }
    let mut p = parts.join("/");
    for ext in [".ts", ".tsx", ".mts", ".cts", ".js", ".jsx"] {
        if let Some(stripped) = p.strip_suffix(ext) {
            p = stripped.to_string();
            break;
        }
    }
    Some(p)
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

pub(crate) fn extract(file: &str, src: &str) -> IrGraph {
    let lang = lang_for(file);

    let mut parser = Parser::new();
    parser
        .set_language(&lang)
        .expect("load typescript grammar");
    let tree = parser.parse(src, None).expect("parse typescript");
    let root = tree.root_node();

    let mut g = IrGraph::new();

    // File symbol — fqn is the path with TS extension stripped.
    let file_id = g.add_symbol(Symbol {
        id: SymbolId(0),
        kind: SymbolKind::File,
        name: file.to_string(),
        fqn: strip_ts_ext(file),
        span: span(file, root),
        visibility: Visibility::Unknown,
        confidence: Confidence::Certain,
    });

    // The file scope is always an entrypoint (top-level module code runs on import).
    g.mark_entrypoint(file_id);

    emit_tokens(&mut g, file, src, root);

    let query = Query::new(
        &lang,
        r#"
        (class_declaration name: (type_identifier) @class.name) @class.node
        (function_declaration name: (identifier) @func.name) @func.node
        (method_definition name: (property_identifier) @method.name) @method.node
        (variable_declarator name: (identifier) @arrow.name value: (arrow_function)) @arrow.node
        (variable_declarator name: (identifier) @arrow.name value: (function_expression)) @arrow.node
        "#,
    )
    .expect("ts definition query");

    let class_name_idx = query.capture_index_for_name("class.name").unwrap();
    let class_node_idx = query.capture_index_for_name("class.node").unwrap();
    let func_name_idx = query.capture_index_for_name("func.name").unwrap();
    let func_node_idx = query.capture_index_for_name("func.node").unwrap();
    let method_name_idx = query.capture_index_for_name("method.name").unwrap();
    let method_node_idx = query.capture_index_for_name("method.node").unwrap();
    let arrow_name_idx = query.capture_index_for_name("arrow.name").unwrap();
    let arrow_node_idx = query.capture_index_for_name("arrow.node").unwrap();

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
            } else if cap.index == method_name_idx {
                name_node = Some(cap.node);
                kind = SymbolKind::Function;
            } else if cap.index == method_node_idx {
                decl_node = Some(cap.node);
            } else if cap.index == arrow_name_idx {
                name_node = Some(cap.node);
                kind = SymbolKind::Function;
            } else if cap.index == arrow_node_idx {
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
            // Exported symbols are entrypoints (reachable from other modules).
            if is_exported(decl_node) {
                g.mark_entrypoint(id);
            }
            // Set complexity for functions.
            if kind == SymbolKind::Function {
                g.set_complexity(id, cyclomatic_ts(decl_node));
            }
        }
    }

    // Import pass: resolve relative specifiers to Dependency symbols + Imports edges.
    {
        let import_q = Query::new(
            &lang,
            r#"(import_statement source: (string (string_fragment) @spec))"#,
        )
        .expect("ts import query");

        let spec_idx = import_q.capture_index_for_name("spec").unwrap();
        let mut imp_cursor = QueryCursor::new();
        let mut imp_matches = imp_cursor.matches(&import_q, root, src.as_bytes());
        while let Some(m) = imp_matches.next() {
            for cap in m.captures {
                if cap.index != spec_idx {
                    continue;
                }
                let spec_text = text(cap.node, src);
                if let Some(key) = resolve_ts_import(file, spec_text) {
                    let dep_id = g.add_symbol(Symbol {
                        id: SymbolId(0),
                        kind: SymbolKind::Dependency,
                        name: key.clone(),
                        fqn: key,
                        span: span(file, cap.node),
                        visibility: Visibility::Unknown,
                        confidence: Confidence::Certain,
                    });
                    g.add_reference(Reference {
                        from: file_id,
                        to: dep_id,
                        kind: RefKind::Imports,
                        span: span(file, cap.node),
                        confidence: Confidence::Certain,
                    });
                }
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
        (call_expression function: (member_expression property: (property_identifier) @callee)) @call
        "#,
    )
    .expect("ts call query");

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
    fn extracts_class_function_method_arrow() {
        let src =
            "export class Foo {\n  bar() {}\n}\nfunction baz() {}\nconst qux = () => {};\n";
        let g = extract("Foo.ts", src);
        let names: Vec<_> = g
            .symbols()
            .iter()
            .map(|s| (s.kind, s.name.as_str()))
            .collect();
        assert!(names.contains(&(SymbolKind::File, "Foo.ts")));
        assert!(names.contains(&(SymbolKind::Class, "Foo")));
        assert!(names.contains(&(SymbolKind::Function, "bar")));
        assert!(names.contains(&(SymbolKind::Function, "baz")));
        assert!(names.contains(&(SymbolKind::Function, "qux")));
    }

    #[test]
    fn file_fqn_strips_extension() {
        let g = extract("src/a.ts", "function x() {}");
        let f = g
            .symbols()
            .iter()
            .find(|s| s.kind == SymbolKind::File)
            .unwrap();
        assert_eq!(f.fqn, "src/a");
    }

    #[test]
    fn emits_normalized_tokens() {
        let g = extract("a.ts", "const x = 5;");
        let norms: Vec<&str> = g.tokens().iter().map(|t| t.norm.as_str()).collect();
        assert!(norms.contains(&"const"));
        assert!(norms.contains(&"ID")); // x
        assert!(norms.contains(&"NUM")); // 5
    }

    #[test]
    fn tsx_parses() {
        let g = extract("c.tsx", "function C() { return null; }");
        assert!(g.symbols().iter().any(|s| s.name == "C"));
    }

    #[test]
    fn file_and_exports_are_entrypoints() {
        // File scope is always an entrypoint; an exported function is too.
        let src = "export function api() {}\nfunction helper() {}\n";
        let g = extract("m.ts", src);
        let file = g.symbols().iter().find(|s| s.kind == SymbolKind::File).unwrap().id;
        let api = g.symbols().iter().find(|s| s.name == "api").unwrap().id;
        let eps = g.entrypoints();
        assert!(eps.contains(&file));
        assert!(eps.contains(&api), "exported function should be an entrypoint");
    }

    #[test]
    fn intra_file_call_edge() {
        let src = "function a() { b(); }\nfunction b() {}\na();\n";
        let g = extract("x.ts", src);
        let a = g.symbols().iter().find(|s| s.name == "a").unwrap().id;
        let b = g.symbols().iter().find(|s| s.name == "b").unwrap().id;
        assert!(g.references().iter().any(|r|
            matches!(r.kind, RefKind::Calls) && r.from == a && r.to == b));
    }

    #[test]
    fn computes_complexity() {
        // base 1 + if + && + for = 4
        let src = "function m(x: number) { if (x > 0 && x < 9) {} for (;;) {} }";
        let g = extract("c.ts", src);
        let m = g.symbols().iter().find(|s| s.name == "m").unwrap().id;
        assert_eq!(g.complexity_of(m), Some(4));
    }

    #[test]
    fn relative_import_edge() {
        // from src/a.ts, import from "./b" -> key src/b ; bare specifier ignored.
        let g = extract(
            "src/a.ts",
            "import { x } from \"./b\";\nimport React from \"react\";\n",
        );
        let dep = g
            .symbols()
            .iter()
            .find(|s| s.kind == SymbolKind::Dependency && s.name == "src/b");
        assert!(dep.is_some(), "expected Dependency keyed src/b");
        assert!(
            !g.symbols()
                .iter()
                .any(|s| s.kind == SymbolKind::Dependency && s.name == "react"),
            "bare specifier should be ignored"
        );
        let file_id = g
            .symbols()
            .iter()
            .find(|s| s.kind == SymbolKind::File)
            .unwrap()
            .id;
        assert!(g.references().iter().any(|r| matches!(r.kind, RefKind::Imports)
            && r.from == file_id
            && r.to == dep.unwrap().id));
    }

    #[test]
    fn import_key_matches_file_fqn() {
        // a.ts importing "./b" yields key "b"; b.ts has fqn "b" -> they match.
        let importer = extract("a.ts", "import \"./b\";\n");
        let imported = extract("b.ts", "export const z = 1;\n");
        let key = importer
            .symbols()
            .iter()
            .find(|s| s.kind == SymbolKind::Dependency)
            .unwrap()
            .name
            .clone();
        let fqn = imported
            .symbols()
            .iter()
            .find(|s| s.kind == SymbolKind::File)
            .unwrap()
            .fqn
            .clone();
        assert_eq!(key, fqn);
    }

    #[test]
    fn parent_dir_import() {
        let g = extract("src/sub/a.ts", "import \"../b\";\n");
        assert!(g
            .symbols()
            .iter()
            .any(|s| s.kind == SymbolKind::Dependency && s.name == "src/b"));
    }
}

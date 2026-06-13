use stratify_core::ir::{Span, SymbolId};
use stratify_core::{Confidence, IrGraph, RefKind, Reference, Symbol, SymbolKind, Visibility};
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, Parser, Query, QueryCursor};

pub(crate) fn parser() -> Parser {
    let mut p = Parser::new();
    p.set_language(&tree_sitter_java::LANGUAGE.into())
        .expect("load java grammar");
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

/// Extract classes and their methods into a per-file graph. The file itself
/// becomes a `File` symbol; classes and methods get `Defines` edges from it.
pub(crate) fn extract(file: &str, src: &str) -> IrGraph {
    let mut parser = parser();
    let tree = parser.parse(src, None).expect("parse java");
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

    let query = Query::new(
        &tree_sitter_java::LANGUAGE.into(),
        r#"
        (class_declaration name: (identifier) @class.name) @class.node
        (method_declaration name: (identifier) @method.name) @method.node
        "#,
    )
    .expect("valid query");

    let mut cursor = QueryCursor::new();
    let class_name_idx = query.capture_index_for_name("class.name").unwrap();
    let class_node_idx = query.capture_index_for_name("class.node").unwrap();
    let method_name_idx = query.capture_index_for_name("method.name").unwrap();
    let method_node_idx = query.capture_index_for_name("method.node").unwrap();

    let mut matches = cursor.matches(&query, root, src.as_bytes());
    while let Some(m) = matches.next() {
        let mut name_node = None;
        let mut decl_node = None;
        let mut kind = SymbolKind::Class;
        for cap in m.captures {
            if cap.index == class_name_idx {
                name_node = Some(cap.node);
                kind = SymbolKind::Class;
            } else if cap.index == class_node_idx {
                decl_node = Some(cap.node);
            } else if cap.index == method_name_idx {
                name_node = Some(cap.node);
                kind = SymbolKind::Function;
            } else if cap.index == method_node_idx {
                decl_node = Some(cap.node);
            }
        }
        if let (Some(name_node), Some(decl_node)) = (name_node, decl_node) {
            let name = text(name_node, src).to_string();
            let is_main = kind == SymbolKind::Function && name == "main";
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
            if is_main {
                g.mark_entrypoint(id);
            }
        }
    }

    // Second pass: intra-file calls. Resolve a (method_invocation name) against
    // method names defined in this file. Unresolved calls are skipped in M1.
    let name_to_id: std::collections::HashMap<String, SymbolId> = g
        .symbols()
        .iter()
        .filter(|s| matches!(s.kind, SymbolKind::Function))
        .map(|s| (s.name.clone(), s.id))
        .collect();

    let call_query = Query::new(
        &tree_sitter_java::LANGUAGE.into(),
        r#"
        (method_invocation
          name: (identifier) @call.name) @call.node
        "#,
    )
    .expect("valid call query");

    let call_name_idx = call_query.capture_index_for_name("call.name").unwrap();
    let call_node_idx = call_query.capture_index_for_name("call.node").unwrap();

    // Map each call site to the enclosing method by walking ancestors.
    let mut call_cursor = QueryCursor::new();
    let mut call_matches = call_cursor.matches(&call_query, root, src.as_bytes());
    while let Some(m) = call_matches.next() {
        let mut callee_name = None;
        let mut call_node = None;
        for cap in m.captures {
            if cap.index == call_name_idx {
                callee_name = Some(text(cap.node, src).to_string());
            } else if cap.index == call_node_idx {
                call_node = Some(cap.node);
            }
        }
        let (Some(callee_name), Some(call_node)) = (callee_name, call_node) else {
            continue;
        };
        let Some(&callee_id) = name_to_id.get(&callee_name) else {
            continue;
        };
        let Some(caller_id) = enclosing_method_id(call_node, &g, file) else {
            continue;
        };
        g.add_reference(Reference {
            from: caller_id,
            to: callee_id,
            kind: RefKind::Calls,
            span: span(file, call_node),
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
    fn extracts_class_and_method() {
        let src = "public class Foo {\n  void bar() {}\n}\n";
        let g = extract("Foo.java", src);
        let kinds: Vec<_> = g
            .symbols()
            .iter()
            .map(|s| (s.kind, s.name.as_str()))
            .collect();
        assert!(kinds.contains(&(SymbolKind::File, "Foo.java")));
        assert!(kinds.contains(&(SymbolKind::Class, "Foo")));
        assert!(kinds.contains(&(SymbolKind::Function, "bar")));
    }

    #[test]
    fn file_defines_its_members() {
        let src = "class A { void m() {} }";
        let g = extract("A.java", src);
        // One Defines edge for class A, one for method m.
        assert_eq!(
            g.references()
                .iter()
                .filter(|r| matches!(r.kind, RefKind::Defines))
                .count(),
            2
        );
    }

    #[test]
    fn marks_main_method_as_entrypoint() {
        let src = "class App { public static void main(String[] a) {} void other() {} }";
        let g = extract("App.java", src);
        let main_id = g.symbols().iter().find(|s| s.name == "main").unwrap().id;
        assert_eq!(g.entrypoints(), &[main_id]);
    }

    #[test]
    fn records_intra_file_call_edge() {
        let src = "class A {\n  void a() { b(); }\n  void b() {}\n}\n";
        let g = extract("A.java", src);
        let a_id = g.symbols().iter().find(|s| s.name == "a").unwrap().id;
        let b_id = g.symbols().iter().find(|s| s.name == "b").unwrap().id;
        assert!(g
            .references()
            .iter()
            .any(|r| matches!(r.kind, RefKind::Calls) && r.from == a_id && r.to == b_id));
    }
}

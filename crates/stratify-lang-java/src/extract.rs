use std::collections::HashMap;

use stratify_core::ir::SymbolId;
use stratify_core::{Confidence, IrGraph, RefKind, Reference, Symbol, SymbolKind, Visibility};
use stratify_lang::walk::{self, ComplexityRules, NormalizeRules};
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, Parser, Query, QueryCursor};

pub(crate) fn parser() -> Parser {
    let mut p = Parser::new();
    p.set_language(&tree_sitter_java::LANGUAGE.into())
        .expect("load java grammar");
    p
}

/// Token normalization rules for Java leaves.
fn normalize_rules() -> NormalizeRules<'static> {
    NormalizeRules {
        identifier_kinds: &["identifier", "type_identifier"],
        number_kinds: &[
            "decimal_integer_literal",
            "hex_integer_literal",
            "octal_integer_literal",
            "binary_integer_literal",
            "decimal_floating_point_literal",
            "hex_floating_point_literal",
        ],
        // `character_literal` is a single leaf.
        string_kinds: &["character_literal"],
        // `string_literal` decomposes into quote delimiters plus content
        // children, so treat it as atomic to keep contents from leaking.
        atomic_string_kinds: &["string_literal"],
    }
}

/// Cyclomatic decision rules for Java.
fn complexity_rules() -> ComplexityRules<'static> {
    ComplexityRules {
        decision_kinds: &[
            "if_statement",
            "while_statement",
            "for_statement",
            "enhanced_for_statement",
            "do_statement",
            "catch_clause",
            "switch_label",
            "ternary_expression",
        ],
        operator_texts: &["&&", "||"],
    }
}

fn package_name(root: Node, src: &str) -> String {
    let q = Query::new(
        &tree_sitter_java::LANGUAGE.into(),
        r#"(package_declaration) @pkg"#,
    )
    .expect("pkg query");
    let mut cur = QueryCursor::new();
    let mut it = cur.matches(&q, root, src.as_bytes());
    if let Some(m) = it.next() {
        if let Some(cap) = m.captures.first() {
            let t = walk::node_text(cap.node, src);
            // strip leading "package" and trailing ";"
            return t
                .trim_start_matches("package")
                .trim()
                .trim_end_matches(';')
                .trim()
                .to_string();
        }
    }
    String::new()
}

/// Emit File symbol plus a Class/Function symbol (with Defines edge) per
/// declaration, marking `main` as an entrypoint and recording method complexity.
fn extract_declarations(g: &mut IrGraph, root: Node, src: &str, file: &str, file_id: SymbolId) {
    let ctx = DeclCtx {
        file,
        file_id,
        pkg: package_name(root, src),
    };

    let query = Query::new(
        &tree_sitter_java::LANGUAGE.into(),
        r#"
        (class_declaration name: (identifier) @class.name) @class.node
        (method_declaration name: (identifier) @method.name) @method.node
        "#,
    )
    .expect("valid query");

    let class_name_idx = query.capture_index_for_name("class.name").unwrap();
    let class_node_idx = query.capture_index_for_name("class.node").unwrap();
    let method_name_idx = query.capture_index_for_name("method.name").unwrap();
    let method_node_idx = query.capture_index_for_name("method.node").unwrap();

    let mut cursor = QueryCursor::new();
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
            add_declaration(g, src, &ctx, kind, name_node, decl_node);
        }
    }
}

/// File-level context shared across declaration emits.
struct DeclCtx<'a> {
    file: &'a str,
    file_id: SymbolId,
    pkg: String,
}

/// Add one class/method symbol and its Defines edge from the file.
fn add_declaration(
    g: &mut IrGraph,
    src: &str,
    ctx: &DeclCtx,
    kind: SymbolKind,
    name_node: Node,
    decl_node: Node,
) {
    let file = ctx.file;
    let file_id = ctx.file_id;
    let name = walk::node_text(name_node, src).to_string();
    let is_main = kind == SymbolKind::Function && name == "main";
    let fqn = if matches!(kind, SymbolKind::Class) && !ctx.pkg.is_empty() {
        format!("{}.{name}", ctx.pkg)
    } else {
        name.clone()
    };
    let id = g.add_symbol(Symbol {
        id: SymbolId(0),
        kind,
        name: name.clone(),
        fqn,
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
    if is_main {
        g.mark_entrypoint(id);
    }
    if kind == SymbolKind::Function {
        let cx = walk::cyclomatic(decl_node, src, &complexity_rules());
        g.set_complexity(id, cx);
    }
}

/// Emit a Dependency symbol per import and an Imports edge from the file.
fn extract_imports(g: &mut IrGraph, root: Node, src: &str, file: &str, file_id: SymbolId) {
    let import_q = Query::new(
        &tree_sitter_java::LANGUAGE.into(),
        r#"(import_declaration (scoped_identifier) @imp)"#,
    )
    .expect("import query");
    let imp_idx = import_q.capture_index_for_name("imp").unwrap();
    let mut icur = QueryCursor::new();
    let mut imatches = icur.matches(&import_q, root, src.as_bytes());
    while let Some(m) = imatches.next() {
        for cap in m.captures {
            if cap.index == imp_idx {
                let fqn = walk::node_text(cap.node, src).to_string();
                let dep = g.add_symbol(Symbol {
                    id: SymbolId(0),
                    kind: SymbolKind::Dependency,
                    name: fqn.clone(),
                    fqn,
                    span: walk::span(cap.node, file),
                    visibility: Visibility::Unknown,
                    confidence: Confidence::Certain,
                });
                g.add_reference(Reference {
                    from: file_id,
                    to: dep,
                    kind: RefKind::Imports,
                    span: walk::span(cap.node, file),
                    confidence: Confidence::Certain,
                });
            }
        }
    }
}

/// Resolve intra-file calls. A `(method_invocation name)` resolves against
/// method names defined in this file; unresolved calls are recorded for M1.
fn extract_calls(g: &mut IrGraph, root: Node, src: &str, file: &str, file_id: SymbolId) {
    let name_to_id: HashMap<String, SymbolId> = g
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

    let mut call_cursor = QueryCursor::new();
    let mut call_matches = call_cursor.matches(&call_query, root, src.as_bytes());
    while let Some(m) = call_matches.next() {
        let mut callee_name = None;
        let mut call_node = None;
        for cap in m.captures {
            if cap.index == call_name_idx {
                callee_name = Some(walk::node_text(cap.node, src).to_string());
            } else if cap.index == call_node_idx {
                call_node = Some(cap.node);
            }
        }
        let (Some(callee_name), Some(call_node)) = (callee_name, call_node) else {
            continue;
        };
        if let Some(&callee_id) = name_to_id.get(&callee_name) {
            let Some(caller_id) = enclosing_method_id(call_node, g, file) else {
                continue;
            };
            g.add_reference(Reference {
                from: caller_id,
                to: callee_id,
                kind: RefKind::Calls,
                span: walk::span(call_node, file),
                confidence: Confidence::Likely,
            });
        } else {
            let from = enclosing_method_id(call_node, g, file).unwrap_or(file_id);
            g.add_unresolved_call(from, callee_name.clone());
        }
    }
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
        span: walk::span(root, file),
        visibility: Visibility::Unknown,
        confidence: Confidence::Certain,
    });

    walk::tokenize(&mut g, root, src, file, &normalize_rules());

    extract_declarations(&mut g, root, src, file, file_id);
    extract_imports(&mut g, root, src, file, file_id);
    extract_calls(&mut g, root, src, file, file_id);

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
        let src = "class A { int x = 5; }";
        let g = extract("A.java", src);
        let norms: Vec<&str> = g.tokens().iter().map(|t| t.norm.as_str()).collect();
        // identifiers normalized to ID, the literal 5 to NUM, keywords/punct literal.
        assert!(norms.contains(&"class"));
        assert!(norms.contains(&"ID")); // A / int-name / x
        assert!(norms.contains(&"NUM")); // 5
        assert!(norms.contains(&"{"));
    }

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
    fn computes_method_complexity() {
        // base 1 + two ifs + one && = 4
        let src = "class A { void m(int x) { if (x > 0 && x < 9) {} if (x == 5) {} } }";
        let g = extract("A.java", src);
        let m = g.symbols().iter().find(|s| s.name == "m").unwrap().id;
        assert_eq!(g.complexity_of(m), Some(4));
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

    #[test]
    fn class_fqn_includes_package() {
        let src = "package com.acme;\nclass Foo {}";
        let g = extract("Foo.java", src);
        let foo = g.symbols().iter().find(|s| s.name == "Foo").unwrap();
        assert_eq!(foo.fqn, "com.acme.Foo");
    }

    #[test]
    fn records_unresolved_cross_file_call() {
        // `external` is not defined in this file -> recorded as unresolved.
        let src = "class A { void m() { external(); } }";
        let g = extract("A.java", src);
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
    fn emits_import_dependency_and_edge() {
        let src = "package com.acme;\nimport com.other.Bar;\nclass Foo {}";
        let g = extract("Foo.java", src);
        // a Dependency named after the imported FQN
        let dep = g
            .symbols()
            .iter()
            .find(|s| s.kind == SymbolKind::Dependency && s.name == "com.other.Bar");
        assert!(
            dep.is_some(),
            "expected import Dependency for com.other.Bar"
        );
        let dep_id = dep.unwrap().id;
        let file_id = g
            .symbols()
            .iter()
            .find(|s| s.kind == SymbolKind::File)
            .unwrap()
            .id;
        assert!(g
            .references()
            .iter()
            .any(|r| matches!(r.kind, RefKind::Imports) && r.from == file_id && r.to == dep_id));
    }
}

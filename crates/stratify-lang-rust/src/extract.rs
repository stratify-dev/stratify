use stratify_core::ir::{Span, SymbolId};
use stratify_core::{
    Confidence, IrGraph, RefKind, Reference, Symbol, SymbolKind, Token, Visibility,
};
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, Parser, Query, QueryCursor};

fn package_dir(path: &str) -> String {
    std::path::Path::new(path)
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// Cyclomatic decision points for Rust. Mirrors `count_decisions_go`: walk the
/// subtree and count branching constructs and short-circuit/`?` operators.
/// Deliberately does NOT count `loop_expression` (unconditional).
fn count_decisions_rust(node: Node) -> u32 {
    let mut count = 0u32;
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if n.is_named() {
            match n.kind() {
                "if_expression" | "while_expression" | "for_expression" | "match_arm"
                | "try_expression" => {
                    count += 1;
                }
                _ => {}
            }
        } else {
            // Operator/punctuation tokens are unnamed leaves.
            match n.kind() {
                "&&" | "||" => count += 1,
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

fn cyclomatic_rust(node: Node) -> u32 {
    1 + count_decisions_rust(node)
}

/// Find the function that lexically encloses `node` by matching byte ranges
/// against known Function symbols in this file's graph.
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

fn normalize_rust(kind: &str, text: &str) -> String {
    match kind {
        "identifier" | "field_identifier" | "type_identifier" => "ID".to_string(),
        "integer_literal" | "float_literal" => "NUM".to_string(),
        "string_literal" | "raw_string_literal" | "char_literal" => "STR".to_string(),
        _ => text.to_string(),
    }
}

fn collect_leaves<'a>(node: Node<'a>, out: &mut Vec<Node<'a>>) {
    // In tree-sitter-rust 0.23 string literals are NOT leaves: they decompose
    // into quote delimiters + a `string_content` child. Treat them as atomic so
    // `normalize_rust` collapses the whole literal to a single `STR` token and
    // the contents never leak into the token stream.
    if matches!(node.kind(), "string_literal" | "raw_string_literal") {
        out.push(node);
        return;
    }
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
        let norm = normalize_rust(leaf.kind(), t);
        g.add_token(Token {
            file: file.to_string(),
            start_byte: leaf.start_byte(),
            end_byte: leaf.end_byte(),
            start_line: leaf.start_position().row + 1,
            norm,
        });
    }
}

/// True if the nearest `impl_item` enclosing this `function_item` is a *trait*
/// impl (`impl Trait for Type`). The grammar exposes `impl_item`'s `trait` field
/// only for trait impls, not for inherent impls (`impl Type`). Methods of a trait
/// impl are invoked via the trait and get no in-file Calls edge, so they must be
/// treated as entrypoints to avoid false dead-code reports.
fn in_trait_impl(node: Node) -> bool {
    let mut cur = node.parent();
    while let Some(n) = cur {
        match n.kind() {
            "impl_item" => return n.child_by_field_name("trait").is_some(),
            "source_file" => return false,
            _ => cur = n.parent(),
        }
    }
    false
}

/// True if the `function_item` node has a `visibility_modifier` child
/// (`pub`, `pub(crate)`, ...).
fn has_visibility(node: Node) -> bool {
    let mut c = node.walk();
    let found = node
        .children(&mut c)
        .any(|child| child.kind() == "visibility_modifier");
    found
}

/// True if a `function_item` is preceded by a test attribute. In tree-sitter-rust
/// `attribute_item`s are siblings that precede the `function_item`. We scan
/// backwards over leading attributes and stop at the first non-attribute sibling.
fn has_test_attribute(node: Node, src: &str) -> bool {
    let mut prev = node.prev_sibling();
    while let Some(p) = prev {
        match p.kind() {
            "attribute_item" => {
                if text(p, src).contains("test") {
                    return true;
                }
                prev = p.prev_sibling();
            }
            "line_comment" | "block_comment" => {
                prev = p.prev_sibling();
            }
            _ => break,
        }
    }
    false
}

/// Find the `function_item` that lexically encloses `node` by walking ancestors.
/// Returns its symbol id via the same span-matching used by `enclosing_method_id`.
/// Used for macro-recovered calls, whose callee identifiers live inside an opaque
/// `token_tree` that the normal `enclosing_method_id` byte-range lookup also handles,
/// but walking ancestors keeps intent explicit for the macro pass.
fn enclosing_fn_id(node: Node, g: &IrGraph, file: &str) -> Option<SymbolId> {
    let mut cur = node.parent();
    while let Some(n) = cur {
        if n.kind() == "function_item" {
            return enclosing_method_id(n, g, file);
        }
        cur = n.parent();
    }
    None
}

/// True if `node` is the "open paren" that follows a callee identifier inside a
/// macro token stream. tree-sitter-rust represents `foo()` inside a macro as an
/// `identifier` followed by a `token_tree` whose first child is `(` (the args),
/// not by a bare `(` token. Accept both forms to be safe.
fn starts_call_args(node: Node) -> bool {
    if node.kind() == "(" {
        return true;
    }
    if node.kind() == "token_tree" {
        return node.child(0).map(|c| c.kind() == "(").unwrap_or(false);
    }
    false
}

/// Scan a macro `token_tree` (recursing into nested `token_tree`s) for the
/// conservative `identifier` immediately-followed-by-call-args pattern, collecting
/// each such identifier's text as a recovered callee name.
fn collect_macro_callees<'a>(tree: Node<'a>, src: &'a str, out: &mut Vec<String>) {
    let mut c = tree.walk();
    let children: Vec<Node> = tree.children(&mut c).collect();
    for (i, child) in children.iter().enumerate() {
        if child.kind() == "identifier" {
            if let Some(next) = children.get(i + 1) {
                if starts_call_args(*next) {
                    out.push(text(*child, src).to_string());
                }
            }
        }
        if child.kind() == "token_tree" {
            collect_macro_callees(*child, src, out);
        }
    }
}

/// B5 macro-call recovery pass: walk every `macro_invocation`, recover call-like
/// identifiers from its `token_tree`, and append resolved/unresolved calls to the
/// shared buffers so they dedup and emit alongside real calls (Confidence::Likely).
#[allow(clippy::too_many_arguments)]
fn recover_macro_calls(
    root: Node,
    src: &str,
    g: &IrGraph,
    file: &str,
    file_id: SymbolId,
    name_to_id: &std::collections::HashMap<String, SymbolId>,
    edges: &mut Vec<(SymbolId, SymbolId)>,
    unresolved: &mut Vec<(SymbolId, String)>,
) {
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if n.kind() == "macro_invocation" {
            // The macro's own name is the `macro:` field, not inside the
            // token_tree, so scanning only the token_tree never picks it up. The
            // token_tree child carries no field name, so find it by kind.
            let mut tc = n.walk();
            let tt = n.children(&mut tc).find(|c| c.kind() == "token_tree");
            if let Some(tt) = tt {
                let from = enclosing_fn_id(n, g, file).unwrap_or(file_id);
                let mut callees = Vec::new();
                collect_macro_callees(tt, src, &mut callees);
                for name in callees {
                    if let Some(&callee_id) = name_to_id.get(&name) {
                        edges.push((from, callee_id));
                    } else {
                        unresolved.push((from, name));
                    }
                }
            }
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }
}

pub(crate) fn extract(file: &str, src: &str) -> IrGraph {
    let lang: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();

    let mut parser = Parser::new();
    parser.set_language(&lang).expect("load rust grammar");
    let tree = parser.parse(src, None).expect("parse rust");
    let root = tree.root_node();

    let mut g = IrGraph::new();

    // File symbol — fqn is the parent dir of the path (mirror go).
    let file_id = g.add_symbol(Symbol {
        id: SymbolId(0),
        kind: SymbolKind::File,
        name: file.to_string(),
        fqn: package_dir(file),
        span: span(file, root),
        visibility: Visibility::Unknown,
        confidence: Confidence::Certain,
    });

    emit_tokens(&mut g, file, src, root);

    // NOTE: No import edges. `use`/`mod` resolution is intentionally out of scope
    // for Rust (boundaries/cycles deferred), mirroring how M13 Go shipped.

    let query = Query::new(
        &lang,
        r#"
        (function_item name: (identifier) @func.name) @func.node
        (struct_item name: (type_identifier) @type.name) @type.node
        (enum_item name: (type_identifier) @type.name) @type.node
        (trait_item name: (type_identifier) @type.name) @type.node
        "#,
    )
    .expect("rust query");

    let func_name_idx = query.capture_index_for_name("func.name").unwrap();
    let func_node_idx = query.capture_index_for_name("func.node").unwrap();
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
                // Entrypoints: `main`, any `pub` item, or a test-annotated fn.
                // A Rust lib's public surface and tests are real entry points;
                // without this every public function would be flagged dead.
                if name == "main"
                    || has_visibility(decl_node)
                    || has_test_attribute(decl_node, src)
                    || in_trait_impl(decl_node)
                {
                    g.mark_entrypoint(id);
                }
                g.set_complexity(id, cyclomatic_rust(decl_node));
            }
        }
    }

    // Intra-file calls. Build a map of Function name -> SymbolId.
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
        (call_expression function: (field_expression field: (field_identifier) @callee)) @call
        "#,
    )
    .expect("rust call query");

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

    // B5: recover call-like identifiers hidden inside macro invocations.
    // tree-sitter-rust parses macro arguments as an opaque `token_tree`, so a
    // function called only inside `vec![foo()]`, `assert_eq!(bar(), 1)`, etc. gets
    // no `call_expression` node and would be reported as confidently dead. Walk
    // every `macro_invocation`, scan its `token_tree` for `identifier (` adjacency,
    // and attribute the recovered call to the enclosing function (or the File).
    // These edges resolve the SAME way as real calls but are added to the same
    // `edges`/`unresolved` buffers, so they share dedup and emit at
    // Confidence::Likely — they can only DOWNGRADE a dead verdict, never clear it.
    recover_macro_calls(
        root,
        src,
        &g,
        file,
        file_id,
        &name_to_id,
        &mut edges,
        &mut unresolved,
    );

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

    // Record unresolved (cross-file) calls for the repo-wide resolver.
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
    fn extracts_fns_methods_and_types() {
        let src = r#"
struct Foo {}

trait Greet {}

enum Color { Red }

impl Foo {
    fn bar(&self) {}
}

fn baz() {}
"#;
        let g = extract("foo.rs", src);
        let names: Vec<_> = g
            .symbols()
            .iter()
            .map(|s| (s.kind, s.name.as_str()))
            .collect();
        assert!(names.contains(&(SymbolKind::File, "foo.rs")));
        assert!(names.contains(&(SymbolKind::Class, "Foo")));
        assert!(names.contains(&(SymbolKind::Class, "Greet")));
        assert!(names.contains(&(SymbolKind::Class, "Color")));
        // impl method is a function_item too.
        assert!(names.contains(&(SymbolKind::Function, "bar")));
        assert!(names.contains(&(SymbolKind::Function, "baz")));
    }

    #[test]
    fn emits_normalized_tokens() {
        let g = extract("a.rs", "fn f() { let x = 5; }\n");
        let norms: Vec<&str> = g.tokens().iter().map(|t| t.norm.as_str()).collect();
        assert!(norms.contains(&"ID")); // f / x
        assert!(norms.contains(&"NUM")); // 5
        assert!(norms.contains(&"fn")); // keyword stays literal
    }

    #[test]
    fn main_pub_and_test_are_entrypoints_private_is_not() {
        let src = r#"
fn main() {}

pub fn exported() {}

#[test]
fn it_works() {}

#[tokio::test]
async fn async_test() {}

fn helper() {}
"#;
        let g = extract("m.rs", src);
        let id = |name: &str| g.symbols().iter().find(|s| s.name == name).unwrap().id;
        let eps = g.entrypoints();
        assert!(eps.contains(&id("main")));
        assert!(eps.contains(&id("exported")));
        assert!(eps.contains(&id("it_works")));
        assert!(eps.contains(&id("async_test")));
        assert!(
            !eps.contains(&id("helper")),
            "private uncalled helper is not an entrypoint"
        );
    }

    #[test]
    fn trait_impl_methods_are_entrypoints() {
        let src = r#"
trait Greeter { fn hello(&self); }
struct S;
impl Greeter for S { fn hello(&self) { let _ = 1; } }
impl S { fn inherent_unused(&self) {} }
"#;
        let g = extract("t.rs", src);
        let id = |name: &str| {
            g.symbols()
                .iter()
                .find(|s| s.kind == SymbolKind::Function && s.name == name)
                .unwrap()
                .id
        };
        let eps = g.entrypoints();
        assert!(
            eps.contains(&id("hello")),
            "trait-impl method `hello` must be an entrypoint"
        );
        assert!(
            !eps.contains(&id("inherent_unused")),
            "inherent-impl method must NOT be an entrypoint"
        );
    }

    #[test]
    fn string_literals_normalize_to_str() {
        let g = extract("s.rs", r#"fn f() { let a = "hello"; let b = "world"; }"#);
        let norms: Vec<&str> = g.tokens().iter().map(|t| t.norm.as_str()).collect();
        assert!(norms.contains(&"STR"), "expected STR tokens for literals");
        assert!(
            !norms.iter().any(|n| n.contains("hello")),
            "raw string content `hello` leaked into tokens: {norms:?}"
        );
        assert!(
            !norms.iter().any(|n| n.contains("world")),
            "raw string content `world` leaked into tokens: {norms:?}"
        );
    }

    #[test]
    fn intra_file_call_edge() {
        let src = "fn a() { b() }\nfn b() {}\n";
        let g = extract("x.rs", src);
        let a = g.symbols().iter().find(|s| s.name == "a").unwrap().id;
        let b = g.symbols().iter().find(|s| s.name == "b").unwrap().id;
        assert!(g
            .references()
            .iter()
            .any(|r| matches!(r.kind, RefKind::Calls) && r.from == a && r.to == b));
    }

    #[test]
    fn macro_hidden_call_is_recovered() {
        // `helper()` is called only inside `vec![...]`, whose args tree-sitter
        // parses as an opaque token_tree. Without B5 recovery there is no Calls
        // edge and `helper` is reported as confidently dead.
        let src = "fn helper() -> i32 { 1 }\nfn main() { let v = vec![helper()]; let _ = v; }\n";
        let g = extract("m.rs", src);
        let helper = g.symbols().iter().find(|s| s.name == "helper").unwrap().id;
        assert!(
            g.references()
                .iter()
                .any(|r| matches!(r.kind, RefKind::Calls) && r.to == helper),
            "expected a recovered Calls edge to helper; refs: {:?}",
            g.references()
        );
    }

    #[test]
    fn macro_recovered_call_is_likely() {
        let src = "fn helper() -> i32 { 1 }\nfn main() { let v = vec![helper()]; let _ = v; }\n";
        let g = extract("m.rs", src);
        let helper = g.symbols().iter().find(|s| s.name == "helper").unwrap().id;
        let edge = g
            .references()
            .iter()
            .find(|r| matches!(r.kind, RefKind::Calls) && r.to == helper)
            .expect("recovered call edge present");
        assert_eq!(
            edge.confidence,
            Confidence::Likely,
            "macro-recovered call must be Likely, never Certain"
        );
    }

    #[test]
    fn nested_macro_call_is_recovered() {
        // `vec![a(b())]`: both `a` and `b` are inside nested token_trees.
        let src = "fn a(x: i32) -> i32 { x }\nfn b() -> i32 { 2 }\nfn main() { let v = vec![a(b())]; let _ = v; }\n";
        let g = extract("n.rs", src);
        let id = |name: &str| g.symbols().iter().find(|s| s.name == name).unwrap().id;
        let has_call_to = |to: SymbolId| {
            g.references()
                .iter()
                .any(|r| matches!(r.kind, RefKind::Calls) && r.to == to)
        };
        assert!(has_call_to(id("a")), "expected recovered call to a");
        assert!(has_call_to(id("b")), "expected recovered call to b (nested)");
    }

    #[test]
    fn computes_complexity() {
        // base 1 + if + && + match arms (2) = 5
        let src = r#"
fn m(x: i32) -> i32 {
    if x > 0 && x < 9 {
    }
    match x {
        0 => 1,
        _ => 2,
    }
}
"#;
        let g = extract("c.rs", src);
        let m = g.symbols().iter().find(|s| s.name == "m").unwrap().id;
        assert_eq!(g.complexity_of(m), Some(5));
    }

    #[test]
    fn loop_is_not_a_decision() {
        // loop is unconditional -> complexity stays 1.
        let src = "fn l() { loop { break; } }\n";
        let g = extract("l.rs", src);
        let l = g.symbols().iter().find(|s| s.name == "l").unwrap().id;
        assert_eq!(g.complexity_of(l), Some(1));
    }

    #[test]
    fn records_unresolved_cross_file_call() {
        let g = extract("a.rs", "fn m() { external() }\n");
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
    fn file_fqn_is_parent_dir() {
        let g = extract("src/svc/a.rs", "fn a() {}\n");
        let f = g
            .symbols()
            .iter()
            .find(|s| s.kind == SymbolKind::File)
            .unwrap();
        assert_eq!(f.fqn, "src/svc");
    }
}

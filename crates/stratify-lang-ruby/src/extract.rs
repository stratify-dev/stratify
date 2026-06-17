use stratify_core::ir::SymbolId;
use stratify_core::{Confidence, IrGraph, RefKind, Reference, Symbol, SymbolKind, Visibility};
use stratify_lang::walk::{self, ComplexityRules, NormalizeRules};
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, Parser, Query, QueryCursor};

pub(crate) fn parser() -> Parser {
    let mut p = Parser::new();
    p.set_language(&tree_sitter_ruby::LANGUAGE.into())
        .expect("load ruby grammar");
    p
}

/// Token normalization rules for Ruby leaves.
fn normalize_rules() -> NormalizeRules<'static> {
    NormalizeRules {
        identifier_kinds: &[
            "identifier",
            "constant",
            "instance_variable",
            "global_variable",
            "class_variable",
        ],
        number_kinds: &["integer", "float"],
        // `simple_symbol` is a single leaf.
        string_kinds: &["simple_symbol"],
        // A Ruby `string` decomposes into quote delimiters plus a
        // `string_content` child, so treat it as atomic to keep contents
        // from leaking as a separate token.
        atomic_string_kinds: &["string"],
    }
}

/// Cyclomatic decision rules for Ruby.
fn complexity_rules() -> ComplexityRules<'static> {
    ComplexityRules {
        decision_kinds: &[
            "if",
            "elsif",
            "unless",
            "while",
            "until",
            "for",
            "when",
            "rescue",
            "conditional",
            "if_modifier",
            "unless_modifier",
            "while_modifier",
            "until_modifier",
        ],
        operator_texts: &["&&", "||", "and", "or"],
    }
}

fn resolve_require_relative(importer_file: &str, arg: &str) -> String {
    use std::path::{Component, Path};
    let dir = Path::new(importer_file)
        .parent()
        .unwrap_or_else(|| Path::new(""));
    let joined = dir.join(arg);
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
    if !p.ends_with(".rb") {
        p.push_str(".rb");
    }
    p
}

/// Emit a Module/Class/Function symbol (with a Defines edge from the file) per
/// declaration, recording method complexity.
fn extract_declarations(g: &mut IrGraph, root: Node, src: &str, file: &str, file_id: SymbolId) {
    let query = Query::new(
        &tree_sitter_ruby::LANGUAGE.into(),
        r#"
        (method name: (identifier) @method.name) @method.node
        (class name: (constant) @class.name) @class.node
        (module name: (constant) @module.name) @module.node
        "#,
    )
    .expect("valid ruby query");

    let method_name_idx = query.capture_index_for_name("method.name").unwrap();
    let method_node_idx = query.capture_index_for_name("method.node").unwrap();
    let class_name_idx = query.capture_index_for_name("class.name").unwrap();
    let class_node_idx = query.capture_index_for_name("class.node").unwrap();
    let module_name_idx = query.capture_index_for_name("module.name").unwrap();
    let module_node_idx = query.capture_index_for_name("module.node").unwrap();

    let mut cursor = QueryCursor::new();
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
            add_declaration(g, src, file, file_id, kind, name_node, decl_node);
        }
    }
}

/// Add one module/class/method symbol and its Defines edge from the file.
fn add_declaration(
    g: &mut IrGraph,
    src: &str,
    file: &str,
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
        fqn: name,
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
        let cx = walk::cyclomatic(decl_node, src, &complexity_rules());
        g.set_complexity(id, cx);
    }
}

/// Resolve intra-file calls and record cross-file misses.
///
/// Two passes feed a shared edge/unresolved set:
///   A. explicit calls like `foo(args)` or `recv.foo`.
///   B. bare identifier command calls like `greet` (no parens, no receiver).
///
/// A bare Ruby identifier is syntactically indistinguishable from a local-variable
/// read. We record in-file hits as Calls edges and misses as unresolved calls.
/// The cross_file_calls pass drops any unresolved name with no matching repo
/// function, so variable names and builtins are silently ignored.
fn extract_calls(g: &mut IrGraph, root: Node, src: &str, file: &str, file_id: SymbolId) {
    let name_to_id: std::collections::HashMap<String, SymbolId> = g
        .symbols()
        .iter()
        .filter(|s| matches!(s.kind, SymbolKind::Function))
        .map(|s| (s.name.clone(), s.id))
        .collect();

    let mut edges: Vec<(SymbolId, SymbolId)> = Vec::new();
    let mut unresolved: Vec<(SymbolId, String)> = Vec::new();

    collect_explicit_calls(g, root, src, file, file_id, &name_to_id, &mut edges, &mut unresolved);
    collect_bare_calls(g, root, src, file, file_id, &name_to_id, &mut edges, &mut unresolved);

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

/// Pass A: explicit calls like `foo(args)` or `recv.foo`.
#[allow(clippy::too_many_arguments)]
fn collect_explicit_calls(
    g: &IrGraph,
    root: Node,
    src: &str,
    file: &str,
    file_id: SymbolId,
    name_to_id: &std::collections::HashMap<String, SymbolId>,
    edges: &mut Vec<(SymbolId, SymbolId)>,
    unresolved: &mut Vec<(SymbolId, String)>,
) {
    let call_query = Query::new(
        &tree_sitter_ruby::LANGUAGE.into(),
        r#"(call method: (identifier) @callee)"#,
    )
    .expect("valid ruby call query");
    let callee_idx = call_query.capture_index_for_name("callee").unwrap();

    let mut call_cursor = QueryCursor::new();
    let mut call_matches = call_cursor.matches(&call_query, root, src.as_bytes());
    while let Some(m) = call_matches.next() {
        for cap in m.captures {
            if cap.index == callee_idx {
                let callee_name = walk::node_text(cap.node, src);
                let from = enclosing_method_id(cap.node, g, file).unwrap_or(file_id);
                if let Some(&callee_id) = name_to_id.get(callee_name) {
                    edges.push((from, callee_id));
                } else {
                    unresolved.push((from, callee_name.to_string()));
                }
            }
        }
    }
}

/// Pass B: bare identifier command calls like `greet` (no parens, no receiver).
/// We skip identifiers that are definition sites or parameter nodes.
#[allow(clippy::too_many_arguments)]
fn collect_bare_calls(
    g: &IrGraph,
    root: Node,
    src: &str,
    file: &str,
    file_id: SymbolId,
    name_to_id: &std::collections::HashMap<String, SymbolId>,
    edges: &mut Vec<(SymbolId, SymbolId)>,
    unresolved: &mut Vec<(SymbolId, String)>,
) {
    let ident_query = Query::new(&tree_sitter_ruby::LANGUAGE.into(), r#"(identifier) @ident"#)
        .expect("valid ruby ident query");
    let ident_idx = ident_query.capture_index_for_name("ident").unwrap();

    let mut ident_cursor = QueryCursor::new();
    let mut ident_matches = ident_cursor.matches(&ident_query, root, src.as_bytes());
    while let Some(m) = ident_matches.next() {
        for cap in m.captures {
            if cap.index == ident_idx {
                let callee_name = walk::node_text(cap.node, src);
                // Skip definition sites and parameter nodes.
                let parent_kind = cap.node.parent().map(|p| p.kind()).unwrap_or("");
                if matches!(
                    parent_kind,
                    "method" | "method_parameters" | "block_parameter" | "keyword_parameter"
                ) {
                    continue;
                }
                let from = enclosing_method_id(cap.node, g, file).unwrap_or(file_id);
                if let Some(&callee_id) = name_to_id.get(callee_name) {
                    edges.push((from, callee_id));
                } else {
                    // Miss: could be a cross-file call. Record for later resolution.
                    // The cross_file_calls pass drops names with no repo function match.
                    unresolved.push((from, callee_name.to_string()));
                }
            }
        }
    }
}

/// Emit a Dependency symbol per `require_relative` and an Imports edge from the file.
fn extract_imports(g: &mut IrGraph, root: Node, src: &str, file: &str, file_id: SymbolId) {
    let req_q = Query::new(
        &tree_sitter_ruby::LANGUAGE.into(),
        r#"(call
            method: (identifier) @m
            arguments: (argument_list (string (string_content) @arg)))"#,
    )
    .expect("require query");
    let m_idx = req_q.capture_index_for_name("m").unwrap();
    let arg_idx = req_q.capture_index_for_name("arg").unwrap();
    let mut rcur = QueryCursor::new();
    let mut rmatches = rcur.matches(&req_q, root, src.as_bytes());
    while let Some(mt) = rmatches.next() {
        let mut is_req = false;
        let mut arg_node = None;
        for cap in mt.captures {
            if cap.index == m_idx && walk::node_text(cap.node, src) == "require_relative" {
                is_req = true;
            } else if cap.index == arg_idx {
                arg_node = Some(cap.node);
            }
        }
        if !is_req {
            continue;
        }
        let Some(arg_node) = arg_node else { continue };
        let key = resolve_require_relative(file, walk::node_text(arg_node, src));
        let dep = g.add_symbol(Symbol {
            id: SymbolId(0),
            kind: SymbolKind::Dependency,
            name: key.clone(),
            fqn: key,
            span: walk::span(arg_node, file),
            visibility: Visibility::Unknown,
            confidence: Confidence::Certain,
        });
        g.add_reference(Reference {
            from: file_id,
            to: dep,
            kind: RefKind::Imports,
            span: walk::span(arg_node, file),
            confidence: Confidence::Certain,
        });
    }
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
        span: walk::span(root, file),
        visibility: Visibility::Unknown,
        confidence: Confidence::Certain,
    });

    walk::tokenize(&mut g, root, src, file, &normalize_rules());

    extract_declarations(&mut g, root, src, file, file_id);

    // Mark the file as an entrypoint (Ruby top-level code is the execution entry).
    g.mark_entrypoint(file_id);

    extract_calls(&mut g, root, src, file, file_id);
    extract_imports(&mut g, root, src, file, file_id);

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

    #[test]
    fn file_fqn_is_path() {
        let g = extract("lib/a.rb", "def x\nend\n");
        let f = g
            .symbols()
            .iter()
            .find(|s| s.kind == SymbolKind::File)
            .unwrap();
        assert_eq!(f.fqn, "lib/a.rb");
    }

    #[test]
    fn emits_require_relative_edge_with_resolved_key() {
        // from lib/a.rb, require_relative "b" -> key lib/b.rb
        let g = extract("lib/a.rb", "require_relative \"b\"\n");
        let dep = g
            .symbols()
            .iter()
            .find(|s| s.kind == SymbolKind::Dependency && s.name == "lib/b.rb");
        assert!(dep.is_some(), "expected Dependency keyed lib/b.rb");
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
    fn require_relative_handles_parent_dir() {
        // from lib/sub/a.rb, require_relative "../c" -> key lib/c.rb
        let g = extract("lib/sub/a.rb", "require_relative \"../c\"\n");
        assert!(g
            .symbols()
            .iter()
            .any(|s| s.kind == SymbolKind::Dependency && s.name == "lib/c.rb"));
    }

    #[test]
    fn records_unresolved_cross_file_call() {
        // `external` is not defined in this file -> recorded as unresolved.
        // Use parens so tree-sitter parses it as an explicit `(call ...)` node
        // (without parens, a bare word is an identifier that Query B only matches
        // when it's already a known in-file name, which `external` is not).
        let g = extract("a.rb", "def caller\n  external()\nend\n");
        let caller_id = g.symbols().iter().find(|s| s.name == "caller").unwrap().id;
        assert!(
            g.unresolved_calls()
                .iter()
                .any(|(from, name)| *from == caller_id && name == "external"),
            "expected unresolved call (caller, external); got {:?}",
            g.unresolved_calls()
        );
    }
}

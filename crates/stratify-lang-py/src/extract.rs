use stratify_core::ir::SymbolId;
use stratify_core::{
    Confidence, IrGraph, RefKind, Reference, Symbol, SymbolKind, Visibility,
};
use stratify_lang::walk::{self, ComplexityRules, NormalizeRules};
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, Parser, Query, QueryCursor};

/// Leaf-token normalization for Python duplication detection.
///
/// In tree-sitter-python a `string` decomposes into `string_start` /
/// `string_content` / `string_end`. Descending normally and mapping
/// `string_content` -> STR keeps the quote delimiters as literal tokens, which
/// is the historical output. `string` and `concatenated_string` are never
/// leaves; listing them is harmless and documents intent.
const NORMALIZE_RULES: NormalizeRules = NormalizeRules {
    identifier_kinds: &["identifier"],
    number_kinds: &["integer", "float"],
    string_kinds: &["string", "string_content", "concatenated_string"],
    atomic_string_kinds: &[],
};

/// Named decision-point kinds for cyclomatic complexity. `boolean_operator`
/// covers `and`/`or`, so no operator-text leaves are needed.
const COMPLEXITY_RULES: ComplexityRules = ComplexityRules {
    decision_kinds: &[
        "if_statement",
        "elif_clause",
        "for_statement",
        "while_statement",
        "except_clause",
        "conditional_expression",
        "boolean_operator",
        "case_clause",
    ],
    operator_texts: &[],
};

/// Strip a trailing .py or .pyi extension from a file path.
fn strip_py_ext(path: &str) -> String {
    for ext in [".pyi", ".py"] {
        if let Some(stripped) = path.strip_suffix(ext) {
            return stripped.to_string();
        }
    }
    path.to_string()
}

/// The package/module key for a Python file. `pkg/mod.py` -> `pkg/mod`;
/// a package's `pkg/__init__.py` -> `pkg`; a top-level `__init__.py` -> ``.
fn py_module_key(path: &str) -> String {
    let stripped = strip_py_ext(path);
    if stripped == "__init__" {
        String::new()
    } else if let Some(pkg) = stripped.strip_suffix("/__init__") {
        pkg.to_string()
    } else {
        stripped
    }
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

/// Convert a dotted module name to a path-style key (`a.b.c` -> `a/b/c`).
fn dotted_to_path(dotted: &str) -> String {
    dotted.replace('.', "/")
}

/// Resolve a relative import to a path key.
///
/// `dots` is the number of leading dots; `module` is the dotted module after
/// the dots (may be empty for `from . import name`). `name` is an imported
/// name used only when `module` is empty.
fn resolve_relative_py(
    importer_file: &str,
    dots: usize,
    module: &str,
    name: Option<&str>,
) -> Option<String> {
    use std::path::Path;
    let dir = Path::new(importer_file)
        .parent()
        .unwrap_or_else(|| Path::new(""));
    let mut parts: Vec<String> = dir
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().to_string()),
            _ => None,
        })
        .collect();
    // 1 dot = current package (importer dir); each extra dot pops one level.
    for _ in 0..dots.saturating_sub(1) {
        parts.pop();
    }
    if !module.is_empty() {
        for seg in module.split('.') {
            parts.push(seg.to_string());
        }
    } else if let Some(n) = name {
        parts.push(n.to_string());
    } else {
        return None;
    }
    Some(parts.join("/"))
}

/// Add the File symbol. Its fqn collapses __init__.py to its package dir so that
/// `import pkg` (key "pkg") resolves to pkg/__init__.py (fqn "pkg").
fn add_file_symbol(g: &mut IrGraph, file: &str, root: Node) -> SymbolId {
    g.add_symbol(Symbol {
        id: SymbolId(0),
        kind: SymbolKind::File,
        name: file.to_string(),
        fqn: py_module_key(file),
        span: walk::span(root, file),
        visibility: Visibility::Unknown,
        confidence: Confidence::Certain,
    })
}

/// Extract class/function definitions: add symbols, Defines edges (from the
/// file), and function complexity.
fn extract_definitions(g: &mut IrGraph, file: &str, src: &str, root: Node, file_id: SymbolId) {
    let lang: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
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

        let (Some(name_node), Some(decl_node)) = (name_node, decl_node) else {
            continue;
        };
        add_definition(g, file, src, file_id, kind, name_node, decl_node);
    }
}

/// Add one definition symbol with its Defines edge and (for functions) complexity.
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
        g.set_complexity(id, walk::cyclomatic(decl_node, src, &COMPLEXITY_RULES));
    }
}

/// Extract calls: intra-file Calls edges (resolved by function name) plus
/// unresolved cross-file calls. `from` is the enclosing function or the file.
fn extract_calls(g: &mut IrGraph, file: &str, src: &str, root: Node, file_id: SymbolId) {
    let lang: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
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

/// Path keys for a single `from ... import ...` statement (absolute or relative).
///
/// Actual tree-sitter-python shapes (verified via to_sexp()):
///   from c.d import x
///       -> (import_from_statement module_name: (dotted_name ...) name: (dotted_name ...))
///   from .sib import y
///       -> (import_from_statement
///             module_name: (relative_import (import_prefix) (dotted_name ...)))
///   from . import sub
///       -> (import_from_statement module_name: (relative_import (import_prefix)))
fn import_from_keys(stmt: Node, file: &str, src: &str) -> Vec<String> {
    let Some(mn) = stmt.child_by_field_name("module_name") else {
        return Vec::new();
    };
    match mn.kind() {
        // Absolute: from c.d import x -> key c/d
        "dotted_name" => vec![dotted_to_path(walk::node_text(mn, src))],
        "relative_import" => relative_import_keys(stmt, mn, file, src),
        _ => Vec::new(),
    }
}

/// Path keys for a `relative_import` module_name (`mn`) within statement `stmt`.
fn relative_import_keys(stmt: Node, mn: Node, file: &str, src: &str) -> Vec<String> {
    let mut dots = 0usize;
    let mut dotted_module = String::new();
    let mut mn_cursor = mn.walk();
    for child in mn.children(&mut mn_cursor) {
        match child.kind() {
            "import_prefix" => {
                dots = walk::node_text(child, src)
                    .chars()
                    .filter(|&c| c == '.')
                    .count();
            }
            "dotted_name" => {
                dotted_module = walk::node_text(child, src).to_string();
            }
            _ => {}
        }
    }
    if !dotted_module.is_empty() {
        // from .sib import y -> resolve against file
        return resolve_relative_py(file, dots, &dotted_module, None)
            .into_iter()
            .collect();
    }
    // from . import sub -> one key per imported name (each a dotted_name)
    let mut keys = Vec::new();
    let mut stmt_cursor = stmt.walk();
    for name_node in stmt.children_by_field_name("name", &mut stmt_cursor) {
        let imported_name = walk::node_text(name_node, src);
        if let Some(key) = resolve_relative_py(file, dots, "", Some(imported_name)) {
            keys.push(key);
        }
    }
    keys
}

/// Collect import path keys for every top-level import statement.
fn collect_import_keys(root: Node, file: &str, src: &str) -> Vec<String> {
    let mut import_keys: Vec<String> = Vec::new();
    let mut cursor = root.walk();
    for stmt in root.children(&mut cursor) {
        match stmt.kind() {
            "import_statement" => {
                // `import a.b` — find the `name:` dotted_name child
                for i in 0..stmt.child_count() {
                    let child = stmt.child(i).unwrap();
                    if child.kind() == "dotted_name" {
                        import_keys.push(dotted_to_path(walk::node_text(child, src)));
                    }
                }
            }
            "import_from_statement" => {
                import_keys.extend(import_from_keys(stmt, file, src));
            }
            _ => {}
        }
    }
    import_keys
}

/// Emit Dependency symbols and Imports edges for each resolved import key.
fn extract_imports(g: &mut IrGraph, file: &str, src: &str, root: Node, file_id: SymbolId) {
    let mut import_keys = collect_import_keys(root, file, src);
    import_keys.sort_unstable();
    import_keys.dedup();
    for key in import_keys {
        let dep_id = g.add_symbol(Symbol {
            id: SymbolId(0),
            kind: SymbolKind::Dependency,
            name: key.clone(),
            fqn: key,
            span: walk::span(root, file),
            visibility: Visibility::Unknown,
            confidence: Confidence::Certain,
        });
        g.add_reference(Reference {
            from: file_id,
            to: dep_id,
            kind: RefKind::Imports,
            span: walk::span(root, file),
            confidence: Confidence::Certain,
        });
    }
}

pub(crate) fn extract(file: &str, src: &str) -> IrGraph {
    let lang: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();

    let mut parser = Parser::new();
    parser.set_language(&lang).expect("load python grammar");
    let tree = parser.parse(src, None).expect("parse python");
    let root = tree.root_node();

    let mut g = IrGraph::new();

    let file_id = add_file_symbol(&mut g, file, root);
    // The file is always an entrypoint (Python has no exports; top-level code runs on import).
    g.mark_entrypoint(file_id);

    walk::tokenize(&mut g, root, src, file, &NORMALIZE_RULES);

    extract_definitions(&mut g, file, src, root, file_id);
    extract_calls(&mut g, file, src, root, file_id);
    extract_imports(&mut g, file, src, root, file_id);

    g
}

#[cfg(test)]
mod tests {
    use super::*;
    use stratify_core::SymbolKind;

    #[test]
    fn file_is_entrypoint() {
        let g = extract("m.py", "def a():\n    pass\n");
        let file = g
            .symbols()
            .iter()
            .find(|s| s.kind == SymbolKind::File)
            .unwrap()
            .id;
        assert_eq!(g.entrypoints(), &[file]);
    }

    #[test]
    fn intra_file_and_top_level_calls() {
        let src = "def a():\n    b()\n\ndef b():\n    pass\n\na()\n";
        let g = extract("x.py", src);
        let file = g
            .symbols()
            .iter()
            .find(|s| s.kind == SymbolKind::File)
            .unwrap()
            .id;
        let a = g.symbols().iter().find(|s| s.name == "a").unwrap().id;
        let b = g.symbols().iter().find(|s| s.name == "b").unwrap().id;
        // a calls b
        assert!(g
            .references()
            .iter()
            .any(|r| matches!(r.kind, RefKind::Calls) && r.from == a && r.to == b));
        // file (top-level) calls a
        assert!(g
            .references()
            .iter()
            .any(|r| matches!(r.kind, RefKind::Calls) && r.from == file && r.to == a));
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
        let names: Vec<_> = g
            .symbols()
            .iter()
            .map(|s| (s.kind, s.name.as_str()))
            .collect();
        assert!(names.contains(&(SymbolKind::File, "foo.py")));
        assert!(names.contains(&(SymbolKind::Class, "Foo")));
        assert!(names.contains(&(SymbolKind::Function, "bar")));
        assert!(names.contains(&(SymbolKind::Function, "baz")));
    }

    #[test]
    fn file_fqn_strips_extension() {
        let g = extract("pkg/mod.py", "def x():\n    pass\n");
        let f = g
            .symbols()
            .iter()
            .find(|s| s.kind == SymbolKind::File)
            .unwrap();
        assert_eq!(f.fqn, "pkg/mod");
    }

    #[test]
    fn emits_normalized_tokens() {
        let g = extract("a.py", "x = 5\n");
        let norms: Vec<&str> = g.tokens().iter().map(|t| t.norm.as_str()).collect();
        assert!(norms.contains(&"ID")); // x
        assert!(norms.contains(&"NUM")); // 5
        assert!(norms.contains(&"="));
    }

    #[test]
    fn absolute_import_keys() {
        // import a.b  -> key a/b ; from c.d import x -> key c/d
        let g = extract("m.py", "import a.b\nfrom c.d import x\n");
        let deps: Vec<&str> = g
            .symbols()
            .iter()
            .filter(|s| s.kind == SymbolKind::Dependency)
            .map(|s| s.name.as_str())
            .collect();
        assert!(deps.contains(&"a/b"), "deps: {deps:?}");
        assert!(deps.contains(&"c/d"), "deps: {deps:?}");
    }

    #[test]
    fn import_key_matches_file_fqn() {
        // pkg/a.py: from b import x -> key "b"; pkg/b.py fqn (when scanned at pkg/) -> "b"
        let importer = extract("a.py", "from b import x\n");
        let imported = extract("b.py", "def z():\n    pass\n");
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
    fn relative_import_with_module() {
        // from pkg/mod.py: from .sib import y -> key pkg/sib
        let g = extract("pkg/mod.py", "from .sib import y\n");
        assert!(
            g.symbols()
                .iter()
                .any(|s| s.kind == SymbolKind::Dependency && s.name == "pkg/sib"),
            "{:?}",
            g.symbols()
                .iter()
                .filter(|s| s.kind == SymbolKind::Dependency)
                .map(|s| &s.name)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn relative_import_bare_names() {
        // from pkg/mod.py: from . import sub -> key pkg/sub (imported name is a submodule)
        let g = extract("pkg/mod.py", "from . import sub\n");
        assert!(g
            .symbols()
            .iter()
            .any(|s| s.kind == SymbolKind::Dependency && s.name == "pkg/sub"));
    }

    #[test]
    fn init_file_fqn_is_package_dir() {
        // pkg/__init__.py represents the package `pkg` -> fqn "pkg", so
        // `import pkg` (key "pkg") resolves to it.
        let g = extract("pkg/__init__.py", "x = 1\n");
        let f = g
            .symbols()
            .iter()
            .find(|s| s.kind == SymbolKind::File)
            .unwrap();
        assert_eq!(f.fqn, "pkg");
    }

    #[test]
    fn nested_init_fqn_is_package_path() {
        let g = extract("a/b/__init__.py", "x = 1\n");
        let f = g
            .symbols()
            .iter()
            .find(|s| s.kind == SymbolKind::File)
            .unwrap();
        assert_eq!(f.fqn, "a/b");
    }

    #[test]
    fn module_file_fqn_unchanged() {
        // regular module files keep path-sans-ext (regression guard)
        let g = extract("pkg/mod.py", "x = 1\n");
        let f = g
            .symbols()
            .iter()
            .find(|s| s.kind == SymbolKind::File)
            .unwrap();
        assert_eq!(f.fqn, "pkg/mod");
    }

    #[test]
    fn top_level_init_fqn_is_empty() {
        let g = extract("__init__.py", "x = 1\n");
        let f = g
            .symbols()
            .iter()
            .find(|s| s.kind == SymbolKind::File)
            .unwrap();
        assert_eq!(f.fqn, "");
    }

    #[test]
    fn records_unresolved_cross_file_call() {
        // `external` is not defined in this file -> recorded as unresolved.
        let g = extract("a.py", "def m():\n    external()\n");
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

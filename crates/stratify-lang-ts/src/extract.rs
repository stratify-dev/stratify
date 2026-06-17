use stratify_core::ir::SymbolId;
use stratify_core::{
    Confidence, IrGraph, RefKind, Reference, Symbol, SymbolKind, Visibility,
};
use stratify_lang::walk::{cyclomatic, enclosing, node_text, span, tokenize, ComplexityRules, NormalizeRules};
use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Node, Parser, Query, QueryCursor};

/// Node kinds and operator texts that define TypeScript token normalization.
///
/// In tree-sitter-typescript `string`/`template_string` are not leaves: the
/// leaf carrying text is `string_fragment`. Mapping `string_fragment` to "STR"
/// (and NOT treating templates as atomic) keeps `${a}` substitution identifiers
/// as "ID", matching the original adapter exactly.
fn normalize_rules() -> NormalizeRules<'static> {
    NormalizeRules {
        identifier_kinds: &[
            "identifier",
            "type_identifier",
            "property_identifier",
            "shorthand_property_identifier",
            "shorthand_property_identifier_pattern",
        ],
        number_kinds: &["number"],
        string_kinds: &["string_fragment"],
        atomic_string_kinds: &[],
    }
}

/// Decision kinds and short-circuit operators for TypeScript cyclomatic counting.
/// Ternaries are counted via `ternary_expression`, so `?` is intentionally absent.
fn complexity_rules() -> ComplexityRules<'static> {
    ComplexityRules {
        decision_kinds: &[
            "if_statement",
            "for_statement",
            "for_in_statement",
            "while_statement",
            "do_statement",
            "switch_case",
            "ternary_expression",
            "catch_clause",
        ],
        operator_texts: &["&&", "||", "??"],
    }
}

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

/// Return true if `node` has an ancestor that is an `export_statement`.
fn is_exported(node: Node) -> bool {
    enclosing(node, &["export_statement"]).is_some()
}

/// Cyclomatic complexity of a TypeScript declaration subtree.
fn cyclomatic_ts(node: Node, src: &str) -> u32 {
    cyclomatic(node, src, &complexity_rules())
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

/// Add the File symbol (fqn = path with TS extension stripped) and mark its
/// scope as an entrypoint (top-level module code runs on import).
fn add_file_symbol(g: &mut IrGraph, file: &str, root: Node) -> SymbolId {
    let file_id = g.add_symbol(Symbol {
        id: SymbolId(0),
        kind: SymbolKind::File,
        name: file.to_string(),
        fqn: strip_ts_ext(file),
        span: span(root, file),
        visibility: Visibility::Unknown,
        confidence: Confidence::Certain,
    });
    g.mark_entrypoint(file_id);
    file_id
}

/// Extract class/function/method/arrow declarations as symbols, with Defines
/// edges from the file, entrypoint marking for exports, and complexity for
/// functions.
fn extract_declarations(g: &mut IrGraph, lang: &Language, root: Node, src: &str, file: &str, file_id: SymbolId) {
    let query = Query::new(
        lang,
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

        let (Some(name_node), Some(decl_node)) = (name_node, decl_node) else {
            continue;
        };
        let name = node_text(name_node, src).to_string();
        let id = g.add_symbol(Symbol {
            id: SymbolId(0),
            kind,
            name: name.clone(),
            fqn: name,
            span: span(decl_node, file),
            visibility: Visibility::Unknown,
            confidence: Confidence::Certain,
        });
        g.add_reference(Reference {
            from: file_id,
            to: id,
            kind: RefKind::Defines,
            span: span(decl_node, file),
            confidence: Confidence::Certain,
        });
        // Exported symbols are entrypoints (reachable from other modules).
        if is_exported(decl_node) {
            g.mark_entrypoint(id);
        }
        // Set complexity for functions.
        if kind == SymbolKind::Function {
            g.set_complexity(id, cyclomatic_ts(decl_node, src));
        }
    }
}

/// Resolve relative import specifiers to Dependency symbols + Imports edges.
fn extract_imports(g: &mut IrGraph, lang: &Language, root: Node, src: &str, file: &str, file_id: SymbolId) {
    let import_q = Query::new(
        lang,
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
            let spec_text = node_text(cap.node, src);
            if let Some(key) = resolve_ts_import(file, spec_text) {
                let dep_id = g.add_symbol(Symbol {
                    id: SymbolId(0),
                    kind: SymbolKind::Dependency,
                    name: key.clone(),
                    fqn: key,
                    span: span(cap.node, file),
                    visibility: Visibility::Unknown,
                    confidence: Confidence::Certain,
                });
                g.add_reference(Reference {
                    from: file_id,
                    to: dep_id,
                    kind: RefKind::Imports,
                    span: span(cap.node, file),
                    confidence: Confidence::Certain,
                });
            }
        }
    }
}

/// Collect intra-file Calls edges and unresolved (cross-file) calls, then emit
/// them deduplicated into the graph.
fn extract_calls(g: &mut IrGraph, lang: &Language, root: Node, src: &str, file: &str, file_id: SymbolId) {
    // Build a map of Function name -> SymbolId.
    let name_to_id: std::collections::HashMap<String, SymbolId> = g
        .symbols()
        .iter()
        .filter(|s| matches!(s.kind, SymbolKind::Function))
        .map(|s| (s.name.clone(), s.id))
        .collect();

    let call_q = Query::new(
        lang,
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
    let mut unresolved: Vec<(SymbolId, String)> = Vec::new();
    while let Some(m) = call_matches.next() {
        let mut callee_name = None;
        let mut call_node = None;
        for cap in m.captures {
            if cap.index == callee_idx {
                callee_name = Some(node_text(cap.node, src).to_string());
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
            span: span(root, file),
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
    let lang = lang_for(file);

    let mut parser = Parser::new();
    parser.set_language(&lang).expect("load typescript grammar");
    let tree = parser.parse(src, None).expect("parse typescript");
    let root = tree.root_node();

    let mut g = IrGraph::new();

    let file_id = add_file_symbol(&mut g, file, root);
    tokenize(&mut g, root, src, file, &normalize_rules());
    extract_declarations(&mut g, &lang, root, src, file, file_id);
    extract_imports(&mut g, &lang, root, src, file, file_id);
    extract_calls(&mut g, &lang, root, src, file, file_id);

    g
}

#[cfg(test)]
mod tests {
    use super::*;
    use stratify_core::SymbolKind;

    #[test]
    fn extracts_class_function_method_arrow() {
        let src = "export class Foo {\n  bar() {}\n}\nfunction baz() {}\nconst qux = () => {};\n";
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
        let file = g
            .symbols()
            .iter()
            .find(|s| s.kind == SymbolKind::File)
            .unwrap()
            .id;
        let api = g.symbols().iter().find(|s| s.name == "api").unwrap().id;
        let eps = g.entrypoints();
        assert!(eps.contains(&file));
        assert!(
            eps.contains(&api),
            "exported function should be an entrypoint"
        );
    }

    #[test]
    fn intra_file_call_edge() {
        let src = "function a() { b(); }\nfunction b() {}\na();\n";
        let g = extract("x.ts", src);
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
        assert!(g
            .references()
            .iter()
            .any(|r| matches!(r.kind, RefKind::Imports)
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

    #[test]
    fn records_unresolved_cross_file_call() {
        // `external` is not defined in this file -> recorded as unresolved.
        let g = extract("a.ts", "function m() { external(); }\n");
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

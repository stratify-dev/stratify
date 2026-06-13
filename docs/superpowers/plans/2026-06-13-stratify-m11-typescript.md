# Stratify M11 (TypeScript Adapter) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a TypeScript adapter so all six analyses (dead code, duplication, complexity, hotspots, cycles, layer boundaries) run on TypeScript, the same way they run on Java and Ruby.

**Architecture:** A new `stratify-lang-ts` crate parses `.ts`/`.tsx`/`.mts`/`.cts` with tree-sitter-typescript and emits the full IR: File/Class/Function symbols + `Defines` edges, entrypoints (the file scope plus exported declarations), intra-file/top-level `Calls` edges, normalized tokens (for duplication), per-function cyclomatic complexity, and `import` edges (relative specifiers resolved to extension-stripped path keys). No analysis or core change is needed — the adapter fills the existing IR.

**Tech Stack:** Rust, tree-sitter 0.24 + tree-sitter-typescript 0.23, plus the existing workspace crates.

**Prerequisite reading (the two reference adapters — you will combine their techniques):**
- `crates/stratify-lang-java/src/extract.rs` — symbols, `Defines`, intra-file `Calls` + `enclosing_method_id`, `collect_leaves`, `normalize_java`, `count_decisions_java`/`cyclomatic_java`, package + `import` Dependency edges, `set_complexity`.
- `crates/stratify-lang-ruby/src/extract.rs` — File symbol with `fqn`, file-scope entrypoint via `mark_entrypoint`, top-level vs intra-method call attribution, `resolve_require_relative` (relative path normalization), the streaming-iterator query loop.

**Key TS-specific decisions (the crux):**
- **Grammar selection:** `.tsx` uses `tree_sitter_typescript::LANGUAGE_TSX`; `.ts`/`.mts`/`.cts` use `LANGUAGE_TYPESCRIPT`. `handles_extension` covers `ts`, `tsx`, `mts`, `cts`.
- **File export key:** a TS File symbol's `fqn` is its path WITH the extension stripped (`src/foo.ts` -> `src/foo`). Import specifiers omit extensions, so the import key must match this stripped form.
- **Import resolution:** only RELATIVE specifiers (starting with `.`) become edges. `import x from "./foo/bar"` in `src/a.ts` resolves to key `src/foo/bar` (join dir + specifier, normalize `.`/`..`, strip a trailing TS extension if present). Bare specifiers (`"react"`) are skipped (external). This mirrors Ruby's `resolve_require_relative` but strips rather than appends the extension.
- **Entrypoints:** the File symbol (top-level module code runs on import) AND any exported Function/Class (reachable from other modules). Detect "exported" by an ancestor `export_statement`.
- **Functions come in three shapes:** `function_declaration`, `method_definition` (class methods), and arrow/function-expression consts (`const f = () => {}` / `const f = function(){}`). All become `SymbolKind::Function`.

---

## File Structure

```
Cargo.toml                              MODIFY: workspace member + tree-sitter-typescript dep
crates/stratify-lang-ts/Cargo.toml      CREATE
crates/stratify-lang-ts/src/lib.rs      CREATE: TsAdapter
crates/stratify-lang-ts/src/extract.rs  CREATE: parse -> full IR
crates/stratify-cli/Cargo.toml          MODIFY: depend on stratify-lang-ts
crates/stratify-cli/src/run.rs          MODIFY: register TsAdapter
crates/stratify-cli/tests/sample-ts/    CREATE: fixture .ts files
crates/stratify-cli/tests/e2e_ts.rs     CREATE: end-to-end on TypeScript
```

---

## Task 1: Scaffold + TsAdapter + structure & tokens (`stratify-lang-ts`)

**Files:**
- Modify: `Cargo.toml` (workspace)
- Create: `crates/stratify-lang-ts/Cargo.toml`, `src/lib.rs`, `src/extract.rs`

- [ ] **Step 1: Register the crate and dependency**

In the root `Cargo.toml`: add `"crates/stratify-lang-ts"` to `members`, and add to `[workspace.dependencies]`:

```toml
tree-sitter-typescript = "0.23"
```

- [ ] **Step 2: Crate manifest**

Create `crates/stratify-lang-ts/Cargo.toml`:

```toml
[package]
name = "stratify-lang-ts"
edition.workspace = true
version.workspace = true
license.workspace = true

[dependencies]
stratify-core = { path = "../stratify-core" }
stratify-lang = { path = "../stratify-lang" }
tree-sitter = { workspace = true }
tree-sitter-typescript = { workspace = true }
streaming-iterator = "0.1"
```

- [ ] **Step 3: Adapter**

Create `crates/stratify-lang-ts/src/lib.rs`:

```rust
mod extract;

use stratify_core::IrGraph;
use stratify_lang::{AdapterError, LanguageAdapter};

pub struct TsAdapter;

impl LanguageAdapter for TsAdapter {
    fn language(&self) -> &'static str {
        "typescript"
    }

    fn handles_extension(&self, ext: &str) -> bool {
        matches!(ext, "ts" | "tsx" | "mts" | "cts")
    }

    fn parse_file(&self, path: &str, source: &str) -> Result<IrGraph, AdapterError> {
        Ok(extract::extract(path, source))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_parses_a_function() {
        let a = TsAdapter;
        assert!(a.handles_extension("ts"));
        assert!(a.handles_extension("tsx"));
        let g = a.parse_file("a.ts", "function hi() {}").unwrap();
        assert!(g.symbols().iter().any(|s| s.name == "hi"));
    }
}
```

- [ ] **Step 4: Extractor — structure + tokens**

Create `crates/stratify-lang-ts/src/extract.rs`. Mirror the Java extractor's scaffolding (`parser` builder, `span`, `text`, `collect_leaves`, the streaming-iterator query loop). TS specifics:

- `parser_for(file)` picks the grammar: `.tsx` -> `LANGUAGE_TSX`, else `LANGUAGE_TYPESCRIPT`.
- File symbol: `SymbolKind::File`, `Confidence::Certain`, `name` = the file path, `fqn` = the path with a trailing TS extension stripped (use a helper `strip_ts_ext(path)` that removes a trailing `.ts`/`.tsx`/`.mts`/`.cts`/`.js`/`.jsx`).
- Definition query (verify node/field names against the grammar with a temporary `to_sexp()` print if a test fails; the grammar uses `type_identifier` for class names and `property_identifier` for method names):

```rust
    let query = Query::new(
        &lang,
        r#"
        (class_declaration name: (type_identifier) @class.name) @class.node
        (function_declaration name: (identifier) @func.name) @func.node
        (method_definition name: (property_identifier) @method.name) @method.node
        (variable_declarator name: (identifier) @arrow.name value: (arrow_function)) @arrow.node
        (variable_declarator name: (identifier) @arrow.name value: (function_expression)) @arrow.node
        "#,
    ).expect("ts query");
```

  - class match -> `SymbolKind::Class`, fqn = class name (bare).
  - function/method/arrow match -> `SymbolKind::Function`, fqn = name.
  - Each gets a `Defines` edge from the file, `Confidence::Certain`. (`lang` is the chosen `Language` cloned for reuse — `Query::new` takes `&Language`; build the language once and pass references.)
- Tokens: a `collect_leaves` helper (copy verbatim from Java) and `emit_tokens` calling `normalize_ts`:

```rust
fn normalize_ts(kind: &str, text: &str) -> String {
    match kind {
        "identifier" | "type_identifier" | "property_identifier"
        | "shorthand_property_identifier" | "shorthand_property_identifier_pattern" => "ID".to_string(),
        "number" => "NUM".to_string(),
        "string" | "template_string" | "string_fragment" => "STR".to_string(),
        _ => text.to_string(),
    }
}
```

  Call `emit_tokens(&mut g, file, src, root)` right after the File symbol is created, exactly like Java/Ruby.

Add tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use stratify_core::SymbolKind;

    #[test]
    fn extracts_class_function_method_arrow() {
        let src = "export class Foo {\n  bar() {}\n}\nfunction baz() {}\nconst qux = () => {};\n";
        let g = extract("Foo.ts", src);
        let names: Vec<_> = g.symbols().iter().map(|s| (s.kind, s.name.as_str())).collect();
        assert!(names.contains(&(SymbolKind::File, "Foo.ts")));
        assert!(names.contains(&(SymbolKind::Class, "Foo")));
        assert!(names.contains(&(SymbolKind::Function, "bar")));
        assert!(names.contains(&(SymbolKind::Function, "baz")));
        assert!(names.contains(&(SymbolKind::Function, "qux")));
    }

    #[test]
    fn file_fqn_strips_extension() {
        let g = extract("src/a.ts", "function x() {}");
        let f = g.symbols().iter().find(|s| s.kind == SymbolKind::File).unwrap();
        assert_eq!(f.fqn, "src/a");
    }

    #[test]
    fn emits_normalized_tokens() {
        let g = extract("a.ts", "const x = 5;");
        let norms: Vec<&str> = g.tokens().iter().map(|t| t.norm.as_str()).collect();
        assert!(norms.contains(&"const"));
        assert!(norms.contains(&"ID"));  // x
        assert!(norms.contains(&"NUM")); // 5
    }

    #[test]
    fn tsx_parses() {
        let g = extract("c.tsx", "function C() { return null; }");
        assert!(g.symbols().iter().any(|s| s.name == "C"));
    }
}
```

- [ ] **Step 5: Run, verify pass**

Run: `cargo test -p stratify-lang-ts` (if `cargo` missing: `source "$HOME/.cargo/env"`)
Expected: PASS. If a query matches nothing, discover real node kinds with a temporary `eprintln!("{}", root.to_sexp())` under `-- --nocapture`, fix, REMOVE the print, report it.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml crates/stratify-lang-ts
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(ts): scaffold adapter, extract symbols and normalized tokens"
```

---

## Task 2: Entrypoints + calls + complexity (`stratify-lang-ts`)

**Files:**
- Modify: `crates/stratify-lang-ts/src/extract.rs`

- [ ] **Step 1: Add failing tests**

```rust
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
```

- [ ] **Step 2: Implement**

In `extract`, after the definition pass:

1. **Entrypoints:** `g.mark_entrypoint(file_id);` for the File. When adding a Function/Class symbol, mark it an entrypoint if it is exported. Detect "exported" by walking ancestors of the declaration node: if any ancestor's kind is `export_statement`, it is exported. Add a helper:

```rust
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
```

   For an arrow const, the declaration node to test is the `lexical_declaration` (the `variable_declarator`'s parent); `export const f = ...` wraps the lexical_declaration in an `export_statement`. Testing ancestors of the captured node handles all shapes.

2. **Calls:** copy `enclosing_method_id` from Java verbatim. Query call sites:

```rust
    let call_q = Query::new(&lang, r#"
        (call_expression function: (identifier) @callee) @call
        (call_expression function: (member_expression property: (property_identifier) @callee)) @call
        "#).expect("ts call query");
```

   For each callee whose text matches an in-file Function name (`name_to_id`), add a `Calls` edge `Confidence::Likely` from the enclosing function (via `enclosing_method_id`) or from `file_id` if top-level. Deduplicate identical `(from,to,Calls)` edges (as the Ruby adapter does).

3. **Complexity:** add `count_decisions_ts` / `cyclomatic_ts` and call `g.set_complexity(id, cyclomatic_ts(decl_node))` for each Function symbol:

```rust
fn count_decisions_ts(node: Node) -> u32 {
    let mut count = 0u32;
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "if_statement" | "for_statement" | "for_in_statement" | "while_statement"
            | "do_statement" | "switch_case" | "ternary_expression" | "catch_clause"
            | "&&" | "||" | "??" => {
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
```

- [ ] **Step 3: Run, verify pass**

Run: `cargo test -p stratify-lang-ts`
Expected: PASS. Adjust decision-node / export-detection kinds via `to_sexp()` if a count is off; remove any debug print; report adjustments.

- [ ] **Step 4: Commit**

```bash
git add crates/stratify-lang-ts
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(ts): entrypoints (file + exports), call edges, complexity"
```

---

## Task 3: Import edges (`stratify-lang-ts`)

**Files:**
- Modify: `crates/stratify-lang-ts/src/extract.rs`

- [ ] **Step 1: Add failing tests**

```rust
    #[test]
    fn relative_import_edge() {
        // from src/a.ts, import from "./b" -> key src/b ; bare specifier ignored.
        let g = extract("src/a.ts", "import { x } from \"./b\";\nimport React from \"react\";\n");
        let dep = g.symbols().iter().find(|s| s.kind == SymbolKind::Dependency && s.name == "src/b");
        assert!(dep.is_some(), "expected Dependency keyed src/b");
        assert!(!g.symbols().iter().any(|s| s.kind == SymbolKind::Dependency && s.name == "react"),
            "bare specifier should be ignored");
        let file_id = g.symbols().iter().find(|s| s.kind == SymbolKind::File).unwrap().id;
        assert!(g.references().iter().any(|r|
            matches!(r.kind, RefKind::Imports) && r.from == file_id && r.to == dep.unwrap().id));
    }

    #[test]
    fn import_key_matches_file_fqn() {
        // a.ts importing "./b" yields key "b"; b.ts has fqn "b" -> they match.
        let importer = extract("a.ts", "import \"./b\";\n");
        let imported = extract("b.ts", "export const z = 1;\n");
        let key = importer.symbols().iter().find(|s| s.kind == SymbolKind::Dependency).unwrap().name.clone();
        let fqn = imported.symbols().iter().find(|s| s.kind == SymbolKind::File).unwrap().fqn.clone();
        assert_eq!(key, fqn);
    }

    #[test]
    fn parent_dir_import() {
        let g = extract("src/sub/a.ts", "import \"../b\";\n");
        assert!(g.symbols().iter().any(|s| s.kind == SymbolKind::Dependency && s.name == "src/b"));
    }
```

- [ ] **Step 2: Implement**

Add a resolver that mirrors Ruby's `resolve_require_relative` but STRIPS a trailing TS/JS extension instead of appending one, and only handles relative specifiers:

```rust
fn resolve_ts_import(importer_file: &str, spec: &str) -> Option<String> {
    if !spec.starts_with('.') {
        return None; // bare/package specifier — external, skip
    }
    use std::path::{Component, Path};
    let dir = Path::new(importer_file).parent().unwrap_or_else(|| Path::new(""));
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
```

Add a `strip_ts_ext(path)` helper (used for the File fqn in Task 1; if you implemented File fqn inline, factor it out here so import keys and file fqns use the same stripping logic). Query imports:

```rust
    let import_q = Query::new(&lang,
        r#"(import_statement source: (string (string_fragment) @spec))"#
    ).expect("ts import query");
```

For each `@spec`, `resolve_ts_import(file, spec_text)`; if `Some(key)`, add a `Dependency` symbol named `key` and an `Imports` edge from `file_id` (`Confidence::Certain`). Skip `None` (bare specifiers).

- [ ] **Step 3: Run, verify pass**

Run: `cargo test -p stratify-lang-ts`
Expected: PASS. If the import string node nests differently, discover with `to_sexp()`, fix the query, remove the print, report it.

- [ ] **Step 4: Commit**

```bash
git add crates/stratify-lang-ts
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(ts): relative import edges resolved to extension-stripped keys"
```

---

## Task 4: Wire into the CLI + end-to-end (`stratify-cli`)

**Files:**
- Modify: `crates/stratify-cli/Cargo.toml`, `crates/stratify-cli/src/run.rs`
- Create: `crates/stratify-cli/tests/sample-ts/app.ts`, `crates/stratify-cli/tests/sample-ts/unused.ts`
- Create: `crates/stratify-cli/tests/e2e_ts.rs`

- [ ] **Step 1: Register the adapter**

In `crates/stratify-cli/Cargo.toml` `[dependencies]`, add `stratify-lang-ts = { path = "../stratify-lang-ts" }`. In `run.rs` `analyze_repo`, add `Box::new(stratify_lang_ts::TsAdapter)` to the adapters vector (alongside JavaAdapter and RubyAdapter).

- [ ] **Step 2: Create fixtures**

`crates/stratify-cli/tests/sample-ts/app.ts`:

```typescript
function helper(): string {
  return "used";
}

helper();
```

`crates/stratify-cli/tests/sample-ts/unused.ts`:

```typescript
function neverCalled(): string {
  return "dead";
}
```

(`neverCalled` is not exported and not called -> dead. `helper` is called at top level -> possibly unused / info, matching the Java/Ruby pattern for Likely intra-file edges.)

- [ ] **Step 3: End-to-end test**

Create `crates/stratify-cli/tests/e2e_ts.rs`:

```rust
use std::path::Path;

#[test]
fn sample_ts_reports_dead_code() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sample-ts");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_stratify"))
        .arg("check")
        .arg(&dir)
        .arg("--format")
        .arg("json")
        .output()
        .expect("run stratify binary");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("\"rule\": \"dead_code\""), "stdout: {stdout}");
    assert!(stdout.contains("neverCalled"), "stdout: {stdout}");
}
```

- [ ] **Step 4: Run + manual smoke**

Run: `cargo test -p stratify-cli`
Expected: PASS including `e2e_ts`.

Manual — confirm TS joins the other languages in one run:
```bash
cargo build
./target/debug/stratify check crates/stratify-cli/tests --format json | grep -o '"file": "sample-ts/[a-z.]*"' | sort -u
```
Expected: the sample-ts files appear in findings, proving TS flows through the same pipeline as Java and Ruby.

- [ ] **Step 5: Commit**

```bash
git add crates/stratify-cli
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(cli): register TypeScript adapter, end-to-end TS dead-code"
```

---

## Task 5: fmt, clippy, lockfile

- [ ] **Step 1: Format and lint**

Run: `cargo fmt --all && cargo clippy --all-targets -- -D warnings`
Fix any warning properly (no blanket `#[allow]`). Re-run `cargo test` after any fix.

- [ ] **Step 2: Full suite**

Run: `cargo test`
Expected: all crates green, including the new `stratify-lang-ts` tests and `e2e_ts`.

- [ ] **Step 3: Commit**

```bash
git add -A
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "chore: fmt, clippy clean, update lockfile for typescript"
```

---

## Self-Review Notes

Spec coverage for M11:
- New `stratify-lang-ts` crate emitting the full IR (symbols, Defines, tokens, entrypoints, calls, complexity, imports): Tasks 1-3. Covered.
- All six analyses light up on TS because they read only the IR — no analysis change. Verified end to end by Task 4.
- CLI registration + e2e: Task 4. Covered.

Deferred (correctly out of M11): `index.ts` directory-import resolution (`./foo` -> `foo/index.ts`), `.js`/`.jsx` plain-JavaScript files (the grammar would parse them but they're out of scope here), re-exports (`export { x } from "./y"`), type-only imports vs value imports, namespace/`require` interop, and decorators. The adapter shares the known cross-cutting limitation: intra-file calls are `Likely`, so a function reached only inside its file shows as "possibly unused".

Known M11 characteristics (acceptable, consistent with Java/Ruby):
- Three function shapes (declaration, method, arrow/expression const) are captured; other shapes (object-method shorthand, getters/setters) are not, a later refinement.
- Class names are `type_identifier` nodes in the TS grammar; method names are `property_identifier`. Tokens normalize all identifier-family kinds to `ID`.
- Exported declarations are entrypoints (reachable cross-module), reducing false dead-code on library code; non-exported, never-called functions are flagged.
- Import edges only form for relative specifiers that resolve to an in-repo file fqn; package imports are external and ignored.

Type consistency: `TsAdapter`, `extract::extract`, `strip_ts_ext`, `resolve_ts_import`, `is_exported`, `enclosing_method_id`, `count_decisions_ts`/`cyclomatic_ts`, `normalize_ts`, `collect_leaves`, and the IR APIs (`add_symbol`, `mark_entrypoint`, `set_complexity`, `add_token`, `add_reference`) are used consistently with their M1-M10 definitions.

# Stratify M12 (Python Adapter) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a Python adapter so all six analyses run on Python, alongside Java, Ruby, and TypeScript.

**Architecture:** A new `stratify-lang-py` crate parses `.py`/`.pyi` with tree-sitter-python and emits the full IR: File/Class/Function symbols + `Defines` edges, the file-scope entrypoint, intra-file/top-level `Calls` edges, normalized tokens, per-function cyclomatic complexity, and `import` edges (dotted modules and relative imports resolved to path-style keys that match File fqns). No analysis or core change — the adapter fills the existing IR.

**Tech Stack:** Rust, tree-sitter 0.24 + tree-sitter-python 0.23, plus the existing workspace crates.

**Prerequisite reading (closest reference is the TypeScript adapter — mirror its structure):**
- `crates/stratify-lang-ts/src/extract.rs` — File fqn = path-sans-ext, symbols + Defines, tokens (`collect_leaves`/`normalize_*`), calls + `enclosing_method_id`, `count_decisions_*`/`cyclomatic_*`, import edges + path resolution, the streaming-iterator query loop.
- `crates/stratify-lang-ruby/src/extract.rs` — file-scope entrypoint via `mark_entrypoint` (Python has no exports, so entrypoints work like Ruby: the File only).

**Key Python-specific decisions (the crux):**
- **Methods are `function_definition`** nested inside a `class_definition` body — the same node kind as top-level functions. One query `(function_definition name: (identifier))` captures both. Classes are `(class_definition name: (identifier))`.
- **Entrypoints = the File symbol only** (top-level module code runs on import; `if __name__ == "__main__": main()` calls land at top level and attribute to the File). Python has no `export`, so unlike TS we do NOT mark individual defs as entrypoints. A module-level function never called at top level shows as "possibly unused", consistent with Ruby. (Treating `__all__` / public names as entrypoints is a future refinement.)
- **File fqn = path with extension stripped** (`pkg/mod.py` -> `pkg/mod`), same as TS/Ruby, so import keys match.
- **Import resolution to path keys:**
  - Absolute `import a.b.c` -> key `a/b/c`. `from a.b import x` -> key `a/b` (link importer to the from-module's file).
  - Relative `from .sib import x` from `pkg/mod.py` -> `pkg/sib`. `from ..other import x` from `a/b/mod.py` -> `a/other`. `from . import sub` (dots only, no module) -> one key per imported name: `pkg/sub`.
  - Dotted module `a.b` maps to path `a/b` (dots -> slashes). Bare top-level `import a` -> `a`.

---

## File Structure

```
Cargo.toml                              MODIFY: workspace member + tree-sitter-python dep
crates/stratify-lang-py/Cargo.toml      CREATE
crates/stratify-lang-py/src/lib.rs      CREATE: PyAdapter
crates/stratify-lang-py/src/extract.rs  CREATE: parse -> full IR
crates/stratify-cli/Cargo.toml          MODIFY: depend on stratify-lang-py
crates/stratify-cli/src/run.rs          MODIFY: register PyAdapter
crates/stratify-cli/tests/sample-py/    CREATE: fixture .py files
crates/stratify-cli/tests/e2e_py.rs     CREATE: end-to-end on Python
```

---

## Task 1: Scaffold + PyAdapter + structure & tokens (`stratify-lang-py`)

**Files:**
- Modify: `Cargo.toml` (workspace)
- Create: `crates/stratify-lang-py/Cargo.toml`, `src/lib.rs`, `src/extract.rs`

- [ ] **Step 1: Register crate + dependency**

Root `Cargo.toml`: add `"crates/stratify-lang-py"` to `members`; add `tree-sitter-python = "0.23"` to `[workspace.dependencies]`.

- [ ] **Step 2: Crate manifest**

Create `crates/stratify-lang-py/Cargo.toml`:

```toml
[package]
name = "stratify-lang-py"
edition.workspace = true
version.workspace = true
license.workspace = true

[dependencies]
stratify-core = { path = "../stratify-core" }
stratify-lang = { path = "../stratify-lang" }
tree-sitter = { workspace = true }
tree-sitter-python = { workspace = true }
streaming-iterator = "0.1"
```

- [ ] **Step 3: Adapter**

Create `crates/stratify-lang-py/src/lib.rs`:

```rust
mod extract;

use stratify_core::IrGraph;
use stratify_lang::{AdapterError, LanguageAdapter};

pub struct PyAdapter;

impl LanguageAdapter for PyAdapter {
    fn language(&self) -> &'static str {
        "python"
    }

    fn handles_extension(&self, ext: &str) -> bool {
        matches!(ext, "py" | "pyi")
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
        let a = PyAdapter;
        assert!(a.handles_extension("py"));
        let g = a.parse_file("a.py", "def hi():\n    pass\n").unwrap();
        assert!(g.symbols().iter().any(|s| s.name == "hi"));
    }
}
```

- [ ] **Step 4: Extractor — structure + tokens**

Create `crates/stratify-lang-py/src/extract.rs`, mirroring the TS extractor's scaffolding. Python specifics:

- Language: `tree_sitter_python::LANGUAGE` (one grammar, no tsx-style split). Build `let lang: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();`.
- File symbol: `SymbolKind::File`, `name` = path, `fqn` = `strip_py_ext(path)` (strip a trailing `.py`/`.pyi`).
- Definition query:

```rust
    let query = Query::new(&lang, r#"
        (class_definition name: (identifier) @class.name) @class.node
        (function_definition name: (identifier) @func.name) @func.node
        "#).expect("py query");
```

  - class -> `SymbolKind::Class` (fqn = name), function -> `SymbolKind::Function` (fqn = name; this also catches methods inside classes). `Defines` edge from file, `Confidence::Certain`.
- Tokens: `collect_leaves` (copy from TS/Java) + `emit_tokens` + `normalize_py`:

```rust
fn normalize_py(kind: &str, text: &str) -> String {
    match kind {
        "identifier" => "ID".to_string(),
        "integer" | "float" => "NUM".to_string(),
        "string" | "string_content" | "concatenated_string" => "STR".to_string(),
        _ => text.to_string(),
    }
}
```

  Call `emit_tokens(&mut g, file, src, root)` right after the File symbol.

Tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use stratify_core::SymbolKind;

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
```

- [ ] **Step 5: Run, verify pass**

Run: `cargo test -p stratify-lang-py` (if `cargo` missing: `source "$HOME/.cargo/env"`)
Expected: PASS. Discover real node kinds with a temporary `to_sexp()` print if a query matches nothing; fix, remove the print, report it.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml crates/stratify-lang-py
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(py): scaffold adapter, extract symbols and normalized tokens"
```

---

## Task 2: Entrypoints + calls + complexity (`stratify-lang-py`)

**Files:**
- Modify: `crates/stratify-lang-py/src/extract.rs`

- [ ] **Step 1: Add failing tests**

```rust
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
```

- [ ] **Step 2: Implement**

In `extract`, after the definition pass:

1. **Entrypoint:** `g.mark_entrypoint(file_id)` for the File only.
2. **Complexity:** for each Function symbol, `g.set_complexity(id, cyclomatic_py(decl_node))`:

```rust
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
```

3. **Calls:** copy `enclosing_method_id` from the TS/Java adapter. Query:

```rust
    let call_q = Query::new(&lang, r#"
        (call function: (identifier) @callee) @call
        (call function: (attribute attribute: (identifier) @callee)) @call
        "#).expect("py call query");
```

   For each callee matching an in-file Function name (`name_to_id`), add a `Calls` edge (`Confidence::Likely`) from the enclosing function or `file_id` (top-level). Deduplicate identical `(from,to,Calls)` edges.

- [ ] **Step 3: Run, verify pass**

Run: `cargo test -p stratify-lang-py`
Expected: PASS. If `computes_complexity` is off (`x > 0 and x < 9` should contribute exactly 1 via `boolean_operator`), inspect with `to_sexp()`, fix the decision-kind list, remove the print, report. Don't change the expected count (4 is correct).

- [ ] **Step 4: Commit**

```bash
git add crates/stratify-lang-py
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(py): file entrypoint, call edges, complexity"
```

---

## Task 3: Import edges (`stratify-lang-py`)

**Files:**
- Modify: `crates/stratify-lang-py/src/extract.rs`

- [ ] **Step 1: Add failing tests**

```rust
    #[test]
    fn absolute_import_keys() {
        // import a.b  -> key a/b ; from c.d import x -> key c/d
        let g = extract("m.py", "import a.b\nfrom c.d import x\n");
        let deps: Vec<&str> = g.symbols().iter()
            .filter(|s| s.kind == SymbolKind::Dependency).map(|s| s.name.as_str()).collect();
        assert!(deps.contains(&"a/b"), "deps: {deps:?}");
        assert!(deps.contains(&"c/d"), "deps: {deps:?}");
    }

    #[test]
    fn import_key_matches_file_fqn() {
        // pkg/a.py: from b import x -> key "b"; pkg/b.py fqn (when scanned at pkg/) -> "b"
        let importer = extract("a.py", "from b import x\n");
        let imported = extract("b.py", "def z():\n    pass\n");
        let key = importer.symbols().iter().find(|s| s.kind == SymbolKind::Dependency).unwrap().name.clone();
        let fqn = imported.symbols().iter().find(|s| s.kind == SymbolKind::File).unwrap().fqn.clone();
        assert_eq!(key, fqn);
    }

    #[test]
    fn relative_import_with_module() {
        // from pkg/mod.py: from .sib import y -> key pkg/sib
        let g = extract("pkg/mod.py", "from .sib import y\n");
        assert!(g.symbols().iter().any(|s| s.kind == SymbolKind::Dependency && s.name == "pkg/sib"),
            "{:?}", g.symbols().iter().filter(|s| s.kind == SymbolKind::Dependency).map(|s| &s.name).collect::<Vec<_>>());
    }

    #[test]
    fn relative_import_bare_names() {
        // from pkg/mod.py: from . import sub -> key pkg/sub (imported name is a submodule)
        let g = extract("pkg/mod.py", "from . import sub\n");
        assert!(g.symbols().iter().any(|s| s.kind == SymbolKind::Dependency && s.name == "pkg/sub"));
    }
```

- [ ] **Step 2: Implement**

Add helpers and an import pass. Discover the exact node shapes with `to_sexp()` first (run a throwaway test printing the tree for `import a.b`, `from c.d import x`, `from .sib import y`, `from . import sub`), then implement. Expected shapes in tree-sitter-python:
- `import a.b` -> `(import_statement (dotted_name (identifier) (identifier)))`
- `from c.d import x` -> `(import_from_statement module_name: (dotted_name ...) name: (dotted_name (identifier)))`
- `from .sib import y` -> `(import_from_statement module_name: (relative_import (import_prefix) (dotted_name ...)) name: ...)`
- `from . import sub` -> `(import_from_statement (relative_import (import_prefix)) name: (dotted_name (identifier)))`

Helpers (the path math mirrors Ruby/TS relative resolution):

```rust
fn dotted_to_path(dotted: &str) -> String {
    dotted.replace('.', "/")
}

/// Resolve a relative import. `dots` is the number of leading dots; `module`
/// is the dotted module after the dots (may be empty for `from . import name`).
/// `name` is an imported name used only when `module` is empty.
fn resolve_relative_py(importer_file: &str, dots: usize, module: &str, name: Option<&str>) -> Option<String> {
    use std::path::Path;
    let dir = Path::new(importer_file).parent().unwrap_or_else(|| Path::new(""));
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
```

Import pass (adjust queries to the discovered shapes):
- `import_statement` with a `dotted_name` child: key = `dotted_to_path(text)`.
- `import_from_statement` with `module_name: (dotted_name)` (absolute): key = `dotted_to_path(text)`.
- `import_from_statement` with `module_name: (relative_import)`: parse the `relative_import` text to count leading dots and the trailing dotted module (if any); if a module is present, `resolve_relative_py(file, dots, module, None)`; if only dots, emit one key per imported `name:` via `resolve_relative_py(file, dots, "", Some(name))`.

For each resolved `Some(key)`, add a `Dependency` symbol named `key` + an `Imports` edge from `file_id` (`Confidence::Certain`). Skip unresolved.

- [ ] **Step 3: Run, verify pass**

Run: `cargo test -p stratify-lang-py`
Expected: PASS. Use `to_sexp()` to nail the relative_import / import shapes; remove any debug print; report adjustments.

- [ ] **Step 4: Commit**

```bash
git add crates/stratify-lang-py
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(py): import edges (dotted + relative) resolved to path keys"
```

---

## Task 4: Wire into the CLI + end-to-end (`stratify-cli`)

**Files:**
- Modify: `crates/stratify-cli/Cargo.toml`, `crates/stratify-cli/src/run.rs`
- Create: `crates/stratify-cli/tests/sample-py/app.py`, `crates/stratify-cli/tests/sample-py/unused.py`
- Create: `crates/stratify-cli/tests/e2e_py.rs`

- [ ] **Step 1: Register the adapter**

In `crates/stratify-cli/Cargo.toml` `[dependencies]`, add `stratify-lang-py = { path = "../stratify-lang-py" }`. In `run.rs` `analyze_repo`, add `Box::new(stratify_lang_py::PyAdapter)` to the adapters vector.

- [ ] **Step 2: Fixtures**

`crates/stratify-cli/tests/sample-py/app.py`:

```python
def helper():
    return "used"


helper()
```

`crates/stratify-cli/tests/sample-py/unused.py`:

```python
def never_called():
    return "dead"
```

(`never_called` is never called -> dead; `helper` called at top level -> possibly unused / info.)

- [ ] **Step 3: End-to-end test**

Create `crates/stratify-cli/tests/e2e_py.rs`:

```rust
use std::path::Path;

#[test]
fn sample_py_reports_dead_code() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sample-py");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_stratify"))
        .arg("check")
        .arg(&dir)
        .arg("--format")
        .arg("json")
        .output()
        .expect("run stratify binary");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("\"rule\": \"dead_code\""), "stdout: {stdout}");
    assert!(stdout.contains("never_called"), "stdout: {stdout}");
}
```

- [ ] **Step 4: Run + manual smoke**

Run: `cargo test -p stratify-cli`
Expected: PASS including `e2e_py`.

Manual — prove four languages flow through one run:
```bash
cargo build
./target/debug/stratify check crates/stratify-cli/tests --format json | grep -oE '"file": "sample-(java|ruby|ts|py)/[^"]+"' | sed -E 's#"file": "(sample-[a-z]+)/.*#\1#' | sort | uniq -c
```
Expected: sample-java, sample-py, sample-ruby, sample-ts all present.

- [ ] **Step 5: Commit**

```bash
git add crates/stratify-cli
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(cli): register Python adapter, end-to-end Python dead-code"
```

---

## Task 5: fmt, clippy, lockfile

- [ ] **Step 1: Format and lint**

Run: `cargo fmt --all && cargo clippy --all-targets -- -D warnings`
Fix any warning properly (no blanket `#[allow]`). Re-run `cargo test` after any fix.

- [ ] **Step 2: Full suite**

Run: `cargo test`
Expected: all crates green, including the new `stratify-lang-py` tests and `e2e_py`.

- [ ] **Step 3: Commit**

```bash
git add -A
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "chore: fmt, clippy clean, update lockfile for python"
```

---

## Self-Review Notes

Spec coverage for M12:
- New `stratify-lang-py` crate emitting the full IR (symbols, Defines, tokens, file entrypoint, calls, complexity, imports): Tasks 1-3. Covered.
- All six analyses run on Python (they read only the IR). Verified end to end by Task 4.
- CLI registration + e2e: Task 4. Covered.

Deferred (correctly out of M12): package `__init__.py` resolution (`from pkg import x` where pkg is a package dir -> `pkg/__init__.py`, whose fqn is `pkg/__init__` not `pkg` — a known mismatch, noted), `__all__`/public-name entrypoints (we use file-scope only, like Ruby), aliased imports (`import a.b as c`), star imports, conditional/dynamic imports, decorators, and async function bodies' extra control flow nuances. Shares the cross-cutting limitation: intra-file calls are `Likely`, so a function reached only inside its file shows as "possibly unused".

Known M12 characteristics (acceptable, consistent with the other adapters):
- `function_definition` captures both top-level functions and methods (Python nests methods as function_definition); that is the intended single-rule behavior.
- Entrypoint is the File only; a module-level function not called at top level is flagged, matching Ruby.
- Import edges resolve dotted absolute modules (`a.b` -> `a/b`) and relative imports (`.`/`..` arithmetic) to path keys that match File fqns; package-`__init__` imports are the known gap.
- Complexity counts `boolean_operator` once per `and`/`or`, plus the standard branch/loop/except/case/ternary nodes.

Type consistency: `PyAdapter`, `extract::extract`, `strip_py_ext`, `dotted_to_path`, `resolve_relative_py`, `enclosing_method_id`, `count_decisions_py`/`cyclomatic_py`, `normalize_py`, `collect_leaves`, and the IR APIs are used consistently with their M1-M11 definitions.

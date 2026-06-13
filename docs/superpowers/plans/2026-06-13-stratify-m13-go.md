# Stratify M13 (Go Adapter) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a Go adapter so dead-code, duplication, complexity, and hotspots run on Go, alongside Java, Ruby, TypeScript, and Python.

**Scope note (read first):** Go's `import` references PACKAGES (directories) via module-qualified paths derived from `go.mod` (e.g. `import "github.com/u/r/internal/foo"`). A per-file adapter cannot see `go.mod` to strip the module prefix, so import paths do not resolve to file-level keys the way TS/Python/Ruby specifiers do. Therefore **M13 does NOT emit import edges**: dead-code, duplication, complexity, and hotspots work fully on Go; cycles and layer-boundaries do not apply to Go in this milestone (deferred to a future go.mod-aware resolution pass). This is honest and still high-value.

**Architecture:** A new `stratify-lang-go` crate parses `.go` with tree-sitter-go and emits: File/Type/Function symbols + `Defines` edges, entrypoints (Go's idiomatic roots: `main`, `init`, and exported = capitalized top-level functions/methods), intra-file `Calls` edges, normalized tokens, and per-function cyclomatic complexity. No imports. No analysis or core change.

**Tech Stack:** Rust, tree-sitter 0.24 + tree-sitter-go 0.23, plus the existing workspace crates.

**Prerequisite reading:** `crates/stratify-lang-ts/src/extract.rs` (closest reference: symbols + Defines, tokens, calls + `enclosing_method_id`, complexity, the streaming-iterator query loop — but SKIP its import logic; Go has none here) and `crates/stratify-lang-java/src/extract.rs` (`collect_leaves`).

**Key Go-specific decisions:**
- **No top-level executable code:** unlike Ruby/Python/TS, Go has no module-scope statements that call functions. So the File is NOT an entrypoint. Roots are `func main`, `func init`, and exported (capitalized-name) functions/methods, reachable cross-package.
- **Exported = capitalized:** a top-level identifier whose first character is uppercase is exported (Go's visibility rule). Such functions/methods are entrypoints (otherwise every public API function would look dead).
- **Functions come in two shapes:** `function_declaration` (name `identifier`) and `method_declaration` (name `field_identifier`, has a receiver). Both -> `SymbolKind::Function`.
- **Types:** `type_declaration` -> `type_spec` (name `type_identifier`) -> `SymbolKind::Class` (structs/interfaces/aliases), for structural completeness.

---

## File Structure

```
Cargo.toml                              MODIFY: workspace member + tree-sitter-go dep
crates/stratify-lang-go/Cargo.toml      CREATE
crates/stratify-lang-go/src/lib.rs      CREATE: GoAdapter
crates/stratify-lang-go/src/extract.rs  CREATE: parse -> IR (no imports)
crates/stratify-cli/Cargo.toml          MODIFY: depend on stratify-lang-go
crates/stratify-cli/src/run.rs          MODIFY: register GoAdapter
crates/stratify-cli/tests/sample-go/    CREATE: fixture .go files
crates/stratify-cli/tests/e2e_go.rs     CREATE: end-to-end on Go
```

---

## Task 1: Scaffold + GoAdapter + structure & tokens (`stratify-lang-go`)

**Files:**
- Modify: `Cargo.toml` (workspace)
- Create: `crates/stratify-lang-go/Cargo.toml`, `src/lib.rs`, `src/extract.rs`

- [ ] **Step 1: Register crate + dependency**

Root `Cargo.toml`: add `"crates/stratify-lang-go"` to `members`; add `tree-sitter-go = "0.23"` to `[workspace.dependencies]`.

- [ ] **Step 2: Crate manifest**

Create `crates/stratify-lang-go/Cargo.toml`:

```toml
[package]
name = "stratify-lang-go"
edition.workspace = true
version.workspace = true
license.workspace = true

[dependencies]
stratify-core = { path = "../stratify-core" }
stratify-lang = { path = "../stratify-lang" }
tree-sitter = { workspace = true }
tree-sitter-go = { workspace = true }
streaming-iterator = "0.1"
```

- [ ] **Step 3: Adapter**

Create `crates/stratify-lang-go/src/lib.rs`:

```rust
mod extract;

use stratify_core::IrGraph;
use stratify_lang::{AdapterError, LanguageAdapter};

pub struct GoAdapter;

impl LanguageAdapter for GoAdapter {
    fn language(&self) -> &'static str {
        "go"
    }

    fn handles_extension(&self, ext: &str) -> bool {
        ext == "go"
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
        let a = GoAdapter;
        assert!(a.handles_extension("go"));
        let g = a.parse_file("a.go", "package main\nfunc hi() {}\n").unwrap();
        assert!(g.symbols().iter().any(|s| s.name == "hi"));
    }
}
```

- [ ] **Step 4: Extractor — structure + tokens**

Create `crates/stratify-lang-go/src/extract.rs`, mirroring the TS extractor's scaffolding (parser/span/text/`collect_leaves`/`emit_tokens`, the streaming-iterator query loop). Go specifics:

- Language: `tree_sitter_go::LANGUAGE` (single grammar). `let lang: tree_sitter::Language = tree_sitter_go::LANGUAGE.into();`
- File symbol: `SymbolKind::File`, `name` = path, `fqn` = the path (extension stripping is irrelevant here since there are no import edges; just use the path as fqn).
- Definition query:

```rust
    let query = Query::new(&lang, r#"
        (function_declaration name: (identifier) @func.name) @func.node
        (method_declaration name: (field_identifier) @method.name) @method.node
        (type_spec name: (type_identifier) @type.name) @type.node
        "#).expect("go query");
```

  - function/method -> `SymbolKind::Function` (fqn = name); type_spec -> `SymbolKind::Class` (fqn = name). `Defines` edge from file, `Confidence::Certain`.
- Tokens: `collect_leaves` (copy) + `emit_tokens` + `normalize_go`:

```rust
fn normalize_go(kind: &str, text: &str) -> String {
    match kind {
        "identifier" | "field_identifier" | "type_identifier" | "package_identifier" => "ID".to_string(),
        "int_literal" | "float_literal" | "imaginary_literal" => "NUM".to_string(),
        "interpreted_string_literal" | "raw_string_literal" | "rune_literal" => "STR".to_string(),
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
    fn extracts_func_method_type() {
        let src = "package main\n\ntype Foo struct{}\n\nfunc (f Foo) Bar() {}\n\nfunc baz() {}\n";
        let g = extract("foo.go", src);
        let names: Vec<_> = g.symbols().iter().map(|s| (s.kind, s.name.as_str())).collect();
        assert!(names.contains(&(SymbolKind::File, "foo.go")));
        assert!(names.contains(&(SymbolKind::Class, "Foo")));
        assert!(names.contains(&(SymbolKind::Function, "Bar")));
        assert!(names.contains(&(SymbolKind::Function, "baz")));
    }

    #[test]
    fn emits_normalized_tokens() {
        let g = extract("a.go", "package main\nvar x = 5\n");
        let norms: Vec<&str> = g.tokens().iter().map(|t| t.norm.as_str()).collect();
        assert!(norms.contains(&"ID"));   // x / package name
        assert!(norms.contains(&"NUM"));  // 5
        assert!(norms.contains(&"package"));
    }
}
```

- [ ] **Step 5: Run, verify pass**

Run: `cargo test -p stratify-lang-go` (if `cargo` missing: `source "$HOME/.cargo/env"`)
Expected: PASS. Discover real node kinds with a temporary `to_sexp()` print if a query matches nothing; fix, remove the print, report it.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml crates/stratify-lang-go
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(go): scaffold adapter, extract symbols and normalized tokens"
```

---

## Task 2: Entrypoints + calls + complexity (`stratify-lang-go`)

**Files:**
- Modify: `crates/stratify-lang-go/src/extract.rs`

- [ ] **Step 1: Add failing tests**

```rust
    #[test]
    fn main_init_and_exported_are_entrypoints() {
        let src = "package main\nfunc main() {}\nfunc init() {}\nfunc Exported() {}\nfunc helper() {}\n";
        let g = extract("m.go", src);
        let id = |name: &str| g.symbols().iter().find(|s| s.name == name).unwrap().id;
        let eps = g.entrypoints();
        assert!(eps.contains(&id("main")));
        assert!(eps.contains(&id("init")));
        assert!(eps.contains(&id("Exported")));
        assert!(!eps.contains(&id("helper")), "unexported helper is not an entrypoint");
    }

    #[test]
    fn intra_file_call_edge() {
        let src = "package main\nfunc a() { b() }\nfunc b() {}\n";
        let g = extract("x.go", src);
        let a = g.symbols().iter().find(|s| s.name == "a").unwrap().id;
        let b = g.symbols().iter().find(|s| s.name == "b").unwrap().id;
        assert!(g.references().iter().any(|r|
            matches!(r.kind, RefKind::Calls) && r.from == a && r.to == b));
    }

    #[test]
    fn computes_complexity() {
        // base 1 + if + && + for = 4
        let src = "package main\nfunc m(x int) {\n  if x > 0 && x < 9 {\n  }\n  for {\n  }\n}\n";
        let g = extract("c.go", src);
        let m = g.symbols().iter().find(|s| s.name == "m").unwrap().id;
        assert_eq!(g.complexity_of(m), Some(4));
    }
```

- [ ] **Step 2: Implement**

In `extract`, after the definition pass:

1. **Entrypoints:** for each Function symbol, mark it as an entrypoint if its name is `main`, `init`, or exported (first character uppercase). Add:

```rust
fn is_exported_go(name: &str) -> bool {
    name.chars().next().map(|c| c.is_uppercase()).unwrap_or(false)
}
```

   When adding a Function symbol with `id` and `name`: `if name == "main" || name == "init" || is_exported_go(&name) { g.mark_entrypoint(id); }`. Do NOT mark the File or types.
2. **Complexity:** `count_decisions_go`/`cyclomatic_go` and `g.set_complexity(id, cyclomatic_go(decl_node))` for each Function:

```rust
fn count_decisions_go(node: Node) -> u32 {
    let mut count = 0u32;
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "if_statement" | "for_statement" | "expression_case" | "type_case"
            | "communication_case" | "&&" | "||" => {
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

fn cyclomatic_go(node: Node) -> u32 {
    1 + count_decisions_go(node)
}
```

3. **Calls:** copy `enclosing_method_id` from the TS adapter. Query:

```rust
    let call_q = Query::new(&lang, r#"
        (call_expression function: (identifier) @callee) @call
        (call_expression function: (selector_expression field: (field_identifier) @callee)) @call
        "#).expect("go call query");
```

   For each callee matching an in-file Function name, add a `Calls` edge (`Confidence::Likely`) from the enclosing function (via `enclosing_method_id`) or `file_id` if not inside one (rare in Go: package-level var initializers). Deduplicate identical `(from,to,Calls)` edges.

- [ ] **Step 3: Run, verify pass**

Run: `cargo test -p stratify-lang-go`
Expected: PASS. If `computes_complexity` is off, inspect with `to_sexp()` — Go `&&`/`||` are anonymous operator tokens inside `binary_expression`; confirm they appear as `&&`/`||` node kinds. Fix, remove the print, report. The expected count 4 is correct.

- [ ] **Step 4: Commit**

```bash
git add crates/stratify-lang-go
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(go): entrypoints (main/init/exported), call edges, complexity"
```

---

## Task 3: Wire into the CLI + end-to-end (`stratify-cli`)

**Files:**
- Modify: `crates/stratify-cli/Cargo.toml`, `crates/stratify-cli/src/run.rs`
- Create: `crates/stratify-cli/tests/sample-go/app.go`, `crates/stratify-cli/tests/sample-go/lib.go`
- Create: `crates/stratify-cli/tests/e2e_go.rs`

- [ ] **Step 1: Register the adapter**

In `crates/stratify-cli/Cargo.toml` `[dependencies]`, add `stratify-lang-go = { path = "../stratify-lang-go" }`. In `run.rs` `analyze_repo`, add `Box::new(stratify_lang_go::GoAdapter)` to the adapters vector.

- [ ] **Step 2: Fixtures**

`crates/stratify-cli/tests/sample-go/app.go`:

```go
package main

func main() {
	helper()
}

func helper() string {
	return "used"
}
```

`crates/stratify-cli/tests/sample-go/lib.go`:

```go
package main

func neverCalled() string {
	return "dead"
}

func Exported() string {
	return "public api"
}
```

(`neverCalled` is unexported and uncalled -> dead. `helper` is called from main via a Likely edge -> possibly unused / info. `Exported` is capitalized -> an entrypoint, NOT flagged. `main` is an entrypoint. Both files share `package main` in one directory, which is valid Go.)

- [ ] **Step 3: End-to-end test**

Create `crates/stratify-cli/tests/e2e_go.rs`:

```rust
use std::path::Path;

#[test]
fn sample_go_reports_dead_code() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sample-go");
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
    // Exported is an entrypoint, so it must NOT appear in any finding.
    assert!(!stdout.contains("Exported"), "Exported should not be flagged: {stdout}");
}
```

- [ ] **Step 4: Run + manual smoke**

Run: `cargo test -p stratify-cli`
Expected: PASS including `e2e_go`.

Manual — prove five languages flow through one run:
```bash
cargo build
./target/debug/stratify check crates/stratify-cli/tests --format json | grep -oE '"file": "sample-(java|ruby|ts|py|go)/[^"]+"' | sed -E 's#"file": "(sample-[a-z]+)/.*#\1#' | sort | uniq -c
```
Expected: sample-go, sample-java, sample-py, sample-ruby, sample-ts all present.

- [ ] **Step 5: Commit**

```bash
git add crates/stratify-cli
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(cli): register Go adapter, end-to-end Go dead-code"
```

---

## Task 4: fmt, clippy, lockfile

- [ ] **Step 1: Format and lint**

Run: `cargo fmt --all && cargo clippy --all-targets -- -D warnings`
Fix any warning properly (no blanket `#[allow]`). Re-run `cargo test` after any fix.

- [ ] **Step 2: Full suite**

Run: `cargo test`
Expected: all crates green, including `stratify-lang-go` and `e2e_go`.

- [ ] **Step 3: Commit**

```bash
git add -A
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "chore: fmt, clippy clean, update lockfile for go"
```

---

## Self-Review Notes

Spec coverage for M13:
- New `stratify-lang-go` crate emitting symbols, Defines, tokens, entrypoints, calls, complexity: Tasks 1-2. Covered.
- Four analyses (dead-code, duplication, complexity, hotspots) run on Go. Verified end to end by Task 4. Cycles + boundaries deliberately not applicable to Go (no import edges) — documented in the scope note.
- CLI registration + e2e: Task 3. Covered.

Deferred (correctly out of M13): import edges (and therefore Go cycles/boundaries) — require reading `go.mod` to strip the module prefix and resolving package directories, which needs a module-aware pass the per-file adapter cannot do; this is the headline future refinement for Go. Also deferred: package-level var-initializer call attribution, generics-specific control flow, build tags, and Unicode (non-ASCII) export detection (we use ASCII `is_uppercase`).

Known M13 characteristics (acceptable):
- Go roots are `main`/`init`/exported (capitalized) functions, the idiomatic reachability set; unexported, never-called functions are flagged. This is a more precise dead-code model than the file-scope-entrypoint languages, fitting Go's lack of top-level execution.
- `function_declaration` and `method_declaration` both become Function symbols; types become Class symbols (structural only — dead-code flags only Functions).
- Complexity counts switch/select cases (`expression_case`/`type_case`/`communication_case`), branches, loops, and `&&`/`||`.

Type consistency: `GoAdapter`, `extract::extract`, `is_exported_go`, `enclosing_method_id`, `count_decisions_go`/`cyclomatic_go`, `normalize_go`, `collect_leaves`, and the IR APIs are used consistently with their M1-M12 definitions.

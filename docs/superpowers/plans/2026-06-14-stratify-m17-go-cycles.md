# Stratify M17 (Go Cycles & Boundaries) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make circular-dependency and layer-boundary analysis work on Go, completing the 6-analysis x 5-language matrix. Today Go has no import edges (M13 deferred them), so cycles/boundaries don't fire on Go.

**Why Go is different:** a Go `import "github.com/u/r/internal/svc"` references a PACKAGE (the directory `internal/svc/`), module-qualified by the `go.mod` module path. Multiple `.go` files share one package. So two things must change versus the other languages:
1. **Package-keyed import targets.** The import path is module-prefixed and points at a directory, not a file. We resolve it to an in-repo package directory by longest-suffix-matching against the set of known Go package dirs (so no `go.mod` parsing is needed): `github.com/u/r/internal/svc` ends with `/internal/svc`, which is a known package dir.
2. **Package-collapsed cycle graph.** Cycles are between PACKAGES, but the cycle analysis is keyed per file. So the cycle graph is re-keyed by export-key (fqn): a Go file's fqn is its package dir, so a package's files collapse to one node. For every other language fqn is 1:1 with the file, so this is a no-op.

**Design:**
- Go adapter: File fqn = the file's package directory (parent dir of the path); emit an import Dependency (named with the raw, unquoted import path) + an `Imports` edge per import.
- Core: add `IrGraph::set_symbol_name` so a post-merge pass can rewrite a Dependency's name.
- `stratify-analysis::resolve::go_imports(&mut graph)`: for each `Imports` edge whose source file ends in `.go`, rewrite the target Dependency's name from the raw import path to the longest in-repo Go package dir that is a suffix of it (leave external imports unchanged — they match nothing). Runs in the CLI after merge.
- `stratify-analysis::imports`: add `fqn_import_graph` (adjacency keyed by fqn, collapsing shared-fqn files) and `fqn_spans` (fqn -> a representative File span, for findings). `cycles.rs` switches from `file_import_graph`/`file_spans` to these. Boundaries keep `file_import_graph` (file granularity) unchanged — once Go imports resolve, boundary edges form via the existing file->rep-file mechanism.

**Regression safety:** the cycle re-key from file to fqn is the risky change. For Java/Ruby/TS/Python a file's fqn is unique (1:1 with the file), so the graph is structurally identical; only Go collapses. Existing cycle tests + the Ruby `e2e_cycle` and Python `e2e_pypkg` are the guards — they MUST pass unchanged. Findings still display real file paths (fqn -> representative file).

**Prerequisite reading:** `crates/stratify-lang-go/src/extract.rs` (add import extraction; it currently has none), the other adapters' import passes (Ruby/TS/Python) for the Dependency+Imports-edge pattern, `crates/stratify-analysis/src/imports.rs` (`file_import_graph`/`file_spans`), `crates/stratify-analysis/src/cycles.rs`, `crates/stratify-analysis/src/resolve.rs` (`cross_file_calls` is the model for `go_imports`), `crates/stratify-cli/src/run.rs` (where `cross_file_calls` is called after merge).

---

## File Structure

```
crates/stratify-lang-go/src/extract.rs        MODIFY: File fqn = package dir; emit import edges
crates/stratify-core/src/graph.rs             MODIFY: add set_symbol_name
crates/stratify-analysis/src/resolve.rs        MODIFY: add go_imports pass
crates/stratify-analysis/src/imports.rs        MODIFY: add fqn_import_graph + fqn_spans
crates/stratify-analysis/src/cycles.rs         MODIFY: key cycle graph by fqn
crates/stratify-cli/src/run.rs                 MODIFY: run go_imports after merge
crates/stratify-cli/tests/sample-gocycle/      CREATE: two Go packages importing each other
crates/stratify-cli/tests/sample-goboundary/   CREATE: Go layer-boundary fixture + stratify.toml
crates/stratify-cli/tests/e2e_gocycle.rs       CREATE: Go package cycle e2e
crates/stratify-cli/tests/e2e_goboundary.rs    CREATE: Go boundary e2e
```

---

## Task 1: Go adapter — package-dir fqn + import edges (`stratify-lang-go`)

**Files:**
- Modify: `crates/stratify-lang-go/src/extract.rs`

- [ ] **Step 1: Add failing tests**

```rust
    #[test]
    fn file_fqn_is_package_dir() {
        let g = extract("internal/svc/a.go", "package svc\n");
        let f = g.symbols().iter().find(|s| s.kind == SymbolKind::File).unwrap();
        assert_eq!(f.fqn, "internal/svc");
    }

    #[test]
    fn top_level_file_fqn_is_empty() {
        let g = extract("main.go", "package main\n");
        let f = g.symbols().iter().find(|s| s.kind == SymbolKind::File).unwrap();
        assert_eq!(f.fqn, "");
    }

    #[test]
    fn emits_import_dependency_with_raw_path() {
        let g = extract("a/a.go", "package a\nimport \"example.com/m/b\"\n");
        let dep = g.symbols().iter().find(|s| s.kind == SymbolKind::Dependency && s.name == "example.com/m/b");
        assert!(dep.is_some(), "expected import Dependency for example.com/m/b");
        let file_id = g.symbols().iter().find(|s| s.kind == SymbolKind::File).unwrap().id;
        assert!(g.references().iter().any(|r|
            matches!(r.kind, RefKind::Imports) && r.from == file_id && r.to == dep.unwrap().id));
    }

    #[test]
    fn emits_grouped_imports() {
        let g = extract("a/a.go", "package a\nimport (\n  \"x/y\"\n  \"p/q\"\n)\n");
        let names: Vec<&str> = g.symbols().iter()
            .filter(|s| s.kind == SymbolKind::Dependency).map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"x/y"));
        assert!(names.contains(&"p/q"));
    }
```

- [ ] **Step 2: File fqn = package dir**

Where the File symbol is created, set `fqn` to the parent directory of `path` (the package dir): use `std::path::Path::new(path).parent()` mapped to a `/`-joined string, or `""` if none. Add a helper:

```rust
fn package_dir(path: &str) -> String {
    std::path::Path::new(path)
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default()
}
```

(The File `name` stays the full path; only `fqn` becomes the package dir.)

- [ ] **Step 3: Emit import edges**

Add an import pass (after the definition pass). Discover the exact node shape with a temporary `to_sexp()` on `import "x"` and `import ( "a"\n "b" )`; expected: a query `(import_spec path: (interpreted_string_literal) @path)` captures both single and grouped imports. The captured node text includes surrounding quotes; strip a leading and trailing `"` (and handle backtick raw-string imports defensively). For each import path string, add a `Dependency` symbol named the unquoted path (fqn = same) and an `Imports` edge from the file (`Confidence::Certain`). Mirror the Dependency+Imports emission in the Ruby/TS adapters.

- [ ] **Step 4: Run, verify pass**

Run: `cargo test -p stratify-lang-go` (if `cargo` missing: `source "$HOME/.cargo/env"`)
Expected: PASS (4 new tests + prior). Adjust the import query via `to_sexp()` if needed; remove any debug print; report it.

- [ ] **Step 5: Commit**

```bash
git add crates/stratify-lang-go
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(go): package-dir fqn and import edges (raw paths)"
```

---

## Task 2: Core rename + go_imports resolution + fqn graph helpers

**Files:**
- Modify: `crates/stratify-core/src/graph.rs`
- Modify: `crates/stratify-analysis/src/imports.rs`
- Modify: `crates/stratify-analysis/src/resolve.rs`

- [ ] **Step 1: `IrGraph::set_symbol_name` (core)**

In `graph.rs`, add a method to rename a symbol by id (used to rewrite resolved Go import keys):

```rust
    /// Rename a symbol (used by post-merge resolution to rewrite an import key).
    pub fn set_symbol_name(&mut self, id: SymbolId, name: String) {
        if let Some(s) = self.symbols.get_mut(id.0 as usize) {
            s.name = name;
        }
    }
```

Add a test:

```rust
    #[test]
    fn set_symbol_name_renames() {
        let mut g = IrGraph::new();
        let a = g.add_symbol(sym("a"));
        g.set_symbol_name(a, "renamed".into());
        assert_eq!(g.symbol(a).unwrap().name, "renamed");
    }
```

- [ ] **Step 2: `fqn_import_graph` + `fqn_spans` (imports.rs)**

Add fqn-keyed variants alongside the existing file-keyed ones. `fqn_import_graph` builds adjacency keyed by export-key (a File's fqn), collapsing files that share an fqn (Go packages):

```rust
/// Like `file_import_graph` but keyed by export-key (fqn) instead of file path.
/// Files sharing an fqn (e.g. Go package files) collapse into one node. For
/// languages where fqn is 1:1 with the file, this matches `file_import_graph`.
pub fn fqn_import_graph(graph: &IrGraph) -> BTreeMap<String, BTreeSet<String>> {
    // export key -> file (first wins), to validate import targets.
    let mut export: HashMap<&str, ()> = HashMap::new();
    for s in graph.symbols() {
        if matches!(s.kind, SymbolKind::File | SymbolKind::Class | SymbolKind::Module) {
            export.entry(s.fqn.as_str()).or_insert(());
        }
    }
    // file id -> owning file's fqn (the source node key).
    let file_fqn: HashMap<SymbolId, String> = graph
        .symbols()
        .iter()
        .filter(|s| matches!(s.kind, SymbolKind::File))
        .map(|s| (s.id, s.fqn.clone()))
        .collect();

    let mut adj: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for s in graph.symbols() {
        if matches!(s.kind, SymbolKind::File) {
            adj.entry(s.fqn.clone()).or_default();
        }
    }
    for r in graph.references() {
        if !matches!(r.kind, RefKind::Imports) {
            continue;
        }
        let (Some(from), Some(to)) = (graph.symbol(r.from), graph.symbol(r.to)) else {
            continue;
        };
        let Some(src_fqn) = file_fqn.get(&from.id).or_else(|| {
            // the Imports edge `from` is a File symbol; if not, skip
            None
        }) else {
            continue;
        };
        let tgt = to.name.as_str();
        if export.contains_key(tgt) && tgt != src_fqn.as_str() {
            adj.entry(src_fqn.clone()).or_default().insert(tgt.to_string());
        }
    }
    adj
}

/// fqn -> a representative File span (first File with that fqn), for findings.
pub fn fqn_spans(graph: &IrGraph) -> HashMap<String, Span> {
    let mut spans = HashMap::new();
    for s in graph.symbols() {
        if matches!(s.kind, SymbolKind::File) {
            spans.entry(s.fqn.clone()).or_insert_with(|| s.span.clone());
        }
    }
    spans
}
```

- [ ] **Step 3: `go_imports` resolution (resolve.rs)**

Add a pass that rewrites Go import Dependency names from raw module-qualified paths to in-repo package-dir keys by longest-suffix match:

```rust
use stratify_core::SymbolKind; // ensure imported

/// Resolve Go imports: rewrite each Dependency reached by an `Imports` edge
/// from a `.go` file so its name becomes the longest in-repo Go package dir
/// that is a suffix of the raw import path. External imports (no matching
/// package dir) are left unchanged (they then match no fqn and form no edge).
pub fn go_imports(graph: &mut IrGraph) {
    // Known Go package dirs = fqns of File symbols whose file ends in ".go".
    let mut pkgs: Vec<String> = graph
        .symbols()
        .iter()
        .filter(|s| matches!(s.kind, SymbolKind::File) && s.span.file.ends_with(".go"))
        .map(|s| s.fqn.clone())
        .collect();
    pkgs.sort();
    pkgs.dedup();
    if pkgs.is_empty() {
        return;
    }

    // Collect (dependency id, new name) for Go import edges.
    let mut renames: Vec<(stratify_core::ir::SymbolId, String)> = Vec::new();
    for r in graph.references() {
        if !matches!(r.kind, RefKind::Imports) {
            continue;
        }
        let (Some(from), Some(to)) = (graph.symbol(r.from), graph.symbol(r.to)) else {
            continue;
        };
        if !from.span.file.ends_with(".go") {
            continue;
        }
        let path = to.name.as_str();
        // longest package dir that equals the path or is a trailing path segment of it
        let best = pkgs
            .iter()
            .filter(|d| path == d.as_str() || path.ends_with(&format!("/{d}")))
            .max_by_key(|d| d.len());
        if let Some(dir) = best {
            if dir.as_str() != path {
                renames.push((to.id, dir.clone()));
            }
        }
    }
    for (id, name) in renames {
        graph.set_symbol_name(id, name);
    }
}
```

Add tests to resolve.rs:

```rust
    #[test]
    fn go_imports_resolves_by_suffix() {
        use stratify_core::ir::{Reference, Span, Symbol, Visibility};
        let mut g = IrGraph::new();
        // package "b" exists (b/b.go), file in package "a" imports example.com/m/b
        let a_file = g.add_symbol(Symbol {
            id: SymbolId(0), kind: SymbolKind::File, name: "a/a.go".into(), fqn: "a".into(),
            span: Span { file: "a/a.go".into(), start_byte: 0, end_byte: 1, start_line: 1 },
            visibility: Visibility::Unknown, confidence: Confidence::Certain });
        g.add_symbol(Symbol {
            id: SymbolId(0), kind: SymbolKind::File, name: "b/b.go".into(), fqn: "b".into(),
            span: Span { file: "b/b.go".into(), start_byte: 0, end_byte: 1, start_line: 1 },
            visibility: Visibility::Unknown, confidence: Confidence::Certain });
        let dep = g.add_symbol(Symbol {
            id: SymbolId(0), kind: SymbolKind::Dependency, name: "example.com/m/b".into(), fqn: "example.com/m/b".into(),
            span: Span { file: "x".into(), start_byte: 0, end_byte: 1, start_line: 1 },
            visibility: Visibility::Unknown, confidence: Confidence::Certain });
        g.add_reference(Reference { from: a_file, to: dep, kind: RefKind::Imports,
            span: Span { file: "a/a.go".into(), start_byte: 0, end_byte: 1, start_line: 1 }, confidence: Confidence::Certain });
        go_imports(&mut g);
        assert_eq!(g.symbol(dep).unwrap().name, "b");
    }

    #[test]
    fn go_imports_leaves_external_unchanged() {
        use stratify_core::ir::{Reference, Span, Symbol, Visibility};
        let mut g = IrGraph::new();
        let a_file = g.add_symbol(Symbol {
            id: SymbolId(0), kind: SymbolKind::File, name: "a/a.go".into(), fqn: "a".into(),
            span: Span { file: "a/a.go".into(), start_byte: 0, end_byte: 1, start_line: 1 },
            visibility: Visibility::Unknown, confidence: Confidence::Certain });
        let dep = g.add_symbol(Symbol {
            id: SymbolId(0), kind: SymbolKind::Dependency, name: "fmt".into(), fqn: "fmt".into(),
            span: Span { file: "x".into(), start_byte: 0, end_byte: 1, start_line: 1 },
            visibility: Visibility::Unknown, confidence: Confidence::Certain });
        g.add_reference(Reference { from: a_file, to: dep, kind: RefKind::Imports,
            span: Span { file: "a/a.go".into(), start_byte: 0, end_byte: 1, start_line: 1 }, confidence: Confidence::Certain });
        go_imports(&mut g);
        assert_eq!(g.symbol(dep).unwrap().name, "fmt"); // no matching package dir -> unchanged
    }
```

- [ ] **Step 4: Run, verify pass**

Run: `cargo test -p stratify-core && cargo test -p stratify-analysis`
Expected: PASS (new core test + 2 go_imports tests + all prior, including the existing cycle/import tests which still use file-keyed helpers at this point).

- [ ] **Step 5: Commit**

```bash
git add crates/stratify-core crates/stratify-analysis
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(core,analysis): set_symbol_name, go_imports resolver, fqn import graph"
```

---

## Task 3: Cycle analysis keys by fqn (`stratify-analysis`)

**Files:**
- Modify: `crates/stratify-analysis/src/cycles.rs`

- [ ] **Step 1: Switch cycles to the fqn-keyed graph**

In `cycles.rs::analyze`, replace `crate::imports::file_import_graph(graph)` with `crate::imports::fqn_import_graph(graph)` and `crate::imports::file_spans(graph)` with `crate::imports::fqn_spans(graph)`. The DFS and `canonical_cycle` are unchanged. The finding's span comes from `fqn_spans` (fqn -> representative File span), so messages still show a real file path. The cycle message joins the cycle's nodes; since nodes are now fqns, the message shows fqns. To keep messages showing file PATHS (nicer and matching existing e2e expectations), map each fqn in the cycle to its representative file path via `fqn_spans` before building the message:

```rust
    let spans = crate::imports::fqn_spans(graph);
    // ... in the finding emission, for each fqn in `cycle`, use spans.get(fqn).map(|s| s.file.clone()).unwrap_or(fqn.clone())
    // build the message from the mapped file paths, and set span to the first node's representative span.
```

Concretely, where the finding message is built from `cycle.join(" -> ")`, instead map each node to its file path:

```rust
        let files: Vec<String> = cycle.iter()
            .map(|fqn| spans.get(fqn).map(|s| s.file.clone()).unwrap_or_else(|| fqn.clone()))
            .collect();
        let span = spans.get(&cycle[0]).cloned().unwrap_or(Span { file: files[0].clone(), start_byte: 0, end_byte: 0, start_line: 1 });
        // message: format!("circular dependency: {}", files.join(" -> "))
```

- [ ] **Step 2: Run the existing cycle tests + Ruby/Python e2es (REGRESSION GUARD)**

Run: `cargo test -p stratify-analysis cycles` then `cargo test -p stratify-cli --test e2e_cycle --test e2e_pypkg`
Expected: PASS unchanged. The hand-built cycle tests set File `fqn` == the file name, so fqn-keying is structurally identical; the Ruby cycle (`one.rb`/`two.rb`) and Python package cycle (`pkg_a`/`pkg_b`) still report with file paths. If any cycle test's message assertion breaks because it now shows an fqn instead of a path, the fqn->file mapping in Step 1 is the fix — verify messages show file paths.

- [ ] **Step 3: Commit**

```bash
git add crates/stratify-analysis
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "refactor(analysis): cycle graph keyed by fqn (collapses Go packages)"
```

---

## Task 4: Wire go_imports into the CLI + Go end-to-end

**Files:**
- Modify: `crates/stratify-cli/src/run.rs`
- Create: `crates/stratify-cli/tests/sample-gocycle/{go.mod,a/a.go,b/b.go}`
- Create: `crates/stratify-cli/tests/sample-goboundary/{stratify.toml,db/store.go,handlers/api.go}`
- Create: `crates/stratify-cli/tests/e2e_gocycle.rs`, `crates/stratify-cli/tests/e2e_goboundary.rs`

- [ ] **Step 1: Run go_imports after merge**

In `crates/stratify-cli/src/run.rs::analyze_repo`, after the per-file graphs merge and alongside `cross_file_calls`, add (before the analyses run):

```rust
    stratify_analysis::resolve::go_imports(&mut graph);
```

(Order relative to `cross_file_calls` does not matter; both mutate the graph independently. Place it right after the existing `cross_file_calls(&mut graph)` call.)

- [ ] **Step 2: Go cycle fixture**

`crates/stratify-cli/tests/sample-gocycle/go.mod`:

```
module example.com/m

go 1.22
```

`crates/stratify-cli/tests/sample-gocycle/a/a.go`:

```go
package a

import "example.com/m/b"

func A() {
	b.B()
}
```

`crates/stratify-cli/tests/sample-gocycle/b/b.go`:

```go
package b

import "example.com/m/a"

func B() {
	a.A()
}
```

(Package `a` imports `b` and vice versa. `go_imports` rewrites `example.com/m/b` -> `b` and `example.com/m/a` -> `a` (suffix match against package dirs `a`, `b`), and the fqn-keyed cycle graph detects the package cycle.)

- [ ] **Step 3: Go boundary fixture**

`crates/stratify-cli/tests/sample-goboundary/stratify.toml`:

```toml
[layers]
handlers = ["handlers/**"]
db = ["db/**"]

[[forbid]]
from = "db"
to = "handlers"
```

`crates/stratify-cli/tests/sample-goboundary/db/store.go`:

```go
package db

import "example.com/app/handlers"

func Save() {
	handlers.Render()
}
```

`crates/stratify-cli/tests/sample-goboundary/handlers/api.go`:

```go
package handlers

func Render() {}
```

(`db` imports `handlers`, which the `db -> handlers` forbid rule violates. Boundaries classify by file path: `db/store.go` matches `db/**`, `handlers/api.go` matches `handlers/**`.)

- [ ] **Step 4: End-to-end tests**

Create `crates/stratify-cli/tests/e2e_gocycle.rs`:

```rust
use std::path::Path;

#[test]
fn go_package_cycle_is_detected() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sample-gocycle");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_stratify"))
        .arg("check").arg(&dir).arg("--format").arg("json")
        .output().expect("run stratify");
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("\"rule\": \"cycle\""), "stdout: {stdout}");
    assert!(stdout.contains("a/a.go") && stdout.contains("b/b.go"), "stdout: {stdout}");
}
```

Create `crates/stratify-cli/tests/e2e_goboundary.rs`:

```rust
use std::path::Path;

#[test]
fn go_boundary_violation_is_detected() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sample-goboundary");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_stratify"))
        .arg("check").arg(&dir).arg("--format").arg("json")
        .output().expect("run stratify");
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("\"rule\": \"boundary\""), "stdout: {stdout}");
    assert!(stdout.contains("db") && stdout.contains("handlers"), "stdout: {stdout}");
}
```

- [ ] **Step 5: Run + manual smoke + regression**

Run: `cargo test -p stratify-cli`
Expected: PASS including `e2e_gocycle` and `e2e_goboundary`, and NO regression to any existing e2e (especially `e2e_cycle`, `e2e_pypkg`, `e2e_boundary`, `e2e_preset`).

Manual:
```bash
cargo build
./target/debug/stratify check crates/stratify-cli/tests/sample-gocycle | grep circular
./target/debug/stratify check crates/stratify-cli/tests/sample-goboundary | grep "must not import"
```
Expected: a circular dependency between `a/a.go` and `b/b.go`; a `db must not import handlers` boundary violation.

- [ ] **Step 6: Commit**

```bash
git add crates/stratify-cli
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(cli): run go_imports; Go package cycle and boundary end-to-end"
```

---

## Task 5: Docs + fmt, clippy, lockfile

**Files:**
- Modify: `README.md`, generated `Cargo.lock`, any fmt changes

- [ ] **Step 1: Update the README**

Note that Go is now fully supported (all six analyses), and that Go cycle/boundary detection resolves imports by matching package paths against the repo's package directories (no `go.mod` parsing required). Keep it tight: short active sentences, no em dashes, no semicolons. Update any "languages" table/list to reflect Go cycles+boundaries now working.

- [ ] **Step 2: Format and lint**

Run: `cargo fmt --all && cargo clippy --all-targets -- -D warnings`
Fix any warning properly (no blanket `#[allow]`). Re-run `cargo test` after any fix.

- [ ] **Step 3: Full suite**

Run: `cargo test`
Expected: all crates green.

- [ ] **Step 4: Commit**

```bash
git add -A
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "docs+chore: Go cycles/boundaries docs; fmt, clippy clean, lockfile"
```

---

## Self-Review Notes

Spec coverage for M17:
- Go adapter emits package-dir fqns + import edges: Task 1. Covered.
- `set_symbol_name` + `go_imports` suffix resolution + fqn-keyed graph helpers: Task 2. Covered.
- Cycle analysis keys by fqn (collapses Go packages), findings show file paths: Task 3. Covered.
- CLI wiring + Go cycle/boundary e2es + regression guards: Task 4. Covered.

Deferred (correctly out of M17): `go.mod`-aware exact module resolution (we use longest-suffix matching against repo package dirs, which is correct for in-repo imports and ignores external ones); vendored/replace directives; internal-package visibility rules; build tags; and reporting cycles at file granularity for Go (we report at package granularity, which is the meaningful unit for Go).

Known M17 characteristics (acceptable):
- Go cycles are package-level: the cycle graph collapses a package's files into one node (keyed by package dir), so a cycle is reported between packages (shown via a representative file from each). This is the correct granularity for Go import cycles.
- Suffix matching resolves `modpath/internal/svc` to the package dir `internal/svc` by longest match against known package dirs. Two repo packages with the same trailing path (e.g. `a/util` and `b/util`) could in principle make an import of one ambiguous; longest-suffix prefers the more specific dir, and a bare `util` import would match the first — a rare edge, acceptable, and import edges only affect cycles/boundaries (no false dead-code).
- Boundaries remain file-keyed and unchanged; Go boundary edges form because `go_imports` makes Go import Dependencies resolve to package dirs that match Go File fqns, and the existing file->representative-file mechanism produces the edge.
- The fqn re-key of the cycle graph is a no-op for Java/Ruby/TS/Python (fqn is 1:1 with the file); only Go collapses. Guarded by the existing cycle e2es.

Type consistency: `package_dir` (go adapter), `IrGraph::set_symbol_name`, `resolve::go_imports`, `imports::fqn_import_graph`/`fqn_spans`, and `cycles::analyze` are used consistently with their M1-M16 definitions.

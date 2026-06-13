# Stratify M6 (Circular Dependencies) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a cross-file import graph and report circular dependencies. Java `import` and Ruby `require_relative` become resolvable edges; a language-agnostic analysis detects cycles.

**Architecture:** Adapters emit an import as a `Dependency` symbol whose name is a normalized IMPORT KEY plus an `Imports` edge from the file. Each importable symbol (File / Class / Module) carries an EXPORT KEY in its `fqn`. The cycle analysis matches import keys to export keys (pure string matching, so it stays language-agnostic), builds a file-to-file graph, and reports cycles via DFS back-edge detection. The adapters do the language-specific key construction (Java: package-qualified names; Ruby: paths resolved relative to the importing file).

**Tech Stack:** Rust, existing workspace crates, tree-sitter (Java + Ruby).

**Prerequisite reading:** both adapters' `extract.rs` (you add package/import capture to Java and require capture to Ruby), and `crates/stratify-analysis/src/deadcode.rs` (graph-walking style to mirror).

**Key-matching invariant (the crux):** an import edge resolves iff its Dependency `name` (import key) string-equals some non-Dependency symbol's `fqn` (export key). Adapters must construct keys so a real import matches its target:
- Java: import key = the imported fully-qualified name (`com.acme.Foo`). Export key = a class's `fqn` = `package.ClassName`.
- Ruby: import key = the `require_relative` argument resolved against the importing file's directory, with `.rb` appended (`lib/b.rb`). Export key = a Ruby File symbol's `fqn` = its path (`lib/b.rb`).

---

## File Structure

```
crates/stratify-lang-java/src/extract.rs   MODIFY: capture package, set class fqn = pkg.Class, emit import Dependencies + Imports edges
crates/stratify-lang-ruby/src/extract.rs   MODIFY: set File fqn = path, emit require_relative Dependencies + Imports edges
crates/stratify-analysis/src/cycles.rs      CREATE: import-graph build + DFS cycle detection
crates/stratify-analysis/src/lib.rs         MODIFY: pub mod cycles
crates/stratify-cli/src/run.rs              MODIFY: run cycles in analyze_repo
crates/stratify-cli/tests/sample-cycle/     CREATE: two Ruby files requiring each other
crates/stratify-cli/tests/e2e_cycle.rs      CREATE: end-to-end cycle test
```

No IR type change: `RefKind::Imports`, `SymbolKind::Dependency`, and `fqn` already exist.

---

## Task 1: Java package + import edges (`stratify-lang-java`)

**Files:**
- Modify: `crates/stratify-lang-java/src/extract.rs`

- [ ] **Step 1: Add failing tests**

In `extract.rs` tests module:

```rust
    #[test]
    fn class_fqn_includes_package() {
        let src = "package com.acme;\nclass Foo {}";
        let g = extract("Foo.java", src);
        let foo = g.symbols().iter().find(|s| s.name == "Foo").unwrap();
        assert_eq!(foo.fqn, "com.acme.Foo");
    }

    #[test]
    fn emits_import_dependency_and_edge() {
        let src = "package com.acme;\nimport com.other.Bar;\nclass Foo {}";
        let g = extract("Foo.java", src);
        // a Dependency named after the imported FQN
        let dep = g.symbols().iter().find(|s| s.kind == SymbolKind::Dependency && s.name == "com.other.Bar");
        assert!(dep.is_some(), "expected import Dependency for com.other.Bar");
        let dep_id = dep.unwrap().id;
        let file_id = g.symbols().iter().find(|s| s.kind == SymbolKind::File).unwrap().id;
        assert!(g.references().iter().any(|r|
            matches!(r.kind, RefKind::Imports) && r.from == file_id && r.to == dep_id));
    }
```

- [ ] **Step 2: Run, verify fail**

Run: `cargo test -p stratify-lang-java class_fqn_includes_package` (if `cargo` missing: `source "$HOME/.cargo/env"`)
Expected: FAIL.

- [ ] **Step 3: Capture package, qualify class fqns, emit imports**

In `extract`, before the class/method query pass, extract the package name once:

```rust
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
            let t = text(cap.node, src);
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
```

Capture `let pkg = package_name(root, src);` near the top of `extract` (after the File symbol). When building a `Class` symbol, set its `fqn`:

```rust
            let fqn = if matches!(kind, SymbolKind::Class) && !pkg.is_empty() {
                format!("{pkg}.{name}")
            } else {
                name.clone()
            };
```

and use `fqn` for the symbol's `fqn` field (methods keep `fqn = name`).

Then add an import pass (after the definition pass). Emit a `Dependency` symbol per `import` and an `Imports` edge from the file:

```rust
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
                let fqn = text(cap.node, src).to_string();
                let dep = g.add_symbol(Symbol {
                    id: SymbolId(0),
                    kind: SymbolKind::Dependency,
                    name: fqn.clone(),
                    fqn,
                    span: span(file, cap.node),
                    visibility: Visibility::Unknown,
                    confidence: Confidence::Certain,
                });
                g.add_reference(Reference {
                    from: file_id,
                    to: dep,
                    kind: RefKind::Imports,
                    span: span(file, cap.node),
                    confidence: Confidence::Certain,
                });
            }
        }
    }
```

(`file_id` is the File symbol id created at the start of `extract`. Ensure `SymbolKind`, `RefKind`, `Visibility`, `Confidence`, `Symbol`, `Reference`, `SymbolId` are in scope; they are already used in the file.)

- [ ] **Step 4: Run, verify pass and existing tests still pass**

Run: `cargo test -p stratify-lang-java`
Expected: PASS. If the `scoped_identifier`/`package_declaration` node names differ, discover with a temporary `to_sexp()` print, fix, remove the print, report it.

- [ ] **Step 5: Commit**

```bash
git add crates/stratify-lang-java
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(java): package-qualified class fqns and import edges"
```

---

## Task 2: Ruby require_relative edges (`stratify-lang-ruby`)

**Files:**
- Modify: `crates/stratify-lang-ruby/src/extract.rs`

The Ruby File symbol's `fqn` must be its path so it can be an export key. Confirm/set `fqn = file.to_string()` when creating the File symbol (it likely already is). Then resolve each `require_relative "x"` against the importing file's directory.

- [ ] **Step 1: Add failing tests**

```rust
    #[test]
    fn file_fqn_is_path() {
        let g = extract("lib/a.rb", "def x\nend\n");
        let f = g.symbols().iter().find(|s| s.kind == SymbolKind::File).unwrap();
        assert_eq!(f.fqn, "lib/a.rb");
    }

    #[test]
    fn emits_require_relative_edge_with_resolved_key() {
        // from lib/a.rb, require_relative "b" -> key lib/b.rb
        let g = extract("lib/a.rb", "require_relative \"b\"\n");
        let dep = g.symbols().iter().find(|s| s.kind == SymbolKind::Dependency && s.name == "lib/b.rb");
        assert!(dep.is_some(), "expected Dependency keyed lib/b.rb");
        let file_id = g.symbols().iter().find(|s| s.kind == SymbolKind::File).unwrap().id;
        assert!(g.references().iter().any(|r|
            matches!(r.kind, RefKind::Imports) && r.from == file_id && r.to == dep.unwrap().id));
    }

    #[test]
    fn require_relative_handles_parent_dir() {
        // from lib/sub/a.rb, require_relative "../c" -> key lib/c.rb
        let g = extract("lib/sub/a.rb", "require_relative \"../c\"\n");
        assert!(g.symbols().iter().any(|s| s.kind == SymbolKind::Dependency && s.name == "lib/c.rb"));
    }
```

- [ ] **Step 2: Run, verify fail**

Run: `cargo test -p stratify-lang-ruby emits_require_relative_edge`
Expected: FAIL.

- [ ] **Step 3: Resolve require_relative and emit edges**

Add a path-resolution helper (component-based, normalizes `.` and `..`):

```rust
fn resolve_require_relative(importer_file: &str, arg: &str) -> String {
    use std::path::{Component, Path};
    let dir = Path::new(importer_file).parent().unwrap_or_else(|| Path::new(""));
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
```

Confirm the File symbol is created with `fqn: file.to_string()` (set it if it currently differs). Then add an import pass. `require_relative "x"` parses as a `(call method: (identifier) @m arguments: (argument_list (string (string_content) @arg)))` where `@m` text is `require_relative`:

```rust
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
            if cap.index == m_idx && text(cap.node, src) == "require_relative" {
                is_req = true;
            } else if cap.index == arg_idx {
                arg_node = Some(cap.node);
            }
        }
        if !is_req {
            continue;
        }
        let Some(arg_node) = arg_node else { continue };
        let key = resolve_require_relative(file, text(arg_node, src));
        let dep = g.add_symbol(Symbol {
            id: SymbolId(0),
            kind: SymbolKind::Dependency,
            name: key.clone(),
            fqn: key,
            span: span(file, arg_node),
            visibility: Visibility::Unknown,
            confidence: Confidence::Certain,
        });
        g.add_reference(Reference {
            from: file_id,
            to: dep,
            kind: RefKind::Imports,
            span: span(file, arg_node),
            confidence: Confidence::Certain,
        });
    }
```

(If the grammar nests the string differently, discover with `to_sexp()`, fix the query, remove the print, report it.)

- [ ] **Step 4: Run, verify pass**

Run: `cargo test -p stratify-lang-ruby`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/stratify-lang-ruby
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(ruby): require_relative import edges with resolved path keys"
```

---

## Task 3: Cycle detection analysis (`stratify-analysis`)

**Files:**
- Create: `crates/stratify-analysis/src/cycles.rs`
- Modify: `crates/stratify-analysis/src/lib.rs`

- [ ] **Step 1: Write the analysis with tests on hand-built IR**

Create `crates/stratify-analysis/src/cycles.rs`:

```rust
use std::collections::{BTreeMap, BTreeSet, HashMap};
use stratify_core::ir::Span;
use stratify_core::{Confidence, Finding, IrGraph, RefKind, Severity, SymbolKind};

/// Detect circular dependencies in the cross-file import graph. An `Imports`
/// edge (File -> Dependency) resolves to a file-to-file edge when the
/// Dependency's name (import key) equals some File/Class/Module symbol's fqn
/// (export key). Cycles are found by DFS back-edge detection.
pub fn analyze(graph: &IrGraph) -> Vec<Finding> {
    // export key -> file path. Built from importable symbols only.
    let mut export: HashMap<&str, String> = HashMap::new();
    for s in graph.symbols() {
        if matches!(s.kind, SymbolKind::File | SymbolKind::Class | SymbolKind::Module) {
            export.entry(s.fqn.as_str()).or_insert_with(|| s.span.file.clone());
        }
    }

    // file -> sorted set of files it imports.
    let mut adj: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut span_of: HashMap<String, Span> = HashMap::new();
    for s in graph.symbols() {
        if matches!(s.kind, SymbolKind::File) {
            adj.entry(s.span.file.clone()).or_default();
            span_of.entry(s.span.file.clone()).or_insert_with(|| s.span.clone());
        }
    }
    for r in graph.references() {
        if !matches!(r.kind, RefKind::Imports) {
            continue;
        }
        let (Some(from), Some(to)) = (graph.symbol(r.from), graph.symbol(r.to)) else {
            continue;
        };
        let src_file = &from.span.file;
        if let Some(target_file) = export.get(to.name.as_str()) {
            if target_file != src_file {
                adj.entry(src_file.clone())
                    .or_default()
                    .insert(target_file.clone());
            }
        }
    }

    // DFS back-edge detection. Colors: 0 = white, 1 = gray (on stack), 2 = black.
    let mut color: HashMap<String, u8> = HashMap::new();
    let mut path: Vec<String> = Vec::new();
    let mut reported: BTreeSet<Vec<String>> = BTreeSet::new();
    let nodes: Vec<String> = adj.keys().cloned().collect();
    for start in &nodes {
        if color.get(start).copied().unwrap_or(0) == 0 {
            dfs(start, &adj, &mut color, &mut path, &mut reported);
        }
    }

    // Emit one finding per distinct cycle (canonicalized to its lexicographically
    // smallest rotation so A->B->A and B->A->B are the same cycle).
    let mut findings = Vec::new();
    for cycle in reported {
        let file = &cycle[0];
        let span = span_of
            .get(file)
            .cloned()
            .unwrap_or(Span { file: file.clone(), start_byte: 0, end_byte: 0, start_line: 1 });
        findings.push(Finding {
            rule: "cycle".into(),
            severity: Severity::Warning,
            message: format!("circular dependency: {}", cycle.join(" -> ")),
            span,
            confidence: Confidence::Certain,
        });
    }
    findings
}

fn dfs(
    node: &str,
    adj: &BTreeMap<String, BTreeSet<String>>,
    color: &mut HashMap<String, u8>,
    path: &mut Vec<String>,
    reported: &mut BTreeSet<Vec<String>>,
) {
    color.insert(node.to_string(), 1);
    path.push(node.to_string());
    if let Some(neighbors) = adj.get(node) {
        for next in neighbors {
            match color.get(next).copied().unwrap_or(0) {
                0 => dfs(next, adj, color, path, reported),
                1 => {
                    // Back edge: cycle is path[pos..] where path[pos] == next.
                    if let Some(pos) = path.iter().position(|n| n == next) {
                        let cycle = canonical_cycle(&path[pos..]);
                        reported.insert(cycle);
                    }
                }
                _ => {}
            }
        }
    }
    path.pop();
    color.insert(node.to_string(), 2);
}

/// Rotate a cycle so it starts at its lexicographically smallest node, so the
/// same cycle discovered from different entry points dedupes.
fn canonical_cycle(nodes: &[String]) -> Vec<String> {
    let min_pos = nodes
        .iter()
        .enumerate()
        .min_by_key(|(_, n)| (*n).clone())
        .map(|(i, _)| i)
        .unwrap_or(0);
    let mut out: Vec<String> = nodes[min_pos..].to_vec();
    out.extend_from_slice(&nodes[..min_pos]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use stratify_core::ir::{Reference, Symbol, SymbolId, Visibility};

    fn file_sym(g: &mut IrGraph, path: &str) -> SymbolId {
        g.add_symbol(Symbol {
            id: SymbolId(0),
            kind: SymbolKind::File,
            name: path.into(),
            fqn: path.into(),
            span: Span { file: path.into(), start_byte: 0, end_byte: 1, start_line: 1 },
            visibility: Visibility::Unknown,
            confidence: Confidence::Certain,
        })
    }

    fn dep(g: &mut IrGraph, from: SymbolId, key: &str) {
        let d = g.add_symbol(Symbol {
            id: SymbolId(0),
            kind: SymbolKind::Dependency,
            name: key.into(),
            fqn: key.into(),
            span: Span { file: "x".into(), start_byte: 0, end_byte: 1, start_line: 1 },
            visibility: Visibility::Unknown,
            confidence: Confidence::Certain,
        });
        g.add_reference(Reference {
            from,
            to: d,
            kind: RefKind::Imports,
            span: Span { file: "x".into(), start_byte: 0, end_byte: 1, start_line: 1 },
            confidence: Confidence::Certain,
        });
    }

    #[test]
    fn detects_two_file_cycle() {
        let mut g = IrGraph::new();
        let a = file_sym(&mut g, "a.rb"); // exports key "a.rb"
        let b = file_sym(&mut g, "b.rb"); // exports key "b.rb"
        dep(&mut g, a, "b.rb"); // a imports b
        dep(&mut g, b, "a.rb"); // b imports a
        let findings = analyze(&g);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].rule, "cycle");
        assert!(findings[0].message.contains("a.rb"));
        assert!(findings[0].message.contains("b.rb"));
    }

    #[test]
    fn no_cycle_for_dag() {
        let mut g = IrGraph::new();
        let a = file_sym(&mut g, "a.rb");
        let _b = file_sym(&mut g, "b.rb");
        dep(&mut g, a, "b.rb"); // a -> b only
        assert!(analyze(&g).is_empty());
    }

    #[test]
    fn unresolved_import_is_ignored() {
        let mut g = IrGraph::new();
        let a = file_sym(&mut g, "a.rb");
        dep(&mut g, a, "nonexistent.rb"); // no matching export
        assert!(analyze(&g).is_empty());
    }

    #[test]
    fn detects_three_file_cycle_once() {
        let mut g = IrGraph::new();
        let a = file_sym(&mut g, "a.rb");
        let b = file_sym(&mut g, "b.rb");
        let c = file_sym(&mut g, "c.rb");
        dep(&mut g, a, "b.rb");
        dep(&mut g, b, "c.rb");
        dep(&mut g, c, "a.rb");
        let findings = analyze(&g);
        assert_eq!(findings.len(), 1, "one cycle, not one per entry point");
        assert!(findings[0].message.contains("a.rb -> b.rb -> c.rb"));
    }
}
```

- [ ] **Step 2: Wire lib.rs**

In `crates/stratify-analysis/src/lib.rs`, add:

```rust
pub mod cycles;
```

- [ ] **Step 3: Run, verify pass**

Run: `cargo test -p stratify-analysis`
Expected: PASS (18 tests: 4 deadcode + 4 duplication + 3 complexity + 3 hotspot + 4 cycles).

- [ ] **Step 4: Commit**

```bash
git add crates/stratify-analysis
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(analysis): circular dependency detection over the import graph"
```

---

## Task 4: Wire cycles into the CLI + end-to-end (`stratify-cli`)

**Files:**
- Modify: `crates/stratify-cli/src/run.rs`
- Create: `crates/stratify-cli/tests/sample-cycle/one.rb`
- Create: `crates/stratify-cli/tests/sample-cycle/two.rb`
- Create: `crates/stratify-cli/tests/e2e_cycle.rs`

- [ ] **Step 1: Run cycles in analyze_repo**

In `crates/stratify-cli/src/run.rs`, after the hotspot line in `analyze_repo`, add:

```rust
    findings.extend(stratify_analysis::cycles::analyze(&graph));
```

(No threshold; cycles are always reported.)

- [ ] **Step 2: Create a mutually-requiring fixture**

Create `crates/stratify-cli/tests/sample-cycle/one.rb`:

```ruby
require_relative "two"

def one_thing
  two_thing
end
```

Create `crates/stratify-cli/tests/sample-cycle/two.rb`:

```ruby
require_relative "one"

def two_thing
  one_thing
end
```

These resolve to keys `two.rb` and `one.rb` (the files are at the scan root, so the importer dir is empty and the key is just `two.rb` / `one.rb`), matching the File fqns, forming a cycle.

- [ ] **Step 3: Write the end-to-end test**

Create `crates/stratify-cli/tests/e2e_cycle.rs`:

```rust
use std::path::Path;

#[test]
fn sample_cycle_reports_circular_dependency() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sample-cycle");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_stratify"))
        .arg("check")
        .arg(&dir)
        .arg("--format")
        .arg("json")
        .output()
        .expect("run stratify binary");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("\"rule\": \"cycle\""), "stdout: {stdout}");
    assert!(stdout.contains("one.rb") && stdout.contains("two.rb"), "stdout: {stdout}");
}
```

- [ ] **Step 4: Run + manual smoke**

Run: `cargo test -p stratify-cli`
Expected: PASS including `e2e_cycle`. If the cycle is not reported, verify the require keys resolve to `one.rb`/`two.rb` (files at scan root => no directory prefix) and match the File fqns. Do NOT fake it; report a real resolution bug.

Manual:
```bash
cargo build
./target/debug/stratify check crates/stratify-cli/tests/sample-cycle
```
Expected: a `warn ... circular dependency: one.rb -> two.rb` finding.

- [ ] **Step 5: Commit**

```bash
git add crates/stratify-cli
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(cli): run cycle analysis, end-to-end circular dependency detection"
```

---

## Task 5: fmt, clippy, lockfile

- [ ] **Step 1: Format and lint**

Run: `cargo fmt --all && cargo clippy --all-targets -- -D warnings`
Fix any warning properly (no blanket `#[allow]`). Re-run `cargo test` after any fix.

- [ ] **Step 2: Full suite**

Run: `cargo test`
Expected: all crates green.

- [ ] **Step 3: Commit**

```bash
git add -A
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "chore: fmt, clippy clean, update lockfile for cycles"
```

---

## Self-Review Notes

Spec coverage for M6 (cycles slice of architecture boundaries):
- Cross-file import edges (Java import + package-qualified fqns, Ruby require_relative resolved keys): Tasks 1, 2. Covered.
- Language-agnostic import graph + cycle detection via key matching: Task 3. Covered.
- CLI integration + e2e: Task 4. Covered.

Deferred (correctly out of this slice): configurable layer-boundary rules + `stratify.toml` + Rails/Maven presets become M7 (they reuse this import graph). Java `require`-style dynamic loads, Ruby plain `require` (gem/loadpath, not file-resolvable), wildcard imports, and cross-file CALL resolution remain out of scope.

Known M6 characteristics (acceptable):
- Only resolvable imports create edges: Java imports whose FQN matches an in-repo class, and Ruby `require_relative` whose resolved path matches an in-repo file. Unresolvable imports (external libs, gems, `require`) are silently ignored, which is correct for an in-repo cycle check.
- The graph is file-level. A cycle is reported once, canonicalized to its lexicographically smallest rotation, so multiple DFS entry points and rotations dedupe. Distinct cycles sharing nodes are reported separately.
- DFS recursion depth equals the import chain length; fine for real codebases. (A pathological multi-thousand-deep chain could overflow the stack; not a concern at M6 scale.)

Type consistency: `cycles::analyze(&IrGraph) -> Vec<Finding>`, `SymbolKind::{File,Class,Module,Dependency}`, `RefKind::Imports`, `fqn` as export/import key, `Finding`/`Severity`/`Confidence`/`Span` are used consistently with M1-M5 definitions.

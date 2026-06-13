# Stratify M2 (Ruby Adapter) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a Ruby adapter that flows into the SAME dead-code analysis as Java, proving the universal-IR bet: one analysis, two languages, zero analysis changes per language.

**Architecture:** First make entrypoints a property of the IR (set by adapters) instead of a hardcoded `main` check in the analysis. Then the Java adapter marks `main` methods as entrypoints and the new `stratify-lang-ruby` adapter marks each file's top-level execution scope as an entrypoint, emitting Calls edges from that scope to top-level method calls. The dead-code analysis reads `graph.entrypoints()` and is otherwise unchanged.

**Tech Stack:** Rust, tree-sitter 0.24 + tree-sitter-ruby 0.23, plus the existing workspace crates.

**Prerequisite reading for implementers:** the existing `crates/stratify-lang-java/src/extract.rs` is the template for the Ruby adapter. Mirror its structure (parser builder, span/text helpers, query loop, `enclosing_method_id`). The M1 plan is at `docs/superpowers/plans/2026-06-12-stratify-m1-walking-skeleton.md`.

---

## File Structure

```
crates/stratify-core/src/graph.rs        MODIFY: add entrypoints set + mark_entrypoint + merge remap
crates/stratify-analysis/src/deadcode.rs MODIFY: roots from graph.entrypoints(); drop hardcoded main
crates/stratify-lang-java/src/extract.rs MODIFY: mark `main` methods as entrypoints
crates/stratify-lang-ruby/                CREATE: new crate
  Cargo.toml
  src/lib.rs                              RubyAdapter
  src/extract.rs                          tree-sitter-ruby -> IR
crates/stratify-cli/src/run.rs           MODIFY: register RubyAdapter
crates/stratify-cli/tests/sample-ruby/   CREATE: fixture .rb files
crates/stratify-cli/tests/e2e_ruby.rs    CREATE: end-to-end Ruby test
Cargo.toml                               MODIFY: add member + tree-sitter-ruby dep
```

---

## Task 1: Entrypoints become an IR property (`stratify-core`)

**Files:**
- Modify: `crates/stratify-core/src/graph.rs`

- [ ] **Step 1: Add a failing test for entrypoint storage + merge remap**

In `crates/stratify-core/src/graph.rs`, add these tests to the `#[cfg(test)] mod tests` block:

```rust
    #[test]
    fn mark_and_read_entrypoints() {
        let mut g = IrGraph::new();
        let a = g.add_symbol(sym("a"));
        g.mark_entrypoint(a);
        assert_eq!(g.entrypoints(), &[a]);
    }

    #[test]
    fn merge_remaps_entrypoints() {
        let mut g1 = IrGraph::new();
        g1.add_symbol(sym("a"));

        let mut g2 = IrGraph::new();
        let x = g2.add_symbol(sym("x"));
        g2.mark_entrypoint(x);

        g1.merge(g2);
        // x was id 0 in g2, becomes id 1 after merge (offset 1).
        assert_eq!(g1.entrypoints(), &[SymbolId(1)]);
    }
```

- [ ] **Step 2: Run, verify it fails to compile**

Run: `cargo test -p stratify-core graph`
Expected: FAIL, no method `mark_entrypoint`.

- [ ] **Step 3: Add the field and methods**

In `graph.rs`, add `entrypoints: Vec<SymbolId>` to the struct:

```rust
#[derive(Debug, Default, Clone)]
pub struct IrGraph {
    symbols: Vec<Symbol>,
    references: Vec<Reference>,
    entrypoints: Vec<SymbolId>,
}
```

Add these methods in the `impl IrGraph` block:

```rust
    /// Mark a symbol as an analysis entrypoint (a reachability root).
    /// Adapters decide what is an entrypoint (e.g. Java `main`, Ruby file scope).
    pub fn mark_entrypoint(&mut self, id: SymbolId) {
        self.entrypoints.push(id);
    }

    pub fn entrypoints(&self) -> &[SymbolId] {
        &self.entrypoints
    }
```

In `merge`, after remapping references, remap entrypoints. Add this loop at the end of `merge`:

```rust
        for e in other.entrypoints {
            self.entrypoints.push(SymbolId(e.0 + offset));
        }
```

- [ ] **Step 4: Run, verify pass**

Run: `cargo test -p stratify-core`
Expected: PASS (8 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/stratify-core
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(core): entrypoints as an IR property with merge remap"
```

---

## Task 2: Dead-code reads entrypoints from the IR (`stratify-analysis`)

**Files:**
- Modify: `crates/stratify-analysis/src/deadcode.rs`

- [ ] **Step 1: Update the analysis to use graph entrypoints**

In `deadcode.rs`, DELETE the `is_entrypoint` function entirely. Replace the root-collection block at the top of `analyze` (the loop that pushed symbols where `is_entrypoint(...)`) with:

```rust
    let roots: Vec<SymbolId> = graph.entrypoints().to_vec();
```

Everything else in `analyze` (the BFS, the reached_certain/reached_any sets, the finding emission) stays exactly the same.

- [ ] **Step 2: Update the existing analysis tests to mark entrypoints**

The existing tests built a function named `main` and relied on the hardcoded check. Update them to mark entrypoints explicitly. In each test that has a `main` root, after creating it call `g.mark_entrypoint(main)`. Concretely:

- In `unreached_function_is_dead`: after `let _main = func(&mut g, "main");` change to `let main = func(&mut g, "main"); g.mark_entrypoint(main);`. The assertion (orphan flagged Warning, 1 finding) is unchanged.
- In `reached_via_certain_edge_is_not_reported`: after `let main = func(&mut g, "main");` add `g.mark_entrypoint(main);`.
- In `reached_only_via_likely_edge_is_possibly_unused`: after `let main = func(&mut g, "main");` add `g.mark_entrypoint(main);`.
- In `file_defines_does_not_make_methods_reachable`: this test has a File symbol and an orphan, no entrypoint. Leave it WITHOUT marking any entrypoint (no roots). The orphan is still unreached, so it is still flagged Warning, 1 finding. Assertion unchanged. (This now also documents that with no entrypoints, every function is unreachable.)

- [ ] **Step 3: Run, verify pass**

Run: `cargo test -p stratify-analysis`
Expected: PASS (4 tests).

- [ ] **Step 4: Commit**

```bash
git add crates/stratify-analysis
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "refactor(analysis): take reachability roots from graph entrypoints"
```

---

## Task 3: Java adapter marks `main` as entrypoint (`stratify-lang-java`)

**Files:**
- Modify: `crates/stratify-lang-java/src/extract.rs`

- [ ] **Step 1: Add a failing test**

In `crates/stratify-lang-java/src/extract.rs` tests module, add:

```rust
    #[test]
    fn marks_main_method_as_entrypoint() {
        let src = "class App { public static void main(String[] a) {} void other() {} }";
        let g = extract("App.java", src);
        let main_id = g.symbols().iter().find(|s| s.name == "main").unwrap().id;
        assert_eq!(g.entrypoints(), &[main_id]);
    }
```

- [ ] **Step 2: Run, verify it fails**

Run: `cargo test -p stratify-lang-java marks_main`
Expected: FAIL (entrypoints empty).

- [ ] **Step 3: Mark main methods**

In `extract.rs`, in the class/method extraction loop, right after a method symbol is added with its id (the `g.add_symbol(...)` that returns `id` for a `SymbolKind::Function`), add: if the method name is `main`, mark it. Concretely, after the block that adds the symbol and its `Defines` edge, insert:

```rust
            if kind == SymbolKind::Function && name == "main" {
                g.mark_entrypoint(id);
            }
```

Note: `name` was moved into the symbol; bind it before the move (e.g. keep `let name = text(name_node, src).to_string();` and use `name.clone()` for the symbol so `name` is still available for the comparison), or compare against `g.symbol(id).unwrap().name`. Pick whichever keeps the borrow checker happy.

- [ ] **Step 4: Run, verify pass and existing java tests still pass**

Run: `cargo test -p stratify-lang-java`
Expected: PASS (5 tests).

- [ ] **Step 5: Verify the CLI end-to-end still behaves (Java unchanged)**

Run: `cargo test -p stratify-cli`
Expected: PASS. The Java e2e still reports `neverCalled` as warn and `helper` as info, because `main` is now marked by the adapter instead of by the analysis.

- [ ] **Step 6: Commit**

```bash
git add crates/stratify-lang-java
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(java): mark main methods as IR entrypoints"
```

---

## Task 4: Ruby adapter — modules, classes, methods (`stratify-lang-ruby`)

**Files:**
- Modify: `Cargo.toml` (workspace: add member + dep)
- Create: `crates/stratify-lang-ruby/Cargo.toml`
- Create: `crates/stratify-lang-ruby/src/extract.rs`
- Create: `crates/stratify-lang-ruby/src/lib.rs`

- [ ] **Step 1: Register the crate and dependency in the workspace**

In the root `Cargo.toml`, add `"crates/stratify-lang-ruby"` to `members`. In `[workspace.dependencies]` add:

```toml
tree-sitter-ruby = "0.23"
```

- [ ] **Step 2: Write the crate manifest**

Create `crates/stratify-lang-ruby/Cargo.toml`:

```toml
[package]
name = "stratify-lang-ruby"
edition.workspace = true
version.workspace = true
license.workspace = true

[dependencies]
stratify-core = { path = "../stratify-core" }
stratify-lang = { path = "../stratify-lang" }
tree-sitter = { workspace = true }
tree-sitter-ruby = { workspace = true }
streaming-iterator = "0.1"
```

- [ ] **Step 3: Write the extractor for definitions (mirror the Java extract.rs)**

Create `crates/stratify-lang-ruby/src/extract.rs`. Mirror the structure of `crates/stratify-lang-java/src/extract.rs` (parser builder, `span`, `text` helpers, the streaming-iterator query loop). Differences for Ruby:

- Set language with `parser.set_language(&tree_sitter_ruby::LANGUAGE.into())`.
- Emit a File symbol (kind `File`, `Confidence::Certain`) as in Java.
- Definition query captures Ruby node kinds:

```rust
    let query = Query::new(
        &tree_sitter_ruby::LANGUAGE.into(),
        r#"
        (method name: (identifier) @method.name) @method.node
        (class name: (constant) @class.name) @class.node
        (module name: (constant) @module.name) @module.node
        "#,
    )
    .expect("valid ruby query");
```

- For a `@method.name`/`@method.node` match, add a `SymbolKind::Function` symbol with `Confidence::Certain` and a `Defines` edge from the file (same as Java methods).
- For a `@class.name`/`@class.node` match, add a `SymbolKind::Class` symbol + `Defines` edge.
- For a `@module.name`/`@module.node` match, add a `SymbolKind::Module` symbol + `Defines` edge.

Use the same capture-index dispatch pattern as Java (`capture_index_for_name` for each of the six capture names; in the per-match loop, detect which name/node pair is present and set `kind` accordingly).

Add these tests in `extract.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use stratify_core::SymbolKind;

    #[test]
    fn extracts_module_class_method() {
        let src = "module M\n  class Foo\n    def bar\n    end\n  end\nend\n";
        let g = extract("foo.rb", src);
        let kinds: Vec<_> = g.symbols().iter().map(|s| (s.kind, s.name.as_str())).collect();
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
            g.references().iter().filter(|r| matches!(r.kind, RefKind::Defines)).count(),
            2
        );
    }
}
```

- [ ] **Step 4: Write the adapter**

Create `crates/stratify-lang-ruby/src/lib.rs`:

```rust
mod extract;

use stratify_core::IrGraph;
use stratify_lang::{AdapterError, LanguageAdapter};

pub struct RubyAdapter;

impl LanguageAdapter for RubyAdapter {
    fn language(&self) -> &'static str {
        "ruby"
    }

    fn handles_extension(&self, ext: &str) -> bool {
        ext == "rb"
    }

    fn parse_file(&self, path: &str, source: &str) -> Result<IrGraph, AdapterError> {
        Ok(extract::extract(path, source))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_parses_a_method() {
        let a = RubyAdapter;
        assert!(a.handles_extension("rb"));
        let g = a.parse_file("a.rb", "def hi\nend\n").unwrap();
        assert!(g.symbols().iter().any(|s| s.name == "hi"));
    }
}
```

- [ ] **Step 5: Run, verify pass**

Run: `cargo test -p stratify-lang-ruby`
Expected: PASS (3 tests).

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml crates/stratify-lang-ruby
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(ruby): extract modules, classes, methods into IR"
```

---

## Task 5: Ruby call edges + top-level entrypoint (`stratify-lang-ruby`)

**Files:**
- Modify: `crates/stratify-lang-ruby/src/extract.rs`

This is the heart of the universal-IR test. Ruby has no `main`. The file's top-level code is the entrypoint. The adapter marks the File symbol as an entrypoint and emits Calls edges from it to top-level method calls, and Calls edges from each method to the methods it calls (resolved against in-file method names, `Confidence::Likely`, matching Java's intra-file confidence).

- [ ] **Step 1: Add failing tests**

Add to the `extract.rs` tests module:

```rust
    #[test]
    fn marks_file_as_entrypoint() {
        let src = "def a\nend\n";
        let g = extract("x.rb", src);
        let file_id = g.symbols().iter().find(|s| s.kind == SymbolKind::File).unwrap().id;
        assert_eq!(g.entrypoints(), &[file_id]);
    }

    #[test]
    fn top_level_call_links_file_to_method() {
        // `greet` is defined and called at top level -> File --Calls--> greet.
        let src = "def greet\n  puts 'hi'\nend\n\ngreet\n";
        let g = extract("x.rb", src);
        let file_id = g.symbols().iter().find(|s| s.kind == SymbolKind::File).unwrap().id;
        let greet_id = g.symbols().iter().find(|s| s.name == "greet").unwrap().id;
        assert!(g.references().iter().any(|r|
            matches!(r.kind, RefKind::Calls) && r.from == file_id && r.to == greet_id));
    }

    #[test]
    fn intra_method_call_links_caller_to_callee() {
        let src = "def a\n  b\nend\n\ndef b\nend\n";
        let g = extract("x.rb", src);
        let a_id = g.symbols().iter().find(|s| s.name == "a").unwrap().id;
        let b_id = g.symbols().iter().find(|s| s.name == "b").unwrap().id;
        assert!(g.references().iter().any(|r|
            matches!(r.kind, RefKind::Calls) && r.from == a_id && r.to == b_id));
    }
```

Note: `greet` (a no-receiver, no-arg call) parses in tree-sitter-ruby as an `identifier`, while `b` likewise. To catch these we match BOTH `(call ...)` and bare method-name `(identifier)` nodes that resolve to an in-file method name. See Step 3.

- [ ] **Step 2: Run, verify failure**

Run: `cargo test -p stratify-lang-ruby top_level_call`
Expected: FAIL.

- [ ] **Step 3: Implement call extraction + entrypoint marking**

In `extract`, after the definition pass and before returning `g`:

1. Mark the file as an entrypoint: `g.mark_entrypoint(file_id);` (where `file_id` is the File symbol id from the start of the function).

2. Build `name_to_id` for `SymbolKind::Function` symbols (same as Java).

3. Resolve calls. Ruby call sites appear as two node shapes:
   - `(call method: (identifier) @callee)` for `recv.foo` and `foo(args)`.
   - a bare `(identifier)` used as a command call like `greet` (no parens, no receiver).

   To keep this tractable and avoid matching every identifier (locals, params), run two queries and union the results, but only KEEP a call when the callee name resolves to an in-file method in `name_to_id`. That resolution filter makes the bare-identifier case safe: only identifiers whose text equals a known in-file method name become Calls edges.

   Query A (explicit calls):

```rust
    let call_query = Query::new(
        &tree_sitter_ruby::LANGUAGE.into(),
        r#"(call method: (identifier) @callee) @callsite"#,
    )
    .expect("valid ruby call query");
```

   Query B (bare command-style identifiers):

```rust
    let ident_query = Query::new(
        &tree_sitter_ruby::LANGUAGE.into(),
        r#"(identifier) @ident"#,
    )
    .expect("valid ruby ident query");
```

   For each match in BOTH queries, take the callee identifier node, get its text, and skip unless `name_to_id` contains it. For Query B, additionally skip the identifier node if it is the `name:` child of a `method` definition (so a `def greet` header does not count as a call to `greet`); detect this by checking that the identifier node's parent kind is not `method` with this node as the name. A simple robust filter: skip if the identifier's parent kind is `method`, `block_parameter`, `method_parameters`, or `keyword_parameter`. Keep the filter list small and documented.

4. For each kept call site node, find the enclosing method with the same `enclosing_method_id(node, &g, file)` helper used in Java (copy it). If an enclosing method exists, add a Calls edge `enclosing -> callee` (`Confidence::Likely`). If there is no enclosing method (top-level call), add a Calls edge `file_id -> callee` (`Confidence::Likely`).

5. Deduplicate: because Query A and Query B can both surface the same `foo(...)` site, guard against adding a duplicate identical edge. Before pushing a Calls edge, skip if an identical `(from, to, Calls)` edge already exists in `g.references()`.

Copy the `enclosing_method_id` helper from the Java extractor verbatim (it is language-agnostic, it only reads spans):

```rust
fn enclosing_method_id(node: Node, g: &IrGraph, file: &str) -> Option<SymbolId> {
    let pos = node.start_byte();
    g.symbols()
        .iter()
        .filter(|s| matches!(s.kind, SymbolKind::Function) && s.span.file == file)
        .filter(|s| s.span.start_byte <= pos && pos < s.span.end_byte)
        .min_by_key(|s| s.span.end_byte - s.span.start_byte)
        .map(|s| s.id)
}
```

- [ ] **Step 4: Run, verify all ruby tests pass**

Run: `cargo test -p stratify-lang-ruby`
Expected: PASS (6 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/stratify-lang-ruby
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(ruby): top-level entrypoint and intra-file call edges"
```

---

## Task 6: Wire Ruby into the CLI + end-to-end (`stratify-cli`)

**Files:**
- Modify: `crates/stratify-cli/Cargo.toml`
- Modify: `crates/stratify-cli/src/run.rs`
- Create: `crates/stratify-cli/tests/sample-ruby/app.rb`
- Create: `crates/stratify-cli/tests/sample-ruby/unused.rb`
- Create: `crates/stratify-cli/tests/e2e_ruby.rs`

- [ ] **Step 1: Add the Ruby adapter dependency**

In `crates/stratify-cli/Cargo.toml` `[dependencies]`, add:

```toml
stratify-lang-ruby = { path = "../stratify-lang-ruby" }
```

- [ ] **Step 2: Register the adapter**

In `crates/stratify-cli/src/run.rs`, in `analyze_repo`, change the adapters vector to include Ruby:

```rust
    let adapters: Vec<Box<dyn LanguageAdapter>> =
        vec![Box::new(JavaAdapter), Box::new(stratify_lang_ruby::RubyAdapter)];
```

Add the import at the top: `use stratify_lang_java::JavaAdapter;` already exists; no Ruby `use` needed if you reference it by full path as above.

- [ ] **Step 3: Create the Ruby fixture**

Create `crates/stratify-cli/tests/sample-ruby/app.rb`:

```ruby
def helper
  puts "used"
end

helper
```

Create `crates/stratify-cli/tests/sample-ruby/unused.rb`:

```ruby
def never_called
  puts "dead"
end
```

- [ ] **Step 4: Write the end-to-end test**

Create `crates/stratify-cli/tests/e2e_ruby.rs`:

```rust
use std::path::Path;

#[test]
fn sample_ruby_reports_unused_methods() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sample-ruby");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_stratify"))
        .arg("check")
        .arg(&dir)
        .arg("--format")
        .arg("human")
        .output()
        .expect("run stratify binary");
    let stdout = String::from_utf8(output.stdout).unwrap();

    // never_called is never invoked -> unused (warning).
    assert!(stdout.contains("never_called"), "stdout: {stdout}");
    assert!(stdout.contains("warn"), "stdout: {stdout}");
    // helper is called at top level via a Likely edge -> possibly unused (info).
    assert!(stdout.contains("helper") && stdout.contains("possibly unused"), "stdout: {stdout}");
}
```

- [ ] **Step 5: Run the whole CLI test suite**

Run: `cargo test -p stratify-cli`
Expected: PASS (Java e2e, gate, unit, and the new Ruby e2e).

- [ ] **Step 6: Manual smoke check across both languages**

Run:
```bash
cargo build
./target/debug/stratify check crates/stratify-cli/tests/sample-ruby
```
Expected output (order may vary):
```
warn  unused.rb:1  unused function `never_called`
info  app.rb:1  possibly unused function `helper`

2 finding(s).
```

- [ ] **Step 7: Commit**

```bash
git add crates/stratify-cli
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(cli): register Ruby adapter, end-to-end Ruby dead-code"
```

---

## Task 7: fmt, clippy, lockfile

**Files:**
- Modify: generated `Cargo.lock`, any fmt changes

- [ ] **Step 1: Format and lint**

Run: `cargo fmt --all && cargo clippy --all-targets -- -D warnings`
Expected: no warnings. Fix any clippy findings properly (no blanket `#[allow]`).

- [ ] **Step 2: Full test suite**

Run: `cargo test`
Expected: all crates green, including the new Ruby crate and Ruby e2e.

- [ ] **Step 3: Commit**

```bash
git add -A
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "chore: fmt, clippy clean, update lockfile for ruby adapter"
```

---

## Self-Review Notes

Spec coverage for M2:
- Universal-IR bet (same analysis, two languages): proven by Task 2 (analysis unchanged except root source) + Task 6 (Ruby flows through to identical findings shape). Covered.
- Entrypoints as IR property: Task 1. Covered.
- Java parity preserved: Task 3 + Step 5. Covered.
- Ruby modules/classes/methods + calls + top-level entrypoint: Tasks 4, 5. Covered.
- CLI registration + Ruby e2e: Task 6. Covered.

Deferred (correctly out of M2): cross-file resolution (would raise intra-file calls from Likely to Certain), duplication/complexity/boundaries (M3/M4), Ruby metaprogramming and require-graph dependency tracking, bare-identifier precision beyond in-file-name resolution.

Known M2 imprecision (acceptable, and SAFE because it only downgrades, never false-clears):
- Ruby calls are all `Likely`, so any reachable method is reported "possibly unused" (info), matching Java's intra-file behavior. Cross-file resolution in a later milestone promotes these to Certain.
- The bare-identifier call heuristic only creates an edge when the identifier text matches an in-file method name. A local variable that shadows a method name could create a spurious Likely edge, which at worst downgrades a dead method to "possibly unused". It never hides a genuinely dead method as "used" with certainty.
- Method calls to methods defined in other files are unresolved in M2 (no edge), so a method only called from another file looks dead. This is the known cross-file limitation, resolved in a later milestone.

Type consistency: `IrGraph::mark_entrypoint`, `IrGraph::entrypoints`, `RubyAdapter`, `extract::extract`, `enclosing_method_id`, `SymbolKind::{Module,Class,Function,File}`, `RefKind::{Defines,Calls}`, `Confidence::{Certain,Likely}` are used consistently with their definitions in M1.

# Stratify M14 (Cross-File Call Resolution) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Resolve function calls across files so a function called only from another file is no longer falsely reported as dead. This sharpens dead-code accuracy for all five languages.

**The problem today:** every adapter resolves calls only within a single file (intra-file `Calls` edges, `Confidence::Likely`) and silently drops calls whose callee is not defined in that file. So `helper` defined in `b.rb` and called from `a.rb` gets no incoming edge, is unreachable from any root, and is reported "unused" (Warning) even though it is used.

**The fix:** adapters record the call sites they currently drop as *unresolved calls* (caller + callee name). After the per-file graphs merge, a post-merge pass matches each unresolved callee name against the repo-wide function symbols and adds a `Calls` edge to each match. Resolved cross-file edges are `Confidence::Likely` — cross-file resolution by bare name is a heuristic without full import/scope analysis, so it must only ever *downgrade* a "dead" verdict to "possibly unused", never falsely clear one. This preserves the confidence model's safety guarantee (the same one that makes Ruby's dynamism safe).

**Architecture:** IR gains an `unresolved_calls` list (set by adapters, remapped on merge). A new `stratify-analysis::resolve::cross_file_calls(&mut graph)` runs in the CLI after merge and before the analyses, adding the resolved edges. No change to dead-code itself — it just sees a more complete call graph.

**Scope:** This fixes false-dead for cross-file-called functions. It does NOT promote intra-file calls to `Certain` (so a function used only inside its own file still shows "possibly unused" — that is the honest confidence stance, and eliminating it needs import-aware Certain resolution, a larger future feature). Cross-file edges are Likely, so a genuinely-used cross-file function moves from "unused" (Warning) to "possibly unused" (Info) or, when reached via a Certain chain, disappears from findings.

**Prerequisite reading:** `crates/stratify-core/src/graph.rs` (the `entrypoints` field is the exact pattern to copy for `unresolved_calls`), `crates/stratify-analysis/src/deadcode.rs` (how Calls edges drive reachability), and the call pass in each adapter's `extract.rs` (where unresolved callees are currently dropped).

---

## File Structure

```
crates/stratify-core/src/graph.rs            MODIFY: unresolved_calls storage + merge remap
crates/stratify-analysis/src/resolve.rs       CREATE: cross_file_calls pass
crates/stratify-analysis/src/lib.rs           MODIFY: pub mod resolve
crates/stratify-lang-java/src/extract.rs      MODIFY: record unresolved calls
crates/stratify-lang-ruby/src/extract.rs      MODIFY: record unresolved calls
crates/stratify-lang-ts/src/extract.rs        MODIFY: record unresolved calls
crates/stratify-lang-py/src/extract.rs        MODIFY: record unresolved calls
crates/stratify-lang-go/src/extract.rs        MODIFY: record unresolved calls
crates/stratify-cli/src/run.rs                MODIFY: run cross_file_calls after merge
crates/stratify-cli/tests/sample-xfile/       CREATE: cross-file fixture
crates/stratify-cli/tests/e2e_xfile.rs        CREATE: end-to-end cross-file resolution
```

---

## Task 1: IR unresolved-calls storage (`stratify-core`) + resolution pass (`stratify-analysis`)

**Files:**
- Modify: `crates/stratify-core/src/graph.rs`
- Create: `crates/stratify-analysis/src/resolve.rs`
- Modify: `crates/stratify-analysis/src/lib.rs`

- [ ] **Step 1: Add IR storage with a failing test**

In `crates/stratify-core/src/graph.rs`, add `unresolved_calls: Vec<(SymbolId, String)>` to `IrGraph` (it derives Default, so the field initializes empty). Add methods:

```rust
    /// Record a call whose callee was not found in the caller's own file, for
    /// later cross-file resolution. `from` is the caller (enclosing function or
    /// the file), `name` is the callee identifier.
    pub fn add_unresolved_call(&mut self, from: SymbolId, name: String) {
        self.unresolved_calls.push((from, name));
    }

    pub fn unresolved_calls(&self) -> &[(SymbolId, String)] {
        &self.unresolved_calls
    }
```

In `merge`, after the existing remap loops, remap the `from` id (names are unchanged):

```rust
        for (from, name) in other.unresolved_calls {
            self.unresolved_calls.push((SymbolId(from.0 + offset), name));
        }
```

Add tests to the graph tests module:

```rust
    #[test]
    fn records_unresolved_calls() {
        let mut g = IrGraph::new();
        let a = g.add_symbol(sym("a"));
        g.add_unresolved_call(a, "other".into());
        assert_eq!(g.unresolved_calls(), &[(a, "other".to_string())]);
    }

    #[test]
    fn merge_remaps_unresolved_call_from() {
        let mut g1 = IrGraph::new();
        g1.add_symbol(sym("a"));
        let mut g2 = IrGraph::new();
        let x = g2.add_symbol(sym("x"));
        g2.add_unresolved_call(x, "target".into());
        g1.merge(g2);
        // x was id 0 in g2 -> id 1 after merge (offset 1); name unchanged.
        assert_eq!(g1.unresolved_calls(), &[(SymbolId(1), "target".to_string())]);
    }
```

- [ ] **Step 2: Run core tests**

Run: `cargo test -p stratify-core` (if `cargo` missing: `source "$HOME/.cargo/env"`)
Expected: PASS.

- [ ] **Step 3: Write the resolution pass with tests on hand-built graphs**

Create `crates/stratify-analysis/src/resolve.rs`:

```rust
use std::collections::HashMap;
use stratify_core::ir::{Reference, Span, SymbolId};
use stratify_core::{Confidence, IrGraph, RefKind, SymbolKind};

/// Resolve cross-file calls: for each recorded unresolved call, add a `Calls`
/// edge to every repo-wide Function whose name matches the callee, when that
/// function lives in a DIFFERENT file than the caller. Edges are `Likely`
/// (bare-name matching is a heuristic, so this only ever downgrades a "dead"
/// verdict, never falsely clears one). Existing identical edges are not
/// duplicated.
pub fn cross_file_calls(graph: &mut IrGraph) {
    // Repo-wide function name -> (symbol id, file).
    let mut by_name: HashMap<String, Vec<(SymbolId, String)>> = HashMap::new();
    for s in graph.symbols() {
        if matches!(s.kind, SymbolKind::Function) {
            by_name
                .entry(s.name.clone())
                .or_default()
                .push((s.id, s.span.file.clone()));
        }
    }

    // Existing Calls edges, to avoid duplicates.
    let mut existing: std::collections::HashSet<(SymbolId, SymbolId)> = graph
        .references()
        .iter()
        .filter(|r| matches!(r.kind, RefKind::Calls))
        .map(|r| (r.from, r.to))
        .collect();

    // Caller id -> caller file, for the cross-file check.
    let caller_file: HashMap<SymbolId, String> = graph
        .symbols()
        .iter()
        .map(|s| (s.id, s.span.file.clone()))
        .collect();

    let mut to_add: Vec<Reference> = Vec::new();
    for (from, name) in graph.unresolved_calls() {
        let Some(candidates) = by_name.get(name) else {
            continue; // not a repo function (stdlib/builtin/external) — skip
        };
        let from_file = caller_file.get(from);
        for (to, to_file) in candidates {
            // cross-file only: intra-file calls were already resolved by adapters
            if from_file.map(|f| f == to_file).unwrap_or(false) {
                continue;
            }
            if to == from {
                continue;
            }
            if existing.insert((*from, *to)) {
                to_add.push(Reference {
                    from: *from,
                    to: *to,
                    kind: RefKind::Calls,
                    span: Span { file: "<resolved>".into(), start_byte: 0, end_byte: 0, start_line: 0 },
                    confidence: Confidence::Likely,
                });
            }
        }
    }
    for r in to_add {
        graph.add_reference(r);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stratify_core::ir::{Symbol, Visibility};

    fn func(g: &mut IrGraph, name: &str, file: &str) -> SymbolId {
        g.add_symbol(Symbol {
            id: SymbolId(0),
            kind: SymbolKind::Function,
            name: name.into(),
            fqn: name.into(),
            span: Span { file: file.into(), start_byte: 0, end_byte: 1, start_line: 1 },
            visibility: Visibility::Unknown,
            confidence: Confidence::Certain,
        })
    }

    #[test]
    fn resolves_cross_file_call() {
        let mut g = IrGraph::new();
        let caller = func(&mut g, "caller", "a.rb");
        let target = func(&mut g, "target", "b.rb");
        g.add_unresolved_call(caller, "target".into());
        cross_file_calls(&mut g);
        assert!(g.references().iter().any(|r|
            matches!(r.kind, RefKind::Calls) && r.from == caller && r.to == target && r.confidence == Confidence::Likely));
    }

    #[test]
    fn ignores_unknown_callee() {
        let mut g = IrGraph::new();
        let caller = func(&mut g, "caller", "a.rb");
        g.add_unresolved_call(caller, "println".into()); // no repo function named this
        cross_file_calls(&mut g);
        assert!(g.references().iter().all(|r| !matches!(r.kind, RefKind::Calls)));
    }

    #[test]
    fn does_not_resolve_same_file() {
        // An unresolved call whose only match is in the caller's own file is not
        // re-added here (intra-file resolution is the adapter's job).
        let mut g = IrGraph::new();
        let caller = func(&mut g, "caller", "a.rb");
        let _same = func(&mut g, "target", "a.rb");
        g.add_unresolved_call(caller, "target".into());
        cross_file_calls(&mut g);
        assert!(g.references().iter().all(|r| !matches!(r.kind, RefKind::Calls)));
    }

    #[test]
    fn dedupes_against_existing_edge() {
        let mut g = IrGraph::new();
        let caller = func(&mut g, "caller", "a.rb");
        let target = func(&mut g, "target", "b.rb");
        g.add_reference(Reference {
            from: caller, to: target, kind: RefKind::Calls,
            span: Span { file: "a.rb".into(), start_byte: 0, end_byte: 1, start_line: 1 },
            confidence: Confidence::Likely,
        });
        g.add_unresolved_call(caller, "target".into());
        cross_file_calls(&mut g);
        let count = g.references().iter().filter(|r| matches!(r.kind, RefKind::Calls) && r.from == caller && r.to == target).count();
        assert_eq!(count, 1, "should not duplicate the existing edge");
    }
}
```

- [ ] **Step 4: Wire lib.rs**

In `crates/stratify-analysis/src/lib.rs`, add `pub mod resolve;`.

- [ ] **Step 5: Run, verify pass**

Run: `cargo test -p stratify-core && cargo test -p stratify-analysis`
Expected: PASS (4 new resolve tests + 2 new core tests, plus all prior).

- [ ] **Step 6: Commit**

```bash
git add crates/stratify-core crates/stratify-analysis
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(core,analysis): unresolved-call IR storage + cross-file resolution pass"
```

---

## Task 2: Adapters record unresolved calls (all five languages)

**Files:**
- Modify: `crates/stratify-lang-java/src/extract.rs`, `stratify-lang-ruby/src/extract.rs`, `stratify-lang-ts/src/extract.rs`, `stratify-lang-py/src/extract.rs`, `stratify-lang-go/src/extract.rs`

Each adapter's call pass currently iterates call sites, computes the callee name, looks it up in the in-file `name_to_id`, and on a hit adds a `Calls` edge. On a miss it currently does nothing (`continue` / skip). Change the miss branch to record an unresolved call.

- [ ] **Step 1: Update each adapter's call pass**

In every adapter, at the point where a callee name is NOT found in `name_to_id`, compute the caller the same way resolved calls do (`enclosing_method_id(call_node, &g, file).unwrap_or(file_id)`) and record it:

```rust
            // existing: if let Some(&callee_id) = name_to_id.get(&name) { ...add edge... }
            // add an else branch:
            else {
                let from = enclosing_method_id(call_node, &g, file).unwrap_or(file_id);
                g.add_unresolved_call(from, name.clone());
            }
```

Adapt to each adapter's local variable names (`call_node`, the callee name binding, `file`, `file_id`). The exact call-pass structure differs slightly per adapter (Ruby has two query passes for `(call ...)` and bare identifiers; TS/Go have identifier + member/selector; Python uses node traversal) — in each, the principle is the same: when the callee name does not resolve in-file, record an unresolved call from the enclosing function (or file) with that name. Do NOT record when it DID resolve in-file (that already became a real edge). For Ruby/Python where bare identifiers are filtered, only record names that look like calls you attempted to resolve (i.e. the same call sites you currently inspect) — do not flood with every identifier; keep the same call-site set, just route the misses to `add_unresolved_call` instead of dropping them.

- [ ] **Step 2: Add a per-adapter test**

In each adapter, add one test that a call to a name NOT defined in the file is recorded as unresolved. Example (Ruby):

```rust
    #[test]
    fn records_unresolved_cross_file_call() {
        // `external` is not defined in this file -> recorded as unresolved.
        let g = extract("a.rb", "def caller\n  external\nend\n");
        let caller = g.symbols().iter().find(|s| s.name == "caller").unwrap().id;
        assert!(g.unresolved_calls().iter().any(|(from, name)| *from == caller && name == "external"));
    }
```

Write the analogous test for Java (`void m(){ external(); }`), TS (`function m(){ external(); }`), Python (`def m():\n    external()`), and Go (`func m(){ external() }`), each asserting the call to `external` is recorded as an unresolved call from the enclosing function. (For Go, the enclosing function `m` must itself be reachable for the name to matter, but the test only checks recording, not reachability.)

- [ ] **Step 3: Run each adapter's tests**

Run: `cargo test -p stratify-lang-java -p stratify-lang-ruby -p stratify-lang-ts -p stratify-lang-py -p stratify-lang-go`
Expected: PASS, with the new `records_unresolved_cross_file_call` test in each. Existing intra-file call tests must still pass unchanged.

- [ ] **Step 4: Commit**

```bash
git add crates/stratify-lang-java crates/stratify-lang-ruby crates/stratify-lang-ts crates/stratify-lang-py crates/stratify-lang-go
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(adapters): record unresolved (cross-file) calls in all five languages"
```

---

## Task 3: Wire resolution into the CLI + end-to-end

**Files:**
- Modify: `crates/stratify-cli/src/run.rs`
- Create: `crates/stratify-cli/tests/sample-xfile/main.rb`, `crates/stratify-cli/tests/sample-xfile/greeter.rb`
- Create: `crates/stratify-cli/tests/e2e_xfile.rs`

- [ ] **Step 1: Run the resolution pass after merge**

In `crates/stratify-cli/src/run.rs::analyze_repo`, after the per-file graphs are merged into `graph` and BEFORE the analyses run, add:

```rust
    stratify_analysis::resolve::cross_file_calls(&mut graph);
```

(`graph` must be `mut`; it already is for `merge`. This adds the cross-file edges so dead-code sees the complete call graph.)

- [ ] **Step 2: Create a cross-file fixture (Ruby)**

`crates/stratify-cli/tests/sample-xfile/greeter.rb`:

```ruby
def greet
  puts "hello"
end
```

`crates/stratify-cli/tests/sample-xfile/main.rb`:

```ruby
greet
```

(`main.rb` calls `greet` at top level; `greet` is defined in `greeter.rb`. Before cross-file resolution `greet` had no incoming edge and was reported "unused" / Warning. After resolution, the file-scope entrypoint of `main.rb` reaches `greet` via a Likely edge, so `greet` is reported "possibly unused" / Info, not "unused" / Warning.)

- [ ] **Step 3: End-to-end test**

Create `crates/stratify-cli/tests/e2e_xfile.rs`:

```rust
use std::path::Path;

#[test]
fn cross_file_call_downgrades_dead_to_possibly_unused() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sample-xfile");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_stratify"))
        .arg("check")
        .arg(&dir)
        .arg("--format")
        .arg("json")
        .output()
        .expect("run stratify binary");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let greet = v["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["message"].as_str().unwrap().contains("greet"))
        .expect("a finding mentioning greet");
    // Cross-file resolution connected main.rb -> greet, so greet is reachable
    // (Likely) and reported as info "possibly unused", not warning "unused".
    assert_eq!(greet["severity"], "info", "greet should be possibly-unused, not dead: {stdout}");
    assert!(greet["message"].as_str().unwrap().contains("possibly unused"), "{stdout}");
}
```

(`serde_json` is already a dev-dependency of `stratify-cli`.)

- [ ] **Step 4: Run + manual smoke**

Run: `cargo test -p stratify-cli`
Expected: PASS including `e2e_xfile`, and all prior e2e tests still pass (cross-file resolution must not regress the per-language fixtures — each is a single file or self-contained, so their findings are unchanged).

Manual — show the before/after intuition:
```bash
cargo build
./target/debug/stratify check crates/stratify-cli/tests/sample-xfile
```
Expected: `info  greeter.rb:1  possibly unused function `greet`` (info), not a `warn ... unused` line.

- [ ] **Step 5: Commit**

```bash
git add crates/stratify-cli
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(cli): run cross-file call resolution, end-to-end cross-file test"
```

---

## Task 4: fmt, clippy, lockfile

- [ ] **Step 1: Format and lint**

Run: `cargo fmt --all && cargo clippy --all-targets -- -D warnings`
Fix any warning properly (no blanket `#[allow]`). Re-run `cargo test` after any fix.

- [ ] **Step 2: Full suite**

Run: `cargo test`
Expected: all crates green.

- [ ] **Step 3: Commit**

```bash
git add -A
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "chore: fmt, clippy clean, update lockfile for cross-file calls"
```

---

## Self-Review Notes

Spec coverage for M14:
- IR records unresolved calls, remapped on merge: Task 1. Covered.
- Language-agnostic cross-file resolution pass (Likely, cross-file only, deduped): Task 1. Covered.
- All five adapters record their dropped cross-file calls: Task 2. Covered.
- CLI runs resolution after merge; e2e proves a cross-file-called function is no longer falsely dead: Task 3. Covered.

Deferred (correctly out of M14): promoting calls to `Certain` (intra-file-only-used functions still show "possibly unused" — the honest confidence stance; eliminating it needs import-aware Certain resolution), qualified/scoped resolution (we match by bare function name repo-wide, so a call to `foo` links to every repo function named `foo` — safe because Likely-only, but imprecise for overloaded/shared names), and method-receiver type resolution.

Known M14 characteristics (acceptable, and SAFE by construction):
- Resolution matches by bare callee name across the whole repo. A name shared by several functions links the caller to all of them. Because every resolved edge is `Likely`, this can only downgrade a "dead" verdict to "possibly unused" — it can never falsely mark a dead function as certainly used. This is the same safety property the confidence model relies on elsewhere.
- Calls to stdlib/builtins/external names (no repo function of that name) resolve to nothing and are dropped.
- A cross-file-called function whose caller is itself reachable becomes reachable (Likely) and reports as "possibly unused" rather than "unused"; if the caller is not reachable, the callee stays flagged. Either way the result is more accurate than the pre-M14 "always dead" outcome.

Type consistency: `IrGraph::add_unresolved_call`/`unresolved_calls`, `resolve::cross_file_calls`, `enclosing_method_id` (per adapter), `RefKind::Calls`, `Confidence::Likely`, `SymbolKind::Function` are used consistently with their M1-M13 definitions.

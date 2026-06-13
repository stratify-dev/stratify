# Stratify M4 (Complexity) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Compute cyclomatic complexity per function in each adapter, store it as an IR metric, and report overly complex functions through one language-agnostic analysis. Java and Ruby both covered.

**Architecture:** Add a complexity metric to the IR keyed by SymbolId (same pattern as entrypoints: a `Vec<(SymbolId, u32)>` remapped on merge). Each adapter, when it adds a function symbol, counts decision points in that function's subtree and records the complexity. A new `complexity` analysis flags functions above a threshold. Hotspots (complexity x git churn) are deferred to M5.

**Tech Stack:** Rust, existing workspace crates, tree-sitter (already wired).

**Prerequisite reading:** `crates/stratify-lang-java/src/extract.rs` and `crates/stratify-lang-ruby/src/extract.rs` (you will add a complexity count where function symbols are created), and `crates/stratify-core/src/graph.rs` (the `entrypoints` field is the exact pattern to copy for the complexity metric).

---

## File Structure

```
crates/stratify-core/src/graph.rs          MODIFY: complexity metric (set_complexity, complexity_of, complexities, merge remap)
crates/stratify-lang-java/src/extract.rs    MODIFY: count cyclomatic complexity per method
crates/stratify-lang-ruby/src/extract.rs    MODIFY: count cyclomatic complexity per method
crates/stratify-analysis/src/complexity.rs  CREATE: threshold analysis
crates/stratify-analysis/src/lib.rs         MODIFY: pub mod complexity
crates/stratify-cli/src/run.rs              MODIFY: run complexity in analyze_repo
crates/stratify-cli/tests/sample-complex/   CREATE: a high-complexity fixture
crates/stratify-cli/tests/e2e_complexity.rs CREATE: end-to-end complexity test
```

---

## Task 1: Complexity metric in the IR (`stratify-core`)

**Files:**
- Modify: `crates/stratify-core/src/graph.rs`

- [ ] **Step 1: Add failing tests (mirror the entrypoint tests)**

In `crates/stratify-core/src/graph.rs` tests module, add:

```rust
    #[test]
    fn set_and_read_complexity() {
        let mut g = IrGraph::new();
        let a = g.add_symbol(sym("a"));
        g.set_complexity(a, 7);
        assert_eq!(g.complexity_of(a), Some(7));
        assert_eq!(g.complexity_of(SymbolId(999)), None);
    }

    #[test]
    fn merge_remaps_complexity() {
        let mut g1 = IrGraph::new();
        g1.add_symbol(sym("a"));
        let mut g2 = IrGraph::new();
        let x = g2.add_symbol(sym("x"));
        g2.set_complexity(x, 5);
        g1.merge(g2);
        // x was id 0 in g2, becomes id 1 after merge (offset 1).
        assert_eq!(g1.complexity_of(SymbolId(1)), Some(5));
    }
```

- [ ] **Step 2: Run, verify fail**

Run: `cargo test -p stratify-core` (if `cargo` missing: `source "$HOME/.cargo/env"`)
Expected: FAIL, no method `set_complexity`.

- [ ] **Step 3: Add the metric**

In `graph.rs`, add `complexity: Vec<(SymbolId, u32)>` to the struct. Add methods:

```rust
    /// Record a function's cyclomatic complexity. Set by adapters.
    pub fn set_complexity(&mut self, id: SymbolId, value: u32) {
        self.complexity.push((id, value));
    }

    pub fn complexity_of(&self, id: SymbolId) -> Option<u32> {
        self.complexity.iter().find(|(i, _)| *i == id).map(|(_, v)| *v)
    }

    pub fn complexities(&self) -> &[(SymbolId, u32)] {
        &self.complexity
    }
```

In `merge`, after the tokens line, remap complexity ids by `offset`:

```rust
        for (id, v) in other.complexity {
            self.complexity.push((SymbolId(id.0 + offset), v));
        }
```

- [ ] **Step 4: Run, verify pass**

Run: `cargo test -p stratify-core`
Expected: PASS (13 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/stratify-core
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(core): per-function complexity metric in the IR"
```

---

## Task 2: Java cyclomatic complexity (`stratify-lang-java`)

**Files:**
- Modify: `crates/stratify-lang-java/src/extract.rs`

Cyclomatic complexity = 1 + number of decision points in the function subtree.

- [ ] **Step 1: Add a failing test**

In `crates/stratify-lang-java/src/extract.rs` tests module:

```rust
    #[test]
    fn computes_method_complexity() {
        // base 1 + two ifs + one && = 4
        let src = "class A { void m(int x) { if (x > 0 && x < 9) {} if (x == 5) {} } }";
        let g = extract("A.java", src);
        let m = g.symbols().iter().find(|s| s.name == "m").unwrap().id;
        assert_eq!(g.complexity_of(m), Some(4));
    }
```

- [ ] **Step 2: Run, verify fail**

Run: `cargo test -p stratify-lang-java computes_method_complexity`
Expected: FAIL.

- [ ] **Step 3: Add the counter and record it**

In `extract.rs`, add a decision-counting helper:

```rust
/// Count decision points in a subtree for cyclomatic complexity.
fn count_decisions_java(node: Node) -> u32 {
    let mut count = 0u32;
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "if_statement" | "while_statement" | "for_statement"
            | "enhanced_for_statement" | "do_statement" | "catch_clause"
            | "switch_label" | "ternary_expression" | "&&" | "||" => {
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

fn cyclomatic_java(node: Node) -> u32 {
    1 + count_decisions_java(node)
}
```

Where the method `Function` symbol is added (you get its `id` and have `decl_node`), record complexity right after marking entrypoints:

```rust
            if kind == SymbolKind::Function {
                let cx = cyclomatic_java(decl_node);
                g.set_complexity(id, cx);
            }
```

(Place this inside the same branch that handles a matched declaration, using the `decl_node` captured for that match. Only set complexity for `SymbolKind::Function`, not classes.)

- [ ] **Step 4: Run, verify pass and existing java tests still pass**

Run: `cargo test -p stratify-lang-java`
Expected: PASS (7 tests). If the count is off by the `switch_label`/ternary mapping, inspect actual node kinds with a temporary `to_sexp()` print, fix the match arm, REMOVE the print, and report it.

- [ ] **Step 5: Commit**

```bash
git add crates/stratify-lang-java
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(java): cyclomatic complexity per method"
```

---

## Task 3: Ruby cyclomatic complexity (`stratify-lang-ruby`)

**Files:**
- Modify: `crates/stratify-lang-ruby/src/extract.rs`

- [ ] **Step 1: Add a failing test**

In `crates/stratify-lang-ruby/src/extract.rs` tests module:

```rust
    #[test]
    fn computes_method_complexity() {
        // base 1 + if + elsif + while = 4
        let src = "def m(x)\n  if x > 0\n  elsif x < 9\n  end\n  while x > 0\n  end\nend\n";
        let g = extract("m.rb", src);
        let m = g.symbols().iter().find(|s| s.name == "m").unwrap().id;
        assert_eq!(g.complexity_of(m), Some(4));
    }
```

- [ ] **Step 2: Run, verify fail**

Run: `cargo test -p stratify-lang-ruby computes_method_complexity`
Expected: FAIL.

- [ ] **Step 3: Add the Ruby counter and record it**

```rust
fn count_decisions_ruby(node: Node) -> u32 {
    let mut count = 0u32;
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "if" | "elsif" | "unless" | "while" | "until" | "for" | "when"
            | "rescue" | "conditional" | "if_modifier" | "unless_modifier"
            | "while_modifier" | "until_modifier" | "&&" | "||" | "and" | "or" => {
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

fn cyclomatic_ruby(node: Node) -> u32 {
    1 + count_decisions_ruby(node)
}
```

Record it where the `method` Function symbol is added (after the entrypoint/Defines handling for that method):

```rust
            let cx = cyclomatic_ruby(decl_node);
            g.set_complexity(id, cx);
```

Set complexity ONLY for method (Function) symbols, not classes/modules.

Note on Ruby `if`: a top-level `if x` expression is kind `if`; the `elsif` clause is kind `elsif`. If the test count is off (e.g. the grammar nests differently), discover the real kinds with a temporary `to_sexp()` print on the test source, fix the match arms, REMOVE the print, and report the adjustment.

- [ ] **Step 4: Run, verify pass**

Run: `cargo test -p stratify-lang-ruby`
Expected: PASS (9 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/stratify-lang-ruby
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(ruby): cyclomatic complexity per method"
```

---

## Task 4: Complexity analysis (`stratify-analysis`)

**Files:**
- Create: `crates/stratify-analysis/src/complexity.rs`
- Modify: `crates/stratify-analysis/src/lib.rs`

- [ ] **Step 1: Write the analysis with tests on hand-built graphs**

Create `crates/stratify-analysis/src/complexity.rs`:

```rust
use stratify_core::{Confidence, Finding, IrGraph, Severity, SymbolKind};

/// Flag functions whose cyclomatic complexity exceeds `threshold`.
/// At or above 2x the threshold the finding is a Warning, otherwise Info.
pub fn analyze(graph: &IrGraph, threshold: u32) -> Vec<Finding> {
    let mut findings = Vec::new();
    for s in graph.symbols() {
        if !matches!(s.kind, SymbolKind::Function) {
            continue;
        }
        let Some(cx) = graph.complexity_of(s.id) else {
            continue;
        };
        if cx <= threshold {
            continue;
        }
        let severity = if cx >= threshold.saturating_mul(2) {
            Severity::Warning
        } else {
            Severity::Info
        };
        findings.push(Finding {
            rule: "complexity".into(),
            severity,
            message: format!(
                "function `{}` has high cyclomatic complexity ({cx})",
                s.name
            ),
            span: s.span.clone(),
            confidence: Confidence::Certain,
        });
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use stratify_core::ir::{Span, Symbol, SymbolId, Visibility};

    fn func(g: &mut IrGraph, name: &str, cx: u32) -> SymbolId {
        let id = g.add_symbol(Symbol {
            id: SymbolId(0),
            kind: SymbolKind::Function,
            name: name.into(),
            fqn: name.into(),
            span: Span { file: "T.rb".into(), start_byte: 0, end_byte: 1, start_line: 1 },
            visibility: Visibility::Unknown,
            confidence: Confidence::Certain,
        });
        g.set_complexity(id, cx);
        id
    }

    #[test]
    fn flags_function_above_threshold() {
        let mut g = IrGraph::new();
        func(&mut g, "simple", 3);
        func(&mut g, "gnarly", 12);
        let findings = analyze(&g, 10);
        assert_eq!(findings.len(), 1);
        assert!(findings[0].message.contains("gnarly"));
        assert!(findings[0].message.contains("12"));
    }

    #[test]
    fn severity_escalates_at_double_threshold() {
        let mut g = IrGraph::new();
        func(&mut g, "high", 15); // > 10 but < 20 -> Info
        func(&mut g, "extreme", 25); // >= 20 -> Warning
        let findings = analyze(&g, 10);
        let high = findings.iter().find(|f| f.message.contains("high")).unwrap();
        let extreme = findings.iter().find(|f| f.message.contains("extreme")).unwrap();
        assert_eq!(high.severity, Severity::Info);
        assert_eq!(extreme.severity, Severity::Warning);
    }

    #[test]
    fn nothing_at_or_below_threshold() {
        let mut g = IrGraph::new();
        func(&mut g, "ok", 10);
        assert!(analyze(&g, 10).is_empty());
    }
}
```

- [ ] **Step 2: Wire lib.rs**

In `crates/stratify-analysis/src/lib.rs`, add:

```rust
pub mod complexity;
```

- [ ] **Step 3: Run, verify pass**

Run: `cargo test -p stratify-analysis`
Expected: PASS (11 tests: 4 deadcode + 4 duplication + 3 complexity).

- [ ] **Step 4: Commit**

```bash
git add crates/stratify-analysis
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(analysis): cyclomatic complexity threshold analysis"
```

---

## Task 5: Wire complexity into the CLI + end-to-end (`stratify-cli`)

**Files:**
- Modify: `crates/stratify-cli/src/run.rs`
- Create: `crates/stratify-cli/tests/sample-complex/gnarly.rb`
- Create: `crates/stratify-cli/tests/e2e_complexity.rs`

- [ ] **Step 1: Run complexity in analyze_repo**

In `crates/stratify-cli/src/run.rs`, add a constant near `DUP_MIN_TOKENS`:

```rust
/// Cyclomatic complexity above this is reported.
const COMPLEXITY_THRESHOLD: u32 = 10;
```

In `analyze_repo`, after the duplication line, extend findings:

```rust
    findings.extend(stratify_analysis::complexity::analyze(&graph, COMPLEXITY_THRESHOLD));
```

(Keep the existing dead-code and duplication lines; complexity findings come last.)

- [ ] **Step 2: Create a high-complexity fixture**

Create `crates/stratify-cli/tests/sample-complex/gnarly.rb`:

```ruby
def classify(n)
  if n < 0
    return "negative"
  elsif n == 0
    return "zero"
  elsif n < 10
    return "small"
  elsif n < 100
    return "medium"
  elsif n < 1000
    return "large"
  elsif n < 10000
    return "huge"
  elsif n < 100000
    return "massive"
  elsif n < 1000000
    return "enormous"
  elsif n < 10000000
    return "gigantic"
  else
    return "unknown"
  end
end

classify(5)
```

This `classify` method has 1 base + 1 `if` + 9 `elsif` = complexity 11, above the threshold of 10. (`classify` is called at top level, so it is not also flagged as dead code.)

- [ ] **Step 3: Write the end-to-end test**

Create `crates/stratify-cli/tests/e2e_complexity.rs`:

```rust
use std::path::Path;

#[test]
fn sample_complex_reports_high_complexity() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sample-complex");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_stratify"))
        .arg("check")
        .arg(&dir)
        .arg("--format")
        .arg("json")
        .output()
        .expect("run stratify binary");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("\"rule\": \"complexity\""), "stdout: {stdout}");
    assert!(stdout.contains("classify"), "stdout: {stdout}");
}
```

- [ ] **Step 4: Run + manual smoke**

Run: `cargo test -p stratify-cli`
Expected: PASS (including the new complexity e2e). If `classify` is not flagged, check the Ruby `elsif` counting from Task 3 (the count must reach 11); fix the adapter, not the fixture.

Manual:
```bash
cargo build
./target/debug/stratify check crates/stratify-cli/tests/sample-complex
```
Expected: a finding `... function `classify` has high cyclomatic complexity (11)`.

- [ ] **Step 5: Commit**

```bash
git add crates/stratify-cli
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(cli): run complexity analysis, end-to-end high-complexity detection"
```

---

## Task 6: fmt, clippy, lockfile

- [ ] **Step 1: Format and lint**

Run: `cargo fmt --all && cargo clippy --all-targets -- -D warnings`
Fix any warning properly (no blanket `#[allow]`). Re-run until clean.

- [ ] **Step 2: Full suite**

Run: `cargo test`
Expected: all crates green (core 13, java 7, ruby 9, analysis 11, cli incl. 4 e2e + gate, lang 1, report 3).

- [ ] **Step 3: Commit**

```bash
git add -A
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "chore: fmt, clippy clean, update lockfile for complexity"
```

---

## Self-Review Notes

Spec coverage for M4 (complexity slice):
- IR complexity metric with merge remap: Task 1. Covered.
- Both adapters compute cyclomatic complexity per function: Tasks 2, 3. Covered.
- Language-agnostic threshold analysis: Task 4. Covered.
- CLI integration + e2e: Task 5. Covered.

Deferred (correctly out of this slice): hotspots (complexity x git churn) become M5, including the git side-effect and path normalization between churn data and IR file paths. Cognitive complexity (a refinement over cyclomatic) and per-language tuning of decision-node weights are later refinements.

Known M4 characteristics (acceptable):
- Cyclomatic complexity is an approximation: `switch_label` counts each case (including default) and the decision-node lists may not enumerate every grammar edge case. Counts can be off by small amounts on exotic syntax. This is standard for cyclomatic tools and acceptable; the threshold is a heuristic, not a contract.
- Complexity is computed over the whole declaration subtree (including the signature), which adds no decision points in practice.
- Nested functions/lambdas count their inner decisions toward the enclosing function. Acceptable for M4.

Type consistency: `IrGraph::set_complexity`/`complexity_of`/`complexities`, `complexity::analyze(&IrGraph, u32)`, `SymbolKind::Function`, `Finding`/`Severity`/`Confidence` are used consistently with their M1-M3 definitions.

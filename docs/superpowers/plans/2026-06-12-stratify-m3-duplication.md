# Stratify M3 (Duplication) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Detect duplicated code across files and languages by emitting a normalized token stream into the IR and running an exact-match clone detector on it. Like dead-code, the analysis reads only the IR, so Java and Ruby both get duplication detection for free.

**Architecture:** Extend the IR with a per-file normalized token stream that adapters emit while parsing. Normalization maps identifiers to `ID`, numbers to `NUM`, strings to `STR`, and keeps keywords/operators/punctuation literal, so renamed clones still match. A new `duplication` analysis interns the tokens, groups identical k-token windows, and reports left-maximal duplicated regions as findings.

**Tech Stack:** Rust, existing workspace crates, tree-sitter (already wired for Java and Ruby).

**Prerequisite reading:** the existing adapters `crates/stratify-lang-java/src/extract.rs` and `crates/stratify-lang-ruby/src/extract.rs` (you will add a leaf-walk to each). The M1 and M2 plans are in `docs/superpowers/plans/`.

---

## File Structure

```
crates/stratify-core/src/ir.rs           MODIFY: add Token type
crates/stratify-core/src/graph.rs         MODIFY: tokens storage (add_token, tokens(), merge concat)
crates/stratify-core/src/lib.rs           MODIFY: re-export Token
crates/stratify-lang-java/src/extract.rs  MODIFY: emit normalized tokens
crates/stratify-lang-ruby/src/extract.rs  MODIFY: emit normalized tokens
crates/stratify-analysis/src/duplication.rs CREATE: clone detector
crates/stratify-analysis/src/lib.rs       MODIFY: pub mod duplication
crates/stratify-cli/src/run.rs            MODIFY: run duplication in analyze_repo
crates/stratify-cli/tests/sample-dup/     CREATE: fixture with a clone
crates/stratify-cli/tests/e2e_dup.rs      CREATE: end-to-end duplication test
```

Tokens carry their `file` by value, so `merge` just concatenates them. No id remapping (tokens are independent of SymbolId).

---

## Task 1: Token type + IR storage (`stratify-core`)

**Files:**
- Modify: `crates/stratify-core/src/ir.rs`
- Modify: `crates/stratify-core/src/graph.rs`
- Modify: `crates/stratify-core/src/lib.rs`

- [ ] **Step 1: Add the Token type**

In `crates/stratify-core/src/ir.rs`, add:

```rust
/// A normalized source token used for duplication detection. `norm` is the
/// normalized class: "ID" for identifiers, "NUM" for numbers, "STR" for
/// strings, and the literal text for keywords/operators/punctuation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Token {
    pub file: String,
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_line: usize,
    pub norm: String,
}
```

- [ ] **Step 2: Add a failing test for token storage + merge**

In `crates/stratify-core/src/graph.rs` tests module, add:

```rust
    fn tok(file: &str, norm: &str) -> crate::ir::Token {
        crate::ir::Token {
            file: file.into(),
            start_byte: 0,
            end_byte: 1,
            start_line: 1,
            norm: norm.into(),
        }
    }

    #[test]
    fn add_and_read_tokens() {
        let mut g = IrGraph::new();
        g.add_token(tok("a.rb", "ID"));
        assert_eq!(g.tokens().len(), 1);
        assert_eq!(g.tokens()[0].norm, "ID");
    }

    #[test]
    fn merge_concatenates_tokens() {
        let mut g1 = IrGraph::new();
        g1.add_token(tok("a.rb", "if"));
        let mut g2 = IrGraph::new();
        g2.add_token(tok("b.rb", "ID"));
        g1.merge(g2);
        assert_eq!(g1.tokens().len(), 2);
    }
```

- [ ] **Step 3: Run, verify fail**

Run: `cargo test -p stratify-core` (if `cargo` missing: `source "$HOME/.cargo/env"`)
Expected: FAIL, no method `add_token`.

- [ ] **Step 4: Add storage to IrGraph**

In `graph.rs`, add `tokens: Vec<Token>` to the struct (import `use crate::ir::{Reference, Symbol, SymbolId, Token};`). Add methods:

```rust
    pub fn add_token(&mut self, token: Token) {
        self.tokens.push(token);
    }

    pub fn tokens(&self) -> &[Token] {
        &self.tokens
    }
```

In `merge`, after the entrypoints loop, concatenate tokens (no remap needed):

```rust
        self.tokens.extend(other.tokens);
```

- [ ] **Step 5: Re-export Token**

In `crates/stratify-core/src/lib.rs`, add `Token` to the `ir` re-export line so it reads:

```rust
pub use ir::{RefKind, Reference, Span, Symbol, SymbolId, SymbolKind, Token, Visibility};
```

- [ ] **Step 6: Run, verify pass**

Run: `cargo test -p stratify-core`
Expected: PASS (11 tests).

- [ ] **Step 7: Commit**

```bash
git add crates/stratify-core
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(core): normalized Token type and IR token stream"
```

---

## Task 2: Java adapter emits tokens (`stratify-lang-java`)

**Files:**
- Modify: `crates/stratify-lang-java/src/extract.rs`

- [ ] **Step 1: Add a failing test**

In `crates/stratify-lang-java/src/extract.rs` tests module, add:

```rust
    #[test]
    fn emits_normalized_tokens() {
        let src = "class A { int x = 5; }";
        let g = extract("A.java", src);
        let norms: Vec<&str> = g.tokens().iter().map(|t| t.norm.as_str()).collect();
        // identifiers normalized to ID, the literal 5 to NUM, keywords/punct literal.
        assert!(norms.contains(&"class"));
        assert!(norms.contains(&"ID"));   // A / int-name / x
        assert!(norms.contains(&"NUM"));  // 5
        assert!(norms.contains(&"{"));
    }
```

- [ ] **Step 2: Run, verify fail**

Run: `cargo test -p stratify-lang-java emits_normalized_tokens`
Expected: FAIL (tokens empty).

- [ ] **Step 3: Add a leaf-walk token emitter**

In `extract.rs`, add a function that walks every leaf node in document order and emits a normalized token. Java identifier/literal node kinds are mapped; everything else (keywords, operators, punctuation — which are anonymous leaf nodes) uses its literal text.

```rust
fn normalize_java(kind: &str, text: &str) -> String {
    match kind {
        "identifier" | "type_identifier" => "ID".to_string(),
        "decimal_integer_literal" | "hex_integer_literal" | "octal_integer_literal"
        | "binary_integer_literal" | "decimal_floating_point_literal"
        | "hex_floating_point_literal" => "NUM".to_string(),
        "string_literal" | "character_literal" => "STR".to_string(),
        _ => text.to_string(),
    }
}

fn emit_tokens(g: &mut IrGraph, file: &str, src: &str, root: Node) {
    let mut cursor = root.walk();
    let mut stack = vec![root];
    // Iterative preorder, collecting leaves in source order.
    let mut leaves: Vec<Node> = Vec::new();
    collect_leaves(root, &mut leaves);
    let _ = &mut cursor;
    for leaf in leaves {
        // skip zero-width or whitespace-only error nodes
        if leaf.start_byte() >= leaf.end_byte() {
            continue;
        }
        let text = text(leaf, src);
        if text.trim().is_empty() {
            continue;
        }
        let norm = normalize_java(leaf.kind(), text);
        g.add_token(Token {
            file: file.to_string(),
            start_byte: leaf.start_byte(),
            end_byte: leaf.end_byte(),
            start_line: leaf.start_position().row + 1,
            norm,
        });
    }
    let _ = stack.pop();
}

fn collect_leaves<'a>(node: Node<'a>, out: &mut Vec<Node<'a>>) {
    if node.child_count() == 0 {
        out.push(node);
        return;
    }
    let mut c = node.walk();
    for child in node.children(&mut c) {
        collect_leaves(child, out);
    }
}
```

Add the needed imports at the top of `extract.rs`: `use stratify_core::Token;` (and `Node` is already imported). Call `emit_tokens(&mut g, file, src, root);` inside `extract`, right after the File symbol is created (so tokens are emitted regardless of which definitions match). Make sure `g` is declared `mut` (it already is).

- [ ] **Step 4: Run, verify pass and existing java tests still pass**

Run: `cargo test -p stratify-lang-java`
Expected: PASS (6 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/stratify-lang-java
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(java): emit normalized token stream for duplication"
```

---

## Task 3: Ruby adapter emits tokens (`stratify-lang-ruby`)

**Files:**
- Modify: `crates/stratify-lang-ruby/src/extract.rs`

- [ ] **Step 1: Add a failing test**

In `crates/stratify-lang-ruby/src/extract.rs` tests module, add:

```rust
    #[test]
    fn emits_normalized_tokens() {
        let src = "def a\n  x = 5\nend\n";
        let g = extract("a.rb", src);
        let norms: Vec<&str> = g.tokens().iter().map(|t| t.norm.as_str()).collect();
        assert!(norms.contains(&"def"));
        assert!(norms.contains(&"ID"));   // a / x
        assert!(norms.contains(&"NUM"));  // 5
    }
```

- [ ] **Step 2: Run, verify fail**

Run: `cargo test -p stratify-lang-ruby emits_normalized_tokens`
Expected: FAIL.

- [ ] **Step 3: Add the Ruby leaf-walk emitter**

Mirror the Java emitter. Ruby normalization maps Ruby leaf kinds:

```rust
fn normalize_ruby(kind: &str, text: &str) -> String {
    match kind {
        "identifier" | "constant" | "instance_variable" | "global_variable"
        | "class_variable" => "ID".to_string(),
        "integer" | "float" => "NUM".to_string(),
        "string_content" | "string" | "simple_symbol" => "STR".to_string(),
        _ => text.to_string(),
    }
}
```

Add the same `collect_leaves` helper (copy it verbatim from the Java adapter — it is language-agnostic) and an `emit_tokens` that uses `normalize_ruby`. Import `use stratify_core::Token;`. Call `emit_tokens(&mut g, file, src, root);` in `extract` right after the File symbol is created.

To avoid duplicating `collect_leaves` logic conceptually in two crates without sharing, that is acceptable for now (small, language-agnostic, and the two adapter crates do not depend on each other). Do NOT create a new shared crate for this in M3.

- [ ] **Step 4: Run, verify pass**

Run: `cargo test -p stratify-lang-ruby`
Expected: PASS (7 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/stratify-lang-ruby
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(ruby): emit normalized token stream for duplication"
```

---

## Task 4: Duplication analysis (`stratify-analysis`)

**Files:**
- Create: `crates/stratify-analysis/src/duplication.rs`
- Modify: `crates/stratify-analysis/src/lib.rs`

- [ ] **Step 1: Write the analysis with tests on hand-built token graphs**

Create `crates/stratify-analysis/src/duplication.rs`:

```rust
use std::collections::HashMap;
use stratify_core::ir::Span;
use stratify_core::{Confidence, Finding, IrGraph, Severity};

/// Detect duplicated code as identical windows of `min_tokens` normalized
/// tokens. Reports one finding per left-maximal duplicated region, pointing at
/// another copy. Exact token-sequence match, so confidence is Certain.
pub fn analyze(graph: &IrGraph, min_tokens: usize) -> Vec<Finding> {
    let tokens = graph.tokens();
    let n = tokens.len();
    let k = min_tokens;
    if k == 0 || n < k {
        return Vec::new();
    }

    // Intern normalized token text to dense u32 ids.
    let mut interner: HashMap<&str, u32> = HashMap::new();
    let mut ids: Vec<u32> = Vec::with_capacity(n);
    for t in tokens {
        let next = interner.len() as u32;
        let id = *interner.entry(t.norm.as_str()).or_insert(next);
        ids.push(id);
    }

    // Group identical k-token windows by their exact content.
    let mut groups: HashMap<&[u32], Vec<usize>> = HashMap::new();
    for s in 0..=(n - k) {
        groups.entry(&ids[s..s + k]).or_default().push(s);
    }

    // duplicated[s] = the window starting at s appears at >= 2 positions.
    let mut duplicated = vec![false; n - k + 1];
    for starts in groups.values() {
        if starts.len() >= 2 {
            for &s in starts {
                duplicated[s] = true;
            }
        }
    }

    // Emit one finding per left-maximal duplicated region.
    let mut findings = Vec::new();
    for s in 0..duplicated.len() {
        if duplicated[s] && (s == 0 || !duplicated[s - 1]) {
            let group = &groups[&ids[s..s + k]];
            if let Some(&other) = group.iter().find(|&&o| o != s) {
                let here = &tokens[s];
                let there = &tokens[other];
                let last = &tokens[s + k - 1];
                findings.push(Finding {
                    rule: "duplication".into(),
                    severity: Severity::Warning,
                    message: format!(
                        "duplicated code block (>= {k} tokens) also at {}:{}",
                        there.file, there.start_line
                    ),
                    span: Span {
                        file: here.file.clone(),
                        start_byte: here.start_byte,
                        end_byte: last.end_byte,
                        start_line: here.start_line,
                    },
                    confidence: Confidence::Certain,
                });
            }
        }
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use stratify_core::ir::Token;

    fn push(g: &mut IrGraph, file: &str, norms: &[&str], base_line: usize) {
        for (i, nrm) in norms.iter().enumerate() {
            g.add_token(Token {
                file: file.into(),
                start_byte: i,
                end_byte: i + 1,
                start_line: base_line + i,
                norm: (*nrm).into(),
            });
        }
    }

    #[test]
    fn finds_a_cross_file_clone() {
        let mut g = IrGraph::new();
        let block = ["ID", "=", "ID", "+", "NUM", "ID"];
        push(&mut g, "a.rb", &block, 10);
        push(&mut g, "b.rb", &block, 20);
        let findings = analyze(&g, 5);
        assert!(!findings.is_empty());
        assert_eq!(findings[0].rule, "duplication");
        // The first region is in a.rb and points at b.rb.
        assert!(findings.iter().any(|f| f.span.file == "a.rb" && f.message.contains("b.rb")));
    }

    #[test]
    fn no_clone_when_unique() {
        let mut g = IrGraph::new();
        push(&mut g, "a.rb", &["ID", "=", "NUM"], 1);
        push(&mut g, "b.rb", &["ID", "+", "STR"], 1);
        assert!(analyze(&g, 5).is_empty());
    }

    #[test]
    fn ignores_blocks_shorter_than_min() {
        let mut g = IrGraph::new();
        let block = ["ID", "+", "ID"];
        push(&mut g, "a.rb", &block, 1);
        push(&mut g, "b.rb", &block, 1);
        // window of 5 over a 3-token block per file: each file alone is < k,
        // and the two files' tokens are not adjacent in a single 5-run, so no finding.
        assert!(analyze(&g, 5).is_empty());
    }
}
```

- [ ] **Step 2: Wire lib.rs**

In `crates/stratify-analysis/src/lib.rs`, add:

```rust
pub mod duplication;
```

- [ ] **Step 3: Run, verify pass**

Run: `cargo test -p stratify-analysis`
Expected: PASS (7 tests: 4 deadcode + 3 duplication).

- [ ] **Step 4: Commit**

```bash
git add crates/stratify-analysis
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(analysis): exact-match duplication detector over IR tokens"
```

---

## Task 5: Wire duplication into the CLI + end-to-end (`stratify-cli`)

**Files:**
- Modify: `crates/stratify-cli/src/run.rs`
- Create: `crates/stratify-cli/tests/sample-dup/one.rb`
- Create: `crates/stratify-cli/tests/sample-dup/two.rb`
- Create: `crates/stratify-cli/tests/e2e_dup.rs`

- [ ] **Step 1: Run duplication in analyze_repo**

In `crates/stratify-cli/src/run.rs`, import the analysis and run it alongside dead-code, concatenating findings. Find where `deadcode::analyze(&graph)` produces findings and the `Report::new(findings)` is built. Change to:

```rust
    let mut findings = stratify_analysis::deadcode::analyze(&graph);
    findings.extend(stratify_analysis::duplication::analyze(&graph, DUP_MIN_TOKENS));
    let report = Report::new(findings);
```

Add a module-level constant near the top of `run.rs`:

```rust
/// Minimum identical normalized-token run length to count as a clone.
const DUP_MIN_TOKENS: usize = 40;
```

(If `run.rs` currently imports `deadcode` via a `use`, keep using the path style that matches the existing file. The goal is: dead-code findings first, then duplication findings, in one Report.)

- [ ] **Step 2: Create a duplicated fixture**

Create `crates/stratify-cli/tests/sample-dup/one.rb`:

```ruby
def compute_total(items)
  total = 0
  items.each do |item|
    total = total + item.price * item.quantity
    total = total - item.discount
  end
  total
end
```

Create `crates/stratify-cli/tests/sample-dup/two.rb` with the same body under a different method name (a renamed clone, which normalization still catches):

```ruby
def sum_amounts(records)
  total = 0
  records.each do |record|
    total = total + record.price * record.quantity
    total = total - record.discount
  end
  total
end
```

- [ ] **Step 2b: Confirm the fixture exceeds the threshold**

These two method bodies share well over 40 normalized tokens (the bodies are token-identical after ID/NUM normalization; only the method and parameter names differ, and those are all `ID`). If, when you run Step 4, no duplication is reported, the fixture is too short for `DUP_MIN_TOKENS = 40`. In that case, lower the e2e's expectation by NOT changing the constant but instead making the fixture longer (duplicate the inner two `total = ...` lines once more in BOTH files so the shared run clearly exceeds 40 tokens). Do not lower `DUP_MIN_TOKENS`; keep the product default realistic.

- [ ] **Step 3: Write the end-to-end test**

Create `crates/stratify-cli/tests/e2e_dup.rs`:

```rust
use std::path::Path;

#[test]
fn sample_dup_reports_duplication() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sample-dup");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_stratify"))
        .arg("check")
        .arg(&dir)
        .arg("--format")
        .arg("json")
        .output()
        .expect("run stratify binary");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("\"rule\": \"duplication\""), "stdout: {stdout}");
    // The clone spans the two files.
    assert!(stdout.contains("one.rb") && stdout.contains("two.rb"), "stdout: {stdout}");
}
```

- [ ] **Step 4: Run the CLI suite + manual smoke**

Run: `cargo test -p stratify-cli`
Expected: PASS (including the new duplication e2e). If duplication is not detected, apply Step 2b's fix (lengthen the fixture), not a constant change.

Manual check:
```bash
cargo build
./target/debug/stratify check crates/stratify-cli/tests/sample-dup
```
Expected: a `warn ... duplicated code block ...` finding referencing the other file.

- [ ] **Step 5: Commit**

```bash
git add crates/stratify-cli
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(cli): run duplication analysis, end-to-end clone detection"
```

---

## Task 6: fmt, clippy, lockfile

**Files:**
- Modify: generated `Cargo.lock`, any fmt changes

- [ ] **Step 1: Format and lint**

Run: `cargo fmt --all && cargo clippy --all-targets -- -D warnings`
Fix any warning properly (no blanket `#[allow]`). A likely clippy note: `needless_range_loop` on the `for s in 0..duplicated.len()` loop. If clippy flags it, rewrite as `for (s, &is_dup) in duplicated.iter().enumerate()` and use `is_dup` plus `duplicated[s-1]`, keeping behavior identical. Re-run until clean.

- [ ] **Step 2: Full suite**

Run: `cargo test`
Expected: all crates green (core 11, java 6, ruby 7, analysis 7, cli incl. 3 e2e + gate, lang 1, report 3).

- [ ] **Step 3: Commit**

```bash
git add -A
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "chore: fmt, clippy clean, update lockfile for duplication"
```

---

## Self-Review Notes

Spec coverage for M3 (duplication slice):
- IR token stream: Task 1. Covered.
- Both adapters emit normalized tokens (renamed-clone safe): Tasks 2, 3. Covered.
- Language-agnostic duplication analysis reading only the IR: Task 4. Covered.
- CLI integration + cross-file e2e: Task 5. Covered.

Deferred (correctly out of this slice): complexity + hotspots (now M4), maximal-region length reporting beyond the fixed window start (the finding reports the region start and a single partner; a richer report with exact clone length and all copies is a later refinement), suffix-array replacement of the exact-window grouping (the current grouping is exact and correct, just O(n*k) memory in window slices; fine at current scale), and cross-language clones (tokens are language-tagged only by file; identical normalized sequences across Java and Ruby would match, which is acceptable and rare).

Known M3 characteristics (acceptable):
- The detector reports exact normalized-token matches of length >= DUP_MIN_TOKENS (40 in the CLI). Confidence is Certain because the match is exact.
- A 3+ way clone yields a finding at each copy's region start, each pointing at one other copy. Bounded by region starts, so no quadratic flood.
- Normalization makes type-2 (renamed) clones match; it will not match type-3 (gapped) clones. That is the intended scope.

Type consistency: `IrGraph::add_token`/`tokens`, `Token` fields (`file`, `start_byte`, `end_byte`, `start_line`, `norm`), `duplication::analyze(&IrGraph, usize)`, `Finding`/`Severity`/`Confidence`/`Span` are used consistently with their M1/M2 definitions.

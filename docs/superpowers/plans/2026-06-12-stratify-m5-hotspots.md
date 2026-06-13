# Stratify M5 (Hotspots) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rank risk by crossing per-function complexity (already in the IR) with git churn (how often a file changes). Report hotspots: complex code that also changes a lot.

**Architecture:** The hotspot analysis stays pure: it reads the IR and a churn map (`file -> commit count`) passed in. The git side-effect lives in the CLI, which computes churn from `git log` and normalizes churn's file paths to the same scan-root-relative strings the IR uses. No IR change is needed.

**Tech Stack:** Rust, existing workspace crates, the `git` CLI (invoked from `stratify-cli`).

**Prerequisite reading:** `crates/stratify-analysis/src/complexity.rs` (the hotspot analysis mirrors its shape), `crates/stratify-cli/src/run.rs` (where `analyze_repo` builds the graph; note the IR file strings come from `path.strip_prefix(root)`, so churn keys must match that).

---

## File Structure

```
crates/stratify-analysis/src/hotspot.rs   CREATE: complexity x churn analysis (pure)
crates/stratify-analysis/src/lib.rs       MODIFY: pub mod hotspot
crates/stratify-cli/src/churn.rs          CREATE: git_churn(root) -> HashMap<String,u32>
crates/stratify-cli/src/main.rs           MODIFY: mod churn;
crates/stratify-cli/src/run.rs            MODIFY: run hotspot with computed churn
crates/stratify-cli/tests/e2e_hotspot.rs  CREATE: hermetic temp-git-repo end-to-end test
```

---

## Task 1: Hotspot analysis (`stratify-analysis`)

**Files:**
- Create: `crates/stratify-analysis/src/hotspot.rs`
- Modify: `crates/stratify-analysis/src/lib.rs`

- [ ] **Step 1: Write the analysis with tests on synthetic churn**

Create `crates/stratify-analysis/src/hotspot.rs`:

```rust
use std::collections::HashMap;
use stratify_core::{Confidence, Finding, IrGraph, Severity, SymbolKind};

/// Hotspot = function complexity x churn of its file. Flags functions whose
/// score exceeds `threshold`. Churn is supplied by the caller (the CLI reads
/// it from git), keyed by the same file string the IR uses in spans.
pub fn analyze(
    graph: &IrGraph,
    churn: &HashMap<String, u32>,
    threshold: u32,
) -> Vec<Finding> {
    let mut findings = Vec::new();
    for s in graph.symbols() {
        if !matches!(s.kind, SymbolKind::Function) {
            continue;
        }
        let Some(cx) = graph.complexity_of(s.id) else {
            continue;
        };
        let ch = churn.get(&s.span.file).copied().unwrap_or(0);
        let score = cx.saturating_mul(ch);
        if score <= threshold {
            continue;
        }
        findings.push(Finding {
            rule: "hotspot".into(),
            severity: Severity::Warning,
            message: format!(
                "hotspot: `{}` complexity {cx} x churn {ch} = {score}",
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

    fn func(g: &mut IrGraph, name: &str, file: &str, cx: u32) -> SymbolId {
        let id = g.add_symbol(Symbol {
            id: SymbolId(0),
            kind: SymbolKind::Function,
            name: name.into(),
            fqn: name.into(),
            span: Span { file: file.into(), start_byte: 0, end_byte: 1, start_line: 1 },
            visibility: Visibility::Unknown,
            confidence: Confidence::Certain,
        });
        g.set_complexity(id, cx);
        id
    }

    #[test]
    fn flags_complex_and_churny() {
        let mut g = IrGraph::new();
        func(&mut g, "hot", "a.rb", 11);
        func(&mut g, "calm", "b.rb", 11);
        let mut churn = HashMap::new();
        churn.insert("a.rb".to_string(), 6); // 11*6 = 66 > 50
        churn.insert("b.rb".to_string(), 1); // 11*1 = 11 <= 50
        let findings = analyze(&g, &churn, 50);
        assert_eq!(findings.len(), 1);
        assert!(findings[0].message.contains("hot"));
        assert!(findings[0].message.contains("66"));
    }

    #[test]
    fn no_hotspot_without_churn() {
        let mut g = IrGraph::new();
        func(&mut g, "complex", "a.rb", 30);
        let churn = HashMap::new(); // no churn data -> score 0
        assert!(analyze(&g, &churn, 50).is_empty());
    }

    #[test]
    fn function_without_complexity_is_skipped() {
        let mut g = IrGraph::new();
        // a function with no recorded complexity
        g.add_symbol(Symbol {
            id: SymbolId(0),
            kind: SymbolKind::Function,
            name: "x".into(),
            fqn: "x".into(),
            span: Span { file: "a.rb".into(), start_byte: 0, end_byte: 1, start_line: 1 },
            visibility: Visibility::Unknown,
            confidence: Confidence::Certain,
        });
        let mut churn = HashMap::new();
        churn.insert("a.rb".to_string(), 100);
        assert!(analyze(&g, &churn, 50).is_empty());
    }
}
```

- [ ] **Step 2: Wire lib.rs**

In `crates/stratify-analysis/src/lib.rs`, add:

```rust
pub mod hotspot;
```

- [ ] **Step 3: Run, verify pass**

Run: `cargo test -p stratify-analysis` (if `cargo` missing: `source "$HOME/.cargo/env"`)
Expected: PASS (14 tests: 4 deadcode + 4 duplication + 3 complexity + 3 hotspot).

- [ ] **Step 4: Commit**

```bash
git add crates/stratify-analysis
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(analysis): hotspot detection (complexity x churn)"
```

---

## Task 2: Git churn computation (`stratify-cli`)

**Files:**
- Create: `crates/stratify-cli/src/churn.rs`
- Modify: `crates/stratify-cli/src/main.rs`

The IR's file strings are paths relative to the scan `root` (from `path.strip_prefix(root)` in `run.rs`). The churn map MUST use the same keys. Git reports paths relative to the git root, so we convert: make each git path absolute under the git root, then strip the canonicalized scan root.

- [ ] **Step 1: Write the churn module with a hermetic temp-repo test**

Create `crates/stratify-cli/src/churn.rs`:

```rust
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

/// Count how many commits touched each file, keyed by path relative to `root`
/// (matching the IR's file strings). Returns an empty map if `root` is not in
/// a git repository or git is unavailable. Best-effort: never panics.
pub fn git_churn(root: &Path) -> HashMap<String, u32> {
    let mut churn = HashMap::new();

    let toplevel = match Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--show-toplevel"])
        .output()
    {
        Ok(o) if o.status.success() => {
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        }
        _ => return churn,
    };
    let git_root = Path::new(&toplevel);

    let root_abs = match root.canonicalize() {
        Ok(p) => p,
        Err(_) => return churn,
    };

    let out = match Command::new("git")
        .arg("-C")
        .arg(git_root)
        .args(["log", "--format=", "--name-only"])
        .output()
    {
        Ok(o) if o.status.success() => o.stdout,
        _ => return churn,
    };

    let text = String::from_utf8_lossy(&out);
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let abs = git_root.join(line);
        if let Ok(rel) = abs.strip_prefix(&root_abs) {
            let key = rel.to_string_lossy().to_string();
            *churn.entry(key).or_insert(0) += 1;
        }
    }
    churn
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed");
    }

    #[test]
    fn counts_commits_per_file() {
        // Hermetic temp repo. Commit a file three times.
        let dir = std::env::temp_dir().join("stratify-churn-test-1");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q"]);
        git(&dir, &["config", "user.email", "t@example.com"]);
        git(&dir, &["config", "user.name", "Test"]);

        for i in 0..3 {
            std::fs::write(dir.join("foo.rb"), format!("def m\n  {i}\nend\n")).unwrap();
            git(&dir, &["add", "foo.rb"]);
            git(&dir, &["commit", "-q", "-m", "change"]);
        }
        // A second file committed once.
        std::fs::write(dir.join("bar.rb"), "def b\nend\n").unwrap();
        git(&dir, &["add", "bar.rb"]);
        git(&dir, &["commit", "-q", "-m", "add bar"]);

        let churn = git_churn(&dir);
        assert_eq!(churn.get("foo.rb"), Some(&3));
        assert_eq!(churn.get("bar.rb"), Some(&1));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_when_not_a_repo() {
        let dir = std::env::temp_dir().join("stratify-churn-test-not-a-repo");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(git_churn(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
```

- [ ] **Step 2: Declare the module**

In `crates/stratify-cli/src/main.rs`, add `mod churn;` near the existing `mod run;`.

- [ ] **Step 3: Run, verify pass**

Run: `cargo test -p stratify-cli churn`
Expected: PASS (2 churn tests). Note: these tests run real `git`; that is fine in this environment.

- [ ] **Step 4: Commit**

```bash
git add crates/stratify-cli
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(cli): git churn computation keyed to scan-relative paths"
```

---

## Task 3: Wire hotspots into the CLI + end-to-end (`stratify-cli`)

**Files:**
- Modify: `crates/stratify-cli/src/run.rs`
- Create: `crates/stratify-cli/tests/e2e_hotspot.rs`

- [ ] **Step 1: Run hotspot in analyze_repo**

In `crates/stratify-cli/src/run.rs`, add a constant near the others:

```rust
/// complexity x churn above this is reported as a hotspot.
const HOTSPOT_THRESHOLD: u32 = 50;
```

`analyze_repo` already receives `root: &Path`. After the complexity line, add:

```rust
    let churn = crate::churn::git_churn(root);
    findings.extend(stratify_analysis::hotspot::analyze(&graph, &churn, HOTSPOT_THRESHOLD));
```

(`crate::churn` resolves because `mod churn;` is declared in `main.rs`. If `run.rs` is compiled as part of the same binary crate, `crate::churn::git_churn` is correct.)

- [ ] **Step 2: Write the hermetic end-to-end test**

This test builds a temp git repo with a high-complexity Ruby file committed enough times that complexity (11) x churn (>= 6) exceeds the threshold (50), then runs the built binary on it.

Create `crates/stratify-cli/tests/e2e_hotspot.rs`:

```rust
use std::path::Path;
use std::process::Command;

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

const GNARLY: &str = r#"def classify(n)
  if n < 0
    return "a"
  elsif n < 1
    return "b"
  elsif n < 2
    return "c"
  elsif n < 3
    return "d"
  elsif n < 4
    return "e"
  elsif n < 5
    return "f"
  elsif n < 6
    return "g"
  elsif n < 7
    return "h"
  elsif n < 8
    return "i"
  else
    return "z"
  end
end

classify(5)
"#;

#[test]
fn high_complexity_high_churn_is_a_hotspot() {
    let dir = std::env::temp_dir().join("stratify-hotspot-e2e");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q"]);
    git(&dir, &["config", "user.email", "t@example.com"]);
    git(&dir, &["config", "user.name", "Test"]);

    // Commit the same complex file 6 times -> churn 6, complexity 11 -> 66 > 50.
    for i in 0..6 {
        std::fs::write(dir.join("classify.rb"), format!("{GNARLY}# rev {i}\n")).unwrap();
        git(&dir, &["add", "classify.rb"]);
        git(&dir, &["commit", "-q", "-m", "change"]);
    }

    let output = Command::new(env!("CARGO_BIN_EXE_stratify"))
        .arg("check")
        .arg(&dir)
        .arg("--format")
        .arg("json")
        .output()
        .expect("run stratify binary");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("\"rule\": \"hotspot\""), "stdout: {stdout}");
    assert!(stdout.contains("classify"), "stdout: {stdout}");

    let _ = std::fs::remove_dir_all(&dir);
}
```

- [ ] **Step 3: Run + manual smoke**

Run: `cargo test -p stratify-cli`
Expected: PASS (including `e2e_hotspot`). If the hotspot does not fire, check: (a) `classify` complexity is 11 (1 if + 8 elsif), (b) churn is 6, (c) the constant is 50. Do NOT mask a real wiring bug by lowering the threshold; report it.

Manual (does not assert, just shows behavior): the regular fixtures dir likely shows no hotspot because the fixtures have low churn:
```bash
cargo build
./target/debug/stratify check crates/stratify-cli/tests --format json | grep -o '"rule": "[a-z_]*"' | sort | uniq -c
```
Expected: dead_code, duplication, complexity counts (hotspot may be 0 here, which is fine — the fixtures have little history).

- [ ] **Step 4: Commit**

```bash
git add crates/stratify-cli
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(cli): run hotspot analysis, hermetic end-to-end test"
```

---

## Task 4: fmt, clippy, lockfile

- [ ] **Step 1: Format and lint**

Run: `cargo fmt --all && cargo clippy --all-targets -- -D warnings`
Fix any warning properly (no blanket `#[allow]`). Re-run until clean. Re-run `cargo test` after any fix.

- [ ] **Step 2: Full suite**

Run: `cargo test`
Expected: all crates green (core 13, java 7, ruby 8, analysis 14, cli incl. churn + 5 e2e + gate, lang 1, report 3).

- [ ] **Step 3: Commit**

```bash
git add -A
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "chore: fmt, clippy clean, update lockfile for hotspots"
```

---

## Self-Review Notes

Spec coverage for M5:
- Pure hotspot analysis (complexity x churn) reading the IR + a churn map: Task 1. Covered.
- Git churn computation with scan-relative path keys matching the IR: Task 2. Covered.
- CLI integration + hermetic end-to-end: Task 3. Covered.

Deferred (correctly out of M5): architecture boundaries (M6), SARIF/CI output (M7), MCP (M8), LSP (M9). Cross-file call resolution remains a known limitation. Churn weighting refinements (recency-weighting, lines-changed instead of commit-count) are later refinements.

Known M5 characteristics (acceptable):
- Churn is commit-count per file (how many commits touched it), not lines changed. Simple and robust.
- A file renamed in history is counted under its current path only (git log without `--follow`), so churn may undercount across renames. Acceptable for M5.
- If the scan root is not in a git repo, churn is empty and no hotspots are reported (the other analyses still run). This is the correct graceful degradation.
- The path-matching relies on the IR's file strings being `path.strip_prefix(root)` and churn keys being `git_path` made absolute then stripped of the canonical scan root. Both reduce to the file's path within the scan directory, so they match. The churn unit test and the hotspot e2e together verify this end to end.

Type consistency: `hotspot::analyze(&IrGraph, &HashMap<String,u32>, u32)`, `churn::git_churn(&Path) -> HashMap<String,u32>`, `graph.complexity_of`, `SymbolKind::Function`, `Finding`/`Severity`/`Confidence` are used consistently with M1-M4 definitions.

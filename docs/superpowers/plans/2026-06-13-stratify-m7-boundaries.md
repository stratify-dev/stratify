# Stratify M7 (Layer Boundaries) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Enforce architecture layer rules. A `stratify.toml` assigns files to layers by path glob and declares forbidden imports (e.g. models must not import controllers). A language-agnostic analysis flags violating edges in the cross-file import graph.

**Architecture:** Reuse the M6 import graph. First extract the file-level import-graph builder out of `cycles.rs` into a shared `imports.rs` (so cycles and boundaries share it, DRY). Then add a `boundaries` analysis that classifies each file into a layer (first matching glob, in config order) and reports any import edge whose (source layer, target layer) pair is forbidden. The CLI reads `stratify.toml` from the scan root and parses it into the config the analysis consumes; with no config, the analysis reports nothing.

**Tech Stack:** Rust, existing crates, plus `globset` (glob matching, in `stratify-analysis`), `toml` + `serde` (config parsing). 

**Prerequisite reading:** `crates/stratify-analysis/src/cycles.rs` (you extract its graph builder), `crates/stratify-cli/src/run.rs` (where config is read and analyses run).

---

## File Structure

```
crates/stratify-analysis/Cargo.toml        MODIFY: add serde, globset deps
crates/stratify-analysis/src/imports.rs     CREATE: shared file_import_graph + file_spans
crates/stratify-analysis/src/cycles.rs      MODIFY: use the shared imports helper
crates/stratify-analysis/src/boundaries.rs  CREATE: BoundaryConfig + layer-rule analysis
crates/stratify-analysis/src/lib.rs         MODIFY: pub mod imports; pub mod boundaries;
crates/stratify-cli/Cargo.toml              MODIFY: add toml dep
crates/stratify-cli/src/run.rs              MODIFY: read stratify.toml, run boundaries
crates/stratify-cli/tests/sample-boundary/  CREATE: stratify.toml + layered ruby files
crates/stratify-cli/tests/e2e_boundary.rs   CREATE: end-to-end boundary test
Cargo.toml                                   MODIFY: workspace deps globset, toml
```

---

## Task 1: Extract the shared import-graph builder (`stratify-analysis`)

**Files:**
- Create: `crates/stratify-analysis/src/imports.rs`
- Modify: `crates/stratify-analysis/src/cycles.rs`
- Modify: `crates/stratify-analysis/src/lib.rs`

- [ ] **Step 1: Create the shared module**

Create `crates/stratify-analysis/src/imports.rs`:

```rust
use std::collections::{BTreeMap, BTreeSet, HashMap};
use stratify_core::ir::Span;
use stratify_core::{IrGraph, RefKind, SymbolKind};

/// Build the file-level import graph: each file maps to the set of files it
/// imports. An `Imports` edge (File -> Dependency) resolves to a file edge when
/// the Dependency's name (import key) equals some File/Class/Module fqn (export
/// key). Every File symbol appears as a key (possibly with an empty set).
/// Self-edges are excluded.
pub fn file_import_graph(graph: &IrGraph) -> BTreeMap<String, BTreeSet<String>> {
    let mut export: HashMap<&str, String> = HashMap::new();
    for s in graph.symbols() {
        if matches!(s.kind, SymbolKind::File | SymbolKind::Class | SymbolKind::Module) {
            export.entry(s.fqn.as_str()).or_insert_with(|| s.span.file.clone());
        }
    }

    let mut adj: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for s in graph.symbols() {
        if matches!(s.kind, SymbolKind::File) {
            adj.entry(s.span.file.clone()).or_default();
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
    adj
}

/// Map each file to a representative span (its File symbol's span).
pub fn file_spans(graph: &IrGraph) -> HashMap<String, Span> {
    let mut spans = HashMap::new();
    for s in graph.symbols() {
        if matches!(s.kind, SymbolKind::File) {
            spans.entry(s.span.file.clone()).or_insert_with(|| s.span.clone());
        }
    }
    spans
}
```

- [ ] **Step 2: Refactor cycles.rs to use it**

In `crates/stratify-analysis/src/cycles.rs`, replace the inline `export`/`adj`/`span_of` construction at the top of `analyze` with calls to the shared helpers:

```rust
    let adj = crate::imports::file_import_graph(graph);
    let span_of = crate::imports::file_spans(graph);
```

Delete the now-unused inline building of `export`, `adj`, and `span_of` (and any now-unused imports like `RefKind`, `SymbolKind`, `HashMap` if they are no longer referenced in cycles.rs). Keep the DFS, `canonical_cycle`, and finding emission exactly as they are. The four existing cycle tests must still pass unchanged.

- [ ] **Step 3: Wire lib.rs**

In `crates/stratify-analysis/src/lib.rs`, add (before `pub mod cycles;`):

```rust
pub mod imports;
```

- [ ] **Step 4: Run, verify pass**

Run: `cargo test -p stratify-analysis` (if `cargo` missing: `source "$HOME/.cargo/env"`)
Expected: PASS (18 tests, unchanged). `cargo build` warning-free (fix any now-unused-import warnings in cycles.rs).

- [ ] **Step 5: Commit**

```bash
git add crates/stratify-analysis
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "refactor(analysis): extract shared file import-graph builder"
```

---

## Task 2: Boundary config + analysis (`stratify-analysis`)

**Files:**
- Modify: `crates/stratify-analysis/Cargo.toml`
- Modify: `Cargo.toml` (workspace deps)
- Create: `crates/stratify-analysis/src/boundaries.rs`
- Modify: `crates/stratify-analysis/src/lib.rs`

- [ ] **Step 1: Add dependencies**

In the workspace root `Cargo.toml` `[workspace.dependencies]`, add:

```toml
globset = "0.4"
toml = "0.8"
```

In `crates/stratify-analysis/Cargo.toml` `[dependencies]`, add:

```toml
serde = { workspace = true }
globset = { workspace = true }
```

- [ ] **Step 2: Write the config types and analysis with tests**

Create `crates/stratify-analysis/src/boundaries.rs`:

```rust
use std::collections::HashSet;
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use serde::Deserialize;
use stratify_core::ir::Span;
use stratify_core::{Confidence, Finding, IrGraph, Severity};

/// Layer-boundary configuration, parsed from `stratify.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct BoundaryConfig {
    /// Layer name -> glob patterns matching files in that layer.
    #[serde(default)]
    pub layers: std::collections::BTreeMap<String, Vec<String>>,
    /// Forbidden import rules.
    #[serde(default)]
    pub forbid: Vec<ForbidRule>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ForbidRule {
    pub from: String,
    pub to: String,
}

/// Compile each layer's globs into a GlobSet (path separators are literal, so
/// `*` does not cross `/` but `**` does). Bad patterns are skipped.
fn compile_layers(config: &BoundaryConfig) -> Vec<(String, GlobSet)> {
    let mut out = Vec::new();
    for (layer, patterns) in &config.layers {
        let mut b = GlobSetBuilder::new();
        for p in patterns {
            if let Ok(g) = GlobBuilder::new(p).literal_separator(true).build() {
                b.add(g);
            }
        }
        if let Ok(set) = b.build() {
            out.push((layer.clone(), set));
        }
    }
    out
}

/// Classify a file path into the first matching layer (config order).
fn classify<'a>(file: &str, layers: &'a [(String, GlobSet)]) -> Option<&'a str> {
    layers
        .iter()
        .find(|(_, set)| set.is_match(file))
        .map(|(name, _)| name.as_str())
}

/// Report import edges that cross a forbidden layer boundary.
pub fn analyze(graph: &IrGraph, config: &BoundaryConfig) -> Vec<Finding> {
    if config.forbid.is_empty() {
        return Vec::new();
    }
    let layers = compile_layers(config);
    let forbidden: HashSet<(String, String)> = config
        .forbid
        .iter()
        .map(|r| (r.from.clone(), r.to.clone()))
        .collect();

    let adj = crate::imports::file_import_graph(graph);
    let span_of = crate::imports::file_spans(graph);

    let mut findings = Vec::new();
    for (src, targets) in &adj {
        let Some(src_layer) = classify(src, &layers) else {
            continue;
        };
        for tgt in targets {
            let Some(tgt_layer) = classify(tgt, &layers) else {
                continue;
            };
            if forbidden.contains(&(src_layer.to_string(), tgt_layer.to_string())) {
                let span = span_of.get(src).cloned().unwrap_or(Span {
                    file: src.clone(),
                    start_byte: 0,
                    end_byte: 0,
                    start_line: 1,
                });
                findings.push(Finding {
                    rule: "boundary".into(),
                    severity: Severity::Warning,
                    message: format!(
                        "layer `{src_layer}` must not import `{tgt_layer}` ({src} -> {tgt})"
                    ),
                    span,
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
    use stratify_core::ir::{Reference, Symbol, SymbolId, Visibility};
    use stratify_core::{RefKind, SymbolKind};

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

    fn import(g: &mut IrGraph, from: SymbolId, key: &str) {
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

    fn config() -> BoundaryConfig {
        let mut layers = std::collections::BTreeMap::new();
        layers.insert("models".to_string(), vec!["models/**".to_string()]);
        layers.insert("controllers".to_string(), vec!["controllers/**".to_string()]);
        BoundaryConfig {
            layers,
            forbid: vec![ForbidRule { from: "models".into(), to: "controllers".into() }],
        }
    }

    #[test]
    fn glob_classifies_nested_file() {
        let layers = compile_layers(&config());
        assert_eq!(classify("models/user.rb", &layers), Some("models"));
        assert_eq!(classify("controllers/users.rb", &layers), Some("controllers"));
        assert_eq!(classify("lib/util.rb", &layers), None);
    }

    #[test]
    fn flags_forbidden_edge() {
        let mut g = IrGraph::new();
        let m = file_sym(&mut g, "models/user.rb");
        file_sym(&mut g, "controllers/users.rb");
        import(&mut g, m, "controllers/users.rb"); // models -> controllers (forbidden)
        let findings = analyze(&g, &config());
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].rule, "boundary");
        assert!(findings[0].message.contains("models"));
        assert!(findings[0].message.contains("controllers"));
    }

    #[test]
    fn allows_reverse_direction() {
        let mut g = IrGraph::new();
        file_sym(&mut g, "models/user.rb");
        let c = file_sym(&mut g, "controllers/users.rb");
        import(&mut g, c, "models/user.rb"); // controllers -> models (allowed)
        assert!(analyze(&g, &config()).is_empty());
    }

    #[test]
    fn no_config_no_findings() {
        let mut g = IrGraph::new();
        let m = file_sym(&mut g, "models/user.rb");
        file_sym(&mut g, "controllers/users.rb");
        import(&mut g, m, "controllers/users.rb");
        assert!(analyze(&g, &BoundaryConfig::default()).is_empty());
    }
}
```

- [ ] **Step 3: Wire lib.rs**

In `crates/stratify-analysis/src/lib.rs`, add:

```rust
pub mod boundaries;
```

- [ ] **Step 4: Run, verify pass**

Run: `cargo test -p stratify-analysis`
Expected: PASS (22 tests: 18 prior + 4 boundary). If `glob_classifies_nested_file` fails because `models/**` does not match `models/user.rb` under `literal_separator(true)`, adjust the glob handling so a `dir/**` pattern matches files directly under `dir` (e.g. also test the pattern `models/**/*` or strip a trailing `/**` to a prefix check) AND keep the test's expectation (`models/user.rb` -> `models`). The unit test is the oracle; make classification actually work. Report any adjustment.

- [ ] **Step 5: Commit**

```bash
git add crates/stratify-analysis Cargo.toml
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(analysis): layer-boundary rule analysis with glob layers"
```

---

## Task 3: CLI config + end-to-end (`stratify-cli`)

**Files:**
- Modify: `crates/stratify-cli/Cargo.toml`
- Modify: `crates/stratify-cli/src/run.rs`
- Create: `crates/stratify-cli/tests/sample-boundary/stratify.toml`
- Create: `crates/stratify-cli/tests/sample-boundary/models/user.rb`
- Create: `crates/stratify-cli/tests/sample-boundary/controllers/users_controller.rb`
- Create: `crates/stratify-cli/tests/e2e_boundary.rs`

- [ ] **Step 1: Add the toml dependency**

In `crates/stratify-cli/Cargo.toml` `[dependencies]`, add:

```toml
toml = { workspace = true }
```

- [ ] **Step 2: Read stratify.toml and run boundaries**

In `crates/stratify-cli/src/run.rs`, add a helper to load config:

```rust
fn load_boundary_config(root: &std::path::Path) -> stratify_analysis::boundaries::BoundaryConfig {
    let path = root.join("stratify.toml");
    match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).unwrap_or_default(),
        Err(_) => stratify_analysis::boundaries::BoundaryConfig::default(),
    }
}
```

In `analyze_repo`, after the cycles line, add:

```rust
    let boundary_config = load_boundary_config(root);
    findings.extend(stratify_analysis::boundaries::analyze(&graph, &boundary_config));
```

- [ ] **Step 3: Create the fixture**

Create `crates/stratify-cli/tests/sample-boundary/stratify.toml`:

```toml
[layers]
models = ["models/**"]
controllers = ["controllers/**"]

[[forbid]]
from = "models"
to = "controllers"
```

Create `crates/stratify-cli/tests/sample-boundary/models/user.rb`:

```ruby
require_relative "../controllers/users_controller"

def user_name
  "alice"
end
```

Create `crates/stratify-cli/tests/sample-boundary/controllers/users_controller.rb`:

```ruby
def show_user
  "showing"
end
```

`models/user.rb` requires `../controllers/users_controller` -> resolves to key `controllers/users_controller.rb`, matching that File's fqn. The edge `models/user.rb -> controllers/users_controller.rb` crosses models -> controllers, which is forbidden.

- [ ] **Step 4: Write the end-to-end test**

Create `crates/stratify-cli/tests/e2e_boundary.rs`:

```rust
use std::path::Path;

#[test]
fn sample_boundary_reports_violation() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sample-boundary");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_stratify"))
        .arg("check")
        .arg(&dir)
        .arg("--format")
        .arg("json")
        .output()
        .expect("run stratify binary");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("\"rule\": \"boundary\""), "stdout: {stdout}");
    assert!(stdout.contains("models") && stdout.contains("controllers"), "stdout: {stdout}");
}
```

- [ ] **Step 5: Run + manual smoke**

Run: `cargo test -p stratify-cli`
Expected: PASS including `e2e_boundary`. If no boundary finding, verify: (a) `stratify.toml` is read from the scan root, (b) the require_relative resolves to `controllers/users_controller.rb`, (c) the globs classify `models/user.rb` -> models and `controllers/users_controller.rb` -> controllers. Report a real bug rather than masking it.

Manual:
```bash
cargo build
./target/debug/stratify check crates/stratify-cli/tests/sample-boundary
```
Expected: a `warn ... layer `models` must not import `controllers` (models/user.rb -> controllers/users_controller.rb)` finding.

- [ ] **Step 6: Commit**

```bash
git add crates/stratify-cli
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(cli): read stratify.toml, run layer-boundary analysis end to end"
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
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "chore: fmt, clippy clean, update lockfile for boundaries"
```

---

## Self-Review Notes

Spec coverage for M7 (layer-boundary slice of architecture boundaries):
- Shared import-graph builder (DRY across cycles + boundaries): Task 1. Covered.
- `stratify.toml` layer + forbid config, glob classification, forbidden-edge detection: Task 2. Covered.
- CLI config read + e2e: Task 3. Covered.

Deferred (correctly out of M7): zero-config presets (auto-detect Rails/Maven layouts) are a later refinement; M7 requires an explicit `stratify.toml`. Allow-lists (only X may import Y) and per-layer visibility beyond simple forbid pairs are later refinements. Cross-file CALL resolution remains the open cross-cutting limitation.

Known M7 characteristics (acceptable):
- A file matches the FIRST layer (in config order, which is BTreeMap key order = alphabetical) whose glob set matches. Overlapping layer globs resolve by that order. Documented behavior.
- Only resolvable import edges are checked (same as cycles); imports of external/unresolved targets are ignored.
- No `stratify.toml`, or one with no `forbid` rules, yields zero boundary findings; the other five analyses still run. Correct graceful default.
- Glob semantics use `literal_separator(true)` so `*` stays within a path component and `**` crosses; if the chosen glob crate behaves differently for `dir/**` against a direct child, Task 2 Step 4 directs making classification match the unit-test oracle.

Type consistency: `boundaries::analyze(&IrGraph, &BoundaryConfig)`, `BoundaryConfig`/`ForbidRule`, `imports::file_import_graph`/`file_spans`, `Finding`/`Severity`/`Confidence`/`Span` are used consistently with M1-M6 definitions.

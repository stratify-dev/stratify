# Stratify M8 (SARIF Output) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Emit findings as SARIF 2.1.0 so GitHub code scanning and GitLab render them as native inline annotations. Add `stratify check --format sarif`.

**Architecture:** A new `sarif` renderer in `stratify-report` builds a typed SARIF 2.1.0 document from the existing `Report` and serializes it. The CLI gains a `sarif` value for `--format`. No analysis changes; this is purely an output surface over the same findings.

**Tech Stack:** Rust, serde + serde_json (already used by `stratify-report`).

**Prerequisite reading:** `crates/stratify-report/src/json.rs` and `human.rs` (existing renderers), `crates/stratify-cli/src/main.rs` (the `FormatArg` enum and render dispatch), and `crates/stratify-core/src/finding.rs` (`Report`, `Finding`, `Severity`).

**SARIF reference (minimal valid 2.1.0):** a document has `$schema`, `version: "2.1.0"`, and `runs: [{ tool: { driver: { name, informationUri, version, rules: [...] } }, results: [...] }]`. Each result has `ruleId`, `level` (one of `none`/`note`/`warning`/`error`), `message: { text }`, and `locations: [{ physicalLocation: { artifactLocation: { uri }, region: { startLine } } }]`. `startLine` is 1-based and required.

---

## File Structure

```
crates/stratify-report/src/sarif.rs   CREATE: typed SARIF 2.1.0 model + render
crates/stratify-report/src/lib.rs      MODIFY: pub mod sarif
crates/stratify-cli/Cargo.toml         MODIFY: serde_json dev-dependency (for e2e validation)
crates/stratify-cli/src/main.rs        MODIFY: FormatArg::Sarif -> Format::Sarif
crates/stratify-cli/src/run.rs         MODIFY: Format::Sarif renders via sarif
crates/stratify-cli/tests/e2e_sarif.rs CREATE: parse + validate SARIF end to end
README.md                              MODIFY: SARIF / GitHub code scanning section
```

---

## Task 1: SARIF renderer (`stratify-report`)

**Files:**
- Create: `crates/stratify-report/src/sarif.rs`
- Modify: `crates/stratify-report/src/lib.rs`

- [ ] **Step 1: Write the renderer with tests**

Create `crates/stratify-report/src/sarif.rs`:

```rust
use serde::Serialize;
use stratify_core::{Report, Severity};

/// Render a report as a SARIF 2.1.0 document. GitHub code scanning and GitLab
/// render this as inline annotations.
pub fn render(report: &Report) -> String {
    serde_json::to_string_pretty(&build(report)).expect("sarif serializes")
}

fn level_of(sev: Severity) -> &'static str {
    match sev {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Info => "note",
    }
}

fn rule_description(rule: &str) -> &'static str {
    match rule {
        "dead_code" => "Unused code: functions never reached from an entrypoint.",
        "duplication" => "Duplicated code blocks.",
        "complexity" => "Functions with high cyclomatic complexity.",
        "hotspot" => "Complex code that also changes frequently.",
        "cycle" => "Circular dependencies between files.",
        "boundary" => "Imports that violate configured layer boundaries.",
        _ => "Stratify finding.",
    }
}

fn build(report: &Report) -> Sarif {
    // Distinct rule ids, in first-seen order, become the driver's rule metadata.
    let mut seen: Vec<String> = Vec::new();
    for f in &report.findings {
        if !seen.contains(&f.rule) {
            seen.push(f.rule.clone());
        }
    }
    let rules = seen
        .iter()
        .map(|id| RuleMeta {
            id: id.clone(),
            name: id.clone(),
            short_description: Text { text: rule_description(id).to_string() },
        })
        .collect();

    let results = report
        .findings
        .iter()
        .map(|f| SarifResult {
            rule_id: f.rule.clone(),
            level: level_of(f.severity),
            message: Text { text: f.message.clone() },
            locations: vec![Location {
                physical_location: PhysicalLocation {
                    artifact_location: ArtifactLocation { uri: f.span.file.clone() },
                    region: Region { start_line: f.span.start_line.max(1) },
                },
            }],
        })
        .collect();

    Sarif {
        schema: "https://json.schemastore.org/sarif-2.1.0.json",
        version: "2.1.0",
        runs: vec![Run {
            tool: Tool {
                driver: Driver {
                    name: "Stratify",
                    information_uri: "https://github.com/stratify-dev/stratify",
                    version: env!("CARGO_PKG_VERSION"),
                    rules,
                },
            },
            results,
        }],
    }
}

#[derive(Serialize)]
struct Sarif {
    #[serde(rename = "$schema")]
    schema: &'static str,
    version: &'static str,
    runs: Vec<Run>,
}

#[derive(Serialize)]
struct Run {
    tool: Tool,
    results: Vec<SarifResult>,
}

#[derive(Serialize)]
struct Tool {
    driver: Driver,
}

#[derive(Serialize)]
struct Driver {
    name: &'static str,
    #[serde(rename = "informationUri")]
    information_uri: &'static str,
    version: &'static str,
    rules: Vec<RuleMeta>,
}

#[derive(Serialize)]
struct RuleMeta {
    id: String,
    name: String,
    #[serde(rename = "shortDescription")]
    short_description: Text,
}

#[derive(Serialize)]
struct SarifResult {
    #[serde(rename = "ruleId")]
    rule_id: String,
    level: &'static str,
    message: Text,
    locations: Vec<Location>,
}

#[derive(Serialize)]
struct Location {
    #[serde(rename = "physicalLocation")]
    physical_location: PhysicalLocation,
}

#[derive(Serialize)]
struct PhysicalLocation {
    #[serde(rename = "artifactLocation")]
    artifact_location: ArtifactLocation,
    region: Region,
}

#[derive(Serialize)]
struct ArtifactLocation {
    uri: String,
}

#[derive(Serialize)]
struct Region {
    #[serde(rename = "startLine")]
    start_line: usize,
}

#[derive(Serialize)]
struct Text {
    text: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use stratify_core::ir::Span;
    use stratify_core::{Confidence, Finding, Severity};

    fn finding(rule: &str, sev: Severity, file: &str, line: usize) -> Finding {
        Finding {
            rule: rule.into(),
            severity: sev,
            message: format!("{rule} message"),
            span: Span { file: file.into(), start_byte: 0, end_byte: 1, start_line: line },
            confidence: Confidence::Certain,
        }
    }

    #[test]
    fn renders_valid_sarif_shape() {
        let report = Report::new(vec![
            finding("dead_code", Severity::Warning, "A.java", 5),
            finding("complexity", Severity::Info, "b.rb", 1),
        ]);
        let v: serde_json::Value = serde_json::from_str(&render(&report)).unwrap();
        assert_eq!(v["version"], "2.1.0");
        assert_eq!(v["runs"][0]["tool"]["driver"]["name"], "Stratify");
        // two distinct rules
        assert_eq!(v["runs"][0]["tool"]["driver"]["rules"].as_array().unwrap().len(), 2);
        // two results
        let results = v["runs"][0]["results"].as_array().unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["ruleId"], "dead_code");
        assert_eq!(results[0]["level"], "warning");
        assert_eq!(results[0]["locations"][0]["physicalLocation"]["artifactLocation"]["uri"], "A.java");
        assert_eq!(results[0]["locations"][0]["physicalLocation"]["region"]["startLine"], 5);
        // Info maps to SARIF "note"
        assert_eq!(results[1]["level"], "note");
    }

    #[test]
    fn empty_report_is_valid_sarif() {
        let v: serde_json::Value = serde_json::from_str(&render(&Report::new(vec![]))).unwrap();
        assert_eq!(v["version"], "2.1.0");
        assert!(v["runs"][0]["results"].as_array().unwrap().is_empty());
        assert!(v["runs"][0]["tool"]["driver"]["rules"].as_array().unwrap().is_empty());
    }

    #[test]
    fn distinct_rules_dedupe_in_driver() {
        let report = Report::new(vec![
            finding("dead_code", Severity::Warning, "a", 1),
            finding("dead_code", Severity::Warning, "b", 2),
        ]);
        let v: serde_json::Value = serde_json::from_str(&render(&report)).unwrap();
        assert_eq!(v["runs"][0]["tool"]["driver"]["rules"].as_array().unwrap().len(), 1);
        assert_eq!(v["runs"][0]["results"].as_array().unwrap().len(), 2);
    }
}
```

- [ ] **Step 2: Wire lib.rs**

In `crates/stratify-report/src/lib.rs`, add:

```rust
pub mod sarif;
```

- [ ] **Step 3: Run, verify pass**

Run: `cargo test -p stratify-report` (if `cargo` missing: `source "$HOME/.cargo/env"`)
Expected: PASS (3 prior + 3 sarif = 6 tests).

- [ ] **Step 4: Commit**

```bash
git add crates/stratify-report
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(report): SARIF 2.1.0 renderer"
```

---

## Task 2: Wire `--format sarif` + end-to-end + README (`stratify-cli`)

**Files:**
- Modify: `crates/stratify-cli/src/run.rs`
- Modify: `crates/stratify-cli/src/main.rs`
- Modify: `crates/stratify-cli/Cargo.toml`
- Create: `crates/stratify-cli/tests/e2e_sarif.rs`
- Modify: `README.md`

- [ ] **Step 1: Add the Sarif format variant**

In `crates/stratify-cli/src/run.rs`, the `Format` enum currently has `Human` and `Json`. Add `Sarif`:

```rust
pub enum Format {
    Human,
    Json,
    Sarif,
}
```

Wherever the renderer is selected (the match on `Format` that calls `stratify_report::human::render` / `json::render`), add the `Sarif` arm calling `stratify_report::sarif::render(&report)`. (This match lives in `main.rs` per M1's refactor; if it is in `run.rs`, update it there. Find the existing `match format { Format::Human => ..., Format::Json => ... }` and add `Format::Sarif => stratify_report::sarif::render(&report)`.)

- [ ] **Step 2: Add the CLI value**

In `crates/stratify-cli/src/main.rs`, the `FormatArg` value-enum has `Human` and `Json`. Add `Sarif`, and map it in the `From<FormatArg> for Format` impl:

```rust
#[derive(Clone, Copy, ValueEnum)]
enum FormatArg {
    Human,
    Json,
    Sarif,
}
```

```rust
impl From<FormatArg> for Format {
    fn from(f: FormatArg) -> Self {
        match f {
            FormatArg::Human => Format::Human,
            FormatArg::Json => Format::Json,
            FormatArg::Sarif => Format::Sarif,
        }
    }
}
```

- [ ] **Step 3: Add serde_json dev-dependency for the e2e**

In `crates/stratify-cli/Cargo.toml`, add under `[dev-dependencies]`:

```toml
serde_json = { workspace = true }
```

- [ ] **Step 4: Write the end-to-end test**

Create `crates/stratify-cli/tests/e2e_sarif.rs`:

```rust
use std::path::Path;

#[test]
fn sarif_output_is_valid_and_has_results() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sample-ruby");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_stratify"))
        .arg("check")
        .arg(&dir)
        .arg("--format")
        .arg("sarif")
        .output()
        .expect("run stratify binary");
    let stdout = String::from_utf8(output.stdout).unwrap();

    // Must be parseable JSON and a well-formed SARIF 2.1.0 document.
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert_eq!(v["version"], "2.1.0", "stdout: {stdout}");
    assert_eq!(v["runs"][0]["tool"]["driver"]["name"], "Stratify");
    let results = v["runs"][0]["results"].as_array().expect("results array");
    assert!(!results.is_empty(), "expected findings on sample-ruby");
    // sample-ruby has a dead-code finding (never_called).
    assert!(
        results.iter().any(|r| r["ruleId"] == "dead_code"),
        "stdout: {stdout}"
    );
}
```

- [ ] **Step 5: Run + manual smoke**

Run: `cargo test -p stratify-cli`
Expected: PASS including `e2e_sarif`.

Manual:
```bash
cargo build
./target/debug/stratify check crates/stratify-cli/tests/sample-ruby --format sarif | head -30
```
Expected: a SARIF JSON document with `"version": "2.1.0"` and a `results` array.

- [ ] **Step 6: Document SARIF in the README**

In `README.md`, add a section after the existing GitHub Action section:

```markdown
## SARIF / GitHub code scanning

Stratify emits SARIF 2.1.0 so GitHub and GitLab render findings as inline annotations:

```sh
stratify check . --format sarif > stratify.sarif
```

Upload it to GitHub code scanning in a workflow:

```yaml
- uses: actions/checkout@v4
- uses: stratify-dev/stratify@main
  with:
    fail-on: never
- run: stratify check . --format sarif > stratify.sarif
- uses: github/codeql-action/upload-sarif@v3
  with:
    sarif_file: stratify.sarif
```

The Stratify action installs the `stratify` binary and runs the gate; the
follow-up step reuses the installed binary to write a SARIF file for upload.
Set `fail-on: never` on the action step if you want findings to appear in code
scanning without failing the build.
```

(Keep the writing tight: short active sentences, no em dashes, no semicolons.)

- [ ] **Step 7: Commit**

```bash
git add crates/stratify-cli README.md
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(cli): --format sarif output and code-scanning docs"
```

---

## Task 3: fmt, clippy, lockfile

- [ ] **Step 1: Format and lint**

Run: `cargo fmt --all && cargo clippy --all-targets -- -D warnings`
Fix any warning properly (no blanket `#[allow]`). Re-run `cargo test` after any fix.

- [ ] **Step 2: Full suite**

Run: `cargo test`
Expected: all crates green (report 6, cli incl. e2e_sarif, others unchanged).

- [ ] **Step 3: Commit**

```bash
git add -A
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "chore: fmt, clippy clean, update lockfile for sarif"
```

---

## Self-Review Notes

Spec coverage for M8:
- SARIF 2.1.0 renderer over the existing Report: Task 1. Covered.
- `--format sarif` CLI value + end-to-end validity check: Task 2. Covered.
- Consumer docs (GitHub code scanning upload): Task 2 Step 6. Covered.

Deferred (correctly out of M8): an `output-file` input on the composite action (the README documents the redirect pattern instead), GitLab-specific code-quality JSON (SARIF covers GitHub natively; GitLab also ingests SARIF), and richer SARIF fields (codeFlows, fingerprints for de-dup across runs, rule help URIs) are later refinements.

Known M8 characteristics (acceptable):
- `level` maps Info -> SARIF `note`, Warning -> `warning`, Error -> `error`. Findings never use `none`.
- One location per finding (the primary span). Multi-location findings (e.g. duplication's other copy, a full cycle path) are summarized in the message text, not as multiple SARIF locations. A later refinement can add related locations.
- `startLine` is clamped to a minimum of 1 to satisfy SARIF (spans already start at 1).
- Driver `rules` lists only rule ids actually present in this run, in first-seen order, deduped.

Type consistency: `sarif::render(&Report) -> String`, `Format::Sarif`, `FormatArg::Sarif`, the SARIF struct field rename attributes, and `Severity`/`Finding`/`Report` are used consistently with M1-M7 definitions.

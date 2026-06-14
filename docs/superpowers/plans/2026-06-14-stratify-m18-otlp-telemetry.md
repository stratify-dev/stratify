# Stratify M18 (OTLP Telemetry Export) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Push a `stratify check` run's metrics and a per-run event to any OpenTelemetry backend over OTLP (Datadog, Grafana, self-hosted Collector), identified per project so a fleet-wide dashboard works.

**Architecture:** A new `stratify-telemetry` crate holds pure mapping functions (`report_to_metrics`, `report_to_event`) plus config helpers, all unit-tested with no network. A thin `emit` wrapper is the only code touching the OTLP wire. The CLI computes a `ScanStats` from the `IrGraph` before it is dropped, gathers git metadata, resolves config from env + flags, and calls `emit` after analysis. Telemetry failure never fails the scan.

**Tech Stack:** Rust workspace (edition 2021), clap derive CLI, `opentelemetry` + `opentelemetry_sdk` + `opentelemetry-otlp` (HTTP/protobuf, reqwest blocking client). `cargo` is on PATH via `source "$HOME/.cargo/env"` (run it first if `cargo` is not found).

---

## Background for the implementer

Key existing types (do not redefine):

- `stratify_core::Finding { rule: String, severity: Severity, message: String, span: Span, confidence: Confidence }`. `Span { file: String, start_byte, end_byte, start_line }`.
- `stratify_core::Report { schema_version: u32, findings: Vec<Finding> }`, built via `Report::new(findings)`.
- `stratify_core::Severity { Info, Warning, Error }` (serde lowercase).
- `stratify_core::Confidence { Unknown, Likely, Certain }` (serde lowercase).
- `stratify_core::{IrGraph, SymbolKind}`. `SymbolKind { File, Module, Class, Function, Dependency }`. `IrGraph::symbols() -> &[Symbol]` (each `Symbol` has `.kind: SymbolKind` and `.span: Span`), `IrGraph::complexities() -> &[(SymbolId, u32)]`.
- Exact analysis rule strings: `dead_code`, `duplication`, `complexity`, `hotspot`, `cycle`, `boundary`.

The CLI is clap-derive in `crates/stratify-cli/src/main.rs` with a `Check { path, format, fail_on }` subcommand. `crates/stratify-cli/src/run.rs::analyze_repo(root) -> io::Result<Report>` builds an `IrGraph`, runs the six analyses, and returns the `Report` (the graph is dropped). `crates/stratify-cli/src/churn.rs` shows the established pattern for shelling `git` with `Command::new("git").arg("-C").arg(root)...`, best-effort, never panics.

---

## File Structure

```
crates/stratify-telemetry/Cargo.toml          CREATE: new crate manifest
crates/stratify-telemetry/src/lib.rs          CREATE: types, lang_of, report_to_metrics, report_to_event, resolve_service_name, parse_headers
crates/stratify-telemetry/src/emit.rs         CREATE: TelemetryConfig + emit (OTLP wire)
crates/stratify-cli/src/run.rs                MODIFY: ScanStats, scan_stats(), analyze_repo_with_stats()
crates/stratify-cli/src/gitmeta.rs            CREATE: GitMeta, parse_remote_url, git_meta
crates/stratify-cli/src/main.rs               MODIFY: mod gitmeta; --otlp-endpoint/--project flags; emit wiring
crates/stratify-cli/Cargo.toml                MODIFY: depend on stratify-telemetry
crates/stratify-cli/tests/e2e_otlp.rs         CREATE: graceful-failure e2e
Cargo.toml                                     MODIFY: add member + workspace OTel deps
README.md                                      MODIFY: telemetry section
```

---

## Task 1: ScanStats from the IrGraph

**Files:**
- Modify: `crates/stratify-cli/src/run.rs`

- [ ] **Step 1: Write the failing test**

Add to the bottom of `crates/stratify-cli/src/run.rs` (create a `#[cfg(test)] mod tests` if none exists; if one exists, add these into it):

```rust
#[cfg(test)]
mod stats_tests {
    use super::*;
    use stratify_core::ir::{Span, Symbol, SymbolId, Visibility};
    use stratify_core::{Confidence, IrGraph, SymbolKind};

    fn sym(g: &mut IrGraph, kind: SymbolKind, file: &str) -> SymbolId {
        g.add_symbol(Symbol {
            id: SymbolId(0),
            kind,
            name: file.into(),
            fqn: file.into(),
            span: Span {
                file: file.into(),
                start_byte: 0,
                end_byte: 1,
                start_line: 1,
            },
            visibility: Visibility::Unknown,
            confidence: Confidence::Certain,
        })
    }

    #[test]
    fn scan_stats_counts_and_aggregates() {
        let mut g = IrGraph::new();
        sym(&mut g, SymbolKind::File, "a.rb");
        sym(&mut g, SymbolKind::File, "b.go");
        let f1 = sym(&mut g, SymbolKind::Function, "a.rb");
        let f2 = sym(&mut g, SymbolKind::Function, "b.go");
        g.set_complexity(f1, 4);
        g.set_complexity(f2, 10);

        let stats = scan_stats(&g, 123);
        assert_eq!(stats.files_scanned, 2);
        assert_eq!(stats.functions, 2);
        assert_eq!(stats.complexity_max, 10);
        assert_eq!(stats.complexity_mean, 7.0);
        assert_eq!(stats.duration_ms, 123);
        assert!(stats.languages.contains("ruby"));
        assert!(stats.languages.contains("go"));
    }

    #[test]
    fn scan_stats_empty_graph_is_zero() {
        let g = IrGraph::new();
        let stats = scan_stats(&g, 0);
        assert_eq!(stats.files_scanned, 0);
        assert_eq!(stats.functions, 0);
        assert_eq!(stats.complexity_max, 0);
        assert_eq!(stats.complexity_mean, 0.0);
        assert!(stats.languages.is_empty());
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p stratify-cli scan_stats` (if `cargo` missing: `source "$HOME/.cargo/env"`)
Expected: FAIL to compile (`scan_stats` and `ScanStats` not defined).

- [ ] **Step 3: Implement `ScanStats`, `lang_of_file`, and `scan_stats`**

At the top of `crates/stratify-cli/src/run.rs`, add to the imports:

```rust
use std::collections::BTreeSet;
use stratify_core::SymbolKind;
```

Add this above `analyze_repo`:

```rust
/// Repo-wide aggregate metrics computed from the IR, for telemetry.
#[derive(Debug, Clone)]
pub struct ScanStats {
    pub files_scanned: u64,
    pub functions: u64,
    pub complexity_max: u32,
    pub complexity_mean: f64,
    pub languages: BTreeSet<String>,
    pub duration_ms: u64,
}

/// Map a file path to its Stratify language name by extension. Unknown
/// extensions are skipped by the caller (returns None).
fn lang_of_file(path: &str) -> Option<&'static str> {
    let ext = path.rsplit('.').next()?;
    match ext {
        "java" => Some("java"),
        "rb" => Some("ruby"),
        "ts" | "tsx" | "mts" | "cts" => Some("typescript"),
        "py" | "pyi" => Some("python"),
        "go" => Some("go"),
        _ => None,
    }
}

/// Compute repo-wide stats from the merged IR plus the measured wall time.
pub fn scan_stats(graph: &IrGraph, duration_ms: u64) -> ScanStats {
    let mut files_scanned = 0u64;
    let mut functions = 0u64;
    let mut languages = BTreeSet::new();
    for s in graph.symbols() {
        match s.kind {
            SymbolKind::File => {
                files_scanned += 1;
                if let Some(lang) = lang_of_file(&s.span.file) {
                    languages.insert(lang.to_string());
                }
            }
            SymbolKind::Function => functions += 1,
            _ => {}
        }
    }
    let complexities: Vec<u32> = graph.complexities().iter().map(|(_, c)| *c).collect();
    let complexity_max = complexities.iter().copied().max().unwrap_or(0);
    let complexity_mean = if complexities.is_empty() {
        0.0
    } else {
        complexities.iter().map(|c| *c as f64).sum::<f64>() / complexities.len() as f64
    };
    ScanStats {
        files_scanned,
        functions,
        complexity_max,
        complexity_mean,
        languages,
        duration_ms,
    }
}
```

- [ ] **Step 4: Refactor `analyze_repo` to expose stats**

Rename the current body of `analyze_repo` into `analyze_repo_with_stats`, measure wall time, and make `analyze_repo` delegate. Replace the existing `pub fn analyze_repo(root: &Path) -> std::io::Result<Report> { ... }` signature line and its final `Ok(Report::new(findings))` so the function becomes:

```rust
/// Walk `root`, parse + merge into one IrGraph, run all analyses, and return
/// both the Report and repo-wide ScanStats (the graph is consumed here).
pub fn analyze_repo_with_stats(root: &Path) -> std::io::Result<(Report, ScanStats)> {
    let start = std::time::Instant::now();
    // ... existing body unchanged, from the `if !root.exists()` check
    //     through building `findings`, but DO NOT return yet ...
    let stats = scan_stats(&graph, start.elapsed().as_millis() as u64);
    Ok((Report::new(findings), stats))
}

/// Convenience wrapper for callers that only need the Report (mcp, lsp).
pub fn analyze_repo(root: &Path) -> std::io::Result<Report> {
    Ok(analyze_repo_with_stats(root)?.0)
}
```

Concretely: change `pub fn analyze_repo(root: &Path) -> std::io::Result<Report> {` to `pub fn analyze_repo_with_stats(root: &Path) -> std::io::Result<(Report, ScanStats)> {`, add `let start = std::time::Instant::now();` as the first line, change the final `Ok(Report::new(findings))` to the two lines computing `stats` and returning `Ok((Report::new(findings), stats))`, then add the new thin `analyze_repo` wrapper after the function. `graph` is already `mut` and in scope at the end (it is used by `scan_stats` which borrows it before `findings` is moved into `Report::new`).

- [ ] **Step 5: Run the tests**

Run: `cargo test -p stratify-cli`
Expected: PASS, including the two new stats tests and all existing CLI tests (mcp/lsp still compile against `analyze_repo`).

- [ ] **Step 6: Commit**

```bash
git add crates/stratify-cli/src/run.rs
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(cli): ScanStats + analyze_repo_with_stats for telemetry"
```

---

## Task 2: `stratify-telemetry` crate — types + `report_to_metrics`

**Files:**
- Modify: `Cargo.toml` (workspace)
- Create: `crates/stratify-telemetry/Cargo.toml`
- Create: `crates/stratify-telemetry/src/lib.rs`

- [ ] **Step 1: Add the crate to the workspace**

In the root `Cargo.toml`, add to `members` (after `"crates/stratify-report",`):

```toml
  "crates/stratify-telemetry",
```

And add to `[workspace.dependencies]`:

```toml
opentelemetry = "0.27"
opentelemetry_sdk = "0.27"
opentelemetry-otlp = { version = "0.27", default-features = false, features = ["http-proto", "reqwest-blocking-client", "metrics", "logs"] }
```

- [ ] **Step 2: Create the crate manifest**

`crates/stratify-telemetry/Cargo.toml`:

```toml
[package]
name = "stratify-telemetry"
edition.workspace = true
version.workspace = true
license.workspace = true

[dependencies]
stratify-core = { path = "../stratify-core" }
opentelemetry = { workspace = true }
opentelemetry_sdk = { workspace = true }
opentelemetry-otlp = { workspace = true }
```

Note: `ScanStats` is defined in `stratify-cli`, not `stratify-core`. To keep `stratify-telemetry` free of a CLI dependency, the mapping functions take the **fields they need** as plain parameters (see signatures below), not a `ScanStats` value. The CLI destructures its `ScanStats` at the call site.

- [ ] **Step 3: Write the failing test (lib.rs with `report_to_metrics`)**

Create `crates/stratify-telemetry/src/lib.rs`:

```rust
//! Pure mapping from a Stratify Report + scan aggregates to OTLP-ready
//! metric points and a per-run event. No network, no OTel SDK here.

use stratify_core::{Confidence, Finding, Report, Severity};

/// One gauge data point: a metric name, a value, and low-cardinality attributes.
#[derive(Debug, Clone, PartialEq)]
pub struct MetricPoint {
    pub name: String,
    pub value: f64,
    pub attributes: Vec<(String, String)>,
}

fn severity_str(s: Severity) -> &'static str {
    match s {
        Severity::Info => "info",
        Severity::Warning => "warning",
        Severity::Error => "error",
    }
}

fn confidence_str(c: Confidence) -> &'static str {
    match c {
        Confidence::Unknown => "unknown",
        Confidence::Likely => "likely",
        Confidence::Certain => "certain",
    }
}

/// Language name from a file path by extension, or "unknown".
pub fn lang_of(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("java") => "java",
        Some("rb") => "ruby",
        Some("ts") | Some("tsx") | Some("mts") | Some("cts") => "typescript",
        Some("py") | Some("pyi") => "python",
        Some("go") => "go",
        _ => "unknown",
    }
}

/// Build the metric points for one run. `findings` drives the per-combination
/// `stratify.findings` counts and the cycle/boundary/duplication gauges; the
/// remaining scalars come from scan aggregates passed by the caller.
pub fn report_to_metrics(
    report: &Report,
    files_scanned: u64,
    functions: u64,
    complexity_max: u32,
    complexity_mean: f64,
    duration_ms: u64,
) -> Vec<MetricPoint> {
    use std::collections::BTreeMap;

    // stratify.findings grouped by (rule, severity, language, confidence).
    let mut grouped: BTreeMap<(String, &'static str, &'static str, &'static str), u64> =
        BTreeMap::new();
    let mut by_rule: BTreeMap<&str, u64> = BTreeMap::new();
    for f in &report.findings {
        *by_rule.entry(f.rule.as_str()).or_insert(0) += 1;
        let key = (
            f.rule.clone(),
            severity_str(f.severity),
            lang_of(&f.span.file),
            confidence_str(f.confidence),
        );
        *grouped.entry(key).or_insert(0) += 1;
    }

    let mut out = Vec::new();
    for ((rule, sev, lang, conf), count) in grouped {
        out.push(MetricPoint {
            name: "stratify.findings".into(),
            value: count as f64,
            attributes: vec![
                ("rule".into(), rule),
                ("severity".into(), sev.into()),
                ("language".into(), lang.into()),
                ("confidence".into(), conf.into()),
            ],
        });
    }

    let scalar = |name: &str, value: f64| MetricPoint {
        name: name.into(),
        value,
        attributes: vec![],
    };
    out.push(scalar("stratify.cycles", *by_rule.get("cycle").unwrap_or(&0) as f64));
    out.push(scalar(
        "stratify.boundary_violations",
        *by_rule.get("boundary").unwrap_or(&0) as f64,
    ));
    out.push(scalar(
        "stratify.duplication.regions",
        *by_rule.get("duplication").unwrap_or(&0) as f64,
    ));
    out.push(scalar("stratify.complexity.max", complexity_max as f64));
    out.push(scalar("stratify.complexity.mean", complexity_mean));
    out.push(scalar("stratify.files_scanned", files_scanned as f64));
    out.push(scalar("stratify.functions", functions as f64));
    out.push(scalar("stratify.scan.duration_ms", duration_ms as f64));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use stratify_core::ir::Span;

    fn finding(rule: &str, sev: Severity, file: &str, conf: Confidence) -> Finding {
        Finding {
            rule: rule.into(),
            severity: sev,
            message: "m".into(),
            span: Span {
                file: file.into(),
                start_byte: 0,
                end_byte: 1,
                start_line: 1,
            },
            confidence: conf,
        }
    }

    #[test]
    fn lang_of_maps_extensions() {
        assert_eq!(lang_of("a/b.go"), "go");
        assert_eq!(lang_of("x.tsx"), "typescript");
        assert_eq!(lang_of("no_ext"), "unknown");
    }

    #[test]
    fn findings_grouped_and_counted() {
        let report = Report::new(vec![
            finding("dead_code", Severity::Warning, "a.go", Confidence::Certain),
            finding("dead_code", Severity::Warning, "b.go", Confidence::Certain),
            finding("cycle", Severity::Warning, "c.rb", Confidence::Certain),
        ]);
        let m = report_to_metrics(&report, 3, 5, 9, 4.5, 42);

        // The two dead_code/go/warning/certain findings collapse into one point=2.
        let dc = m
            .iter()
            .find(|p| p.name == "stratify.findings"
                && p.attributes.contains(&("rule".into(), "dead_code".into())))
            .unwrap();
        assert_eq!(dc.value, 2.0);
        assert!(dc.attributes.contains(&("language".into(), "go".into())));

        let cycles = m.iter().find(|p| p.name == "stratify.cycles").unwrap();
        assert_eq!(cycles.value, 1.0);
        let cmax = m.iter().find(|p| p.name == "stratify.complexity.max").unwrap();
        assert_eq!(cmax.value, 9.0);
        let cmean = m.iter().find(|p| p.name == "stratify.complexity.mean").unwrap();
        assert_eq!(cmean.value, 4.5);
        let dur = m.iter().find(|p| p.name == "stratify.scan.duration_ms").unwrap();
        assert_eq!(dur.value, 42.0);
    }
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p stratify-telemetry`
Expected: PASS (3 tests). This also confirms the OTel deps resolve (they are declared but unused so far, which is fine).

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock crates/stratify-telemetry
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(telemetry): stratify-telemetry crate with report_to_metrics"
```

---

## Task 3: `report_to_event`, `parse_headers`, `resolve_service_name`

**Files:**
- Modify: `crates/stratify-telemetry/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Add these tests into the existing `mod tests` in `crates/stratify-telemetry/src/lib.rs`:

```rust
    #[test]
    fn event_carries_commit_and_totals() {
        let report = Report::new(vec![
            finding("dead_code", Severity::Warning, "a.go", Confidence::Certain),
            finding("cycle", Severity::Error, "c.rb", Confidence::Likely),
        ]);
        let git = GitMeta {
            commit: Some("abc123".into()),
            branch: Some("main".into()),
            remote_url: None,
        };
        let langs = ["go".to_string(), "ruby".to_string()].into_iter().collect();
        let ev = report_to_event(&report, &git, "org/repo", 99, &langs);

        assert_eq!(ev.body, "stratify.run");
        let get = |k: &str| ev.attributes.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        assert_eq!(get("project"), Some(AttrValue::Str("org/repo".into())));
        assert_eq!(get("commit"), Some(AttrValue::Str("abc123".into())));
        assert_eq!(get("branch"), Some(AttrValue::Str("main".into())));
        assert_eq!(get("total_findings"), Some(AttrValue::Int(2)));
        assert_eq!(get("warning"), Some(AttrValue::Int(1)));
        assert_eq!(get("error"), Some(AttrValue::Int(1)));
        assert_eq!(get("info"), Some(AttrValue::Int(0)));
        assert_eq!(get("rule.dead_code"), Some(AttrValue::Int(1)));
        assert_eq!(get("rule.cycle"), Some(AttrValue::Int(1)));
        assert_eq!(get("duration_ms"), Some(AttrValue::Int(99)));
        assert_eq!(get("languages"), Some(AttrValue::Str("go,ruby".into())));
    }

    #[test]
    fn event_omits_absent_git_fields() {
        let report = Report::new(vec![]);
        let git = GitMeta { commit: None, branch: None, remote_url: None };
        let ev = report_to_event(&report, &git, "p", 0, &Default::default());
        assert!(ev.attributes.iter().all(|(n, _)| n != "commit"));
        assert!(ev.attributes.iter().all(|(n, _)| n != "branch"));
    }

    #[test]
    fn parse_headers_splits_pairs() {
        assert_eq!(
            parse_headers("a=1,b=2"),
            vec![("a".to_string(), "1".to_string()), ("b".to_string(), "2".to_string())]
        );
        assert!(parse_headers("").is_empty());
        assert_eq!(parse_headers("only=this,broken"), vec![("only".to_string(), "this".to_string())]);
    }

    #[test]
    fn service_name_precedence() {
        assert_eq!(resolve_service_name(Some("flag"), Some("env"), Some("git"), "dir"), "flag");
        assert_eq!(resolve_service_name(None, Some("env"), Some("git"), "dir"), "env");
        assert_eq!(resolve_service_name(None, None, Some("git"), "dir"), "git");
        assert_eq!(resolve_service_name(None, None, None, "dir"), "dir");
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p stratify-telemetry`
Expected: FAIL to compile (`GitMeta`, `AttrValue`, `report_to_event`, `parse_headers`, `resolve_service_name` undefined).

- [ ] **Step 3: Implement the event + config helpers**

Add to `crates/stratify-telemetry/src/lib.rs` (above the `#[cfg(test)]` block):

```rust
use std::collections::BTreeSet;

/// Typed attribute value for the per-run event.
#[derive(Debug, Clone, PartialEq)]
pub enum AttrValue {
    Str(String),
    Int(i64),
}

/// Git metadata for the run. Fields are None outside a git repo.
#[derive(Debug, Clone, Default)]
pub struct GitMeta {
    pub commit: Option<String>,
    pub branch: Option<String>,
    pub remote_url: Option<String>,
}

/// One structured log record summarizing a run. Holds the high-cardinality
/// fields (commit, branch) that must never go on metric attributes.
#[derive(Debug, Clone, PartialEq)]
pub struct RunEvent {
    pub body: String,
    pub attributes: Vec<(String, AttrValue)>,
}

/// Build the per-run event from the report, git metadata, project name,
/// duration, and the set of languages seen.
pub fn report_to_event(
    report: &Report,
    git: &GitMeta,
    project: &str,
    duration_ms: u64,
    languages: &BTreeSet<String>,
) -> RunEvent {
    let mut attrs: Vec<(String, AttrValue)> = Vec::new();
    attrs.push(("project".into(), AttrValue::Str(project.into())));
    if let Some(c) = &git.commit {
        attrs.push(("commit".into(), AttrValue::Str(c.clone())));
    }
    if let Some(b) = &git.branch {
        attrs.push(("branch".into(), AttrValue::Str(b.clone())));
    }
    attrs.push((
        "total_findings".into(),
        AttrValue::Int(report.findings.len() as i64),
    ));

    let count_sev = |s: Severity| {
        report.findings.iter().filter(|f| f.severity == s).count() as i64
    };
    attrs.push(("info".into(), AttrValue::Int(count_sev(Severity::Info))));
    attrs.push(("warning".into(), AttrValue::Int(count_sev(Severity::Warning))));
    attrs.push(("error".into(), AttrValue::Int(count_sev(Severity::Error))));

    let mut by_rule: std::collections::BTreeMap<&str, i64> = std::collections::BTreeMap::new();
    for f in &report.findings {
        *by_rule.entry(f.rule.as_str()).or_insert(0) += 1;
    }
    for (rule, n) in by_rule {
        attrs.push((format!("rule.{rule}"), AttrValue::Int(n)));
    }

    attrs.push(("duration_ms".into(), AttrValue::Int(duration_ms as i64)));
    attrs.push((
        "languages".into(),
        AttrValue::Str(languages.iter().cloned().collect::<Vec<_>>().join(",")),
    ));

    RunEvent {
        body: "stratify.run".into(),
        attributes: attrs,
    }
}

/// Parse `OTEL_EXPORTER_OTLP_HEADERS` (`k1=v1,k2=v2`). Entries without `=` are
/// skipped. Surrounding whitespace is trimmed.
pub fn parse_headers(raw: &str) -> Vec<(String, String)> {
    raw.split(',')
        .filter_map(|pair| {
            let (k, v) = pair.split_once('=')?;
            let k = k.trim();
            if k.is_empty() {
                return None;
            }
            Some((k.to_string(), v.trim().to_string()))
        })
        .collect()
}

/// Resolve `service.name`: flag > env > git remote basename > directory name.
pub fn resolve_service_name(
    flag: Option<&str>,
    env: Option<&str>,
    git_basename: Option<&str>,
    dir_name: &str,
) -> String {
    flag.or(env)
        .or(git_basename)
        .unwrap_or(dir_name)
        .to_string()
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p stratify-telemetry`
Expected: PASS (all prior plus the 4 new tests).

- [ ] **Step 5: Commit**

```bash
git add crates/stratify-telemetry/src/lib.rs
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(telemetry): report_to_event, parse_headers, resolve_service_name"
```

---

## Task 4: Git metadata in the CLI

**Files:**
- Create: `crates/stratify-cli/src/gitmeta.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/stratify-cli/src/gitmeta.rs` with the parser and tests first:

```rust
use std::path::Path;
use std::process::Command;
use stratify_telemetry::GitMeta;

/// Parse a git remote URL into (namespace, repo). Handles https and scp-like
/// ssh forms, stripping a trailing `.git`. Returns None for unrecognizable
/// input; a best-effort basename is still returned when a namespace is absent.
pub fn parse_remote_url(url: &str) -> (Option<String>, Option<String>) {
    let url = url.trim();
    // Normalize ssh scp form `git@host:org/repo.git` to a path-ish tail.
    let tail = if let Some((_, rest)) = url.split_once('@') {
        // git@github.com:org/repo.git -> github.com:org/repo.git -> org/repo.git
        rest.split_once(':').map(|(_, p)| p).unwrap_or(rest)
    } else if let Some(rest) = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://")) {
        // github.com/org/repo.git -> drop the host segment
        rest.split_once('/').map(|(_, p)| p).unwrap_or(rest)
    } else {
        url
    };
    let tail = tail.strip_suffix(".git").unwrap_or(tail);
    let mut segs: Vec<&str> = tail.split('/').filter(|s| !s.is_empty()).collect();
    let repo = segs.pop().map(|s| s.to_string());
    let namespace = segs.pop().map(|s| s.to_string());
    (namespace, repo)
}

/// Gather commit, branch, and origin remote for `root`. Best-effort: any field
/// that git cannot supply is None. Never panics.
pub fn git_meta(root: &Path) -> GitMeta {
    let run = |args: &[&str]| -> Option<String> {
        let out = Command::new("git").arg("-C").arg(root).args(args).output().ok()?;
        if !out.status.success() {
            return None;
        }
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    };
    GitMeta {
        commit: run(&["rev-parse", "HEAD"]),
        branch: run(&["rev-parse", "--abbrev-ref", "HEAD"]),
        remote_url: run(&["remote", "get-url", "origin"]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_https_and_ssh_remotes() {
        assert_eq!(
            parse_remote_url("https://github.com/org/repo.git"),
            (Some("org".into()), Some("repo".into()))
        );
        assert_eq!(
            parse_remote_url("git@github.com:org/repo.git"),
            (Some("org".into()), Some("repo".into()))
        );
        assert_eq!(
            parse_remote_url("https://example.com/a/b/c"),
            (Some("b".into()), Some("c".into()))
        );
    }

    #[test]
    fn git_meta_outside_repo_is_empty() {
        let dir = std::env::temp_dir().join("stratify-gitmeta-not-a-repo");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let m = git_meta(&dir);
        assert!(m.commit.is_none());
        assert!(m.branch.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn git_meta_reads_a_real_repo() {
        let dir = std::env::temp_dir().join("stratify-gitmeta-real");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            assert!(Command::new("git").arg("-C").arg(&dir).args(args).status().unwrap().success());
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@example.com"]);
        git(&["config", "user.name", "T"]);
        git(&["remote", "add", "origin", "git@github.com:org/repo.git"]);
        std::fs::write(dir.join("f.txt"), "x").unwrap();
        git(&["add", "f.txt"]);
        git(&["commit", "-q", "-m", "init"]);

        let m = git_meta(&dir);
        assert!(m.commit.is_some());
        assert_eq!(m.remote_url.as_deref(), Some("git@github.com:org/repo.git"));
        let (ns, repo) = parse_remote_url(m.remote_url.as_deref().unwrap());
        assert_eq!(ns, Some("org".into()));
        assert_eq!(repo, Some("repo".into()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
```

- [ ] **Step 2: Wire the module + dependency, then run the test to verify it fails**

Add `stratify-telemetry = { path = "../stratify-telemetry" }` under `[dependencies]` in `crates/stratify-cli/Cargo.toml`. Add `mod gitmeta;` to the top of `crates/stratify-cli/src/main.rs` (alphabetical with the other `mod` lines: `mod churn; mod gitmeta; mod lsp; mod mcp; mod run;`).

Run: `cargo test -p stratify-cli gitmeta`
Expected: PASS (the module is self-contained and complete). If it does not compile, fix per the compiler before continuing.

- [ ] **Step 3: Commit**

```bash
git add crates/stratify-cli/src/gitmeta.rs crates/stratify-cli/src/main.rs crates/stratify-cli/Cargo.toml Cargo.lock
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(cli): git_meta + remote-URL parsing"
```

---

## Task 5: OTLP exporter (`emit`) and CLI wiring

**Files:**
- Create: `crates/stratify-telemetry/src/emit.rs`
- Modify: `crates/stratify-telemetry/src/lib.rs` (add `pub mod emit;`)
- Modify: `crates/stratify-cli/src/main.rs`

- [ ] **Step 1: Implement the exporter**

Create `crates/stratify-telemetry/src/emit.rs`. The OTel SDK builder names are version-sensitive: this targets `opentelemetry` 0.27. If the resolved version differs and a builder name does not exist, consult docs.rs for `opentelemetry-otlp` and `opentelemetry_sdk` for that version. The CONTRACT this function must satisfy: build an OTLP HTTP exporter pointed at `config.endpoint` with `config.headers`, attach a resource carrying `service.name` / `service.namespace` / `service.version`, record every `MetricPoint` as an `f64` gauge with its attributes, emit the `RunEvent` as one log record (body + attributes), flush, and return `Err` (never panic) on any failure.

```rust
use crate::{AttrValue, MetricPoint, RunEvent};
use opentelemetry::metrics::MeterProvider as _;
use opentelemetry::{KeyValue, Value};
use opentelemetry_sdk::metrics::SdkMeterProvider;
use opentelemetry_sdk::Resource;

pub type Error = Box<dyn std::error::Error + Send + Sync>;

/// Resolved telemetry configuration. An empty `endpoint` means do not call this.
#[derive(Debug, Clone)]
pub struct TelemetryConfig {
    pub endpoint: String,
    pub headers: Vec<(String, String)>,
    pub service_name: String,
    pub namespace: Option<String>,
    pub version: String,
}

fn resource(config: &TelemetryConfig) -> Resource {
    let mut kvs = vec![
        KeyValue::new("service.name", config.service_name.clone()),
        KeyValue::new("service.version", config.version.clone()),
    ];
    if let Some(ns) = &config.namespace {
        kvs.push(KeyValue::new("service.namespace", ns.clone()));
    }
    Resource::builder().with_attributes(kvs).build()
}

fn header_map(headers: &[(String, String)]) -> std::collections::HashMap<String, String> {
    headers.iter().cloned().collect()
}

/// Push metrics + the run event to the OTLP endpoint. Best-effort: returns Err
/// on any transport or build failure, never panics. Caller logs and continues.
pub fn emit(
    metrics: &[MetricPoint],
    event: &RunEvent,
    config: &TelemetryConfig,
) -> Result<(), Error> {
    // --- Metrics ---
    let metric_exporter = opentelemetry_otlp::MetricExporter::builder()
        .with_http()
        .with_endpoint(format!("{}/v1/metrics", config.endpoint.trim_end_matches('/')))
        .with_headers(header_map(&config.headers))
        .build()?;
    let reader =
        opentelemetry_sdk::metrics::PeriodicReader::builder(metric_exporter).build();
    let meter_provider = SdkMeterProvider::builder()
        .with_reader(reader)
        .with_resource(resource(config))
        .build();
    let meter = meter_provider.meter("stratify");
    for point in metrics {
        let gauge = meter.f64_gauge(point.name.clone()).build();
        let attrs: Vec<KeyValue> = point
            .attributes
            .iter()
            .map(|(k, v)| KeyValue::new(k.clone(), v.clone()))
            .collect();
        gauge.record(point.value, &attrs);
    }
    meter_provider.force_flush()?;

    // --- Per-run event (log record) ---
    let log_exporter = opentelemetry_otlp::LogExporter::builder()
        .with_http()
        .with_endpoint(format!("{}/v1/logs", config.endpoint.trim_end_matches('/')))
        .with_headers(header_map(&config.headers))
        .build()?;
    let logger_provider = opentelemetry_sdk::logs::SdkLoggerProvider::builder()
        .with_simple_exporter(log_exporter)
        .with_resource(resource(config))
        .build();
    {
        use opentelemetry::logs::{LogRecord, Logger, LoggerProvider};
        let logger = logger_provider.logger("stratify");
        let mut record = logger.create_log_record();
        record.set_body(event.body.clone().into());
        for (k, v) in &event.attributes {
            let val: Value = match v {
                AttrValue::Str(s) => Value::from(s.clone()),
                AttrValue::Int(i) => Value::from(*i),
            };
            record.add_attribute(k.clone(), val);
        }
        logger.emit(record);
    }
    logger_provider.force_flush()?;

    // Best-effort shutdown; ignore shutdown errors (data already flushed).
    let _ = meter_provider.shutdown();
    let _ = logger_provider.shutdown();
    Ok(())
}
```

Add `pub mod emit;` and a re-export to `crates/stratify-telemetry/src/lib.rs` (top, after the `//!` docs):

```rust
pub mod emit;
pub use emit::{emit, TelemetryConfig};
```

- [ ] **Step 2: Build the crate**

Run: `cargo build -p stratify-telemetry`
Expected: PASS. If a builder method name does not exist for the resolved OTel version, fix it against docs.rs for that version while preserving the contract described above. Re-run until it builds.

- [ ] **Step 3: Wire telemetry into the `check` command**

In `crates/stratify-cli/src/main.rs`, add two flags to the `Check` variant (after `fail_on`):

```rust
        /// OTLP endpoint base URL. Overrides OTEL_EXPORTER_OTLP_ENDPOINT.
        #[arg(long)]
        otlp_endpoint: Option<String>,
        /// Project name (service.name). Overrides OTEL_SERVICE_NAME.
        #[arg(long)]
        project: Option<String>,
```

Update the `Command::Check { .. }` destructure in `main` to bind `path, format, fail_on, otlp_endpoint, project`. Replace the `let report = match run::analyze_repo(&path)` block with `analyze_repo_with_stats`, and after rendering + before the `fail_on` gate, push telemetry. The full `Check` arm becomes:

```rust
        Command::Check {
            path,
            format,
            fail_on,
            otlp_endpoint,
            project,
        } => {
            let (report, stats) = match run::analyze_repo_with_stats(&path) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("stratify: {e}");
                    return ExitCode::FAILURE;
                }
            };

            let rendered = match Format::from(format) {
                Format::Human => stratify_report::human::render(&report),
                Format::Json => stratify_report::json::render(&report),
                Format::Sarif => stratify_report::sarif::render(&report),
            };
            print!("{rendered}");

            maybe_emit_telemetry(&path, &report, &stats, otlp_endpoint, project);

            if let Some(threshold) = fail_on.threshold() {
                if run::gate(&report, threshold) {
                    return ExitCode::FAILURE;
                }
            }

            ExitCode::SUCCESS
        }
```

Add this helper function to `crates/stratify-cli/src/main.rs` (below `main`), which resolves config and calls `emit`, logging any failure to stderr:

```rust
fn maybe_emit_telemetry(
    path: &std::path::Path,
    report: &stratify_core::Report,
    stats: &run::ScanStats,
    otlp_endpoint: Option<String>,
    project: Option<String>,
) {
    let endpoint = otlp_endpoint
        .or_else(|| std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").ok())
        .filter(|s| !s.trim().is_empty());
    let Some(endpoint) = endpoint else {
        return; // no endpoint configured -> silent no-op
    };

    let git = gitmeta::git_meta(path);
    let (namespace, git_basename) = match git.remote_url.as_deref() {
        Some(url) => gitmeta::parse_remote_url(url),
        None => (None, None),
    };
    let env_service = std::env::var("OTEL_SERVICE_NAME").ok();
    let dir_name = path
        .canonicalize()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
        .unwrap_or_else(|| "stratify".to_string());
    let service_name = stratify_telemetry::resolve_service_name(
        project.as_deref(),
        env_service.as_deref(),
        git_basename.as_deref(),
        &dir_name,
    );
    let headers = std::env::var("OTEL_EXPORTER_OTLP_HEADERS")
        .ok()
        .map(|h| stratify_telemetry::parse_headers(&h))
        .unwrap_or_default();

    let metrics = stratify_telemetry::report_to_metrics(
        report,
        stats.files_scanned,
        stats.functions,
        stats.complexity_max,
        stats.complexity_mean,
        stats.duration_ms,
    );
    let event = stratify_telemetry::report_to_event(
        report,
        &git,
        &service_name,
        stats.duration_ms,
        &stats.languages,
    );
    let config = stratify_telemetry::TelemetryConfig {
        endpoint,
        headers,
        service_name,
        namespace,
        version: env!("CARGO_PKG_VERSION").to_string(),
    };
    if let Err(e) = stratify_telemetry::emit(&metrics, &event, &config) {
        eprintln!("warning: telemetry export failed: {e}");
    }
}
```

Note: `report_to_event` takes `&git` (`GitMeta`), which `git_meta` returns. `stats.languages` is the `BTreeSet<String>` from Task 1. The `gitmeta` module already returns `stratify_telemetry::GitMeta`, so the types line up.

- [ ] **Step 4: Build and run the unit suites**

Run: `cargo build && cargo test -p stratify-cli -p stratify-telemetry`
Expected: PASS. Builds the binary with the new flags and helper.

- [ ] **Step 5: Commit**

```bash
git add crates/stratify-telemetry/src/emit.rs crates/stratify-telemetry/src/lib.rs crates/stratify-cli/src/main.rs Cargo.lock
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(telemetry): OTLP exporter + wire into stratify check"
```

---

## Task 6: Graceful-failure e2e, docs, lint

**Files:**
- Create: `crates/stratify-cli/tests/e2e_otlp.rs`
- Modify: `README.md`

- [ ] **Step 1: Write the graceful-failure e2e**

A bad endpoint must not change the scan's stdout or exit code. Use the existing `sample-go` fixture (has findings) and an unreachable endpoint. Create `crates/stratify-cli/tests/e2e_otlp.rs`:

```rust
use std::path::Path;
use std::process::Command;

fn run_check(extra: &[&str]) -> (String, Option<i32>) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sample-go");
    let mut args = vec!["check", dir.to_str().unwrap(), "--format", "json"];
    args.extend_from_slice(extra);
    let output = Command::new(env!("CARGO_BIN_EXE_stratify"))
        .args(&args)
        .output()
        .expect("run stratify binary");
    (String::from_utf8(output.stdout).unwrap(), output.status.code())
}

#[test]
fn bad_otlp_endpoint_does_not_change_scan_result() {
    let (baseline, baseline_code) = run_check(&[]);
    // Unreachable endpoint (port 1). Export fails; scan output must be identical.
    let (with_otlp, otlp_code) =
        run_check(&["--otlp-endpoint", "http://127.0.0.1:1"]);

    assert_eq!(
        baseline, with_otlp,
        "stdout (JSON findings) must be unchanged by telemetry"
    );
    assert_eq!(baseline_code, otlp_code, "exit code must be unchanged");
    // Sanity: the fixture actually produces findings, so the comparison is meaningful.
    assert!(baseline.contains("\"findings\""), "baseline: {baseline}");
}
```

- [ ] **Step 2: Run the e2e**

Run: `cargo test -p stratify-cli --test e2e_otlp`
Expected: PASS. The export attempt to `127.0.0.1:1` fails fast; the warning goes to stderr (not captured here), and stdout/exit code match the baseline. If the export hangs instead of failing fast, the exporter must use a short connect timeout; rebuild with the OTLP exporter's default timeout (the reqwest-blocking client fails quickly on a closed port, so a hang indicates the wrong client feature).

- [ ] **Step 3: Update the README**

In `README.md`, add a new section after the editor-LSP section (before "## Layer boundaries", or at a natural spot near the other surfaces):

```markdown
## Telemetry (OpenTelemetry / Datadog)

`stratify check` can push results to any OpenTelemetry backend over OTLP, so
many projects roll up into one dashboard. It emits only when an OTLP endpoint
is configured, otherwise it does nothing.

```sh
# Standard OTel env vars
export OTEL_EXPORTER_OTLP_ENDPOINT=https://otlp.example.com
export OTEL_SERVICE_NAME=my-service   # optional; defaults to the git repo name
stratify check .

# Or via flags (override the env vars)
stratify check . --otlp-endpoint https://otlp.example.com --project my-service
```

It sends gauges (`stratify.findings` by rule/severity/language/confidence,
`stratify.complexity.max`/`.mean`, `stratify.cycles`,
`stratify.boundary_violations`, `stratify.duplication.regions`,
`stratify.files_scanned`, `stratify.functions`, `stratify.scan.duration_ms`)
and one `stratify.run` log event per run carrying the git commit, branch, and
finding totals. Each run is tagged with `service.name` (the project) so a
single dashboard templates across your repos.

**Datadog:** point the endpoint at Datadog's OTLP intake and pass the API key
as a header:

```sh
export OTEL_EXPORTER_OTLP_ENDPOINT=https://otlp.datadoghq.com
export OTEL_EXPORTER_OTLP_HEADERS=DD-API-KEY=<your-key>
stratify check .
```

Telemetry never fails the scan: export errors print a warning to stderr and the
exit code still follows `--fail-on`.
```
````

(Note: the README already uses fenced code; match its existing fence style. If the triple-backtick nesting above is awkward, write the section with the same fence conventions as the surrounding README.)

- [ ] **Step 4: Format, lint, full suite**

Run: `cargo fmt --all && cargo clippy --all-targets -- -D warnings`
Fix any warning properly (no blanket `#[allow]`). Then:
Run: `cargo test`
Expected: all crates green, including the new telemetry and e2e tests.

- [ ] **Step 5: Commit**

```bash
git add -A
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "test(telemetry): graceful-failure e2e; docs; fmt + clippy clean"
```

---

## Self-Review Notes

Spec coverage for M18:
- Activation when endpoint resolves, env + flag override, silent no-op otherwise: Task 5 `maybe_emit_telemetry`. Covered.
- Endpoint/headers/project/namespace resolution precedence: Task 3 `resolve_service_name` + `parse_headers`, Task 4 `parse_remote_url`, Task 5 wiring. Covered.
- Only `check` emits (not mcp/lsp): Task 5 wires only the `Check` arm; `analyze_repo` wrapper keeps mcp/lsp unchanged. Covered.
- OTLP over HTTP/protobuf, reqwest blocking, one push + flush + shutdown: Task 2 deps, Task 5 `emit`. Covered.
- Resource attributes service.name/namespace/version: Task 5 `resource`. Covered.
- Metrics set (findings grouped + scalars): Task 2 `report_to_metrics`. Covered.
- Cardinality guardrail (no SHA on metrics): metric attributes are only rule/severity/language/confidence; SHA only in the event. Covered.
- Per-run event with commit/branch/totals/duration/languages: Task 3 `report_to_event`. Covered.
- ScanStats threaded from the graph: Task 1. Covered.
- GitMeta best-effort, None outside repo: Task 4. Covered.
- Error handling: failure to stderr, scan unaffected: Task 5 helper + Task 6 e2e proves it. Covered.
- Testing (URL parse, precedence, headers, mapping, event, graceful e2e): Tasks 2-4, 6. Covered.

Type consistency check:
- `ScanStats` (Task 1) fields `files_scanned/functions/complexity_max/complexity_mean/languages/duration_ms` are consumed exactly by the Task 5 calls to `report_to_metrics`/`report_to_event`.
- `report_to_metrics(report, files_scanned, functions, complexity_max, complexity_mean, duration_ms)` signature matches between Task 2 definition and Task 5 call.
- `report_to_event(report, &GitMeta, project, duration_ms, &BTreeSet<String>)` matches between Task 3 definition and Task 5 call.
- `GitMeta` is defined once in `stratify-telemetry` (Task 3) and reused by `gitmeta.rs` (Task 4) and the CLI (Task 5) - single source of truth, no duplicate type.
- `TelemetryConfig`/`emit` (Task 5) field names match the `maybe_emit_telemetry` constructor.

Known version sensitivity (acceptable, flagged in Task 5 Step 1/2): the `opentelemetry` SDK builder method names (`Resource::builder`, `SdkMeterProvider::builder`, `PeriodicReader::builder`, `SdkLoggerProvider::builder`, `MetricExporter`/`LogExporter` builders) target 0.27. The contract is fixed; if the resolved minor version renames a builder, adjust to docs.rs while preserving the contract. The pure mapping functions (the bulk of the tested logic) are version-independent.
```

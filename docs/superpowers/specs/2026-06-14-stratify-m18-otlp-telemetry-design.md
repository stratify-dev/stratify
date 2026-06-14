# Stratify M18: OTLP Telemetry Export — Design

**Date:** 2026-06-14
**Status:** Approved (design), pending spec review

## Goal

Let a `stratify check` run push its results to any OpenTelemetry backend over OTLP, so results across many projects aggregate into one dashboard. Datadog works through its native OTLP intake. Self-hosted Collector + Prometheus + Grafana works the same way. No vendor lock-in.

## Why OTLP-first

Stratify is a batch job. It scans once and exits. The fit is a single OTLP push at the end of a run, force-flushed before exit. A central backend stores and aggregates. Each project is identified by the `service.name` resource attribute, so one templated dashboard spans the whole fleet. The natural emit point for many projects is CI: each repo's existing Stratify GitHub Action gains an endpoint, and the dashboard trends by repo over PR and main-branch runs.

## Activation and configuration

Telemetry emits only when an OTLP endpoint resolves. With no endpoint, the exporter is a silent no-op (no network, no warning). Resolution honors standard OTel env vars as the base, with thin CLI flag overrides:

- **Endpoint:** `--otlp-endpoint` > `OTEL_EXPORTER_OTLP_ENDPOINT`
- **Headers** (auth, e.g. Datadog `DD-API-KEY`): `OTEL_EXPORTER_OTLP_HEADERS` (standard `k1=v1,k2=v2` form). Env only, no flag.
- **Project** (`service.name`): `--project` > `OTEL_SERVICE_NAME` > git `origin` remote basename (`repo` from `org/repo`) > scan directory name
- **Namespace** (`service.namespace`, the org): git `origin` remote org segment, when present

Only `stratify check` emits. `stratify mcp` and `stratify lsp` do not.

The OTel deps are always compiled into the binary. One install story, no feature variants.

## Transport

OTLP over HTTP/protobuf using the reqwest **blocking** client, so the CLI needs no async runtime. Not gRPC. CI runners and proxies handle HTTP cleanly, and Datadog's OTLP intake accepts HTTP/protobuf. One export call at end of run, then force-flush and shutdown before process exit.

## Data model

### Resource attributes (project identity across the fleet)

- `service.name` — project (resolved as above)
- `service.namespace` — org, when derivable from the remote
- `service.version` — Stratify version (`CARGO_PKG_VERSION`)

### Metrics (low cardinality, recorded once per run)

Each is an OTLP gauge recorded a single time per run.

- `stratify.findings` — one data point per distinct `{rule, severity, language, confidence}` combination, value = count of findings in that combination
- `stratify.complexity.max` — highest function cyclomatic complexity in the repo
- `stratify.complexity.mean` — mean function cyclomatic complexity
- `stratify.cycles` — number of dependency-cycle findings
- `stratify.boundary_violations` — number of layer-boundary findings
- `stratify.duplication.regions` — number of duplication findings
- `stratify.files_scanned` — number of File symbols
- `stratify.functions` — number of Function symbols
- `stratify.scan.duration_ms` — wall-clock analysis time

`language` is derived from the finding's file extension (`java`, `ruby`, `typescript`, `python`, `go`). `rule`, `severity`, and `confidence` come straight off the `Finding`.

**Cardinality guardrail:** no commit SHA, branch, file path, or message on any metric attribute. Metric attributes stay in the fixed low-cardinality set above.

### Per-run event (one OTLP log record)

Carries the high-cardinality fields metrics must not hold, so a metric change links back to a commit.

- **body:** `stratify.run`
- **attributes:** `project`, `commit` (full SHA), `branch`, `total_findings`, per-severity counts (`info`, `warning`, `error`), per-rule counts, `duration_ms`, `languages` (sorted, comma-joined)

Git fields are absent when the scan root is not a git repo. Metrics still emit in that case.

## Architecture

### New crate: `stratify-telemetry`

Depends on `stratify-core` (the `Report`, `Finding`, `Severity`, `Confidence` types) and the OTel stack. Two pure mapping functions hold all the logic and are unit-tested with no live exporter:

- `report_to_metrics(report: &Report, stats: &ScanStats) -> Vec<MetricPoint>`
- `report_to_event(report: &Report, stats: &ScanStats, git: &GitMeta, project: &str) -> RunEvent`

Supporting types in the crate:

- `MetricPoint { name: String, value: f64, attributes: Vec<(String, String)> }`
- `RunEvent { body: String, attributes: Vec<(String, AttrValue)> }` where `AttrValue` is `Str(String) | Int(i64) | Float(f64)`
- `TelemetryConfig { endpoint: String, headers: Vec<(String, String)>, service_name: String, namespace: Option<String>, version: String }`
- `GitMeta { commit: Option<String>, branch: Option<String>, remote_url: Option<String> }`

A thin exporter wraps the pure functions:

- `emit(metrics: &[MetricPoint], event: &RunEvent, config: &TelemetryConfig) -> Result<(), TelemetryError>` builds the OTLP metric + log exporters with the resource attributes, records each `MetricPoint` as a gauge and the `RunEvent` as a log record, force-flushes, and shuts down.

The mapping is fully testable without a network. The exporter is the only part touching the OTel SDK and the wire.

### CLI changes (`stratify-cli`)

- **`run.rs`:** add `pub struct ScanStats { files_scanned: u64, functions: u64, complexity_max: u32, complexity_mean: f64, languages: BTreeSet<String>, duration_ms: u64 }` and `pub fn analyze_repo_with_stats(root) -> io::Result<(Report, ScanStats)>` that measures wall time and computes `ScanStats` from the `IrGraph` before it is dropped. `analyze_repo` delegates to it and returns just the `Report`, so existing `mcp`/`lsp` callers are untouched.
- **`gitmeta.rs` (new):** `git_meta(root) -> GitMeta` shells `git rev-parse HEAD`, `git rev-parse --abbrev-ref HEAD`, and `git remote get-url origin`, following the existing `churn.rs` git-invocation pattern. Returns `None` fields gracefully outside a repo. A helper parses a remote URL (https and ssh forms) into `(namespace, project)`.
- **`check` handler:** new flags `--otlp-endpoint <url>` and `--project <name>`. After `analyze_repo_with_stats`, resolve `TelemetryConfig`. If no endpoint resolves, skip. Otherwise build metrics + event from the pure functions and call `emit`.

## Error handling

Telemetry failure never fails the scan. `emit` returns `Result`. On error the `check` handler writes `warning: telemetry export failed: <e>` to **stderr** and continues with the normal analysis exit code (driven by `--fail-on`). Warnings go to stderr so `--format json` / `sarif` stdout stays valid for piping.

## Testing

- **Remote-URL parsing:** `https://github.com/org/repo.git` and `git@github.com:org/repo.git` both yield `(org, repo)`; non-GitHub and malformed URLs degrade to `None` namespace and a best-effort basename.
- **Config resolution precedence:** `resolve_service_name` returns flag > env > git basename > dir name, with each layer tested in isolation.
- **Header parsing:** `k1=v1,k2=v2` parses to two pairs; empty string yields none; a malformed entry is skipped.
- **`report_to_metrics`:** a `Report` with a mix of rules, severities, languages, and confidences plus a `ScanStats` produces the expected `MetricPoint` set (correct `stratify.findings` grouping and counts, correct gauges).
- **`report_to_event`:** asserts body `stratify.run` and attributes `commit`, `branch`, `total_findings`, per-severity and per-rule counts, `duration_ms`, `languages`.
- **Graceful-failure e2e:** `stratify check <fixture> --otlp-endpoint http://127.0.0.1:1/` (unreachable) exits with the same code and prints the same findings as a no-endpoint run, with the telemetry warning on stderr only.

## Out of scope (YAGNI)

- Traces / span tree for per-phase latency (the chosen scope is metrics + per-run event)
- A `--otlp-required` mode that fails the build on export error
- Telemetry from `mcp` / `lsp`
- gRPC transport
- A bundled dashboard definition (documented backend setup is enough for v1)

## File structure

```
crates/stratify-telemetry/                       CREATE: new crate
  Cargo.toml
  src/lib.rs                                      mapping types + report_to_metrics/report_to_event
  src/emit.rs                                     OTLP exporter wrapper (emit)
crates/stratify-cli/src/run.rs                    MODIFY: ScanStats + analyze_repo_with_stats
crates/stratify-cli/src/gitmeta.rs                CREATE: git_meta + remote-URL parsing
crates/stratify-cli/src/main.rs (or cli.rs)       MODIFY: --otlp-endpoint/--project flags, emit wiring
crates/stratify-cli/Cargo.toml                    MODIFY: depend on stratify-telemetry
crates/stratify-cli/tests/e2e_otlp.rs             CREATE: graceful-failure e2e
README.md                                         MODIFY: telemetry section (OTel env vars + Datadog OTLP intake)
```

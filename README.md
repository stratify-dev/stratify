# Stratify

Stratify is a polyglot codebase-intelligence tool that parses repos into one language-agnostic IR and runs static analyses. It speaks Java, Ruby, TypeScript, Python, and Go through a single binary.

## Status

Working today:

- Six analyses: dead code, duplication, complexity, churn hotspots, dependency cycles, layer boundaries
- Five languages, each with the full analysis set: Java, Ruby, TypeScript, Python, Go
- Cross-file call resolution and package-aware import resolution (Go packages, Python `__init__.py`)
- Surfaces: CLI (human + JSON + SARIF), GitHub Action quality gate, MCP server, and an LSP for inline editor diagnostics
- `--fail-on` exit-code control for CI quality gates

Go support is complete: all six analyses run on Go code, with package-directory import resolution so cycles and layer boundaries see package-level dependencies.

## Install

```sh
cargo install --git https://github.com/stratify-dev/stratify stratify-cli --locked
```

The binary is `stratify`.

## Usage

```sh
# Analyse the current directory, human output
stratify check .

# JSON output
stratify check . --format json

# Fail the process when any warning-or-above finding is found
stratify check . --fail-on warning
```

Sample output:

```
warn  Unused.java:2  unused function `neverCalled`
info  App.java:6  possibly unused function `helper`

2 finding(s).
```

`--fail-on` accepts `never` (default, always exits 0), `info`, `warning`, or `error`. The GitHub Action input `fail-on` defaults to `warning` instead, so dead code fails the build.

## Use as a GitHub Action

Add Stratify as a quality gate in any workflow:

```yaml
- uses: actions/checkout@v4
- uses: stratify-dev/stratify@main
  with:
    path: .
    fail-on: warning
```

Pin to a released tag (for example `@v1`) once releases are published. `@main` tracks the latest.

The first run compiles Stratify from source, which takes a few minutes.

### Inputs

| Input | Default | Description |
|-------|---------|-------------|
| `path` | `.` | Directory to analyse. |
| `fail-on` | `warning` | Minimum severity that fails the step: `never`, `info`, `warning`, or `error`. |
| `format` | `human` | Output format: `human`, `json`, or `sarif`. |

The step exits non-zero (and fails the job) when at least one finding meets or exceeds the `fail-on` threshold.

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

## MCP server

Stratify speaks the Model Context Protocol so coding agents can query findings:

```sh
stratify mcp
```

It runs a stdio JSON-RPC server exposing one tool, `analyze`, which takes a
`path` (and an optional `rule` filter) and returns the findings as JSON.

Register it in an MCP client. For Claude Code:

```json
{
  "mcpServers": {
    "stratify": { "command": "stratify", "args": ["mcp"] }
  }
}
```

The agent can then call `analyze` with `{ "path": ".", "rule": "dead_code" }`
to get structured findings without parsing CLI output.

## Editor diagnostics (LSP)

Stratify ships a Language Server so findings appear inline in your editor:

```sh
stratify lsp
```

It runs a stdio Language Server. On open and save it analyzes the workspace and
publishes diagnostics (dead code, duplication, complexity, hotspots, cycles,
boundary violations), each tagged with its rule as the diagnostic code.

Point your editor's LSP client at the `stratify lsp` command for files in the
workspace. The server reads the workspace root from the `initialize` request.

## Layer boundaries

Enforce architecture layers in `stratify.toml`:

```toml
preset = "rails"   # or "layered" for controller/service/repository/domain
```

A preset ships layer globs and forbidden imports. The `rails` preset forbids models from importing controllers, views, or mailers. The `layered` preset enforces the controller/service/repository/domain stack common in Spring, NestJS, and similar frameworks.

Add your own `[layers]` and `[[forbid]]` entries to extend or override a preset. User-defined layer keys replace preset keys of the same name. User `[[forbid]]` rules are appended to the preset rules.

```toml
preset = "rails"

[layers]
models = ["lib/models/**"]   # replaces the preset's app/models/** glob

[[forbid]]
from = "models"
to = "jobs"
```

With no `stratify.toml`, Stratify auto-detects a Rails app (`app/controllers/` directory) or a Maven/Gradle project (`pom.xml` or `build.gradle`) and applies the matching preset. A project that matches no marker gets no boundary checks.

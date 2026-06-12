# Stratify Design

Date: 2026-06-12
Status: Approved design, ready for implementation planning

## Summary

Stratify is a deterministic codebase-intelligence tool. It parses a repository into a
language-agnostic graph, runs static analyses on that graph, and reports findings for
humans, CI, and AI agents. One fast binary, written in Rust, on top of tree-sitter.

It starts with Ruby and Java and is built to extend to more languages by adding one
adapter per language. Inspiration is fallow (JS/TS, Rust). Stratify generalizes the same
idea to a polyglot engine where analyses are written once and run on every language.

## Goals

- Ship a real, adoptable product, not a personal script.
- Cover four analyses in v1: dead code, duplication, complexity and hotspots, architecture boundaries.
- Support Ruby and Java in v1.
- Expose findings through CLI, JSON, CI (SARIF), MCP, and LSP.
- Make adding a new language cheap: one adapter, no analysis changes.

## Non-goals (v1)

- Runtime or production-coverage layer (fallow's optional V8 feature). No clean cross-language analog yet. Revisit post-v1.
- Dependency-hygiene beyond unused-dependency detection. Revisit post-v1.
- Auto-fix or code modification. Stratify reports, it does not rewrite.

## Core decision: universal IR

Stratify parses every language into one shared intermediate representation (IR). All
analyses read only the IR. They never call tree-sitter and never branch on language.

This isolates the hard, language-specific work (parsing and symbol resolution) behind a
single trait. Ruby's dynamism can degrade resolution quality without leaking into any
analysis. Adding a language means writing one adapter that fills the IR.

## Architecture

Pipeline:

```
discover files
  -> per-language parse + resolve (tree-sitter)  [LanguageAdapter]
  -> emit IR graph
  -> language-agnostic analyses read IR
  -> findings
  -> renderers (CLI / JSON / SARIF / MCP / LSP)
```

Everything after the IR is language-blind.

## The IR (the contract)

A directed graph.

Nodes (`Symbol`):

- Kinds: file, module/namespace, class, method/function, constant, dependency.
- Fields: id, kind, name, fully-qualified path, source span, visibility, confidence.

Edges (`Reference`):

- Kinds: `defines`, `calls`, `imports`/`requires`, `inherits`, `references`.
- Fields: source span, confidence.

Confidence is first-class. Java resolves statically with high confidence. Ruby often
resolves at "likely" or "unknown". Analyses use confidence to avoid false claims. This is
the single most important design decision for trustworthiness: a low-confidence edge
downgrades a "dead" verdict to "possibly unused", never the reverse.

## Crate layout

```
stratify-core        IR types, graph, confidence, finding schema
stratify-lang        LanguageAdapter trait + shared resolution helpers
stratify-lang-ruby   tree-sitter-ruby  -> IR
stratify-lang-java   tree-sitter-java  -> IR
stratify-analysis    deadcode, duplication, complexity, boundaries (IR-only)
stratify-report      renderers: human CLI, JSON, SARIF
stratify-cli         argument parsing, config, orchestration; builds the `stratify` binary
stratify-mcp         MCP server over the JSON contract
stratify-lsp         LSP server over the finding stream
```

One responsibility per crate, each testable alone. A new language adds one
`stratify-lang-*` crate and changes nothing else.

Note on publishing: the bare `strata` crate name is taken and abandoned on crates.io. The
`stratify` name is free. The user-facing command is `stratify`.

## The four analyses (written once, on IR)

### Dead code

Graph reachability from entrypoints.

- Ruby entrypoints: `bin/`, config and routes, rake tasks.
- Java entrypoints: `main`, framework annotations (controllers, jobs).
- Unreached symbols with only high-confidence edges = dead.
- Any symbol touching a low-confidence edge = "possibly unused", never "dead".
- Unused dependencies: manifest (`Gemfile`, `pom.xml`, `build.gradle`) minus imported packages.

### Duplication

Suffix-array over a normalized token stream emitted from the IR. Language-agnostic by
construction once the IR exists.

### Complexity and hotspots

Cyclomatic and cognitive complexity per function from the tree-sitter parse, crossed with
`git log` churn. Ranks "complex and frequently changed" code for review focus.

### Architecture boundaries

Rules over the import graph: no cycles, layer X must not import layer Y. Ships with
zero-config presets (Rails layout, Maven/Gradle standard layout) plus a `stratify.toml`
override.

## Surfaces

- CLI + JSON: `stratify check .` for humans, `--format json` for machines. The versioned JSON schema is the contract every other surface reuses.
- CI: SARIF output (GitHub and GitLab render it natively), threshold config, non-zero exit on violations.
- MCP server: tools such as `analyze_repo`, `find_dead_code`, `explain_finding` over the same JSON. Agents query the repo instead of guessing.
- LSP server: diagnostics from the finding stream, inline in editors. Highest effort, lands last.

## Milestones

Each milestone is independently usable and shippable.

- M1 walking skeleton: file discovery + Java adapter + dead-code + JSON/CLI. Proves the IR end to end.
- M2: add Ruby adapter against the same dead-code analysis. The real test of the universal-IR bet.
- M3: duplication + complexity/hotspots (both languages free, IR-only).
- M4: architecture boundaries + presets + `stratify.toml`.
- M5: SARIF + CI packaging.
- M6: MCP server.
- M7: LSP.

A usable tool exists at M1 and a compelling polyglot demo at M2.

## Testing strategy

- Language adapters: fixture repos with known symbols, assert the emitted IR.
- Analyses: hand-built IR graphs (no parsing), assert findings, including confidence handling.
- Renderers: golden-file tests on JSON and SARIF output.
- End-to-end: run against a checked-in sample Ruby app and Java app.

## Risks

- Ruby symbol resolution is hard (metaprogramming, open classes, dynamic dispatch). Mitigation: confidence model degrades verdicts instead of producing false positives. M2 is the go/no-go checkpoint for the universal-IR approach.
- Scope is large for a v1 (two languages, four analyses, four surfaces). Mitigation: the milestone sequence ships value early and defers LSP and MCP to the end.
- tree-sitter grammar gaps for edge syntax. Mitigation: treat unparsable regions as low-confidence rather than failing the run.

# Changelog

All notable changes to Stratify are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and versioning follows [Semantic Versioning](https://semver.org/) (pre-1.0,
so a minor bump can still carry a behavior change; patch bumps stay
backward-compatible).

## [0.5.0] - 2026-09-01

Fixes from a tool-comparison exercise: hand-built fixtures covering all six
analyses across all six languages, run through Stratify and an established
per-language tool for the same job (ESLint, PMD, rubocop, gocyclo, clippy,
jscpd, madge, dependency-cruiser, import-linter, pylint, vulture, debride,
the Go compiler, rustc), then diffed. Full backlog:
`docs/backlog/2026-08-16-tool-comparison.md`.

### Added
- `[dead_code] mode` in `stratify.toml`: `"library"` (default) or
  `"application"`. An unreached `public`/`export`/`pub` symbol now reports at
  reduced confidence (`possibly unused`, Info) instead of either staying
  fully silent (TypeScript, Go, Rust) or a full `unused` Warning (Java),
  matching the confidence tier a low-confidence cross-file match already
  gets. `mode = "application"` restores full-strength reporting for a repo
  nothing outside it imports from.
- Java methods now carry real visibility: `public` methods get
  `Visibility::Public`; package-private and `private` do not. Previously
  every Java symbol was `Visibility::Unknown`.
- TypeScript, Go, and Rust `pub`/`export`/exported symbols carry
  `Visibility::Public` instead of being hard-marked as unconditional
  entrypoints — they're now evaluated for reachability like everything
  else, with `[dead_code] mode` controlling the confidence tier when
  unreached. `main`, `init`, `#[test]`s, and trait-impl methods remain true
  hard entrypoints in every language, unaffected by this change.

### Fixed
- **Java dependency-cycle detection didn't fire on realistic code.** A
  genuine mutual import between two classes in different packages produced
  zero cycle findings. The cycle graph keyed its adjacency map by each
  file's own fqn but inserted edges pointing at the imported *class's* fqn —
  two namespaces that never intersected for Java, so the cycle DFS could
  never close the loop. Since real Java import cycles are almost always
  between differently-named classes, cycle detection was effectively
  non-functional for Java despite being advertised as fully supported.
- **Duplication's same-file overlap suppression (from the 2026-06-16 fix,
  B1+B2) had a gap for longer repetitive ladders.** It only suppressed a
  same-file match within one `min_tokens` window of the earliest occurrence.
  A long enough if/else-if or match/switch chain drifts past that single-hop
  check even though every consecutive branch sits well within range,
  reproducing the original false-positive symptom on a longer ladder.
  Reproduced independently in from-scratch TypeScript and Rust fixtures.
  Now suppresses the whole connected chain, not just members within one
  direct hop of the anchor.
- **Rust call-graph extraction never recorded a call made through a
  qualified path** (`module::function()`, `Type::method()`) — only a bare
  identifier or a `.method()` call. This is the ordinary way to call across
  crates or modules in Rust, so a `pub` function called only that way looked
  unreachable. Found by self-scanning Stratify's own multi-crate workspace
  after the `pub`-visibility change above: two genuinely-used private
  helpers in `stratify-telemetry` showed up as dead. Always present, only
  invisible before because unconditional `pub`-entrypoint marking made a
  function's own reachability irrelevant.
- Java's cyclomatic complexity no longer counts a `switch`'s `default:`
  label as a decision point (tree-sitter's Java grammar gives it the same
  node kind as a `case` label; the shared walker can't tell them apart by
  kind alone). Matches PMD's `CyclomaticComplexity` rule and the common
  McCabe convention — `default:` is the fallthrough, not an added branch.

### Documentation
- Noted that Stratify's complexity is specifically cyclomatic complexity,
  and that a cognitive-complexity tool (clippy's `cognitive_complexity`,
  SonarQube) can diverge by an order of magnitude on `match`/`switch`-heavy
  code — a metric-definition difference, not a tool disagreement.
- Documented the new `[dead_code] mode` setting.

## [0.4.0] - 2026-06-16

### Added
- Churn hotspot findings gain a complexity floor (a simple function isn't a
  hotspot no matter how often it changes) and report at Info severity — an
  advisory prioritization signal, not a gate-failing warning.

## [0.3.1] - 2026-06-16

### Fixed
- Dead-code analysis promotes an unambiguous intra-file call to Certain
  confidence, so a function only ever called from elsewhere in the same
  file stops being flagged as `possibly unused`.

## [0.3.0] - 2026-06-16

### Added
- `[duplication] min_tokens` is configurable via `stratify.toml`; the
  default was raised to cut noise from short structurally-parallel
  fragments that naturally recur across the six language adapters.
- Shared tree-walking helpers (span, tokenize, cyclomatic complexity,
  enclosing-symbol lookup) factored out across all six language adapters.
- CI runs Stratify on every commit (report-only) and uploads SARIF to code
  scanning; test fixtures are excluded from self-scan.

### Fixed
- Calls hidden inside macro invocations are recovered for Rust, instead of
  being invisible to the call graph and producing false "certain" dead-code
  findings.

## [0.2.0] - 2026-06-16

### Added
- Rust language adapter: dead code, duplication, complexity, and churn
  hotspots. Dependency cycles and layer boundaries were left for later —
  Rust's `use`/module resolution wasn't in place yet.
- `[ignore]` exclude globs in `stratify.toml`.

### Fixed
- Duplication reported a clone pair bidirectionally (once from each side).
  Now reports once per clone cluster, from its earliest occurrence, and
  suppresses same-file matches that overlap or sit immediately adjacent to
  each other (a repetitive ladder self-matching, not an actionable clone).

## [0.1.0] - 2026-06-14

Initial release. Six analyses (dead code, duplication, complexity, churn
hotspots, dependency cycles, layer boundaries) over one shared
language-agnostic IR, covering Java, Ruby, TypeScript, Python, and Go.

- Dead-code reachability with confidence downgrade (`unused` vs.
  `possibly unused`), cross-file call resolution.
- Duplication detection over normalized IR tokens.
- Cyclomatic complexity per function.
- Churn hotspots (complexity × git history).
- Dependency-cycle detection over the cross-file import graph, aware of
  Go's package-level imports and Python's `__init__.py` package resolution.
- Layer-boundary rules via `stratify.toml`, with `rails` and `layered`
  presets and layout auto-detection.
- Output: human-readable, JSON, and SARIF 2.1.0.
- Surfaces: `stratify check` CLI with `--fail-on` as a CI gate, a GitHub
  Action, an MCP server (`stratify mcp`) for coding agents, an LSP server
  (`stratify lsp`) for editor diagnostics, and OpenTelemetry/Datadog export.
- Distribution: prebuilt cross-platform binaries via cargo-dist, a Homebrew
  tap, and a shell installer.

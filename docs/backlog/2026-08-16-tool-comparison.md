# Tool-comparison backlog (2026-08-16)

## How this was produced

Built a small, hand-crafted fixture per supported language (TypeScript, Python, Java,
Ruby, Go, Rust), each with one deliberately-planted instance of every finding type
Stratify's engine detects, with numeric targets (complexity thresholds, hotspot score,
duplication token count) computed from the engine's own source in advance. Ran every
fixture through Stratify and through at least one independent, established tool for the
same job in that language (ESLint, knip, madge, dependency-cruiser, vulture, radon,
pylint, import-linter, PMD, rubocop, debride, `golang.org/x/tools/cmd/deadcode`, gocyclo,
the Go compiler, rustc, clippy, and jscpd across every language for duplication).

Full method, per-language detail, and the complete comparison — including everywhere
Stratify agreed with the established tools, which was most of the result — is written up
separately (not committed to this repo; available on request). This file carries only the
items that need engineering decisions or fixes, highest value first.

None of the fixtures or comparison tooling used to produce this are part of this repo.

---

## C1 — Java dependency-cycle detection does not fire on realistic code [High]

**Symptom:** a genuine mutual import between two Java classes in different packages
(`pkga.ClassA` imports `pkgb.ClassB` and vice versa — the ordinary shape of a real Java
import cycle) produces zero cycle findings, even though the README lists Java among the
languages that get the full six analyses.

**Root cause:** `crates/stratify-lang-java/src/extract.rs:274-281` gives a Java File
symbol `fqn: file.to_string()` (the raw file path). `extract.rs:151-155` gives a Java
Class symbol `fqn: format!("{}.{name}", ctx.pkg)` (`package.ClassName`). The cycle graph
builder in `crates/stratify-analysis/src/imports.rs:60-107` keys its adjacency map by
File fqn but inserts edges pointing at Class fqn — two namespaces that never intersect for
Java. The DFS walks one hop to a dangling node; the back-edge that would close the cycle
is never found, in either direction. Confirmed reproducible from a from-scratch,
independently-built two-package fixture — not a corner case, the ordinary shape of a Java
import.

Go and TypeScript don't have this problem: `crates/stratify-lang-go/src/extract.rs:75-112`
gives a Go File the same path-based fqn its own imports resolve into, and TypeScript's
file fqn is self-consistent the same way. Java is the one language where the two
namespaces genuinely don't line up.

**Impact:** since the overwhelming majority of real Java import cycles are between
differently-named classes (not two classes crammed in one file), Java cycle detection is
likely non-functional on real Java code today, despite being advertised as fully
supported.

**Fix:** give Java File symbols an fqn in the same namespace their own class-level import
edges resolve into (e.g. the file's package name, or resolve edges through the file's own
class list rather than a raw Class fqn), so the cycle DFS can close the loop the way it
already does for Go.

**Tests:** two Java classes in different packages, each importing and referencing the
other, should produce exactly one cycle finding (matching the existing single-report dedup
behavior already correct for TS/Go/Python).

---

## C2 — Duplication self-overlap suppression (B2) doesn't cover longer ladders [High]

**Symptom:** `docs/backlog/2026-06-16-self-analysis.md`'s B2 was fixed
(`b75d1f7`/`bdb5183`): a same-file match is suppressed when the two occurrences are within
`min_tokens` of each other in the token stream. That guard has a gap the original bug
report's ~25-line example didn't surface. Two independent, unrelated fixtures — a
21-branch TypeScript if/else-if chain and a 21-branch Rust if/else-if chain — both
reproduce the exact original symptom: the function's own later branches get reported as
"duplicated... also at itself."

**Root cause:** `crates/stratify-analysis/src/duplication.rs`'s B2 guard
(`!(tokens[o].file == tokens[s].file && (o as isize - s as isize).abs() < k as isize)`)
only suppresses occurrences within one `min_tokens` window of each other. A repetitive
ladder long enough that its own repeat period exceeds `min_tokens` (100 by default) drifts
past the guard by around branch 9 in both reproductions, and suppression stops applying.

**Impact:** the functions most likely to trigger this are, by construction, exactly the
functions already flagged by the complexity analysis for having many branches — the
noisiest duplication false positives land on the same functions already carrying a
complexity warning.

**Fix:** the guard needs to reason about the whole cluster's shape (e.g. treat any chain
of same-file occurrences connected by gaps under `min_tokens` as one suppressed group,
transitively, rather than only comparing each pair against the single anchor occurrence),
not just the pairwise distance from the earliest occurrence.

**Tests:** a same-file if/elif ladder with enough branches that consecutive branch-pairs
sit within `min_tokens` of each other but the first and last branch don't (e.g. 20+
branches at the default 100-token threshold) should produce zero duplication findings
against itself, matching the original B2 test's intent at a larger scale.

---

## C3 — Java flags unused `public` methods the same as unused `private` ones [Medium]

**Symptom:** Stratify's Java adapter marks only a method literally named `main` as an
entrypoint (`crates/stratify-lang-java/src/extract.rs:143-166`) — no visibility check
anywhere. An unused `public` method (a getter consumed only by a serializer, a handler
invoked only by a framework via reflection) gets flagged `unused function` at the same
Warning severity as genuinely-dead private code. PMD's equivalent rule
(`UnusedPrivateMethod`) is deliberately scoped to private methods only, for exactly this
reason — a public method might be reached from outside what any static analysis can see.

**Impact:** real false-positive risk on real Java code, with no lower-confidence tier the
way cross-file "possibly unused" already gets for dead-code resolution elsewhere.

**Fix options, needs a decision:** either drop `public` methods from Java dead-code
detection entirely (matching TS/Go/Rust's export/pub protection), or keep flagging them
but at Info/Likely confidence rather than a flat Warning, mirroring the existing
possibly-unused tier used elsewhere in `deadcode.rs`.

---

## C4 — No signal distinguishes "library" from "application" repos, and it cuts both ways [Medium]

**Symptom, direction A (false negative):** TypeScript, Go, and Rust all treat every
exported/public symbol as a protected entrypoint, correct for a published library. In an
application-shaped repo (a real `main`, not a published package — the common case), this
produces false negatives both `golang.org/x/tools/cmd/deadcode` (true whole-program
reachability from a real `main`) and `knip` (project-aware dead-export detection) caught
cleanly and Stratify missed.

**Symptom, direction B (false positive):** see C3 — Java has no such protection at all,
producing the opposite failure.

**Impact:** the current behavior is a single hardcoded heuristic per language with no way
to tell it which situation it's in.

**Fix:** consider a config signal (e.g. `stratify.toml [dead_code] mode = "library" |
"application"`, or auto-detection from `package.json`/`go.mod`/`Cargo.toml` — `"private":
true`, no `main` package export, etc.) that lets export/pub protection be turned off for
application repos, and gives Java a path to add protection where it currently has none.

---

## C5 — Java cyclomatic complexity counts `default:` as a decision point; PMD doesn't [Low]

**Symptom:** isolated and confirmed exactly: a Java `switch` with a `default:` label
scores one higher under Stratify than under PMD's `CyclomaticComplexity` rule, every time;
removing the label removes the discrepancy.

**Root cause:** `crates/stratify-lang-java/src/extract.rs`'s complexity decision-kind list
includes `"switch_label"` unconditionally, and tree-sitter's Java grammar represents
`default:` as a `switch_label` node identical to a `case`. PMD (and the common McCabe
convention) only counts `case` labels, not the fallthrough `default`.

**Fix:** this is a methodology choice, not obviously a bug — decide deliberately whether
to keep counting `default:` (with a documented rationale) or drop it to match the more
common convention, rather than leave it as an unexplained one-off gap.

---

## C6 — Document that Rust's complexity comparison (clippy) measures a different metric [Low]

**Symptom:** clippy's `cognitive_complexity` lint and Stratify's cyclomatic complexity can
diverge by an order of magnitude on `match`-heavy code (13 vs. 2 on a 12-arm match,
confirmed in this run) — not a bug in either tool, they're answering different questions
(cyclomatic: how many paths exist; cognitive: how hard is this to read), but an
undocumented 6x gap reads like a defect to anyone comparing the two numbers.

**Fix:** a line in the docs (README or the six-analyses doc) noting Stratify reports
cyclomatic complexity specifically, and that cognitive-complexity tools (clippy, SonarQube)
measure something related but numerically different, especially on switch/match-shaped
code.

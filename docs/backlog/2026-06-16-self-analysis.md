# Stratify self-analysis backlog (2026-06-16)

## How this was produced

Ran the released engine against its own repository:

```sh
stratify check . --format json
```

Result: **27 findings, all from `crates/stratify-cli/tests/sample-*` fixtures. Zero from real source.**

| rule | count | where |
|------|-------|-------|
| dead_code | 22 | all in test fixtures |
| duplication | 3 | all in test fixtures |
| cycle | 1 | test fixture |
| complexity | 1 | test fixture |

The dead-code, cycle, and complexity findings are **correct** for the fixtures (they are intentionally broken test data). The duplication findings exposed two real engine bugs, and the all-fixtures result exposed a coverage gap and a verbosity problem. Items below, highest value first.

---

## B1 — Duplication clone pairs are reported bidirectionally (double-counted) [High]

**Symptom:** one clone pair produces two findings.

```
duplication  sample-dup/one.rb:1  duplicated code block (>= 40 tokens) also at sample-dup/two.rb:1
duplication  sample-dup/two.rb:1  duplicated code block (>= 40 tokens) also at sample-dup/one.rb:1
```

`one.rb` and `two.rb` are a single type-2 clone (same body, renamed variables). Detecting the clone is correct. Reporting it from both sides is not.

**Root cause:** `crates/stratify-analysis/src/duplication.rs:48-72`. The emit loop pushes a finding for every left-maximal duplicated region in the global token stream. Each half of a clone is a separate region, so both emit, each pointing at the other. There is no pair/cluster dedup. A block copied into N files would emit N findings (and the "also at" target is arbitrary).

**Fix:** collapse all occurrences of an identical region into one finding.
- Emit only for the occurrence with the smallest stream position in its group; skip the rest.
- Message should name the other location(s) and, for 3+ copies, the count, instead of a single arbitrary "also at".
- Mirror the canonical-dedup pattern already used in `cycles.rs` (`canonical_cycle` + `BTreeSet`).

**Tests:** a 2-file clone yields exactly one finding; a 3-file identical block yields one finding (not three, not six).

---

## B2 — Duplication fires on repetitive same-shape control flow (overlapping self-match) [High]

**Symptom:**

```
duplication  sample-complex/gnarly.rb:4  duplicated code block (>= 40 tokens) also at sample-complex/gnarly.rb:6
```

`gnarly.rb` is a single `if/elsif` ladder (lines 1-25). After normalization (numbers -> `NUM`, strings -> `STR`), every `elsif n < N` / `return "..."` branch normalizes identically, so a long repeated run trips the 40-token threshold. The two reported regions are ~2 lines apart and **overlap in the token stream** (`other - s` is well under the window length `k`). This is a natural ladder, not copy-paste, and the "line 4 also at line 6" framing is confusing.

**Root cause:** `duplication.rs` matches normalized token windows with no guard against overlapping or adjacent self-matches within the same file. A region reported as a clone of itself shifted by a few tokens is noise.

**Fix (minimum viable):** when `here.file == there.file`, drop the finding if the two regions overlap or are within the window (`abs(s - other) < k`). Optional follow-ups: require the two regions to live in different symbols (functions), or suppress windows made of a single repeating sub-pattern.

**Tests:** the `gnarly.rb` ladder yields no duplication finding; a genuine non-adjacent in-file copy-paste still does; cross-file clones are unaffected.

---

## B3 — No Rust adapter: Stratify cannot analyze its own codebase [High]

**Symptom:** 57 `.rs` source files are skipped. All findings come from non-Rust test fixtures. `stratify check .` on this repo reads only `.rb`/`.go`/`.java`/`.py`/`.ts` files, all of which are fixtures.

**Impact:** the project cannot dogfood itself, Rust users get an empty report, and "run against our own code" silently analyzes test data, which is misleading.

**Fix:** add a `stratify-lang-rust` crate (tree-sitter-rust), same shape as the existing adapters: File/Function/Struct/Enum symbols + `Defines`, normalized tokens, entrypoints (`fn main`, and a policy for `pub` items), intra-file `Calls`, cyclomatic complexity, and import edges (`use` / `mod`). Until it exists, the README should state that Rust is not yet supported.

---

## B4 — Scanning a project reports its own test fixtures (verbosity) [Medium]

**Symptom:** all 27 findings are intentionally-broken fixtures under `crates/stratify-cli/tests/sample-*`. On any real project, test fixtures, generated code, and vendored code get reported the same way.

**Impact:** noise. This is the main driver of the "too verbose on a real repo" impression, separate from B1/B2.

**Fix:** support exclude globs in `stratify.toml`, for example:

```toml
[ignore]
paths = ["**/tests/sample-*/**", "**/fixtures/**", "**/vendor/**"]
```

Honor them in `crates/stratify-cli/src/run.rs` where the `ignore::WalkBuilder` walks the tree (it already respects `.gitignore`; add the configured globs). Consider a small default ignore set.

**Tests:** a `stratify.toml` `[ignore]` entry removes the fixture findings end to end (e2e).

---

## Resolution (shipped in v0.2.0)

- **B1 — done.** `duplication.rs` now emits one finding per clone cluster (anchored at the earliest occurrence). Verified: `sample-dup` went from 2 findings to 1.
- **B2 — done.** Overlapping/adjacent same-file self-matches (`|o - s| < k`) are dropped. Verified: `gnarly.rb` went from 1 confusing self-dup to 0, complexity finding retained.
- **B4 — done.** `[ignore] paths` globs in `stratify.toml` are honored by the walker.
- **B3 — done.** `stratify-lang-rust` adapter added (dead code, duplication, complexity, hotspots). Trait-impl methods are marked entrypoints so they are not falsely flagged dead; string literals normalize to `STR`. Cycles/boundaries for Rust still need `use`/`mod` resolution (roadmap).

## B5 — Calls hidden inside macros are invisible (false dead-code) [Medium, found via B3]

**Symptom:** a function called only from inside a macro invocation (`assert!(f())`, `println!("{}", g())`, `vec![h()]`) is reported as dead, because tree-sitter parses macro arguments as an unparsed `token_tree`, so the call node is never seen. Surfaced in Rust (common in tests) but the same blind spot exists for any language's macro-like constructs.

**Impact:** over-reports dead code on macro-heavy or test-heavy code.

**Fix (later):** for Rust, descend into `macro_invocation` / `token_tree` and recover identifier-shaped call tokens as unresolved-call candidates (Likely only, so it can only downgrade a dead verdict, never falsely clear). Scope carefully to avoid noise.

## B6 — Intra-file helpers show as `possibly unused` (info) [Low, known tradeoff]

Internal private functions reached only through other in-file functions are reported as `possibly unused` (Likely/info), because intra-file call edges are Likely, not Certain (the M14 cross-file-resolution deferral). Not a false `unused` warning, but it adds info-level noise on real code. Promoting unambiguous intra-file calls to Certain is the long-standing refinement that closes this.

## Priorities

1. **B1 + B2** — directly fix the "verbose / duplications that don't make sense" report. Small, localized changes in `duplication.rs` with clear tests. Do first.
2. **B4** — exclude globs cut fixture/vendor noise on real repos. Small change in `run.rs` + config.
3. **B3** — Rust adapter. Larger (a new milestone), unblocks self-dogfooding and Rust users.

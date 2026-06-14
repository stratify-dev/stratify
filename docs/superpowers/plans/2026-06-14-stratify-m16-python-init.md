# Stratify M16 (Python `__init__.py` Import Resolution) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Resolve Python package imports to `__init__.py` files so cycles and layer-boundaries see package-level imports. Fixes the documented M12 gap.

**The problem:** A Python package `pkg` is the directory `pkg/` with a `pkg/__init__.py`. An import `import pkg` or `from pkg import thing` keys to `pkg` (the M12 resolver maps dotted modules to path keys: `pkg` -> `pkg`). But `pkg/__init__.py`'s File fqn is currently `pkg/__init__` (just the extension stripped). So the import key `pkg` never matches the export key `pkg/__init__`, and an edge to the package is dropped. Cycles and boundaries involving package `__init__.py` files are missed.

**The fix:** when the Python adapter computes a File's fqn, a `__init__.py` file collapses to its package directory: `pkg/__init__.py` -> fqn `pkg` (not `pkg/__init__`), and a top-level `__init__.py` -> fqn `""` (the root package, rare). Module files are unchanged (`pkg/mod.py` -> `pkg/mod`). Now `import pkg` (key `pkg`) matches `pkg/__init__.py` (fqn `pkg`), and the import graph forms package edges. Submodule imports (`from pkg.mod import x` -> `pkg/mod`) still match `pkg/mod.py` as before.

**Scope:** Contained entirely to the Python adapter's fqn computation (`strip_py_ext` / the File-symbol fqn). No change to the import resolver (it already produces `pkg` for package imports), no change to any analysis, no ripple to other languages. Safe and bounded.

**Prerequisite reading:** `crates/stratify-lang-py/src/extract.rs` (the `strip_py_ext` helper and where the File symbol's fqn is set; the `resolve_relative_py` / `dotted_to_path` import helpers for context).

---

## File Structure

```
crates/stratify-lang-py/src/extract.rs        MODIFY: __init__.py fqn collapses to its package dir
crates/stratify-cli/tests/sample-pypkg/        CREATE: two-package fixture importing each other via __init__.py
crates/stratify-cli/tests/e2e_pypkg.rs         CREATE: end-to-end package cycle through __init__.py
```

---

## Task 1: `__init__.py` fqn resolution + fixture + e2e

**Files:**
- Modify: `crates/stratify-lang-py/src/extract.rs`
- Create: `crates/stratify-cli/tests/sample-pypkg/pkg_a/__init__.py`, `crates/stratify-cli/tests/sample-pypkg/pkg_b/__init__.py`
- Create: `crates/stratify-cli/tests/e2e_pypkg.rs`

- [ ] **Step 1: Add a failing adapter test**

In `crates/stratify-lang-py/src/extract.rs` tests module:

```rust
    #[test]
    fn init_file_fqn_is_package_dir() {
        // pkg/__init__.py represents the package `pkg` -> fqn "pkg", so
        // `import pkg` (key "pkg") resolves to it.
        let g = extract("pkg/__init__.py", "x = 1\n");
        let f = g.symbols().iter().find(|s| s.kind == SymbolKind::File).unwrap();
        assert_eq!(f.fqn, "pkg");
    }

    #[test]
    fn nested_init_fqn_is_package_path() {
        let g = extract("a/b/__init__.py", "x = 1\n");
        let f = g.symbols().iter().find(|s| s.kind == SymbolKind::File).unwrap();
        assert_eq!(f.fqn, "a/b");
    }

    #[test]
    fn module_file_fqn_unchanged() {
        // regular module files keep path-sans-ext (regression guard)
        let g = extract("pkg/mod.py", "x = 1\n");
        let f = g.symbols().iter().find(|s| s.kind == SymbolKind::File).unwrap();
        assert_eq!(f.fqn, "pkg/mod");
    }

    #[test]
    fn top_level_init_fqn_is_empty() {
        let g = extract("__init__.py", "x = 1\n");
        let f = g.symbols().iter().find(|s| s.kind == SymbolKind::File).unwrap();
        assert_eq!(f.fqn, "");
    }
```

- [ ] **Step 2: Run, verify the new tests fail**

Run: `cargo test -p stratify-lang-py init_file_fqn` (if `cargo` missing: `source "$HOME/.cargo/env"`)
Expected: FAIL (current fqn is `pkg/__init__`, not `pkg`).

- [ ] **Step 3: Collapse `__init__.py` to the package dir in the File fqn**

Find where the File symbol's `fqn` is computed (it uses `strip_py_ext(path)`). Update the fqn computation so a `__init__.py` file collapses to its directory. Implement by post-processing the stripped path: if it equals `__init__` or ends with `/__init__`, drop that final segment.

```rust
/// The package/module key for a Python file. `pkg/mod.py` -> `pkg/mod`;
/// a package's `pkg/__init__.py` -> `pkg`; a top-level `__init__.py` -> ``.
fn py_module_key(path: &str) -> String {
    let stripped = strip_py_ext(path); // existing helper: removes .py/.pyi
    if stripped == "__init__" {
        String::new()
    } else if let Some(pkg) = stripped.strip_suffix("/__init__") {
        pkg.to_string()
    } else {
        stripped
    }
}
```

Set the File symbol's `fqn` to `py_module_key(path)` instead of `strip_py_ext(path)`. Keep `strip_py_ext` for any other use (and as the building block here). The File symbol's `name` stays the full path (only `fqn` changes).

- [ ] **Step 4: Run adapter tests**

Run: `cargo test -p stratify-lang-py`
Expected: PASS, including the 4 new tests and all prior (the `file_fqn_strips_extension` test asserts `pkg/mod.py` -> `pkg/mod`, still true).

- [ ] **Step 5: Create a two-package fixture that cycles through `__init__.py`**

`crates/stratify-cli/tests/sample-pypkg/pkg_a/__init__.py`:

```python
from pkg_b import b_thing


def a_thing():
    return b_thing()
```

`crates/stratify-cli/tests/sample-pypkg/pkg_b/__init__.py`:

```python
from pkg_a import a_thing


def b_thing():
    return a_thing()
```

(`pkg_a/__init__.py` imports `pkg_b` (key `pkg_b`, matching `pkg_b/__init__.py`'s fqn `pkg_b`), and vice versa, so the two packages form an import cycle. Before this fix, neither import resolved (the `__init__.py` fqn was `pkg_a/__init__` / `pkg_b/__init__`), so no cycle was reported.)

- [ ] **Step 6: End-to-end test**

Create `crates/stratify-cli/tests/e2e_pypkg.rs`:

```rust
use std::path::Path;

#[test]
fn python_package_cycle_through_init_is_detected() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sample-pypkg");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_stratify"))
        .arg("check")
        .arg(&dir)
        .arg("--format")
        .arg("json")
        .output()
        .expect("run stratify binary");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("\"rule\": \"cycle\""), "stdout: {stdout}");
    // the cycle spans the two packages' __init__.py files
    assert!(stdout.contains("pkg_a") && stdout.contains("pkg_b"), "stdout: {stdout}");
}
```

- [ ] **Step 7: Run + manual smoke**

Run: `cargo test -p stratify-cli`
Expected: PASS including `e2e_pypkg`. No regressions to other e2e suites.

Manual:
```bash
cargo build
./target/debug/stratify check crates/stratify-cli/tests/sample-pypkg
```
Expected: a `warn ... circular dependency: ... pkg_a ... pkg_b ...` finding (the two `__init__.py` files form a package cycle).

- [ ] **Step 8: Commit**

```bash
git add crates/stratify-lang-py crates/stratify-cli
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(py): resolve __init__.py to its package dir so package imports form edges"
```

---

## Task 2: fmt, clippy, lockfile

- [ ] **Step 1: Format and lint**

Run: `cargo fmt --all && cargo clippy --all-targets -- -D warnings`
Fix any warning properly (no blanket `#[allow]`). Re-run `cargo test` after any fix.

- [ ] **Step 2: Full suite**

Run: `cargo test`
Expected: all crates green.

- [ ] **Step 3: Commit**

```bash
git add -A
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "chore: fmt, clippy clean, update lockfile for python init resolution"
```

---

## Self-Review Notes

Spec coverage for M16:
- `__init__.py` File fqn collapses to its package dir, so `import pkg`/`from pkg import x` resolves to `pkg/__init__.py`: Task 1. Covered.
- Module-file fqns unchanged (regression guard test): Task 1. Covered.
- Package cycle through `__init__.py` detected end to end: Task 1. Covered.

Deferred (correctly out of M16): `from pkg import submodule` linking to `pkg/submodule.py` when `submodule` is a module rather than a name in `__init__.py` (the resolver keys the from-module `pkg`, not the imported names as submodules, for absolute imports — only relative `from . import sub` keys names as submodules, per M12); namespace packages (no `__init__.py`, PEP 420); and re-exports.

Known M16 characteristics (acceptable):
- A package's representative node in the import graph is its `__init__.py` (fqn = package dir). Modules within the package keep their own per-file fqns. A cycle between two packages' `__init__.py` files is reported as a cycle between those files.
- This only changes Python File fqns for `__init__.py`; all other Python files and all other languages are unaffected.

Type consistency: `py_module_key`, `strip_py_ext`, the File-symbol fqn, and the IR/analysis APIs are used consistently with their M1-M15 definitions.

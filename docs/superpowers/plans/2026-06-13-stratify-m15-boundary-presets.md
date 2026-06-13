# Stratify M15 (Layer-Boundary Presets) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the layer-boundaries analysis usable without hand-writing globs. Add built-in presets (`rails`, `layered`) selectable via `preset = "..."` in `stratify.toml`, plus zero-config auto-detection so common project layouts get boundary checking with no setup.

**Today:** boundaries (M7) only runs when `stratify.toml` declares `[layers]` globs and `[[forbid]]` rules. Most projects have neither, so the analysis is effectively dormant.

**The fix:** ship opinionated layer definitions for common conventions. A user writes `preset = "rails"` (one line) to get Rails layering, or `preset = "layered"` for the controller/service/repository/domain pattern (Spring, NestJS, etc.). With no `stratify.toml` at all, the CLI auto-detects a layout (Rails app dir, or a Maven/Gradle project) and applies the matching preset. Explicit `[layers]`/`[[forbid]]` entries still work and extend/override the preset.

**Architecture:** `BoundaryConfig` gains an optional `preset` field. A pure `boundaries::resolve(config)` merges the named preset's layers+forbid under the user's own entries. The CLI calls `resolve` after parsing `stratify.toml`, and when no config file exists, runs a small auto-detect that synthesizes a `{ preset: Some(...) }` config. No change to the boundary-checking logic itself — it just receives a populated config more often.

**Prerequisite reading:** `crates/stratify-analysis/src/boundaries.rs` (`BoundaryConfig`, `ForbidRule`, `analyze`, glob classification), `crates/stratify-cli/src/run.rs` (`load_boundary_config`).

---

## File Structure

```
crates/stratify-analysis/src/boundaries.rs   MODIFY: preset field + builtin presets + resolve()
crates/stratify-cli/src/run.rs                MODIFY: resolve presets + auto-detect when no config
crates/stratify-cli/tests/sample-rails/       CREATE: Rails-layout fixture + stratify.toml (preset = "rails")
crates/stratify-cli/tests/e2e_preset.rs       CREATE: end-to-end preset + auto-detect
```

---

## Task 1: Preset config + resolution (`stratify-analysis`)

**Files:**
- Modify: `crates/stratify-analysis/src/boundaries.rs`

- [ ] **Step 1: Add the `preset` field**

In `BoundaryConfig`, add a `preset` field (keep the existing `layers` and `forbid`):

```rust
#[derive(Debug, Clone, Default, Deserialize)]
pub struct BoundaryConfig {
    /// Optional built-in preset name (e.g. "rails", "layered").
    #[serde(default)]
    pub preset: Option<String>,
    #[serde(default)]
    pub layers: std::collections::BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub forbid: Vec<ForbidRule>,
}
```

- [ ] **Step 2: Built-in presets**

Add a helper returning the built-in preset configs. Use a small builder to keep it readable:

```rust
fn layer(name: &str, globs: &[&str]) -> (String, Vec<String>) {
    (name.to_string(), globs.iter().map(|s| s.to_string()).collect())
}

fn forbid(from: &str, to: &str) -> ForbidRule {
    ForbidRule { from: from.to_string(), to: to.to_string() }
}

/// Return the layers + forbid rules for a built-in preset, or None if unknown.
pub fn builtin_preset(name: &str) -> Option<BoundaryConfig> {
    match name {
        "rails" => Some(BoundaryConfig {
            preset: None,
            layers: [
                layer("controllers", &["app/controllers/**"]),
                layer("models", &["app/models/**"]),
                layer("views", &["app/views/**"]),
                layer("mailers", &["app/mailers/**"]),
                layer("jobs", &["app/jobs/**"]),
            ]
            .into_iter()
            .collect(),
            // Domain models must not depend on the web/delivery layers.
            forbid: vec![
                forbid("models", "controllers"),
                forbid("models", "views"),
                forbid("models", "mailers"),
            ],
        }),
        "layered" => Some(BoundaryConfig {
            preset: None,
            layers: [
                layer("controller", &["**/controller/**", "**/controllers/**"]),
                layer("service", &["**/service/**", "**/services/**"]),
                layer("repository", &["**/repository/**", "**/repositories/**", "**/dao/**"]),
                layer("domain", &["**/domain/**", "**/model/**", "**/models/**", "**/entity/**"]),
            ]
            .into_iter()
            .collect(),
            // Lower layers must not import higher ones; domain is innermost.
            forbid: vec![
                forbid("repository", "controller"),
                forbid("repository", "service"),
                forbid("domain", "controller"),
                forbid("domain", "service"),
                forbid("domain", "repository"),
            ],
        }),
        _ => None,
    }
}
```

- [ ] **Step 3: Resolution (merge preset under user entries)**

```rust
/// Resolve a config: if it names a known preset, start from the preset's
/// layers + forbid, then layer the user's own entries on top (user layer keys
/// override preset keys; user forbid rules are appended). Unknown or absent
/// preset -> the config is returned unchanged.
pub fn resolve(config: BoundaryConfig) -> BoundaryConfig {
    let Some(base) = config.preset.as_deref().and_then(builtin_preset) else {
        return config;
    };
    let mut layers = base.layers;
    for (k, v) in config.layers {
        layers.insert(k, v); // user overrides preset for same-named layer
    }
    let mut forbid = base.forbid;
    forbid.extend(config.forbid); // user rules appended
    BoundaryConfig { preset: config.preset, layers, forbid }
}
```

- [ ] **Step 4: Tests**

Add to the boundaries tests module:

```rust
    #[test]
    fn rails_preset_has_expected_layers_and_rules() {
        let c = builtin_preset("rails").unwrap();
        assert!(c.layers.contains_key("models"));
        assert!(c.layers.contains_key("controllers"));
        assert!(c.forbid.iter().any(|r| r.from == "models" && r.to == "controllers"));
    }

    #[test]
    fn resolve_expands_named_preset() {
        let c = resolve(BoundaryConfig { preset: Some("rails".into()), ..Default::default() });
        assert!(c.layers.contains_key("models"));
        assert!(!c.forbid.is_empty());
    }

    #[test]
    fn resolve_unknown_preset_is_noop() {
        let c = resolve(BoundaryConfig { preset: Some("nope".into()), ..Default::default() });
        assert!(c.layers.is_empty());
        assert!(c.forbid.is_empty());
    }

    #[test]
    fn user_entries_extend_preset() {
        let mut layers = std::collections::BTreeMap::new();
        layers.insert("models".to_string(), vec!["lib/models/**".to_string()]); // override
        layers.insert("custom".to_string(), vec!["lib/custom/**".to_string()]); // add
        let c = resolve(BoundaryConfig {
            preset: Some("rails".into()),
            layers,
            forbid: vec![ForbidRule { from: "custom".into(), to: "controllers".into() }],
        });
        assert_eq!(c.layers.get("models").unwrap(), &vec!["lib/models/**".to_string()]); // overridden
        assert!(c.layers.contains_key("custom")); // added
        assert!(c.layers.contains_key("controllers")); // from preset
        assert!(c.forbid.iter().any(|r| r.from == "custom" && r.to == "controllers")); // user rule appended
        assert!(c.forbid.iter().any(|r| r.from == "models" && r.to == "controllers")); // preset rule kept
    }

    #[test]
    fn analyze_works_through_a_resolved_preset() {
        // models/user.rb importing controllers/x.rb violates the rails preset.
        use stratify_core::ir::{Reference, Span, Symbol, SymbolId, Visibility};
        use stratify_core::{Confidence, RefKind, SymbolKind};
        let mut g = IrGraph::new();
        let m = g.add_symbol(Symbol {
            id: SymbolId(0), kind: SymbolKind::File, name: "app/models/user.rb".into(),
            fqn: "app/models/user.rb".into(),
            span: Span { file: "app/models/user.rb".into(), start_byte: 0, end_byte: 1, start_line: 1 },
            visibility: Visibility::Unknown, confidence: Confidence::Certain,
        });
        g.add_symbol(Symbol {
            id: SymbolId(0), kind: SymbolKind::File, name: "app/controllers/x.rb".into(),
            fqn: "app/controllers/x.rb".into(),
            span: Span { file: "app/controllers/x.rb".into(), start_byte: 0, end_byte: 1, start_line: 1 },
            visibility: Visibility::Unknown, confidence: Confidence::Certain,
        });
        let dep = g.add_symbol(Symbol {
            id: SymbolId(0), kind: SymbolKind::Dependency, name: "app/controllers/x.rb".into(),
            fqn: "app/controllers/x.rb".into(),
            span: Span { file: "x".into(), start_byte: 0, end_byte: 1, start_line: 1 },
            visibility: Visibility::Unknown, confidence: Confidence::Certain,
        });
        g.add_reference(Reference { from: m, to: dep, kind: RefKind::Imports,
            span: Span { file: "app/models/user.rb".into(), start_byte: 0, end_byte: 1, start_line: 1 },
            confidence: Confidence::Certain });
        let config = resolve(BoundaryConfig { preset: Some("rails".into()), ..Default::default() });
        let findings = analyze(&g, &config);
        assert!(findings.iter().any(|f| f.rule == "boundary" && f.message.contains("models") && f.message.contains("controllers")));
    }
```

- [ ] **Step 5: Run, verify pass**

Run: `cargo test -p stratify-analysis` (if `cargo` missing: `source "$HOME/.cargo/env"`)
Expected: PASS (5 new boundary tests + prior).

- [ ] **Step 6: Commit**

```bash
git add crates/stratify-analysis
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(analysis): layer-boundary presets (rails, layered) + resolve()"
```

---

## Task 2: CLI preset resolution + auto-detect + end-to-end (`stratify-cli`)

**Files:**
- Modify: `crates/stratify-cli/src/run.rs`
- Create: `crates/stratify-cli/tests/sample-rails/stratify.toml`, `app/models/user.rb`, `app/controllers/users_controller.rb`
- Create: `crates/stratify-cli/tests/e2e_preset.rs`

- [ ] **Step 1: Resolve presets and auto-detect in `load_boundary_config`**

In `crates/stratify-cli/src/run.rs`, update `load_boundary_config` so that (a) a parsed config is passed through `boundaries::resolve`, and (b) when no `stratify.toml` exists, a layout is auto-detected:

```rust
fn load_boundary_config(root: &std::path::Path) -> stratify_analysis::boundaries::BoundaryConfig {
    use stratify_analysis::boundaries::{resolve, BoundaryConfig};
    let path = root.join("stratify.toml");
    match std::fs::read_to_string(&path) {
        Ok(text) => resolve(toml::from_str(&text).unwrap_or_default()),
        Err(_) => resolve(autodetect_preset(root)),
    }
}

/// With no stratify.toml, guess a preset from the project layout. Returns an
/// empty config (no boundary checks) when nothing matches.
fn autodetect_preset(root: &std::path::Path) -> stratify_analysis::boundaries::BoundaryConfig {
    use stratify_analysis::boundaries::BoundaryConfig;
    let preset = if root.join("app/controllers").is_dir() || root.join("config/routes.rb").is_file() {
        Some("rails".to_string())
    } else if root.join("pom.xml").is_file() || root.join("build.gradle").is_file() {
        Some("layered".to_string())
    } else {
        None
    };
    BoundaryConfig { preset, ..Default::default() }
}
```

(The existing call `boundaries::analyze(&graph, &boundary_config)` in `analyze_repo` stays as-is; it now receives a resolved config.)

- [ ] **Step 2: Rails fixture**

`crates/stratify-cli/tests/sample-rails/stratify.toml`:

```toml
preset = "rails"
```

`crates/stratify-cli/tests/sample-rails/app/models/user.rb`:

```ruby
require_relative "../controllers/users_controller"

def user_name
  "alice"
end
```

`crates/stratify-cli/tests/sample-rails/app/controllers/users_controller.rb`:

```ruby
def show_user
  "showing"
end
```

(`app/models/user.rb` imports `app/controllers/users_controller.rb` -> a `models -> controllers` edge, which the rails preset forbids. Only the one-line `preset = "rails"` config is needed.)

- [ ] **Step 3: End-to-end test (explicit preset + auto-detect)**

Create `crates/stratify-cli/tests/e2e_preset.rs`:

```rust
use std::path::Path;

#[test]
fn rails_preset_flags_models_importing_controllers() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sample-rails");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_stratify"))
        .arg("check").arg(&dir).arg("--format").arg("json")
        .output().expect("run stratify");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("\"rule\": \"boundary\""), "stdout: {stdout}");
    assert!(stdout.contains("models") && stdout.contains("controllers"), "stdout: {stdout}");
}

#[test]
fn rails_layout_autodetects_without_config() {
    // Same fixture, but we delete the toml's effect by scanning the app/ subtree
    // where there is no stratify.toml — the app/controllers dir triggers autodetect.
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sample-rails");
    // Scan a copy-free path that has app/controllers but (for this test) we rely
    // on the real fixture having stratify.toml; to test autodetect specifically,
    // point at a directory with app/controllers and NO stratify.toml.
    // The sample-rails dir HAS stratify.toml, so this test instead verifies the
    // autodetect helper path via a temp dir.
    let tmp = std::env::temp_dir().join("stratify-autodetect-rails");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(tmp.join("app/models")).unwrap();
    std::fs::create_dir_all(tmp.join("app/controllers")).unwrap();
    std::fs::write(tmp.join("app/models/user.rb"),
        "require_relative \"../controllers/c\"\n\ndef n\n  1\nend\n").unwrap();
    std::fs::write(tmp.join("app/controllers/c.rb"), "def show\n  1\nend\n").unwrap();
    // No stratify.toml in tmp -> autodetect should apply the rails preset.
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_stratify"))
        .arg("check").arg(&tmp).arg("--format").arg("json")
        .output().expect("run stratify");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("\"rule\": \"boundary\""), "autodetect stdout: {stdout}");
    let _ = std::fs::remove_dir_all(&tmp);
    let _ = dir; // keep the explicit-preset test's import tidy
}
```

- [ ] **Step 4: Run + manual smoke**

Run: `cargo test -p stratify-cli`
Expected: PASS including both `e2e_preset` tests, no regressions to other e2e suites (they have no `stratify.toml` and don't match an autodetect marker, so boundaries stay off for them — confirm the per-language fixtures are unaffected).

Manual:
```bash
cargo build
./target/debug/stratify check crates/stratify-cli/tests/sample-rails
```
Expected: a `warn ... layer `models` must not import `controllers` ...` finding from just the one-line preset.

- [ ] **Step 5: Commit**

```bash
git add crates/stratify-cli
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(cli): resolve boundary presets, autodetect Rails/Maven layouts, e2e"
```

---

## Task 3: Docs + fmt, clippy, lockfile

**Files:**
- Modify: `README.md`, generated `Cargo.lock`, any fmt changes

- [ ] **Step 1: Document presets in the README**

Add to the layer-boundaries section of `README.md` (or create a short section): explain `preset = "rails"` / `preset = "layered"` in `stratify.toml`, that explicit `[layers]`/`[[forbid]]` extend a preset, and that Rails/Maven layouts are auto-detected when no `stratify.toml` is present. Keep it tight: short active sentences, no em dashes, no semicolons. Example block:

````markdown
## Layer boundaries

Enforce architecture layers in `stratify.toml`:

```toml
preset = "rails"   # or "layered" for controller/service/repository/domain
```

A preset ships layer globs and forbidden imports (Rails: models must not import
controllers, views, or mailers). Add your own `[layers]` and `[[forbid]]` to
extend or override a preset. With no `stratify.toml`, Stratify auto-detects a
Rails app (`app/controllers/`) or a Maven/Gradle project (`pom.xml`) and applies
the matching preset.
````

- [ ] **Step 2: Format and lint**

Run: `cargo fmt --all && cargo clippy --all-targets -- -D warnings`
Fix any warning properly (no blanket `#[allow]`). Re-run `cargo test` after any fix.

- [ ] **Step 3: Full suite**

Run: `cargo test`
Expected: all crates green.

- [ ] **Step 4: Commit**

```bash
git add -A
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "docs+chore: document boundary presets; fmt, clippy clean, lockfile"
```

---

## Self-Review Notes

Spec coverage for M15:
- `preset` config field + built-in `rails`/`layered` presets + `resolve()` merge: Task 1. Covered.
- CLI resolves presets and auto-detects Rails/Maven when no config: Task 2. Covered.
- Docs: Task 3. Covered.

Deferred (correctly out of M15): more presets (Django, NestJS, Phoenix, hexagonal), allow-list rules (only X may import Y), per-layer visibility beyond forbid pairs, and confidence/severity tuning per preset rule.

Known M15 characteristics (acceptable):
- Presets are opinionated conventions. `rails` keys off `app/` paths; `layered` keys off `**/controller(s)/`, `**/service(s)/`, `**/repository|repositories|dao/`, `**/domain|model(s)|entity/` path segments. Projects that deviate can override via explicit `[layers]`.
- Auto-detect is best-effort from marker files; it only fires when no `stratify.toml` exists, so it never overrides an explicit config. A project matching no marker gets no boundary checks (unchanged from today).
- Layer precedence is alphabetical (BTreeMap order), the documented M7 behavior; a catch-all layer named early still shadows. Presets avoid catch-alls.
- `resolve` lets user `[layers]` override same-named preset layers and appends user `[[forbid]]` rules, so a preset is a starting point, not a straitjacket.

Type consistency: `BoundaryConfig.preset`, `builtin_preset`, `resolve`, `ForbidRule`, `boundaries::analyze`, and `load_boundary_config`/`autodetect_preset` are used consistently with their M1-M14 definitions.

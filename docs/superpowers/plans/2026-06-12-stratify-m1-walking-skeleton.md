# Stratify M1 (Walking Skeleton) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a Rust workspace that parses a Java repository into a language-agnostic IR, runs dead-code analysis on that IR, and prints findings as human text and JSON via `stratify check <path>`.

**Architecture:** A Cargo workspace. `stratify-core` defines the IR (symbols, references, confidence, findings). `stratify-lang` defines the `LanguageAdapter` trait. `stratify-lang-java` turns Java source into IR via tree-sitter. `stratify-analysis` runs dead-code reachability on the IR only. `stratify-report` renders findings. `stratify-cli` discovers files, orchestrates, and exposes the `stratify` binary. Every analysis reads only the IR; it never sees Java.

**Tech Stack:** Rust (edition 2021), tree-sitter 0.24 + tree-sitter-java 0.23, serde + serde_json, clap 4, ignore (file walking), insta (snapshot tests).

---

## File Structure

```
Cargo.toml                              workspace manifest
crates/
  stratify-core/
    Cargo.toml
    src/lib.rs                          re-exports
    src/ir.rs                           Symbol, Reference, SymbolKind, RefKind, Span, SymbolId
    src/confidence.rs                   Confidence enum + ordering
    src/graph.rs                        IrGraph: storage + lookups
    src/finding.rs                      Finding, Severity, finding schema
  stratify-lang/
    Cargo.toml
    src/lib.rs                          LanguageAdapter trait, AdapterError
  stratify-lang-java/
    Cargo.toml
    src/lib.rs                          JavaAdapter
    src/extract.rs                      tree-sitter queries -> symbols + references
    tests/fixtures/                     sample .java files
  stratify-analysis/
    Cargo.toml
    src/lib.rs                          re-exports
    src/deadcode.rs                     reachability analysis
  stratify-report/
    Cargo.toml
    src/lib.rs                          Renderer trait
    src/json.rs                         JSON renderer
    src/human.rs                        human text renderer
  stratify-cli/
    Cargo.toml
    src/main.rs                         clap CLI, builds `stratify` binary
    src/run.rs                          discover -> parse -> analyze -> render
    tests/sample-java/                  end-to-end fixture app
```

Lockfile note: commit `Cargo.lock` (this is a binary product, not a library-only repo).

---

## Task 1: Workspace scaffold

**Files:**
- Create: `Cargo.toml`
- Create: `rust-toolchain.toml`
- Modify: `.gitignore`

- [ ] **Step 1: Write the workspace manifest**

Create `Cargo.toml`:

```toml
[workspace]
resolver = "2"
members = [
  "crates/stratify-core",
  "crates/stratify-lang",
  "crates/stratify-lang-java",
  "crates/stratify-analysis",
  "crates/stratify-report",
  "crates/stratify-cli",
]

[workspace.package]
edition = "2021"
version = "0.1.0"
license = "MIT OR Apache-2.0"
repository = "https://github.com/dynaum/stratify"

[workspace.dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tree-sitter = "0.24"
tree-sitter-java = "0.23"
clap = { version = "4", features = ["derive"] }
ignore = "0.4"
insta = { version = "1", features = ["json"] }
```

- [ ] **Step 2: Pin the toolchain**

Create `rust-toolchain.toml`:

```toml
[toolchain]
channel = "stable"
components = ["rustfmt", "clippy"]
```

- [ ] **Step 3: Fix .gitignore to commit the lockfile**

Replace `.gitignore` contents with:

```
/target
```

- [ ] **Step 4: Verify the workspace resolves (no members yet will error, so defer build)**

Run: `cargo --version`
Expected: prints a cargo 1.x version. (Full `cargo build` runs after Task 2 adds the first member.)

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml rust-toolchain.toml .gitignore
git commit -m "chore: scaffold cargo workspace"
```

---

## Task 2: IR core types (`stratify-core`)

**Files:**
- Create: `crates/stratify-core/Cargo.toml`
- Create: `crates/stratify-core/src/lib.rs`
- Create: `crates/stratify-core/src/confidence.rs`
- Create: `crates/stratify-core/src/ir.rs`
- Test: inline `#[cfg(test)]` in `ir.rs` and `confidence.rs`

- [ ] **Step 1: Write the crate manifest**

Create `crates/stratify-core/Cargo.toml`:

```toml
[package]
name = "stratify-core"
edition.workspace = true
version.workspace = true
license.workspace = true

[dependencies]
serde = { workspace = true }
```

- [ ] **Step 2: Write the failing test for Confidence ordering**

Create `crates/stratify-core/src/confidence.rs`:

```rust
use serde::{Deserialize, Serialize};

/// How sure the adapter is about a symbol or reference.
/// Ordering matters: Unknown < Likely < Certain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    Unknown,
    Likely,
    Certain,
}

impl Confidence {
    /// The weaker of two confidences. Used when combining edges along a path.
    pub fn min_with(self, other: Confidence) -> Confidence {
        self.min(other)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordering_is_unknown_lowest() {
        assert!(Confidence::Unknown < Confidence::Likely);
        assert!(Confidence::Likely < Confidence::Certain);
    }

    #[test]
    fn min_with_picks_weaker() {
        assert_eq!(Confidence::Certain.min_with(Confidence::Unknown), Confidence::Unknown);
        assert_eq!(Confidence::Likely.min_with(Confidence::Certain), Confidence::Likely);
    }
}
```

- [ ] **Step 3: Write the IR types**

Create `crates/stratify-core/src/ir.rs`:

```rust
use serde::{Deserialize, Serialize};
use crate::confidence::Confidence;

/// Stable identifier for a symbol within one IrGraph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SymbolId(pub u32);

/// A source location, half-open byte range plus 1-based line for display.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub file: String,
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_line: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    File,
    Module,
    Class,
    Function,
    Constant,
    Dependency,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    Public,
    Private,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Symbol {
    pub id: SymbolId,
    pub kind: SymbolKind,
    pub name: String,
    /// Fully-qualified path, e.g. "com.acme.Foo#bar".
    pub fqn: String,
    pub span: Span,
    pub visibility: Visibility,
    pub confidence: Confidence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefKind {
    Defines,
    Calls,
    Imports,
    Inherits,
    References,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reference {
    pub from: SymbolId,
    pub to: SymbolId,
    pub kind: RefKind,
    pub span: Span,
    pub confidence: Confidence,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::confidence::Confidence;

    #[test]
    fn symbol_round_trips_through_serde() {
        let sym = Symbol {
            id: SymbolId(1),
            kind: SymbolKind::Class,
            name: "Foo".into(),
            fqn: "com.acme.Foo".into(),
            span: Span { file: "Foo.java".into(), start_byte: 0, end_byte: 10, start_line: 1 },
            visibility: Visibility::Public,
            confidence: Confidence::Certain,
        };
        let json = serde_json::to_string(&sym).unwrap();
        let back: Symbol = serde_json::from_str(&json).unwrap();
        assert_eq!(sym, back);
    }
}
```

The serde test needs `serde_json` as a dev-dependency. Add to `crates/stratify-core/Cargo.toml`:

```toml
[dev-dependencies]
serde_json = { workspace = true }
```

- [ ] **Step 4: Wire up lib.rs**

Create `crates/stratify-core/src/lib.rs`:

```rust
pub mod confidence;
pub mod ir;

pub use confidence::Confidence;
pub use ir::{RefKind, Reference, Span, Symbol, SymbolId, SymbolKind, Visibility};
```

- [ ] **Step 5: Run the tests, verify they pass**

Run: `cargo test -p stratify-core`
Expected: PASS, 3 tests.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml crates/stratify-core
git commit -m "feat(core): IR symbol, reference, and confidence types"
```

---

## Task 3: IR graph storage (`stratify-core`)

**Files:**
- Create: `crates/stratify-core/src/graph.rs`
- Modify: `crates/stratify-core/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/stratify-core/src/graph.rs`:

```rust
use crate::ir::{Reference, Symbol, SymbolId};

/// The whole repository as one language-agnostic graph.
#[derive(Debug, Default, Clone)]
pub struct IrGraph {
    symbols: Vec<Symbol>,
    references: Vec<Reference>,
}

impl IrGraph {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a symbol and return its assigned id. The caller-provided id on the
    /// symbol is overwritten with the next sequential id to keep ids dense.
    pub fn add_symbol(&mut self, mut symbol: Symbol) -> SymbolId {
        let id = SymbolId(self.symbols.len() as u32);
        symbol.id = id;
        self.symbols.push(symbol);
        id
    }

    pub fn add_reference(&mut self, reference: Reference) {
        self.references.push(reference);
    }

    pub fn symbols(&self) -> &[Symbol] {
        &self.symbols
    }

    pub fn references(&self) -> &[Reference] {
        &self.references
    }

    pub fn symbol(&self, id: SymbolId) -> Option<&Symbol> {
        self.symbols.get(id.0 as usize)
    }

    /// Merge another graph into this one, remapping the other graph's ids so
    /// they stay unique. Returns nothing; used to combine per-file graphs.
    pub fn merge(&mut self, other: IrGraph) {
        let offset = self.symbols.len() as u32;
        for mut sym in other.symbols {
            sym.id = SymbolId(sym.id.0 + offset);
            self.symbols.push(sym);
        }
        for mut r in other.references {
            r.from = SymbolId(r.from.0 + offset);
            r.to = SymbolId(r.to.0 + offset);
            self.references.push(r);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::confidence::Confidence;
    use crate::ir::{RefKind, Span, SymbolId, SymbolKind, Visibility};

    fn sym(name: &str) -> Symbol {
        Symbol {
            id: SymbolId(0),
            kind: SymbolKind::Function,
            name: name.into(),
            fqn: name.into(),
            span: Span { file: "x".into(), start_byte: 0, end_byte: 1, start_line: 1 },
            visibility: Visibility::Public,
            confidence: Confidence::Certain,
        }
    }

    #[test]
    fn add_symbol_assigns_sequential_ids() {
        let mut g = IrGraph::new();
        let a = g.add_symbol(sym("a"));
        let b = g.add_symbol(sym("b"));
        assert_eq!(a, SymbolId(0));
        assert_eq!(b, SymbolId(1));
        assert_eq!(g.symbol(b).unwrap().name, "b");
    }

    #[test]
    fn merge_remaps_reference_ids() {
        let mut g1 = IrGraph::new();
        g1.add_symbol(sym("a"));

        let mut g2 = IrGraph::new();
        let x = g2.add_symbol(sym("x"));
        let y = g2.add_symbol(sym("y"));
        g2.add_reference(Reference {
            from: x, to: y, kind: RefKind::Calls,
            span: Span { file: "x".into(), start_byte: 0, end_byte: 1, start_line: 1 },
            confidence: Confidence::Certain,
        });

        g1.merge(g2);
        assert_eq!(g1.symbols().len(), 3);
        // x was id 0 in g2, becomes id 1 after merge (offset 1).
        let r = &g1.references()[0];
        assert_eq!(r.from, SymbolId(1));
        assert_eq!(r.to, SymbolId(2));
    }
}
```

- [ ] **Step 2: Run, verify it fails to compile (module not declared)**

Run: `cargo test -p stratify-core graph`
Expected: FAIL, unresolved module `graph`.

- [ ] **Step 3: Declare the module and re-export**

Edit `crates/stratify-core/src/lib.rs` to add:

```rust
pub mod graph;
pub use graph::IrGraph;
```

(Place `pub mod graph;` with the other `pub mod` lines and `pub use graph::IrGraph;` with the other re-exports.)

- [ ] **Step 4: Run, verify it passes**

Run: `cargo test -p stratify-core graph`
Expected: PASS, 2 graph tests.

- [ ] **Step 5: Commit**

```bash
git add crates/stratify-core
git commit -m "feat(core): IrGraph storage with merge and lookups"
```

---

## Task 4: Finding schema (`stratify-core`)

**Files:**
- Create: `crates/stratify-core/src/finding.rs`
- Modify: `crates/stratify-core/src/lib.rs`

- [ ] **Step 1: Write the finding types and a test**

Create `crates/stratify-core/src/finding.rs`:

```rust
use serde::{Deserialize, Serialize};
use crate::confidence::Confidence;
use crate::ir::Span;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warning,
    Error,
}

/// One reported problem. `rule` identifies the analysis, e.g. "dead_code".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub rule: String,
    pub severity: Severity,
    pub message: String,
    pub span: Span,
    pub confidence: Confidence,
}

/// The top-level machine output. Versioned so downstream consumers can rely on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub schema_version: u32,
    pub findings: Vec<Finding>,
}

impl Report {
    pub const SCHEMA_VERSION: u32 = 1;

    pub fn new(findings: Vec<Finding>) -> Self {
        Report { schema_version: Self::SCHEMA_VERSION, findings }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::Span;

    #[test]
    fn report_serializes_with_schema_version() {
        let report = Report::new(vec![Finding {
            rule: "dead_code".into(),
            severity: Severity::Warning,
            message: "unused method bar".into(),
            span: Span { file: "Foo.java".into(), start_byte: 0, end_byte: 1, start_line: 3 },
            confidence: Confidence::Certain,
        }]);
        let v: serde_json::Value = serde_json::to_value(&report).unwrap();
        assert_eq!(v["schema_version"], 1);
        assert_eq!(v["findings"][0]["rule"], "dead_code");
    }
}
```

- [ ] **Step 2: Declare module and re-export**

Edit `crates/stratify-core/src/lib.rs` to add:

```rust
pub mod finding;
pub use finding::{Finding, Report, Severity};
```

- [ ] **Step 3: Run, verify pass**

Run: `cargo test -p stratify-core finding`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/stratify-core
git commit -m "feat(core): versioned Finding and Report schema"
```

---

## Task 5: LanguageAdapter trait (`stratify-lang`)

**Files:**
- Create: `crates/stratify-lang/Cargo.toml`
- Create: `crates/stratify-lang/src/lib.rs`

- [ ] **Step 1: Write the manifest**

Create `crates/stratify-lang/Cargo.toml`:

```toml
[package]
name = "stratify-lang"
edition.workspace = true
version.workspace = true
license.workspace = true

[dependencies]
stratify-core = { path = "../stratify-core" }
```

- [ ] **Step 2: Write the trait and a doc test**

Create `crates/stratify-lang/src/lib.rs`:

```rust
use stratify_core::IrGraph;

#[derive(Debug)]
pub enum AdapterError {
    Parse(String),
}

impl std::fmt::Display for AdapterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AdapterError::Parse(m) => write!(f, "parse error: {m}"),
        }
    }
}

impl std::error::Error for AdapterError {}

/// Turns source files of one language into IR. The only language-aware code
/// in the system. Analyses never see this; they read the merged IrGraph.
pub trait LanguageAdapter {
    /// Lowercase language id, e.g. "java".
    fn language(&self) -> &'static str;

    /// True if this adapter handles the given file extension (no dot), e.g. "java".
    fn handles_extension(&self, ext: &str) -> bool;

    /// Parse one file's `source` (already read from `path`) into a per-file IrGraph.
    fn parse_file(&self, path: &str, source: &str) -> Result<IrGraph, AdapterError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Noop;
    impl LanguageAdapter for Noop {
        fn language(&self) -> &'static str { "noop" }
        fn handles_extension(&self, ext: &str) -> bool { ext == "noop" }
        fn parse_file(&self, _path: &str, _source: &str) -> Result<IrGraph, AdapterError> {
            Ok(IrGraph::new())
        }
    }

    #[test]
    fn adapter_contract_holds() {
        let a = Noop;
        assert_eq!(a.language(), "noop");
        assert!(a.handles_extension("noop"));
        assert!(!a.handles_extension("java"));
        assert_eq!(a.parse_file("x.noop", "").unwrap().symbols().len(), 0);
    }
}
```

- [ ] **Step 3: Run, verify pass**

Run: `cargo test -p stratify-lang`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/stratify-lang
git commit -m "feat(lang): LanguageAdapter trait"
```

---

## Task 6: Java adapter, classes and methods (`stratify-lang-java`)

**Files:**
- Create: `crates/stratify-lang-java/Cargo.toml`
- Create: `crates/stratify-lang-java/src/lib.rs`
- Create: `crates/stratify-lang-java/src/extract.rs`
- Test: inline tests in `extract.rs`

- [ ] **Step 1: Write the manifest**

Create `crates/stratify-lang-java/Cargo.toml`:

```toml
[package]
name = "stratify-lang-java"
edition.workspace = true
version.workspace = true
license.workspace = true

[dependencies]
stratify-core = { path = "../stratify-core" }
stratify-lang = { path = "../stratify-lang" }
tree-sitter = { workspace = true }
tree-sitter-java = { workspace = true }
```

- [ ] **Step 2: Write a helper to build a parser and a span**

Create `crates/stratify-lang-java/src/extract.rs`:

```rust
use stratify_core::ir::{Span, SymbolId};
use stratify_core::{Confidence, IrGraph, RefKind, Reference, Symbol, SymbolKind, Visibility};
use tree_sitter::{Node, Parser, Query, QueryCursor};

pub(crate) fn parser() -> Parser {
    let mut p = Parser::new();
    p.set_language(&tree_sitter_java::LANGUAGE.into())
        .expect("load java grammar");
    p
}

fn span(file: &str, node: Node) -> Span {
    Span {
        file: file.to_string(),
        start_byte: node.start_byte(),
        end_byte: node.end_byte(),
        start_line: node.start_position().row + 1,
    }
}

fn text<'a>(node: Node, src: &'a str) -> &'a str {
    node.utf8_text(src.as_bytes()).unwrap_or("")
}

/// Extract classes and their methods into a per-file graph. The file itself
/// becomes a `File` symbol; classes and methods get `Defines` edges from it.
pub(crate) fn extract(file: &str, src: &str) -> IrGraph {
    let mut parser = parser();
    let tree = parser.parse(src, None).expect("parse java");
    let root = tree.root_node();

    let mut g = IrGraph::new();

    // File symbol.
    let file_id = g.add_symbol(Symbol {
        id: SymbolId(0),
        kind: SymbolKind::File,
        name: file.to_string(),
        fqn: file.to_string(),
        span: span(file, root),
        visibility: Visibility::Unknown,
        confidence: Confidence::Certain,
    });

    let query = Query::new(
        &tree_sitter_java::LANGUAGE.into(),
        r#"
        (class_declaration name: (identifier) @class.name) @class.node
        (method_declaration name: (identifier) @method.name) @method.node
        "#,
    )
    .expect("valid query");

    let mut cursor = QueryCursor::new();
    let class_name_idx = query.capture_index_for_name("class.name").unwrap();
    let class_node_idx = query.capture_index_for_name("class.node").unwrap();
    let method_name_idx = query.capture_index_for_name("method.name").unwrap();
    let method_node_idx = query.capture_index_for_name("method.node").unwrap();

    let mut matches = cursor.matches(&query, root, src.as_bytes());
    while let Some(m) = matches.next() {
        let mut name_node = None;
        let mut decl_node = None;
        let mut kind = SymbolKind::Class;
        for cap in m.captures {
            if cap.index == class_name_idx {
                name_node = Some(cap.node);
                kind = SymbolKind::Class;
            } else if cap.index == class_node_idx {
                decl_node = Some(cap.node);
            } else if cap.index == method_name_idx {
                name_node = Some(cap.node);
                kind = SymbolKind::Function;
            } else if cap.index == method_node_idx {
                decl_node = Some(cap.node);
            }
        }
        if let (Some(name_node), Some(decl_node)) = (name_node, decl_node) {
            let name = text(name_node, src).to_string();
            let id = g.add_symbol(Symbol {
                id: SymbolId(0),
                kind,
                name: name.clone(),
                fqn: name,
                span: span(file, decl_node),
                visibility: Visibility::Unknown,
                confidence: Confidence::Certain,
            });
            g.add_reference(Reference {
                from: file_id,
                to: id,
                kind: RefKind::Defines,
                span: span(file, decl_node),
                confidence: Confidence::Certain,
            });
        }
    }

    g
}

#[cfg(test)]
mod tests {
    use super::*;
    use stratify_core::SymbolKind;

    #[test]
    fn extracts_class_and_method() {
        let src = "public class Foo {\n  void bar() {}\n}\n";
        let g = extract("Foo.java", src);
        let kinds: Vec<_> = g.symbols().iter().map(|s| (s.kind, s.name.as_str())).collect();
        assert!(kinds.contains(&(SymbolKind::File, "Foo.java")));
        assert!(kinds.contains(&(SymbolKind::Class, "Foo")));
        assert!(kinds.contains(&(SymbolKind::Function, "bar")));
    }

    #[test]
    fn file_defines_its_members() {
        let src = "class A { void m() {} }";
        let g = extract("A.java", src);
        // One Defines edge for class A, one for method m.
        assert_eq!(g.references().iter().filter(|r| matches!(r.kind, RefKind::Defines)).count(), 2);
    }
}
```

Note on the tree-sitter API: `QueryCursor::matches` returns a streaming iterator in tree-sitter 0.24. Call `.next()` in a `while let` loop as shown, and bring `use tree_sitter::StreamingIterator;` into scope if the compiler asks for it. If `matches.next()` is not found, add `use streaming_iterator::StreamingIterator;` (re-exported by tree-sitter 0.24).

- [ ] **Step 3: Run, verify the extraction tests pass**

Run: `cargo test -p stratify-lang-java extract`
Expected: FAIL first (no lib.rs wiring), then after Step 4, PASS.

- [ ] **Step 4: Write the adapter that implements the trait**

Create `crates/stratify-lang-java/src/lib.rs`:

```rust
mod extract;

use stratify_core::IrGraph;
use stratify_lang::{AdapterError, LanguageAdapter};

pub struct JavaAdapter;

impl LanguageAdapter for JavaAdapter {
    fn language(&self) -> &'static str {
        "java"
    }

    fn handles_extension(&self, ext: &str) -> bool {
        ext == "java"
    }

    fn parse_file(&self, path: &str, source: &str) -> Result<IrGraph, AdapterError> {
        Ok(extract::extract(path, source))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_parses_a_class() {
        let a = JavaAdapter;
        assert!(a.handles_extension("java"));
        let g = a.parse_file("Foo.java", "class Foo { void bar() {} }").unwrap();
        assert!(g.symbols().iter().any(|s| s.name == "bar"));
    }
}
```

- [ ] **Step 5: Run all crate tests, verify pass**

Run: `cargo test -p stratify-lang-java`
Expected: PASS, 3 tests.

- [ ] **Step 6: Commit**

```bash
git add crates/stratify-lang-java
git commit -m "feat(java): extract classes and methods into IR"
```

---

## Task 7: Java adapter, call and import edges (`stratify-lang-java`)

**Files:**
- Modify: `crates/stratify-lang-java/src/extract.rs`

This task adds `Calls` edges between methods in the same file and records imports as
`Dependency` symbols. Cross-file resolution is out of scope for M1; unresolved calls stay
within the file and carry `Confidence::Likely`.

- [ ] **Step 1: Add a failing test for intra-file calls**

Add to the `tests` module in `crates/stratify-lang-java/src/extract.rs`:

```rust
    #[test]
    fn records_intra_file_call_edge() {
        let src = "class A {\n  void a() { b(); }\n  void b() {}\n}\n";
        let g = extract("A.java", src);
        let a_id = g.symbols().iter().find(|s| s.name == "a").unwrap().id;
        let b_id = g.symbols().iter().find(|s| s.name == "b").unwrap().id;
        assert!(g.references().iter().any(|r|
            matches!(r.kind, RefKind::Calls) && r.from == a_id && r.to == b_id));
    }
```

- [ ] **Step 2: Run, verify it fails**

Run: `cargo test -p stratify-lang-java records_intra_file_call`
Expected: FAIL (no Calls edges yet).

- [ ] **Step 3: Implement call-edge extraction**

In `extract.rs`, after the class/method loop and before `g`, add a second pass. Replace the
final `g` return with this block:

```rust
    // Second pass: intra-file calls. Resolve a (method_invocation name) against
    // method names defined in this file. Unresolved calls are skipped in M1.
    let name_to_id: std::collections::HashMap<String, SymbolId> = g
        .symbols()
        .iter()
        .filter(|s| matches!(s.kind, SymbolKind::Function))
        .map(|s| (s.name.clone(), s.id))
        .collect();

    let call_query = Query::new(
        &tree_sitter_java::LANGUAGE.into(),
        r#"
        (method_invocation
          name: (identifier) @call.name) @call.node
        "#,
    )
    .expect("valid call query");

    let call_name_idx = call_query.capture_index_for_name("call.name").unwrap();
    let call_node_idx = call_query.capture_index_for_name("call.node").unwrap();

    // Map each call site to the enclosing method by walking ancestors.
    let mut call_cursor = QueryCursor::new();
    let mut call_matches = call_cursor.matches(&call_query, root, src.as_bytes());
    while let Some(m) = call_matches.next() {
        let mut callee_name = None;
        let mut call_node = None;
        for cap in m.captures {
            if cap.index == call_name_idx {
                callee_name = Some(text(cap.node, src).to_string());
            } else if cap.index == call_node_idx {
                call_node = Some(cap.node);
            }
        }
        let (Some(callee_name), Some(call_node)) = (callee_name, call_node) else { continue };
        let Some(&callee_id) = name_to_id.get(&callee_name) else { continue };
        let Some(caller_id) = enclosing_method_id(call_node, &g, file) else { continue };
        g.add_reference(Reference {
            from: caller_id,
            to: callee_id,
            kind: RefKind::Calls,
            span: span(file, call_node),
            confidence: Confidence::Likely,
        });
    }

    g
}

/// Find the method that lexically encloses `node` by matching byte ranges against
/// known method symbols in this file's graph.
fn enclosing_method_id(node: Node, g: &IrGraph, file: &str) -> Option<SymbolId> {
    let pos = node.start_byte();
    g.symbols()
        .iter()
        .filter(|s| matches!(s.kind, SymbolKind::Function) && s.span.file == file)
        .filter(|s| s.span.start_byte <= pos && pos < s.span.end_byte)
        // Innermost enclosing method = smallest span.
        .min_by_key(|s| s.span.end_byte - s.span.start_byte)
        .map(|s| s.id)
}
```

(Delete the previous bare `g` return at the end of `extract`, since this block now ends the function.)

- [ ] **Step 4: Run, verify the call test passes and old tests still pass**

Run: `cargo test -p stratify-lang-java`
Expected: PASS, 4 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/stratify-lang-java
git commit -m "feat(java): intra-file call edges with Likely confidence"
```

---

## Task 8: Dead-code analysis (`stratify-analysis`)

**Files:**
- Create: `crates/stratify-analysis/Cargo.toml`
- Create: `crates/stratify-analysis/src/lib.rs`
- Create: `crates/stratify-analysis/src/deadcode.rs`
- Test: inline tests on hand-built graphs (no parsing)

- [ ] **Step 1: Write the manifest**

Create `crates/stratify-analysis/Cargo.toml`:

```toml
[package]
name = "stratify-analysis"
edition.workspace = true
version.workspace = true
license.workspace = true

[dependencies]
stratify-core = { path = "../stratify-core" }
```

- [ ] **Step 2: Write the dead-code analysis with tests on hand-built IR**

Create `crates/stratify-analysis/src/deadcode.rs`:

```rust
use std::collections::HashSet;
use stratify_core::ir::SymbolId;
use stratify_core::{Confidence, Finding, IrGraph, RefKind, Severity, SymbolKind};

/// A symbol is an entrypoint if its name matches a known root. For M1 (Java),
/// `main` methods are roots. File symbols are always roots (they anchor defines).
fn is_entrypoint(name: &str, kind: SymbolKind) -> bool {
    matches!(kind, SymbolKind::File) || (matches!(kind, SymbolKind::Function) && name == "main")
}

/// Find functions that no entrypoint can reach via Calls/Defines edges.
/// A function reachable only through a low-confidence edge is reported as
/// "possibly unused" (Info) rather than "dead" (Warning).
pub fn analyze(graph: &IrGraph) -> Vec<Finding> {
    // Build adjacency: from -> [(to, confidence)].
    let mut roots: Vec<SymbolId> = Vec::new();
    for s in graph.symbols() {
        if is_entrypoint(&s.name, s.kind) {
            roots.push(s.id);
        }
    }

    // BFS reachability. Track the weakest edge confidence used to reach a node.
    let mut reached_certain: HashSet<SymbolId> = HashSet::new();
    let mut reached_any: HashSet<SymbolId> = HashSet::new();
    let mut queue: Vec<(SymbolId, bool)> = roots.iter().map(|r| (*r, true)).collect();
    for r in &roots {
        reached_certain.insert(*r);
        reached_any.insert(*r);
    }

    while let Some((node, path_certain)) = queue.pop() {
        for r in graph.references() {
            if r.from != node {
                continue;
            }
            if !matches!(r.kind, RefKind::Calls | RefKind::Defines | RefKind::Inherits) {
                continue;
            }
            let edge_certain = path_certain && r.confidence == Confidence::Certain;
            let newly_certain = edge_certain && reached_certain.insert(r.to);
            let newly_any = reached_any.insert(r.to);
            if newly_certain || newly_any {
                queue.push((r.to, edge_certain));
            }
        }
    }

    let mut findings = Vec::new();
    for s in graph.symbols() {
        if !matches!(s.kind, SymbolKind::Function) {
            continue;
        }
        if reached_certain.contains(&s.id) {
            continue; // definitely used
        }
        if reached_any.contains(&s.id) {
            findings.push(Finding {
                rule: "dead_code".into(),
                severity: Severity::Info,
                message: format!("possibly unused function `{}`", s.name),
                span: s.span.clone(),
                confidence: Confidence::Likely,
            });
        } else {
            findings.push(Finding {
                rule: "dead_code".into(),
                severity: Severity::Warning,
                message: format!("unused function `{}`", s.name),
                span: s.span.clone(),
                confidence: Confidence::Certain,
            });
        }
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use stratify_core::ir::{Reference, Span, Symbol, SymbolId, Visibility};

    fn func(g: &mut IrGraph, name: &str) -> SymbolId {
        g.add_symbol(Symbol {
            id: SymbolId(0),
            kind: SymbolKind::Function,
            name: name.into(),
            fqn: name.into(),
            span: Span { file: "T.java".into(), start_byte: 0, end_byte: 1, start_line: 1 },
            visibility: Visibility::Unknown,
            confidence: Confidence::Certain,
        })
    }

    fn edge(g: &mut IrGraph, from: SymbolId, to: SymbolId, conf: Confidence) {
        g.add_reference(Reference {
            from, to, kind: RefKind::Calls,
            span: Span { file: "T.java".into(), start_byte: 0, end_byte: 1, start_line: 1 },
            confidence: conf,
        });
    }

    #[test]
    fn unreached_function_is_dead() {
        let mut g = IrGraph::new();
        let _main = func(&mut g, "main");
        let _orphan = func(&mut g, "orphan");
        let findings = analyze(&g);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Warning);
        assert!(findings[0].message.contains("orphan"));
    }

    #[test]
    fn reached_via_certain_edge_is_not_reported() {
        let mut g = IrGraph::new();
        let main = func(&mut g, "main");
        let used = func(&mut g, "used");
        edge(&mut g, main, used, Confidence::Certain);
        assert!(analyze(&g).is_empty());
    }

    #[test]
    fn reached_only_via_likely_edge_is_possibly_unused() {
        let mut g = IrGraph::new();
        let main = func(&mut g, "main");
        let maybe = func(&mut g, "maybe");
        edge(&mut g, main, maybe, Confidence::Likely);
        let findings = analyze(&g);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Info);
        assert!(findings[0].message.contains("possibly unused"));
    }
}
```

- [ ] **Step 3: Wire lib.rs**

Create `crates/stratify-analysis/src/lib.rs`:

```rust
pub mod deadcode;
```

- [ ] **Step 4: Run, verify pass**

Run: `cargo test -p stratify-analysis`
Expected: PASS, 3 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/stratify-analysis
git commit -m "feat(analysis): dead-code reachability with confidence downgrade"
```

---

## Task 9: Renderers (`stratify-report`)

**Files:**
- Create: `crates/stratify-report/Cargo.toml`
- Create: `crates/stratify-report/src/lib.rs`
- Create: `crates/stratify-report/src/json.rs`
- Create: `crates/stratify-report/src/human.rs`

- [ ] **Step 1: Write the manifest**

Create `crates/stratify-report/Cargo.toml`:

```toml
[package]
name = "stratify-report"
edition.workspace = true
version.workspace = true
license.workspace = true

[dependencies]
stratify-core = { path = "../stratify-core" }
serde_json = { workspace = true }
```

- [ ] **Step 2: Write the JSON renderer with a test**

Create `crates/stratify-report/src/json.rs`:

```rust
use stratify_core::Report;

/// Render a report as pretty JSON. This is the machine contract.
pub fn render(report: &Report) -> String {
    serde_json::to_string_pretty(report).expect("report serializes")
}

#[cfg(test)]
mod tests {
    use super::*;
    use stratify_core::ir::Span;
    use stratify_core::{Confidence, Finding, Severity};

    #[test]
    fn renders_schema_version_and_finding() {
        let report = Report::new(vec![Finding {
            rule: "dead_code".into(),
            severity: Severity::Warning,
            message: "unused function `orphan`".into(),
            span: Span { file: "T.java".into(), start_byte: 0, end_byte: 1, start_line: 5 },
            confidence: Confidence::Certain,
        }]);
        let out = render(&report);
        assert!(out.contains("\"schema_version\": 1"));
        assert!(out.contains("\"rule\": \"dead_code\""));
    }
}
```

- [ ] **Step 3: Write the human renderer with a test**

Create `crates/stratify-report/src/human.rs`:

```rust
use stratify_core::{Report, Severity};

/// Render a report as human-readable lines: `severity file:line  message`.
pub fn render(report: &Report) -> String {
    if report.findings.is_empty() {
        return "No findings.\n".to_string();
    }
    let mut out = String::new();
    for f in &report.findings {
        let sev = match f.severity {
            Severity::Error => "error",
            Severity::Warning => "warn",
            Severity::Info => "info",
        };
        out.push_str(&format!(
            "{sev:<5} {}:{}  {}\n",
            f.span.file, f.span.start_line, f.message
        ));
    }
    out.push_str(&format!("\n{} finding(s).\n", report.findings.len()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use stratify_core::ir::Span;
    use stratify_core::{Confidence, Finding, Severity};

    #[test]
    fn empty_report_says_no_findings() {
        let r = Report::new(vec![]);
        assert_eq!(render(&r), "No findings.\n");
    }

    #[test]
    fn formats_a_finding_line() {
        let r = Report::new(vec![Finding {
            rule: "dead_code".into(),
            severity: Severity::Warning,
            message: "unused function `orphan`".into(),
            span: Span { file: "T.java".into(), start_byte: 0, end_byte: 1, start_line: 5 },
            confidence: Confidence::Certain,
        }]);
        let out = render(&r);
        assert!(out.contains("warn  T.java:5  unused function `orphan`"));
        assert!(out.contains("1 finding(s)."));
    }
}
```

- [ ] **Step 4: Wire lib.rs**

Create `crates/stratify-report/src/lib.rs`:

```rust
pub mod human;
pub mod json;
```

- [ ] **Step 5: Run, verify pass**

Run: `cargo test -p stratify-report`
Expected: PASS, 3 tests.

- [ ] **Step 6: Commit**

```bash
git add crates/stratify-report
git commit -m "feat(report): JSON and human renderers"
```

---

## Task 10: CLI orchestration (`stratify-cli`)

**Files:**
- Create: `crates/stratify-cli/Cargo.toml`
- Create: `crates/stratify-cli/src/main.rs`
- Create: `crates/stratify-cli/src/run.rs`

- [ ] **Step 1: Write the manifest (binary named `stratify`)**

Create `crates/stratify-cli/Cargo.toml`:

```toml
[package]
name = "stratify-cli"
edition.workspace = true
version.workspace = true
license.workspace = true

[[bin]]
name = "stratify"
path = "src/main.rs"

[dependencies]
stratify-core = { path = "../stratify-core" }
stratify-lang = { path = "../stratify-lang" }
stratify-lang-java = { path = "../stratify-lang-java" }
stratify-analysis = { path = "../stratify-analysis" }
stratify-report = { path = "../stratify-report" }
clap = { workspace = true }
ignore = { workspace = true }
```

- [ ] **Step 2: Write the orchestration with a test on a temp dir**

Create `crates/stratify-cli/src/run.rs`:

```rust
use std::path::Path;
use ignore::WalkBuilder;
use stratify_analysis::deadcode;
use stratify_core::{IrGraph, Report};
use stratify_lang::LanguageAdapter;
use stratify_lang_java::JavaAdapter;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Human,
    Json,
}

/// Walk `root`, parse every file a registered adapter handles, merge into one
/// IrGraph, run dead-code, and return the rendered string.
pub fn run(root: &Path, format: Format) -> std::io::Result<String> {
    let adapters: Vec<Box<dyn LanguageAdapter>> = vec![Box::new(JavaAdapter)];

    let mut graph = IrGraph::new();
    for entry in WalkBuilder::new(root).build() {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let path = entry.path();
        let ext = match path.extension().and_then(|e| e.to_str()) {
            Some(e) => e,
            None => continue,
        };
        let adapter = match adapters.iter().find(|a| a.handles_extension(ext)) {
            Some(a) => a,
            None => continue,
        };
        let source = std::fs::read_to_string(path)?;
        let rel = path.strip_prefix(root).unwrap_or(path).to_string_lossy().to_string();
        if let Ok(file_graph) = adapter.parse_file(&rel, &source) {
            graph.merge(file_graph);
        }
    }

    let findings = deadcode::analyze(&graph);
    let report = Report::new(findings);

    Ok(match format {
        Format::Human => stratify_report::human::render(&report),
        Format::Json => stratify_report::json::render(&report),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_flags_unused_method_in_temp_repo() {
        let dir = std::env::temp_dir().join("stratify-cli-test-1");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("App.java"),
            "class App {\n  public static void main(String[] a) {}\n  void unusedHelper() {}\n}\n",
        )
        .unwrap();

        let out = run(&dir, Format::Human).unwrap();
        assert!(out.contains("unusedHelper"), "got: {out}");

        let json = run(&dir, Format::Json).unwrap();
        assert!(json.contains("\"schema_version\": 1"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
```

- [ ] **Step 3: Write the clap entrypoint**

Create `crates/stratify-cli/src/main.rs`:

```rust
mod run;

use std::path::PathBuf;
use std::process::ExitCode;
use clap::{Parser, Subcommand, ValueEnum};
use run::Format;

#[derive(Parser)]
#[command(name = "stratify", version, about = "Polyglot codebase intelligence")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Analyze a repository and report findings.
    Check {
        /// Path to the repository root.
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Output format.
        #[arg(long, value_enum, default_value_t = FormatArg::Human)]
        format: FormatArg,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum FormatArg {
    Human,
    Json,
}

impl From<FormatArg> for Format {
    fn from(f: FormatArg) -> Self {
        match f {
            FormatArg::Human => Format::Human,
            FormatArg::Json => Format::Json,
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Check { path, format } => match run::run(&path, format.into()) {
            Ok(out) => {
                print!("{out}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("stratify: {e}");
                ExitCode::FAILURE
            }
        },
    }
}
```

- [ ] **Step 4: Run the crate test, verify pass**

Run: `cargo test -p stratify-cli`
Expected: PASS, 1 test.

- [ ] **Step 5: Build the whole workspace and run the binary by hand**

Run: `cargo build && ./target/debug/stratify check crates/stratify-cli/tests/sample-java`
(If the sample dir does not exist yet, create it in Task 11. For now run against any dir with a `.java` file.)
Expected: prints findings or "No findings.".

- [ ] **Step 6: Commit**

```bash
git add crates/stratify-cli
git commit -m "feat(cli): stratify check command wiring discover -> analyze -> render"
```

---

## Task 11: End-to-end fixture and snapshot test

**Files:**
- Create: `crates/stratify-cli/tests/sample-java/App.java`
- Create: `crates/stratify-cli/tests/sample-java/Unused.java`
- Create: `crates/stratify-cli/tests/e2e.rs`
- Modify: `crates/stratify-cli/Cargo.toml` (add insta dev-dependency)

- [ ] **Step 1: Create the sample Java app**

Create `crates/stratify-cli/tests/sample-java/App.java`:

```java
class App {
  public static void main(String[] args) {
    helper();
  }

  static void helper() {
    System.out.println("used");
  }
}
```

Create `crates/stratify-cli/tests/sample-java/Unused.java`:

```java
class Unused {
  void neverCalled() {
    System.out.println("dead");
  }
}
```

- [ ] **Step 2: Add insta dev-dependency**

Add to `crates/stratify-cli/Cargo.toml`:

```toml
[dev-dependencies]
insta = { workspace = true }
```

- [ ] **Step 3: Write the end-to-end snapshot test**

Create `crates/stratify-cli/tests/e2e.rs`:

```rust
use std::path::Path;

// Re-run the same logic the binary uses by shelling out is avoided; instead we
// assert on observable output text to keep the test hermetic.
#[test]
fn sample_java_reports_unused_methods() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sample-java");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_stratify"))
        .arg("check")
        .arg(&dir)
        .arg("--format")
        .arg("human")
        .output()
        .expect("run stratify binary");
    let stdout = String::from_utf8(output.stdout).unwrap();

    // `helper` is reached from main via a Likely intra-file call edge -> possibly unused (info).
    // `neverCalled` is never reached -> unused (warning).
    assert!(stdout.contains("neverCalled"), "stdout: {stdout}");
    assert!(stdout.contains("warn"), "stdout: {stdout}");
}
```

- [ ] **Step 4: Run, verify pass**

Run: `cargo test -p stratify-cli --test e2e`
Expected: PASS. (`neverCalled` reported as a warning. `helper` reported as info "possibly unused" because intra-file calls carry Likely confidence in M1.)

- [ ] **Step 5: Commit**

```bash
git add crates/stratify-cli/tests crates/stratify-cli/Cargo.toml
git commit -m "test(cli): end-to-end dead-code run on sample Java app"
```

---

## Task 12: Workspace lint and lockfile

**Files:**
- Modify: `Cargo.lock` (generated)

- [ ] **Step 1: Format and lint the whole workspace**

Run: `cargo fmt --all && cargo clippy --all-targets -- -D warnings`
Expected: no warnings. Fix any clippy findings inline.

- [ ] **Step 2: Run the full test suite once more**

Run: `cargo test`
Expected: all tests pass across all six crates.

- [ ] **Step 3: Commit the lockfile and any fmt changes**

```bash
git add -A
git commit -m "chore: fmt, clippy clean, commit lockfile"
```

---

## Self-Review Notes

Spec coverage for M1:
- IR (symbols, references, confidence, finding schema): Tasks 2, 3, 4. Covered.
- LanguageAdapter trait: Task 5. Covered.
- Java adapter (symbols + edges): Tasks 6, 7. Covered.
- Dead-code analysis on IR with confidence downgrade: Task 8. Covered.
- JSON + human output: Task 9. Covered.
- `stratify check` CLI + file discovery: Task 10. Covered.
- End-to-end on a sample Java app: Task 11. Covered.

Deferred to later milestones (correctly out of M1 scope): Ruby adapter (M2), duplication and complexity (M3), architecture boundaries (M4), SARIF/CI (M5), MCP (M6), LSP (M7), unused-dependency detection from manifests (folded into M2/M3 when manifests are parsed), cross-file Java call resolution (raises confidence from Likely to Certain; M2+).

Known M1 simplification: intra-file calls carry `Likely` confidence, so a method reachable only inside its own file is reported as "possibly unused" (info), not silently cleared. This is intentional and tested. Cross-file resolution in a later milestone promotes these to `Certain`.

Type consistency check: `Confidence`, `Symbol`, `Reference`, `IrGraph`, `Finding`, `Report`, `Severity`, `LanguageAdapter`, `JavaAdapter`, `deadcode::analyze`, `Format`, `run::run` are referenced with consistent names and signatures across tasks.

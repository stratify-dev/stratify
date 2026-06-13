# Stratify M10 (LSP Server) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Surface Stratify findings as editor diagnostics. Add `stratify lsp`, a stdio Language Server that runs the analyses on the workspace and publishes diagnostics when files open or save.

**Architecture:** A new `lsp` module in `stratify-cli`. LSP uses JSON-RPC 2.0 with `Content-Length` framing (distinct from MCP's newline-delimited transport), so the module has its own `read_message`/`write_message`. A `Server` struct holds the workspace root (captured at `initialize`) and a `handle(&mut self, &Value) -> Vec<Value>` method returning outgoing messages (responses plus pushed `textDocument/publishDiagnostics` notifications). On `didOpen`/`didSave` it runs `run::analyze_repo` on the root and maps findings to LSP diagnostics, grouped per file.

**Tech Stack:** Rust, serde_json (already a runtime dep of `stratify-cli` from M9), the existing analysis pipeline.

**LSP protocol notes (stdio):** each message is JSON-RPC 2.0 framed as `Content-Length: <N>\r\n\r\n<N bytes of UTF-8 JSON>`. Handshake: client sends `initialize` (request, carries `rootUri`/`rootPath`/`workspaceFolders`) → server replies with `capabilities` → client sends `initialized` (notification). Then `textDocument/didOpen`, `didSave`, `didClose` notifications flow in; the server pushes `textDocument/publishDiagnostics` notifications: `{method, params:{uri, diagnostics:[{range, severity, source, code, message}]}}`. A diagnostic `range` is `{start:{line, character}, end:{line, character}}` with **0-based** lines. `DiagnosticSeverity`: Error=1, Warning=2, Information=3, Hint=4. `shutdown` (request) → null result; `exit` (notification) → process exits.

**Prerequisite reading:** `crates/stratify-cli/src/mcp.rs` (the JSON-RPC shape and test style to mirror — but note LSP framing differs), `crates/stratify-cli/src/run.rs` (`analyze_repo`), `crates/stratify-core/src/finding.rs` (`Finding`, `Severity`, `Span`).

---

## File Structure

```
crates/stratify-cli/src/lsp.rs        CREATE: Server + framing + diagnostics mapping + run_stdio + tests
crates/stratify-cli/src/main.rs       MODIFY: mod lsp; + `Lsp` subcommand
crates/stratify-cli/tests/e2e_lsp.rs  CREATE: spawn `stratify lsp`, framed handshake + didOpen → diagnostics
README.md                             MODIFY: LSP / editor diagnostics section
```

---

## Task 1: LSP module (`stratify-cli`)

**Files:**
- Create: `crates/stratify-cli/src/lsp.rs`
- Modify: `crates/stratify-cli/src/main.rs` (add `mod lsp;`)

- [ ] **Step 1: Write the module with tests**

Create `crates/stratify-cli/src/lsp.rs`:

```rust
use std::io::{BufRead, Read, Write};
use std::path::PathBuf;
use serde_json::{json, Value};
use stratify_core::Severity;

/// A minimal stdio Language Server for Stratify diagnostics.
pub struct Server {
    root: Option<PathBuf>,
}

impl Default for Server {
    fn default() -> Self {
        Self::new()
    }
}

impl Server {
    pub fn new() -> Self {
        Server { root: None }
    }

    /// Handle one incoming message, returning zero or more outgoing messages
    /// (responses and/or pushed notifications).
    pub fn handle(&mut self, msg: &Value) -> Vec<Value> {
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        let id = msg.get("id").cloned();

        match method {
            "initialize" => {
                self.root = workspace_root(msg.get("params").unwrap_or(&Value::Null));
                vec![response(
                    id.unwrap_or(Value::Null),
                    json!({
                        "capabilities": {
                            "textDocumentSync": { "openClose": true, "save": true }
                        },
                        "serverInfo": { "name": "stratify", "version": env!("CARGO_PKG_VERSION") }
                    }),
                )]
            }
            "initialized" => vec![],
            "textDocument/didOpen" | "textDocument/didSave" => self.publish_all(),
            "textDocument/didClose" => vec![],
            "shutdown" => vec![response(id.unwrap_or(Value::Null), Value::Null)],
            "exit" => vec![],
            _ => {
                // Unknown request gets an error; unknown notification is ignored.
                match id {
                    Some(id) => vec![error(id, -32601, "method not found")],
                    None => vec![],
                }
            }
        }
    }

    /// Run the analyses on the workspace and emit one publishDiagnostics
    /// notification per file (including files with zero findings would require
    /// tracking open docs; here we publish only files that have findings).
    fn publish_all(&self) -> Vec<Value> {
        let Some(root) = &self.root else {
            return Vec::new();
        };
        let report = match crate::run::analyze_repo(root) {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };

        // Group findings by file (sorted for determinism).
        let mut by_file: std::collections::BTreeMap<String, Vec<Value>> =
            std::collections::BTreeMap::new();
        for f in &report.findings {
            let line = f.span.start_line.saturating_sub(1) as u64;
            let diag = json!({
                "range": {
                    "start": { "line": line, "character": 0 },
                    "end": { "line": line, "character": 100000 }
                },
                "severity": severity_code(f.severity),
                "source": "stratify",
                "code": f.rule,
                "message": f.message,
            });
            by_file.entry(f.span.file.clone()).or_default().push(diag);
        }

        let root_str = root.to_string_lossy();
        by_file
            .into_iter()
            .map(|(file, diags)| {
                let uri = format!("file://{}/{}", root_str.trim_end_matches('/'), file);
                json!({
                    "jsonrpc": "2.0",
                    "method": "textDocument/publishDiagnostics",
                    "params": { "uri": uri, "diagnostics": diags }
                })
            })
            .collect()
    }
}

fn severity_code(s: Severity) -> u64 {
    match s {
        Severity::Error => 1,
        Severity::Warning => 2,
        Severity::Info => 3,
    }
}

/// Extract the workspace root path from initialize params.
fn workspace_root(params: &Value) -> Option<PathBuf> {
    // workspaceFolders[0].uri, then rootUri, then rootPath.
    let uri = params
        .get("workspaceFolders")
        .and_then(|f| f.get(0))
        .and_then(|f| f.get("uri"))
        .and_then(Value::as_str)
        .or_else(|| params.get("rootUri").and_then(Value::as_str));
    if let Some(uri) = uri {
        return Some(PathBuf::from(uri_to_path(uri)));
    }
    params
        .get("rootPath")
        .and_then(Value::as_str)
        .map(PathBuf::from)
}

/// Convert a `file://` URI to a filesystem path (minimal: strip the scheme).
fn uri_to_path(uri: &str) -> String {
    uri.strip_prefix("file://").unwrap_or(uri).to_string()
}

fn response(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// Read one `Content-Length`-framed JSON-RPC message. Returns `Ok(None)` on EOF.
pub fn read_message<R: BufRead>(r: &mut R) -> std::io::Result<Option<Value>> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        let n = r.read_line(&mut line)?;
        if n == 0 {
            return Ok(None); // EOF
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break; // end of headers
        }
        if let Some(rest) = line.strip_prefix("Content-Length:") {
            content_length = rest.trim().parse().ok();
        }
    }
    let len = match content_length {
        Some(l) => l,
        None => return Ok(None),
    };
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    Ok(serde_json::from_slice(&buf).ok())
}

/// Write one `Content-Length`-framed JSON-RPC message.
pub fn write_message<W: Write>(w: &mut W, msg: &Value) -> std::io::Result<()> {
    let body = serde_json::to_vec(msg).expect("serialize");
    write!(w, "Content-Length: {}\r\n\r\n", body.len())?;
    w.write_all(&body)?;
    w.flush()
}

/// Run the LSP server over real stdin/stdout until `exit` or EOF.
pub fn run_stdio() -> std::io::Result<()> {
    let stdin = std::io::stdin();
    let mut reader = stdin.lock();
    let stdout = std::io::stdout();
    let mut writer = stdout.lock();
    let mut server = Server::new();
    while let Some(msg) = read_message(&mut reader)? {
        let is_exit = msg.get("method").and_then(Value::as_str) == Some("exit");
        for out in server.handle(&msg) {
            write_message(&mut writer, &out)?;
        }
        if is_exit {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn fixture_uri() -> String {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sample-ruby");
        format!("file://{}", dir.to_str().unwrap())
    }

    #[test]
    fn initialize_sets_root_and_returns_capabilities() {
        let mut s = Server::new();
        let req = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri": fixture_uri()}});
        let out = s.handle(&req);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["result"]["serverInfo"]["name"], "stratify");
        assert!(out[0]["result"]["capabilities"]["textDocumentSync"]["openClose"].as_bool().unwrap());
        assert!(s.root.is_some());
    }

    #[test]
    fn did_open_publishes_diagnostics() {
        let mut s = Server::new();
        s.handle(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri": fixture_uri()}}));
        let out = s.handle(&json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{}}));
        assert!(!out.is_empty(), "expected publishDiagnostics");
        // every emitted message is a publishDiagnostics notification
        assert!(out.iter().all(|m| m["method"] == "textDocument/publishDiagnostics"));
        // at least one diagnostic from the dead_code rule exists across files
        let has_dead_code = out.iter().any(|m| {
            m["params"]["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .any(|d| d["code"] == "dead_code")
        });
        assert!(has_dead_code, "out: {out:?}");
    }

    #[test]
    fn diagnostic_range_is_zero_based_line() {
        let mut s = Server::new();
        s.handle(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri": fixture_uri()}}));
        let out = s.handle(&json!({"jsonrpc":"2.0","method":"textDocument/didSave","params":{}}));
        // unused.rb's never_called is on line 1 (1-based) -> 0 in LSP.
        let diag = out
            .iter()
            .flat_map(|m| m["params"]["diagnostics"].as_array().unwrap().clone())
            .find(|d| d["message"].as_str().unwrap().contains("never_called"))
            .expect("never_called diagnostic");
        assert_eq!(diag["range"]["start"]["line"], 0);
        assert_eq!(diag["severity"], 2); // Warning
        assert_eq!(diag["source"], "stratify");
    }

    #[test]
    fn uri_is_root_plus_relative_file() {
        let mut s = Server::new();
        s.handle(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri": fixture_uri()}}));
        let out = s.handle(&json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{}}));
        // a published uri ends with /unused.rb (a fixture file with a finding)
        assert!(out.iter().any(|m| m["params"]["uri"].as_str().unwrap().ends_with("/unused.rb")), "out: {out:?}");
    }

    #[test]
    fn shutdown_returns_null_result() {
        let mut s = Server::new();
        let out = s.handle(&json!({"jsonrpc":"2.0","id":9,"method":"shutdown"}));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["id"], 9);
        assert!(out[0]["result"].is_null());
    }

    #[test]
    fn unknown_request_is_method_not_found_notification_ignored() {
        let mut s = Server::new();
        assert_eq!(s.handle(&json!({"jsonrpc":"2.0","id":5,"method":"bogus"}))[0]["error"]["code"], -32601);
        assert!(s.handle(&json!({"jsonrpc":"2.0","method":"$/someNotification"})).is_empty());
    }

    #[test]
    fn framing_round_trips() {
        let msg = json!({"jsonrpc":"2.0","id":1,"method":"initialize"});
        let mut buf: Vec<u8> = Vec::new();
        write_message(&mut buf, &msg).unwrap();
        // the framed bytes start with the Content-Length header
        let s = String::from_utf8(buf.clone()).unwrap();
        assert!(s.starts_with("Content-Length: "));
        assert!(s.contains("\r\n\r\n"));
        // and read_message parses it back
        let mut cur = Cursor::new(buf);
        let back = read_message(&mut cur).unwrap().unwrap();
        assert_eq!(back, msg);
    }

    #[test]
    fn read_message_eof_returns_none() {
        let mut cur = Cursor::new(Vec::<u8>::new());
        assert!(read_message(&mut cur).unwrap().is_none());
    }
}
```

- [ ] **Step 2: Declare the module**

In `crates/stratify-cli/src/main.rs`, add `mod lsp;` near the other `mod` lines.

- [ ] **Step 3: Run, verify pass**

Run: `cargo test -p stratify-cli lsp` (if `cargo` missing: `source "$HOME/.cargo/env"`)
Expected: PASS (8 lsp tests). `cargo build` may warn that `run_stdio` is unused until Task 2 adds the subcommand caller — acceptable here, cleared in Task 2. Do NOT add `#[allow(dead_code)]`.

- [ ] **Step 4: Commit**

```bash
git add crates/stratify-cli/src/lsp.rs crates/stratify-cli/src/main.rs
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(cli): LSP stdio server module (Content-Length framed)"
```

---

## Task 2: `stratify lsp` subcommand + end-to-end + README (`stratify-cli`)

**Files:**
- Modify: `crates/stratify-cli/src/main.rs`
- Create: `crates/stratify-cli/tests/e2e_lsp.rs`
- Modify: `README.md`

- [ ] **Step 1: Add the subcommand**

In `crates/stratify-cli/src/main.rs`, add an `Lsp` variant to the `Command` enum:

```rust
    /// Run a Language Server over stdio for editor diagnostics.
    Lsp,
```

In the `match cli.command` block, add:

```rust
        Command::Lsp => match lsp::run_stdio() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("stratify: {e}");
                ExitCode::FAILURE
            }
        },
```

This gives `run_stdio` a caller, clearing the dead-code warning.

- [ ] **Step 2: Write the framed end-to-end test**

Create `crates/stratify-cli/tests/e2e_lsp.rs`:

```rust
use std::io::{BufReader, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

fn frame(msg: &str) -> Vec<u8> {
    format!("Content-Length: {}\r\n\r\n{}", msg.len(), msg).into_bytes()
}

/// Read one Content-Length-framed message body from `r`.
fn read_framed<R: std::io::BufRead>(r: &mut R) -> Option<String> {
    let mut len: Option<usize> = None;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        if let Some(rest) = line.strip_prefix("Content-Length:") {
            len = rest.trim().parse().ok();
        }
    }
    let len = len?;
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).ok()?;
    Some(String::from_utf8(buf).ok()?)
}

#[test]
fn lsp_publishes_diagnostics_on_did_open() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sample-ruby");
    let root_uri = format!("file://{}", dir.to_str().unwrap());

    let mut child = Command::new(env!("CARGO_BIN_EXE_stratify"))
        .arg("lsp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn stratify lsp");

    {
        let stdin = child.stdin.as_mut().unwrap();
        let init = format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{{\"rootUri\":\"{root_uri}\"}}}}"
        );
        stdin.write_all(&frame(&init)).unwrap();
        stdin
            .write_all(&frame("{\"jsonrpc\":\"2.0\",\"method\":\"initialized\",\"params\":{}}"))
            .unwrap();
        stdin
            .write_all(&frame("{\"jsonrpc\":\"2.0\",\"method\":\"textDocument/didOpen\",\"params\":{}}"))
            .unwrap();
        stdin
            .write_all(&frame("{\"jsonrpc\":\"2.0\",\"method\":\"exit\"}"))
            .unwrap();
    }

    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);

    // First framed message is the initialize response.
    let init_resp = read_framed(&mut reader).expect("initialize response");
    assert!(init_resp.contains("\"name\":\"stratify\""), "init: {init_resp}");

    // Subsequent framed messages include publishDiagnostics with a dead_code finding.
    let mut saw_dead_code = false;
    while let Some(msg) = read_framed(&mut reader) {
        if msg.contains("publishDiagnostics") && msg.contains("dead_code") {
            saw_dead_code = true;
        }
    }
    assert!(saw_dead_code, "expected a publishDiagnostics with dead_code");

    let _ = child.wait();
}
```

- [ ] **Step 3: Run + manual smoke**

Run: `cargo test -p stratify-cli`
Expected: PASS including `e2e_lsp`. `cargo build 2>&1 | grep -i warning` → none.

Manual (framed input is awkward by hand; rely on the e2e + unit tests). Optionally:
```bash
cargo build && printf 'Content-Length: 110\r\n\r\n{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":"file:///tmp"}}' | ./target/debug/stratify lsp | head -c 200; echo
```
(The exact byte count must match; the e2e is the reliable check.)

- [ ] **Step 4: Document the LSP server in the README**

In `README.md`, add a section after the MCP section:

```markdown
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
```

(Keep the writing tight: short active sentences, no em dashes, no semicolons.)

- [ ] **Step 5: Commit**

```bash
git add crates/stratify-cli README.md
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(cli): stratify lsp subcommand, framed end-to-end test, docs"
```

---

## Task 3: fmt, clippy, lockfile

- [ ] **Step 1: Format and lint**

Run: `cargo fmt --all && cargo clippy --all-targets -- -D warnings`
Fix any warning properly (no blanket `#[allow]`). Re-run `cargo test` after any fix.

- [ ] **Step 2: Full suite**

Run: `cargo test`
Expected: all crates green (cli gains 8 lsp unit tests + e2e_lsp).

- [ ] **Step 3: Commit**

```bash
git add -A
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "chore: fmt, clippy clean, update lockfile for lsp"
```

---

## Self-Review Notes

Spec coverage for M10:
- Content-Length-framed stdio LSP (initialize, initialized, didOpen/didSave/didClose, shutdown, exit): Task 1. Covered.
- Findings → diagnostics mapping (0-based lines, severity codes, per-file publish): Task 1. Covered.
- `stratify lsp` subcommand + framed end-to-end: Task 2. Covered.
- Editor-integration docs: Task 2 Step 4. Covered.

Deferred (correctly out of M10): incremental sync and per-keystroke re-analysis (we analyze on open/save only), column-accurate ranges (we highlight the whole line since spans track line + byte, not column), clearing diagnostics for closed/now-clean files (we publish only files that currently have findings; a file that becomes clean keeps its last diagnostics until the next analysis that still omits it — a known limitation), code actions / quick fixes, and `workspace/didChangeWatchedFiles`. URI handling is minimal (strip `file://`, no percent-decoding) — fine for typical local paths, refined later.

Known M10 characteristics (acceptable):
- The server re-runs the whole-workspace analysis on each open/save. For large repos this is heavier than incremental analysis, but correct and simple for a first LSP.
- Diagnostic ranges span the whole line (`character: 0` to a large end), because spans don't carry columns. Editors clamp the end to the line length.
- A file that had findings and later has none will not get a clearing `publishDiagnostics` unless re-analysis still references it. Tracking open documents to emit empty-diagnostic clears is a later refinement.

Type consistency: `lsp::Server::handle`, `lsp::read_message`/`write_message`, `lsp::run_stdio`, `run::analyze_repo`, `Severity`, `Finding`/`Span` are used consistently with their M1-M9 definitions.

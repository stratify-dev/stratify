# Stratify M9 (MCP Server) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let coding agents query Stratify's findings over the Model Context Protocol. Add `stratify mcp`, a stdio JSON-RPC 2.0 MCP server exposing an `analyze` tool that runs the analyses on a path and returns the findings.

**Architecture:** A new `mcp` module in `stratify-cli` implements the MCP wire protocol by hand with `serde_json` (no SDK — the binary is the server). A pure `handle_request(&Value) -> Option<Value>` function dispatches `initialize`, `notifications/initialized`, `tools/list`, and `tools/call`; a thin `run_stdio()` loop reads newline-delimited JSON-RPC from stdin and writes responses to stdout. The `analyze` tool reuses `run::analyze_repo` and the existing JSON renderer, so the MCP surface is just another consumer of the same findings.

**Tech Stack:** Rust, serde_json (already a workspace dep), the existing analysis pipeline.

**MCP protocol notes (stdio transport):** messages are JSON-RPC 2.0 objects, one per line (newline-delimited UTF-8, no embedded newlines). A request has `{"jsonrpc":"2.0","id":<id>,"method":<m>,"params":<p>}` and gets a response `{"jsonrpc":"2.0","id":<id>,"result":{...}}` or `{"jsonrpc":"2.0","id":<id>,"error":{"code":<c>,"message":<m>}}`. A notification has no `id` and gets no response. Handshake: client sends `initialize` → server replies with `protocolVersion`/`capabilities`/`serverInfo` → client sends the `notifications/initialized` notification → client may then call `tools/list` and `tools/call`. A tool definition is `{name, description, inputSchema}` (JSON Schema). A `tools/call` result is `{"content":[{"type":"text","text":...}], "isError":<bool>}`.

**Prerequisite reading:** `crates/stratify-cli/src/run.rs` (`analyze_repo` + the analysis pipeline), `crates/stratify-cli/src/main.rs` (clap `Command` enum and `mod` declarations), `crates/stratify-report/src/json.rs`.

---

## File Structure

```
crates/stratify-cli/src/mcp.rs        CREATE: handle_request + run_stdio + tests
crates/stratify-cli/src/main.rs       MODIFY: mod mcp; + `Mcp` subcommand
crates/stratify-cli/tests/e2e_mcp.rs  CREATE: spawn `stratify mcp`, drive the handshake
README.md                             MODIFY: MCP server section
```

---

## Task 1: MCP module (`stratify-cli`)

**Files:**
- Create: `crates/stratify-cli/src/mcp.rs`
- Modify: `crates/stratify-cli/src/main.rs` (add `mod mcp;`)

- [ ] **Step 1: Write the module with tests**

Create `crates/stratify-cli/src/mcp.rs`:

```rust
use std::io::{BufRead, Write};
use std::path::Path;
use serde_json::{json, Value};

const PROTOCOL_VERSION: &str = "2024-11-05";

/// Handle one JSON-RPC request. Returns `Some(response)` for requests and
/// `None` for notifications (which get no reply).
pub fn handle_request(req: &Value) -> Option<Value> {
    let method = req.get("method").and_then(Value::as_str).unwrap_or("");
    let id = req.get("id").cloned();

    // Notifications have no `id` and never get a response.
    if id.is_none() {
        return None;
    }
    let id = id.unwrap();

    match method {
        "initialize" => {
            let proto = req
                .get("params")
                .and_then(|p| p.get("protocolVersion"))
                .and_then(Value::as_str)
                .unwrap_or(PROTOCOL_VERSION)
                .to_string();
            Some(success(
                id,
                json!({
                    "protocolVersion": proto,
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "stratify", "version": env!("CARGO_PKG_VERSION") }
                }),
            ))
        }
        "tools/list" => Some(success(id, json!({ "tools": [analyze_tool()] }))),
        "tools/call" => Some(handle_tools_call(id, req)),
        _ => Some(error(id, -32601, "method not found")),
    }
}

fn analyze_tool() -> Value {
    json!({
        "name": "analyze",
        "description": "Run Stratify's analyses (dead code, duplication, complexity, hotspots, cycles, layer boundaries) on a repository path and return the findings as JSON.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Repository path to analyze." },
                "rule": { "type": "string", "description": "Optional rule filter: dead_code, duplication, complexity, hotspot, cycle, or boundary." }
            },
            "required": ["path"]
        }
    })
}

fn handle_tools_call(id: Value, req: &Value) -> Value {
    let params = req.get("params").cloned().unwrap_or(Value::Null);
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    if name != "analyze" {
        return tool_error(id, format!("unknown tool `{name}`"));
    }
    let args = params.get("arguments").cloned().unwrap_or(Value::Null);
    let path = match args.get("path").and_then(Value::as_str) {
        Some(p) => p.to_string(),
        None => return tool_error(id, "missing required argument `path`".to_string()),
    };
    let rule = args.get("rule").and_then(Value::as_str).map(|s| s.to_string());

    match crate::run::analyze_repo(Path::new(&path)) {
        Ok(report) => {
            let report = match &rule {
                Some(r) => {
                    let findings = report
                        .findings
                        .into_iter()
                        .filter(|f| &f.rule == r)
                        .collect();
                    stratify_core::Report::new(findings)
                }
                None => report,
            };
            let text = stratify_report::json::render(&report);
            success(
                id,
                json!({ "content": [{ "type": "text", "text": text }], "isError": false }),
            )
        }
        Err(e) => tool_error(id, format!("analysis failed: {e}")),
    }
}

fn success(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// A tool-level failure is reported as a successful JSON-RPC result with
/// `isError: true`, per the MCP convention (so the model sees the error text).
fn tool_error(id: Value, message: String) -> Value {
    success(
        id,
        json!({ "content": [{ "type": "text", "text": message }], "isError": true }),
    )
}

/// Read newline-delimited JSON-RPC from `input`, dispatch each request, and
/// write responses to `output`. Returns on EOF.
pub fn serve<R: BufRead, W: Write>(input: R, mut output: W) -> std::io::Result<()> {
    for line in input.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue, // ignore unparseable lines
        };
        if let Some(resp) = handle_request(&req) {
            let mut s = serde_json::to_string(&resp).expect("response serializes");
            s.push('\n');
            output.write_all(s.as_bytes())?;
            output.flush()?;
        }
    }
    Ok(())
}

/// Run the MCP server over real stdin/stdout.
pub fn run_stdio() -> std::io::Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    serve(stdin.lock(), stdout.lock())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_returns_server_info() {
        let req = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}});
        let resp = handle_request(&req).unwrap();
        assert_eq!(resp["result"]["serverInfo"]["name"], "stratify");
        assert_eq!(resp["result"]["protocolVersion"], "2024-11-05");
        assert!(resp["result"]["capabilities"]["tools"].is_object());
    }

    #[test]
    fn notification_gets_no_response() {
        let req = json!({"jsonrpc":"2.0","method":"notifications/initialized"});
        assert!(handle_request(&req).is_none());
    }

    #[test]
    fn tools_list_advertises_analyze() {
        let req = json!({"jsonrpc":"2.0","id":2,"method":"tools/list"});
        let resp = handle_request(&req).unwrap();
        let tools = resp["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "analyze");
        assert!(tools[0]["inputSchema"]["properties"]["path"].is_object());
    }

    #[test]
    fn unknown_method_is_method_not_found() {
        let req = json!({"jsonrpc":"2.0","id":3,"method":"bogus"});
        let resp = handle_request(&req).unwrap();
        assert_eq!(resp["error"]["code"], -32601);
    }

    #[test]
    fn tools_call_analyze_runs_on_fixture() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/sample-ruby");
        let req = json!({
            "jsonrpc":"2.0","id":4,"method":"tools/call",
            "params": { "name": "analyze", "arguments": { "path": dir.to_str().unwrap() } }
        });
        let resp = handle_request(&req).unwrap();
        assert_eq!(resp["result"]["isError"], false);
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        // sample-ruby has a dead-code finding (never_called) in its JSON report.
        assert!(text.contains("dead_code"), "text: {text}");
        assert!(text.contains("\"schema_version\""), "text: {text}");
    }

    #[test]
    fn tools_call_rule_filter() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/sample-ruby");
        let req = json!({
            "jsonrpc":"2.0","id":5,"method":"tools/call",
            "params": { "name": "analyze", "arguments": { "path": dir.to_str().unwrap(), "rule": "duplication" } }
        });
        let resp = handle_request(&req).unwrap();
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        // sample-ruby has no duplication, so the filtered report has no findings.
        assert!(text.contains("\"findings\": []"), "text: {text}");
    }

    #[test]
    fn tools_call_unknown_tool_is_tool_error() {
        let req = json!({
            "jsonrpc":"2.0","id":6,"method":"tools/call",
            "params": { "name": "nope", "arguments": {} }
        });
        let resp = handle_request(&req).unwrap();
        assert_eq!(resp["result"]["isError"], true);
    }

    #[test]
    fn serve_processes_a_request_line() {
        let input = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}\n" as &[u8];
        let mut out: Vec<u8> = Vec::new();
        serve(input, &mut out).unwrap();
        let line = String::from_utf8(out).unwrap();
        let resp: Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(resp["result"]["tools"][0]["name"], "analyze");
    }
}
```

- [ ] **Step 2: Declare the module**

In `crates/stratify-cli/src/main.rs`, add `mod mcp;` near the existing `mod run;` (and `mod churn;`).

- [ ] **Step 3: Run, verify pass**

Run: `cargo test -p stratify-cli mcp` (if `cargo` missing: `source "$HOME/.cargo/env"`)
Expected: PASS (the 7 mcp unit tests). Also run `cargo build` (warning-free; the `Mcp` subcommand is added in Task 2, so `run_stdio`/`serve` may warn as unused until then — that is acceptable here and cleared in Task 2).

- [ ] **Step 4: Commit**

```bash
git add crates/stratify-cli/src/mcp.rs crates/stratify-cli/src/main.rs
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(cli): MCP stdio JSON-RPC server module"
```

---

## Task 2: `stratify mcp` subcommand + end-to-end + README (`stratify-cli`)

**Files:**
- Modify: `crates/stratify-cli/src/main.rs`
- Create: `crates/stratify-cli/tests/e2e_mcp.rs`
- Modify: `README.md`

- [ ] **Step 1: Add the subcommand**

In `crates/stratify-cli/src/main.rs`, add an `Mcp` variant to the `Command` enum:

```rust
#[derive(Subcommand)]
enum Command {
    /// Analyze a repository and report findings.
    Check {
        // ... existing fields unchanged ...
    },
    /// Run an MCP server over stdio for coding agents.
    Mcp,
}
```

In the `match cli.command` block in `main`, add the arm:

```rust
        Command::Mcp => match mcp::run_stdio() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("stratify: {e}");
                ExitCode::FAILURE
            }
        },
```

After this, `run_stdio`/`serve` have a caller, so `cargo build` is warning-free.

- [ ] **Step 2: Write the end-to-end handshake test**

Create `crates/stratify-cli/tests/e2e_mcp.rs`:

```rust
use std::io::Write;
use std::process::{Command, Stdio};

#[test]
fn mcp_server_handshake_and_tools_list() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_stratify"))
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn stratify mcp");

    {
        let stdin = child.stdin.as_mut().expect("stdin");
        // initialize, then tools/list, then EOF (drop stdin).
        stdin
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2024-11-05\"}}\n")
            .unwrap();
        stdin
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n")
            .unwrap();
    }
    // Dropping stdin (end of block) closes it, so the server hits EOF and exits.

    let output = child.wait_with_output().expect("wait");
    let stdout = String::from_utf8(output.stdout).unwrap();

    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 2, "expected 2 responses, got: {stdout}");

    let init: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(init["id"], 1);
    assert_eq!(init["result"]["serverInfo"]["name"], "stratify");

    let list: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(list["id"], 2);
    assert_eq!(list["result"]["tools"][0]["name"], "analyze");
}
```

`serde_json` is already a dev-dependency of `stratify-cli` (added in M8). If `cargo test` reports it missing, add `serde_json = { workspace = true }` under `[dev-dependencies]`.

- [ ] **Step 3: Run + manual smoke**

Run: `cargo test -p stratify-cli`
Expected: PASS including `e2e_mcp`.

Manual:
```bash
cargo build
printf '%s\n%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
  | ./target/debug/stratify mcp
```
Expected: two JSON lines, the first with `serverInfo.name == "stratify"`, the second listing the `analyze` tool.

- [ ] **Step 4: Document the MCP server in the README**

In `README.md`, add a section after the SARIF section:

```markdown
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
```

(Keep the writing tight: short active sentences, no em dashes, no semicolons.)

- [ ] **Step 5: Commit**

```bash
git add crates/stratify-cli README.md
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "feat(cli): stratify mcp subcommand, end-to-end handshake test, docs"
```

---

## Task 3: fmt, clippy, lockfile

- [ ] **Step 1: Format and lint**

Run: `cargo fmt --all && cargo clippy --all-targets -- -D warnings`
Fix any warning properly (no blanket `#[allow]`). Re-run `cargo test` after any fix.

- [ ] **Step 2: Full suite**

Run: `cargo test`
Expected: all crates green (cli gains 7 mcp unit tests + the e2e_mcp test).

- [ ] **Step 3: Commit**

```bash
git add -A
git -c user.name='Elber' -c user.email='elber@dynaum.com' commit -m "chore: fmt, clippy clean, update lockfile for mcp"
```

---

## Self-Review Notes

Spec coverage for M9:
- stdio JSON-RPC MCP server (initialize, tools/list, tools/call, notification handling): Task 1. Covered.
- `analyze` tool reusing the analysis pipeline + optional rule filter: Task 1. Covered.
- `stratify mcp` subcommand + end-to-end handshake: Task 2. Covered.
- Client-registration docs: Task 2 Step 4. Covered.

Deferred (correctly out of M9): additional MCP tools (`find_dead_code`, `explain_finding` are subsumed by `analyze` + the `rule` filter), MCP resources/prompts, SSE/HTTP transport (stdio is the standard local transport), and request batching. The protocol version is pinned to `2024-11-05` and echoes the client's requested version; negotiating multiple versions is a later refinement.

Known M9 characteristics (acceptable):
- One tool (`analyze`) covers the use cases; the `rule` argument filters to a single analysis. This is simpler for the model than many near-duplicate tools.
- A tool failure (bad path, analysis error) returns a JSON-RPC success with `isError: true` and the error text in `content`, per MCP convention, so the model sees the failure rather than a transport error.
- Unparseable stdin lines are skipped rather than crashing the server.
- The server is synchronous and single-threaded (one request at a time over stdio), which matches the MCP stdio transport model.

Type consistency: `mcp::handle_request(&Value) -> Option<Value>`, `mcp::serve`, `mcp::run_stdio`, `run::analyze_repo`, `stratify_core::Report::new`, `stratify_report::json::render` are used consistently with their M1-M8 definitions.

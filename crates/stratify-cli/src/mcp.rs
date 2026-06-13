use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::Path;

const PROTOCOL_VERSION: &str = "2024-11-05";

/// Handle one JSON-RPC request. Returns `Some(response)` for requests and
/// `None` for notifications (which get no reply).
pub fn handle_request(req: &Value) -> Option<Value> {
    let method = req.get("method").and_then(Value::as_str).unwrap_or("");
    let id = req.get("id").cloned();

    // Notifications have no `id` and never get a response.
    let id = id?;

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
    let rule = args
        .get("rule")
        .and_then(Value::as_str)
        .map(|s| s.to_string());

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
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sample-ruby");
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
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sample-ruby");
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

    #[test]
    fn tools_call_bad_path_is_tool_error() {
        let req = json!({
            "jsonrpc":"2.0","id":7,"method":"tools/call",
            "params": { "name": "analyze", "arguments": { "path": "/tmp/stratify_does_not_exist_zzz" } }
        });
        let resp = handle_request(&req).unwrap();
        assert_eq!(resp["result"]["isError"], true);
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("path not found"), "text: {text}");
    }
}

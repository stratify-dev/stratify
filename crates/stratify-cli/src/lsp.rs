use std::io::{BufRead, Write};
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

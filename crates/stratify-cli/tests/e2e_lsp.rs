use std::io::{BufReader, Write};
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
    String::from_utf8(buf).ok()
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
            .write_all(&frame(
                "{\"jsonrpc\":\"2.0\",\"method\":\"initialized\",\"params\":{}}",
            ))
            .unwrap();
        stdin
            .write_all(&frame(
                "{\"jsonrpc\":\"2.0\",\"method\":\"textDocument/didOpen\",\"params\":{}}",
            ))
            .unwrap();
        stdin
            .write_all(&frame("{\"jsonrpc\":\"2.0\",\"method\":\"exit\"}"))
            .unwrap();
    }

    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);

    // First framed message is the initialize response.
    let init_resp = read_framed(&mut reader).expect("initialize response");
    assert!(
        init_resp.contains("\"name\":\"stratify\""),
        "init: {init_resp}"
    );

    // Subsequent framed messages include publishDiagnostics with a dead_code finding.
    let mut saw_dead_code = false;
    while let Some(msg) = read_framed(&mut reader) {
        if msg.contains("publishDiagnostics") && msg.contains("dead_code") {
            saw_dead_code = true;
        }
    }
    assert!(
        saw_dead_code,
        "expected a publishDiagnostics with dead_code"
    );

    let _ = child.wait();
}

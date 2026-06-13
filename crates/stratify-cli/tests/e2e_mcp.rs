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

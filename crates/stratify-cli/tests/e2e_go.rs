use std::path::Path;

#[test]
fn sample_go_reports_dead_code() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sample-go");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_stratify"))
        .arg("check")
        .arg(&dir)
        .arg("--format")
        .arg("json")
        .output()
        .expect("run stratify binary");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("\"rule\": \"dead_code\""), "stdout: {stdout}");
    assert!(stdout.contains("neverCalled"), "stdout: {stdout}");
    // Exported is an entrypoint, so it must NOT appear in any finding.
    assert!(!stdout.contains("Exported"), "Exported should not be flagged: {stdout}");
}

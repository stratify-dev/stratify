use std::path::Path;

#[test]
fn rust_dead_code_is_detected() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sample-rust");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_stratify"))
        .arg("check")
        .arg(&dir)
        .arg("--format")
        .arg("json")
        .output()
        .expect("run stratify binary");
    let stdout = String::from_utf8(output.stdout).unwrap();
    // `never_used` is private and uncalled -> dead; main (entrypoint) and used (called) are not flagged.
    assert!(stdout.contains("never_used"), "stdout: {stdout}");
    assert!(
        !stdout.contains("\"message\": \"unused function `used`\""),
        "stdout: {stdout}"
    );
    assert!(!stdout.contains("`main`"), "stdout: {stdout}");
}

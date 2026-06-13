use std::path::Path;

#[test]
fn sample_ruby_reports_unused_methods() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sample-ruby");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_stratify"))
        .arg("check")
        .arg(&dir)
        .arg("--format")
        .arg("human")
        .output()
        .expect("run stratify binary");
    let stdout = String::from_utf8(output.stdout).unwrap();

    // never_called is never invoked -> unused (warning).
    assert!(stdout.contains("never_called"), "stdout: {stdout}");
    assert!(stdout.contains("warn"), "stdout: {stdout}");
    // helper is called at top level via a Likely edge -> possibly unused (info).
    assert!(stdout.contains("helper") && stdout.contains("possibly unused"), "stdout: {stdout}");
}

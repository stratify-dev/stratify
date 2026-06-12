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

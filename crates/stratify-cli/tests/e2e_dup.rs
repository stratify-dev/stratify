use std::path::Path;

#[test]
fn sample_dup_reports_duplication() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sample-dup");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_stratify"))
        .arg("check")
        .arg(&dir)
        .arg("--format")
        .arg("json")
        .output()
        .expect("run stratify binary");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("\"rule\": \"duplication\""),
        "stdout: {stdout}"
    );
    // The clone spans the two files.
    assert!(
        stdout.contains("one.rb") && stdout.contains("two.rb"),
        "stdout: {stdout}"
    );
}

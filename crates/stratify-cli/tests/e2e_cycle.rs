use std::path::Path;

#[test]
fn sample_cycle_reports_circular_dependency() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sample-cycle");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_stratify"))
        .arg("check")
        .arg(&dir)
        .arg("--format")
        .arg("json")
        .output()
        .expect("run stratify binary");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("\"rule\": \"cycle\""), "stdout: {stdout}");
    assert!(
        stdout.contains("one.rb") && stdout.contains("two.rb"),
        "stdout: {stdout}"
    );
}

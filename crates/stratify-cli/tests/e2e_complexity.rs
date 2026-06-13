use std::path::Path;

#[test]
fn sample_complex_reports_high_complexity() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sample-complex");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_stratify"))
        .arg("check")
        .arg(&dir)
        .arg("--format")
        .arg("json")
        .output()
        .expect("run stratify binary");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("\"rule\": \"complexity\""),
        "stdout: {stdout}"
    );
    assert!(stdout.contains("classify"), "stdout: {stdout}");
}

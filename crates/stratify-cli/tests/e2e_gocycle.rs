use std::path::Path;

#[test]
fn go_package_cycle_is_detected() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sample-gocycle");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_stratify"))
        .arg("check")
        .arg(&dir)
        .arg("--format")
        .arg("json")
        .output()
        .expect("run stratify binary");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("\"rule\": \"cycle\""), "stdout: {stdout}");
    // the cycle spans the two packages, reported by representative file path
    assert!(
        stdout.contains("a/a.go") && stdout.contains("b/b.go"),
        "stdout: {stdout}"
    );
}

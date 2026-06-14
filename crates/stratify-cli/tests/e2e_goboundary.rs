use std::path::Path;

#[test]
fn go_boundary_violation_is_detected() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sample-goboundary");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_stratify"))
        .arg("check")
        .arg(&dir)
        .arg("--format")
        .arg("json")
        .output()
        .expect("run stratify binary");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("\"rule\": \"boundary\""), "stdout: {stdout}");
    // db importing handlers crosses the forbidden boundary
    assert!(
        stdout.contains("db/store.go") && stdout.contains("handlers/api.go"),
        "stdout: {stdout}"
    );
}

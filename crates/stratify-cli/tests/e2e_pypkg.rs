use std::path::Path;

#[test]
fn python_package_cycle_through_init_is_detected() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sample-pypkg");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_stratify"))
        .arg("check")
        .arg(&dir)
        .arg("--format")
        .arg("json")
        .output()
        .expect("run stratify binary");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("\"rule\": \"cycle\""), "stdout: {stdout}");
    // the cycle spans the two packages' __init__.py files
    assert!(
        stdout.contains("pkg_a") && stdout.contains("pkg_b"),
        "stdout: {stdout}"
    );
}

use std::path::Path;

#[test]
fn java_class_cycle_across_packages_is_detected() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sample-javacycle");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_stratify"))
        .arg("check")
        .arg(&dir)
        .arg("--format")
        .arg("json")
        .output()
        .expect("run stratify binary");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let cycle_count = stdout.matches("\"rule\": \"cycle\"").count();
    assert_eq!(
        cycle_count, 1,
        "expected one cycle finding for the ClassA <-> ClassB mutual import, got {cycle_count}: {stdout}"
    );
    assert!(
        stdout.contains("pkga/ClassA.java") || stdout.contains("pkgb/ClassB.java"),
        "stdout: {stdout}"
    );
}

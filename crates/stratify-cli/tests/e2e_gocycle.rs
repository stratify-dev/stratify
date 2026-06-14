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
    // package `a` has two files (a.go, extra.go) but collapses to one node, so
    // the a<->b cycle is reported exactly once, not once per file.
    let cycle_count = stdout.matches("\"rule\": \"cycle\"").count();
    assert_eq!(
        cycle_count, 1,
        "expected one cycle, got {cycle_count}: {stdout}"
    );
    // the cycle spans the two packages, reported by a representative file path
    // for each (package `a`'s representative is whichever of its files the walk
    // yields first).
    assert!(
        stdout.contains("a/a.go") || stdout.contains("a/extra.go"),
        "stdout: {stdout}"
    );
    assert!(stdout.contains("b/b.go"), "stdout: {stdout}");
}

use std::path::Path;

#[test]
fn ignore_globs_exclude_matching_files() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sample-ignore");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_stratify"))
        .arg("check")
        .arg(&dir)
        .arg("--format")
        .arg("json")
        .output()
        .expect("run stratify binary");
    let stdout = String::from_utf8(output.stdout).unwrap();
    // keep.rb's dead function is reported; skip/ is excluded.
    assert!(stdout.contains("kept_dead"), "stdout: {stdout}");
    assert!(!stdout.contains("ignored_dead"), "stdout: {stdout}");
}

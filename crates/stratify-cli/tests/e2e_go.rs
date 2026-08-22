use std::path::Path;

#[test]
fn sample_go_reports_dead_code() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sample-go");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_stratify"))
        .arg("check")
        .arg(&dir)
        .arg("--format")
        .arg("json")
        .output()
        .expect("run stratify binary");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("\"rule\": \"dead_code\""),
        "stdout: {stdout}"
    );
    assert!(stdout.contains("neverCalled"), "stdout: {stdout}");
    assert!(
        stdout.contains("\"severity\": \"warning\""),
        "neverCalled (unexported, unreached) must be a full Warning: {stdout}"
    );
    // Exported carries Visibility::Public, not a hard entrypoint, so an
    // unreached one is still reported - at Info/Likely under the default
    // library mode, not the Warning/Certain an unexported orphan gets.
    assert!(
        stdout.contains("possibly unused function `Exported`"),
        "an unreached exported function should surface at reduced confidence: {stdout}"
    );
    assert!(
        stdout.contains("\"severity\": \"info\""),
        "Exported must be Info, not a full Warning, under default library mode: {stdout}"
    );
}

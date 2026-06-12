use std::path::Path;

#[test]
fn fail_on_warning_exits_nonzero_when_warnings_present() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sample-java");
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_stratify"))
        .arg("check")
        .arg(&dir)
        .arg("--fail-on")
        .arg("warning")
        .status()
        .expect("run stratify binary");

    assert!(
        !status.success(),
        "expected non-zero exit when --fail-on warning and warnings are present, got: {:?}",
        status.code()
    );
}

#[test]
fn no_fail_on_exits_zero_even_with_findings() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sample-java");
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_stratify"))
        .arg("check")
        .arg(&dir)
        .status()
        .expect("run stratify binary");

    assert!(
        status.success(),
        "expected exit 0 with no --fail-on flag, got: {:?}",
        status.code()
    );
}

#[test]
fn fail_on_never_exits_zero_even_with_findings() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sample-java");
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_stratify"))
        .arg("check")
        .arg(&dir)
        .arg("--fail-on")
        .arg("never")
        .status()
        .expect("run stratify binary");

    assert!(
        status.success(),
        "expected exit 0 with --fail-on never, got: {:?}",
        status.code()
    );
}

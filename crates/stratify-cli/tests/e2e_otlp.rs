use std::path::Path;
use std::process::Command;

fn run_check(extra: &[&str]) -> (String, Option<i32>) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sample-go");
    let mut args = vec!["check", dir.to_str().unwrap(), "--format", "json"];
    args.extend_from_slice(extra);
    let output = Command::new(env!("CARGO_BIN_EXE_stratify"))
        .args(&args)
        .output()
        .expect("run stratify binary");
    (
        String::from_utf8(output.stdout).unwrap(),
        output.status.code(),
    )
}

#[test]
fn bad_otlp_endpoint_does_not_change_scan_result() {
    let (baseline, baseline_code) = run_check(&[]);
    // Unreachable endpoint (port 1). Export fails; scan output must be identical.
    let (with_otlp, otlp_code) = run_check(&["--otlp-endpoint", "http://127.0.0.1:1"]);

    assert_eq!(
        baseline, with_otlp,
        "stdout (JSON findings) must be unchanged by telemetry"
    );
    assert_eq!(baseline_code, otlp_code, "exit code must be unchanged");
    // Sanity: the fixture actually produces findings, so the comparison is meaningful.
    assert!(baseline.contains("\"findings\""), "baseline: {baseline}");
}

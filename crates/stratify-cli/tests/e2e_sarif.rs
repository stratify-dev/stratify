use std::path::Path;

#[test]
fn sarif_output_is_valid_and_has_results() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sample-ruby");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_stratify"))
        .arg("check")
        .arg(&dir)
        .arg("--format")
        .arg("sarif")
        .output()
        .expect("run stratify binary");
    let stdout = String::from_utf8(output.stdout).unwrap();

    // Must be parseable JSON and a well-formed SARIF 2.1.0 document.
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert_eq!(v["version"], "2.1.0", "stdout: {stdout}");
    assert_eq!(v["runs"][0]["tool"]["driver"]["name"], "Stratify");
    let results = v["runs"][0]["results"].as_array().expect("results array");
    assert!(!results.is_empty(), "expected findings on sample-ruby");
    // sample-ruby has a dead-code finding (never_called).
    assert!(
        results.iter().any(|r| r["ruleId"] == "dead_code"),
        "stdout: {stdout}"
    );
}

use std::path::Path;

#[test]
fn cross_file_call_downgrades_dead_to_possibly_unused() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sample-xfile");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_stratify"))
        .arg("check")
        .arg(&dir)
        .arg("--format")
        .arg("json")
        .output()
        .expect("run stratify binary");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let greet = v["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["message"].as_str().unwrap().contains("greet"))
        .expect("a finding mentioning greet");
    // Cross-file resolution connected main.rb -> greet, so greet is reachable
    // (Likely) and reported as info "possibly unused", not warning "unused".
    assert_eq!(greet["severity"], "info", "greet should be possibly-unused, not dead: {stdout}");
    assert!(greet["message"].as_str().unwrap().contains("possibly unused"), "{stdout}");
}

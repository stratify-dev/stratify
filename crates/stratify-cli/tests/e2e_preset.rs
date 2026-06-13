use std::path::Path;

#[test]
fn rails_preset_flags_models_importing_controllers() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sample-rails");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_stratify"))
        .arg("check").arg(&dir).arg("--format").arg("json")
        .output().expect("run stratify");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("\"rule\": \"boundary\""), "stdout: {stdout}");
    assert!(stdout.contains("models") && stdout.contains("controllers"), "stdout: {stdout}");
}

#[test]
fn rails_layout_autodetects_without_config() {
    // Same fixture, but we delete the toml's effect by scanning the app/ subtree
    // where there is no stratify.toml — the app/controllers dir triggers autodetect.
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sample-rails");
    // Scan a copy-free path that has app/controllers but (for this test) we rely
    // on the real fixture having stratify.toml; to test autodetect specifically,
    // point at a directory with app/controllers and NO stratify.toml.
    // The sample-rails dir HAS stratify.toml, so this test instead verifies the
    // autodetect helper path via a temp dir.
    let tmp = std::env::temp_dir().join("stratify-autodetect-rails");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(tmp.join("app/models")).unwrap();
    std::fs::create_dir_all(tmp.join("app/controllers")).unwrap();
    std::fs::write(tmp.join("app/models/user.rb"),
        "require_relative \"../controllers/c\"\n\ndef n\n  1\nend\n").unwrap();
    std::fs::write(tmp.join("app/controllers/c.rb"), "def show\n  1\nend\n").unwrap();
    // No stratify.toml in tmp -> autodetect should apply the rails preset.
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_stratify"))
        .arg("check").arg(&tmp).arg("--format").arg("json")
        .output().expect("run stratify");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("\"rule\": \"boundary\""), "autodetect stdout: {stdout}");
    let _ = std::fs::remove_dir_all(&tmp);
    let _ = dir; // keep the explicit-preset test's import tidy
}

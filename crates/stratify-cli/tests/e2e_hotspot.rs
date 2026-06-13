use std::path::Path;
use std::process::Command;

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

const GNARLY: &str = r#"def classify(n)
  if n < 0
    return "a"
  elsif n < 1
    return "b"
  elsif n < 2
    return "c"
  elsif n < 3
    return "d"
  elsif n < 4
    return "e"
  elsif n < 5
    return "f"
  elsif n < 6
    return "g"
  elsif n < 7
    return "h"
  elsif n < 8
    return "i"
  else
    return "z"
  end
end

classify(5)
"#;

#[test]
fn high_complexity_high_churn_is_a_hotspot() {
    let dir = std::env::temp_dir().join("stratify-hotspot-e2e");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q"]);
    git(&dir, &["config", "user.email", "t@example.com"]);
    git(&dir, &["config", "user.name", "Test"]);

    // Commit the same complex file 6 times -> churn 6, complexity 11 -> 66 > 50.
    for i in 0..6 {
        std::fs::write(dir.join("classify.rb"), format!("{GNARLY}# rev {i}\n")).unwrap();
        git(&dir, &["add", "classify.rb"]);
        git(&dir, &["commit", "-q", "-m", "change"]);
    }

    let output = Command::new(env!("CARGO_BIN_EXE_stratify"))
        .arg("check")
        .arg(&dir)
        .arg("--format")
        .arg("json")
        .output()
        .expect("run stratify binary");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("\"rule\": \"hotspot\""), "stdout: {stdout}");
    assert!(stdout.contains("classify"), "stdout: {stdout}");

    let _ = std::fs::remove_dir_all(&dir);
}

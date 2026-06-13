use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

/// Count how many commits touched each file, keyed by path relative to `root`
/// (matching the IR's file strings). Returns an empty map if `root` is not in
/// a git repository or git is unavailable. Best-effort: never panics.
pub fn git_churn(root: &Path) -> HashMap<String, u32> {
    let mut churn = HashMap::new();

    let toplevel = match Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--show-toplevel"])
        .output()
    {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => return churn,
    };
    let git_root = Path::new(&toplevel);

    let root_abs = match root.canonicalize() {
        Ok(p) => p,
        Err(_) => return churn,
    };

    let out = match Command::new("git")
        .arg("-C")
        .arg(git_root)
        .args(["log", "--format=", "--name-only"])
        .output()
    {
        Ok(o) if o.status.success() => o.stdout,
        _ => return churn,
    };

    let text = String::from_utf8_lossy(&out);
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let abs = git_root.join(line);
        if let Ok(rel) = abs.strip_prefix(&root_abs) {
            let key = rel.to_string_lossy().to_string();
            *churn.entry(key).or_insert(0) += 1;
        }
    }
    churn
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed");
    }

    #[test]
    fn counts_commits_per_file() {
        // Hermetic temp repo. Commit a file three times.
        let dir = std::env::temp_dir().join("stratify-churn-test-1");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q"]);
        git(&dir, &["config", "user.email", "t@example.com"]);
        git(&dir, &["config", "user.name", "Test"]);

        for i in 0..3 {
            std::fs::write(dir.join("foo.rb"), format!("def m\n  {i}\nend\n")).unwrap();
            git(&dir, &["add", "foo.rb"]);
            git(&dir, &["commit", "-q", "-m", "change"]);
        }
        // A second file committed once.
        std::fs::write(dir.join("bar.rb"), "def b\nend\n").unwrap();
        git(&dir, &["add", "bar.rb"]);
        git(&dir, &["commit", "-q", "-m", "add bar"]);

        let churn = git_churn(&dir);
        assert_eq!(churn.get("foo.rb"), Some(&3));
        assert_eq!(churn.get("bar.rb"), Some(&1));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_when_not_a_repo() {
        let dir = std::env::temp_dir().join("stratify-churn-test-not-a-repo");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(git_churn(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

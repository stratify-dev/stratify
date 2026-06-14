use std::path::Path;
use std::process::Command;
use stratify_telemetry::GitMeta;

/// Parse a git remote URL into (namespace, repo). Handles https and scp-like
/// ssh forms, stripping a trailing `.git`. Returns None for unrecognizable
/// input; a best-effort basename is still returned when a namespace is absent.
pub fn parse_remote_url(url: &str) -> (Option<String>, Option<String>) {
    let url = url.trim();
    let tail = if let Some((_, rest)) = url.split_once('@') {
        // git@github.com:org/repo.git -> github.com:org/repo.git -> org/repo.git
        rest.split_once(':').map(|(_, p)| p).unwrap_or(rest)
    } else if let Some(rest) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
    {
        // github.com/org/repo.git -> drop the host segment
        rest.split_once('/').map(|(_, p)| p).unwrap_or(rest)
    } else {
        url
    };
    let tail = tail.strip_suffix(".git").unwrap_or(tail);
    let mut segs: Vec<&str> = tail.split('/').filter(|s| !s.is_empty()).collect();
    let repo = segs.pop().map(|s| s.to_string());
    let namespace = segs.pop().map(|s| s.to_string());
    (namespace, repo)
}

/// Gather commit, branch, and origin remote for `root`. Best-effort: any field
/// that git cannot supply is None. Never panics.
pub fn git_meta(root: &Path) -> GitMeta {
    let run = |args: &[&str]| -> Option<String> {
        let out = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    };
    GitMeta {
        commit: run(&["rev-parse", "HEAD"]),
        branch: run(&["rev-parse", "--abbrev-ref", "HEAD"]),
        remote_url: run(&["remote", "get-url", "origin"]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_https_and_ssh_remotes() {
        assert_eq!(
            parse_remote_url("https://github.com/org/repo.git"),
            (Some("org".into()), Some("repo".into()))
        );
        assert_eq!(
            parse_remote_url("git@github.com:org/repo.git"),
            (Some("org".into()), Some("repo".into()))
        );
        assert_eq!(
            parse_remote_url("https://example.com/a/b/c"),
            (Some("b".into()), Some("c".into()))
        );
    }

    #[test]
    fn git_meta_outside_repo_is_empty() {
        let dir = std::env::temp_dir().join("stratify-gitmeta-not-a-repo");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let m = git_meta(&dir);
        assert!(m.commit.is_none());
        assert!(m.branch.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn git_meta_reads_a_real_repo() {
        let dir = std::env::temp_dir().join("stratify-gitmeta-real");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            assert!(Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(args)
                .status()
                .unwrap()
                .success());
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@example.com"]);
        git(&["config", "user.name", "T"]);
        git(&["remote", "add", "origin", "git@github.com:org/repo.git"]);
        std::fs::write(dir.join("f.txt"), "x").unwrap();
        git(&["add", "f.txt"]);
        git(&["commit", "-q", "-m", "init"]);

        let m = git_meta(&dir);
        assert!(m.commit.is_some());
        assert_eq!(m.remote_url.as_deref(), Some("git@github.com:org/repo.git"));
        let (ns, repo) = parse_remote_url(m.remote_url.as_deref().unwrap());
        assert_eq!(ns, Some("org".into()));
        assert_eq!(repo, Some("repo".into()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

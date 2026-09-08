use crate::model::{Changes, WorktreeInfo};
use crate::util;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const CACHE_TTL_MS: u128 = 10_000;
// ponytail: 5s poll, not inotify — keeps picker create/delete snappy at this repo count
const WORKTREE_CACHE_TTL_MS: u128 = 5_000;

#[derive(Default, Serialize, Deserialize)]
pub struct DiskCache {
    pub diffs: Vec<(PathBuf, Changes)>,
    pub worktrees: Vec<(PathBuf, Vec<WorktreeInfo>)>,
    #[serde(default)]
    pub branches: Vec<(PathBuf, Option<String>)>,
}

pub struct GitCache {
    diffs: Mutex<HashMap<PathBuf, (Changes, Instant)>>,
    worktrees: Mutex<HashMap<PathBuf, (Vec<WorktreeInfo>, Instant)>>,
    branches: Mutex<HashMap<PathBuf, (Option<String>, Instant)>>,
}

impl GitCache {
    pub fn new() -> Self {
        Self {
            diffs: Mutex::new(HashMap::new()),
            worktrees: Mutex::new(HashMap::new()),
            branches: Mutex::new(HashMap::new()),
        }
    }

    fn git_stdout(&self, path: &Path, args: &[&str]) -> Option<String> {
        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(path.to_string_lossy().as_ref());
        cmd.args(args);
        let output = cmd.output().ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).to_string())
    }

    fn compute_changes(&self, path: &Path) -> Changes {
        if !path.join(".git").exists() {
            return Changes::default();
        }
        let mut additions = 0i64;
        let mut deletions = 0i64;
        if let Some(out) = self.git_stdout(path, &["diff", "--numstat", "HEAD"]) {
            for line in out.lines() {
                let p: Vec<&str> = line.split_whitespace().collect();
                if p.len() >= 2 {
                    additions += p[0].parse::<i64>().unwrap_or(0);
                    deletions += p[1].parse::<i64>().unwrap_or(0);
                }
            }
        }
        if let Some(out) = self.git_stdout(path, &["ls-files", "--others", "--exclude-standard"]) {
            additions += out.lines().filter(|l| !l.is_empty()).count() as i64;
        }
        Changes {
            additions,
            deletions,
        }
    }

    fn list_worktrees(&self, path: &Path) -> Vec<WorktreeInfo> {
        if !path.join(".git").exists() {
            return vec![];
        }
        let Some(out) = self.git_stdout(path, &["worktree", "list"]) else {
            return vec![];
        };
        let oc_prefix = opencode_worktree_prefix();
        out.lines()
            .enumerate()
            .filter_map(|(i, line)| {
                let path = PathBuf::from(line.split_whitespace().next()?);
                if path.starts_with(&oc_prefix) {
                    return None;
                }
                Some(WorktreeInfo {
                    path,
                    is_main: i == 0,
                })
            })
            .collect()
    }

    fn compute_branch(&self, path: &Path) -> Option<String> {
        if !path.join(".git").exists() {
            return None;
        }
        let out = self.git_stdout(path, &["rev-parse", "--abbrev-ref", "HEAD"])?;
        let b = out.trim();
        if b.is_empty() || b == "HEAD" { None } else { Some(b.to_string()) }
    }

    fn cached<T: Clone>(&self, lock: &Mutex<HashMap<PathBuf, (T, Instant)>>, path: &Path, ttl: u128, f: impl FnOnce() -> T) -> T {
        if let Ok(c) = lock.lock()
            && let Some((v, t)) = c.get(path)
            && t.elapsed().as_millis() < ttl
        {
            return v.clone();
        }
        let v = f();
        if let Ok(mut c) = lock.lock() {
            c.insert(path.to_path_buf(), (v.clone(), Instant::now()));
        }
        v
    }

    pub fn diff(&self, path: &Path) -> Changes {
        self.cached(&self.diffs, path, CACHE_TTL_MS, || self.compute_changes(path))
    }
    pub fn worktrees(&self, path: &Path) -> Vec<WorktreeInfo> {
        self.cached(&self.worktrees, path, WORKTREE_CACHE_TTL_MS, || self.list_worktrees(path))
    }
    pub fn branch(&self, path: &Path) -> Option<String> {
        self.cached(&self.branches, path, CACHE_TTL_MS, || self.compute_branch(path))
    }

    pub fn load_disk(&self, cache: &DiskCache) {
        let stale = Instant::now() - Duration::from_millis((CACHE_TTL_MS + 1) as u64);
        if let Ok(mut m) = self.diffs.lock() {
            for (k, v) in &cache.diffs {
                m.insert(k.clone(), (v.clone(), stale));
            }
        }
        let stale2 = Instant::now() - Duration::from_millis((WORKTREE_CACHE_TTL_MS + 1) as u64);
        if let Ok(mut m) = self.worktrees.lock() {
            for (k, v) in &cache.worktrees {
                m.insert(k.clone(), (v.clone(), stale2));
            }
        }
        if let Ok(mut m) = self.branches.lock() {
            for (k, v) in &cache.branches {
                m.insert(k.clone(), (v.clone(), stale));
            }
        }
    }
    pub fn to_disk(&self) -> DiskCache {
        DiskCache {
            diffs: self.diffs.lock().map(|c| c.iter().map(|(k, (v, _))| (k.clone(), v.clone())).collect()).unwrap_or_default(),
            worktrees: self.worktrees.lock().map(|c| c.iter().map(|(k, (v, _))| (k.clone(), v.clone())).collect()).unwrap_or_default(),
            branches: self
                .branches
                .lock()
                .map(|c| c.iter().map(|(k, (v, _))| (k.clone(), v.clone())).collect())
                .unwrap_or_default(),
        }
    }
}

fn opencode_worktree_prefix() -> String {
    std::env::var("XDG_DATA_HOME")
        .map(|x| format!("{}/opencode/worktree", x))
        .unwrap_or_else(|_| {
            std::env::var("HOME")
                .map(|h| format!("{}/.local/share/opencode/worktree", h))
                .unwrap_or_default()
        })
}

fn git_run(path: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .map_err(|e| format!("git failed: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
    }
}

pub fn worktree_add(repo: &Path, dest: &Path, branch: &str) -> Result<(), String> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    let arg = dest.to_string_lossy().into_owned();
    match git_run(repo, &["worktree", "add", &arg, "-b", branch]) {
        Ok(_) => Ok(()),
        // branch may already exist — fall back to checking it out
        Err(_) => git_run(repo, &["worktree", "add", &arg, branch]).map(|_| ()).map_err(|e| {
            if e.is_empty() {
                format!("cannot checkout '{branch}' at {}", dest.display())
            } else {
                e
            }
        }),
    }
}

pub fn worktree_remove(repo: &Path, wt: &Path) -> Result<(), String> {
    let arg = wt.to_string_lossy().into_owned();
    git_run(repo, &["worktree", "remove", "--force", &arg]).map(|_| ()).map_err(|e| {
        if e.is_empty() {
            format!("cannot remove {}", wt.display())
        } else {
            e
        }
    })
}

pub fn sanitize_branch_name(raw: &str) -> String {
    let mut out = String::new();
    for c in raw.trim().chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '_' | '/' | '.' | '-') {
            out.push(c);
        } else {
            out.push('-');
        }
    }
    let mut s = out.trim_matches(['-', '/', '.']).to_string();
    while s.contains("--") {
        s = s.replace("--", "-");
    }
    while s.contains("//") {
        s = s.replace("//", "/");
    }
    s.replace("..", "-").replace("@{", "-")
}

pub fn auto_branch_name() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("wt-{secs}")
}

pub fn plan_worktree(repo: &Path, branch: &str, base_cfg: &str) -> PathBuf {
    let repo_name = repo
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".to_string());
    if base_cfg.trim().is_empty() {
        let flat = branch.replace('/', "-");
        repo.parent()
            .map(|p| p.join(format!("{repo_name}--{flat}")))
            .unwrap_or_else(|| PathBuf::from(format!("{repo_name}--{flat}")))
    } else {
        let mut dest = util::expand_tilde(base_cfg.trim()).join(repo_name);
        for seg in branch.split('/') {
            dest.push(seg);
        }
        dest
    }
}

pub fn unique_dest(dest: PathBuf) -> PathBuf {
    if !dest.exists() {
        return dest;
    }
    let s = dest.to_string_lossy().into_owned();
    for i in 2..1000 {
        let c = PathBuf::from(format!("{s}-{i}"));
        if !c.exists() {
            return c;
        }
    }
    dest
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_keeps_branches_readable() {
        assert_eq!(sanitize_branch_name("feat/foo"), "feat/foo");
        assert_eq!(sanitize_branch_name("  my branch! "), "my-branch");
        assert_eq!(sanitize_branch_name("a//b"), "a/b");
        assert_eq!(sanitize_branch_name("../x"), "x");
        assert_eq!(sanitize_branch_name(""), "");
        assert_eq!(sanitize_branch_name("---"), "");
    }

    #[test]
    fn plan_nests_under_base_or_siblings_by_default() {
        let repo = PathBuf::from("/home/u/Projects/myrepo");
        assert_eq!(
            plan_worktree(&repo, "feat/foo", "~/Projects/worktrees"),
            util::expand_tilde("~/Projects/worktrees").join("myrepo/feat/foo")
        );
        assert_eq!(
            plan_worktree(&repo, "feat/foo", ""),
            PathBuf::from("/home/u/Projects/myrepo--feat-foo")
        );
    }

    #[test]
    fn unique_bumps_existing() {
        let base = std::env::temp_dir().join(format!("ramo_wt_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let dest = base.join("myrepo--x");
        std::fs::create_dir_all(&dest).unwrap();
        assert_eq!(unique_dest(dest.clone()), base.join("myrepo--x-2"));
        assert_eq!(unique_dest(base.join("fresh")), base.join("fresh"));
        let _ = std::fs::remove_dir_all(&base);
    }

    fn git_ok(dir: &std::path::Path, args: &[&str]) -> bool {
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[test]
    fn worktree_add_remove_roundtrip() {
        if !git_ok(&std::env::temp_dir(), &["--version"]) {
            return;
        }
        let base = std::env::temp_dir().join(format!("ramo_e2e_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("myrepo");
        std::fs::create_dir_all(&repo).unwrap();
        for args in [
            vec!["init", "-b", "main"],
            vec!["config", "user.email", "t@t"],
            vec!["config", "user.name", "t"],
            vec!["config", "commit.gpgsign", "false"],
        ] {
            assert!(git_ok(&repo, &args), "git {args:?} failed");
        }
        std::fs::write(repo.join("f.txt"), "hi\n").unwrap();
        assert!(git_ok(&repo, &["add", "."]));
        assert!(git_ok(&repo, &["commit", "-m", "init"]));

        // `git worktree list` must see what we add (what the picker lists)
        let dest = unique_dest(plan_worktree(&repo, "feat/foo", ""));
        worktree_add(&repo, &dest, "feat/foo").unwrap();
        assert!(dest.join("f.txt").is_file());
        let listed = git_run(&repo, &["worktree", "list"]).unwrap();
        assert!(listed.contains(dest.to_str().unwrap()), "wt listed");

        // existing-but-unchecked-out branch falls back to checkout instead of `-b`
        assert!(git_ok(&repo, &["branch", "stale"]));
        let dest2 = unique_dest(plan_worktree(&repo, "stale", ""));
        worktree_add(&repo, &dest2, "stale").unwrap();
        assert!(dest2.join("f.txt").is_file());

        worktree_remove(&repo, &dest).unwrap();
        assert!(!dest.exists());
        let _ = std::fs::remove_dir_all(&base);
    }
}

//! Git information + operations for a single repository, built on `git2`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use git2::{BranchType, IndexAddOption, Repository, StatusOptions};

/// Everything the UI needs to render one repo row / detail pane.
#[derive(Debug, Clone, Default)]
pub struct RepoInfo {
    pub name: String,
    pub branch: String,
    pub detached: bool,
    pub ahead: usize,
    pub behind: usize,
    pub staged: usize,
    pub unstaged: usize,
    pub untracked: usize,
    pub conflicted: usize,
    pub remote: Option<String>,
    pub last_commit: Option<String>,
    pub last_commit_ago: Option<Duration>,
    pub has_upstream: bool,
    pub error: Option<String>,
}

impl RepoInfo {
    /// Status glyph used in the list pane.
    pub fn status_char(&self) -> &'static str {
        if self.conflicted > 0 {
            "⚡"
        } else if self.staged > 0 || self.unstaged > 0 || self.untracked > 0 {
            "●" // dirty
        } else if self.ahead > 0 {
            "▲" // needs push
        } else if self.behind > 0 {
            "▼" // needs pull
        } else {
            "○" // clean
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.staged > 0 || self.unstaged > 0 || self.untracked > 0 || self.conflicted > 0
    }

    pub fn needs_push(&self) -> bool {
        self.ahead > 0
    }
}

/// One changed file shown in the status pane.
#[derive(Debug, Clone)]
pub struct FileEntry {
    pub path: String,
    pub code: String, // e.g. "M", "??", "A"
}

/// Collect full repo metadata. Runs off the UI thread (spawned by `App`).
pub fn collect_info(path: &Path) -> (RepoInfo, Vec<FileEntry>) {
    let mut info = RepoInfo::default();
    let mut files = Vec::new();

    let repo = match Repository::open(path) {
        Ok(r) => r,
        Err(e) => {
            info.error = Some(e.message().to_string());
            info.name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            return (info, files);
        }
    };

    info.name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());

    // Branch / HEAD
    match repo.head() {
        Ok(h) => {
            if h.symbolic_target().is_some() {
                info.branch = h
                    .shorthand()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "?".into());
            } else {
                info.detached = true;
                info.branch = h
                    .shorthand()
                    .map(|s| s.to_string())
                    .filter(|s| s.len() >= 7)
                    .unwrap_or_else(|| "HEAD".into());
            }
        }
        Err(_) => info.branch = "(unborn)".into(),
    }

    // Ahead / behind vs upstream
    let head_ref = repo.head();
    let upstream = head_ref
        .as_ref()
        .ok()
        .and_then(|h| h.name())
        .and_then(|name| repo.find_branch(name, BranchType::Local).ok())
        .and_then(|b| b.upstream().ok());
    if let (Ok(head_ref), Some(upstream)) = (&head_ref, upstream.as_ref().map(|b| b.get())) {
        info.has_upstream = true;
        if let (Some(local), Some(remote)) = (head_ref.target(), upstream.target()) {
            if let Ok((a, b)) = repo.graph_ahead_behind(local, remote) {
                info.ahead = a;
                info.behind = b;
            }
        }
    }

    // Last commit time / message
    let head_oid = repo.head().ok().and_then(|h| h.target());
    if let Some(id) = head_oid {
        if let Ok(commit) = repo.find_commit(id) {
            let secs = commit.time().seconds().max(0) as u64;
            let now = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            if now > secs {
                info.last_commit_ago = Some(Duration::from_secs(now - secs));
            }
            info.last_commit = Some(
                commit
                    .summary()
                    .unwrap_or("(no summary)")
                    .chars()
                    .take(80)
                    .collect(),
            );
        }
    }

    info.remote = remote_url(&repo);

    // Working-tree status counts + short file list
    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(false)
        .renames_head_to_index(true);
    if let Ok(statuses) = repo.statuses(Some(&mut opts)) {
        for st in statuses.iter() {
            let p = st.path().unwrap_or("?").to_string();
            let flags = st.status();
            let mut code = String::new();
            if flags.contains(git2::Status::CONFLICTED) {
                info.conflicted += 1;
                code.push('C');
            } else {
                if flags.contains(git2::Status::INDEX_NEW) {
                    code.push('A');
                    info.staged += 1;
                } else if flags.contains(git2::Status::INDEX_MODIFIED) {
                    code.push('M');
                    info.staged += 1;
                } else if flags.contains(git2::Status::INDEX_DELETED) {
                    code.push('D');
                    info.staged += 1;
                } else if flags.contains(git2::Status::INDEX_RENAMED) {
                    code.push('R');
                    info.staged += 1;
                }
                if flags.contains(git2::Status::WT_NEW) {
                    if code.is_empty() {
                        code.push('?');
                    }
                    code.push('?');
                    info.untracked += 1;
                } else {
                    if flags.contains(git2::Status::WT_MODIFIED) {
                        code.push('M');
                        info.unstaged += 1;
                    }
                    if flags.contains(git2::Status::WT_DELETED) {
                        code.push('D');
                        info.unstaged += 1;
                    }
                    if flags.contains(git2::Status::WT_RENAMED) {
                        code.push('R');
                        info.unstaged += 1;
                    }
                }
            }
            if !code.is_empty() {
                files.push(FileEntry { path: p, code });
            }
        }
    }

    (info, files)
}

fn remote_url(repo: &Repository) -> Option<String> {
    let names: Vec<String> = match repo.remotes() {
        Ok(list) => (0..list.len())
            .filter_map(|i| list.get(i).map(|s| s.to_string()))
            .collect(),
        Err(_) => Vec::new(),
    };
    let pick = if names.iter().any(|n| n == "origin") {
        "origin"
    } else if let Some(first) = names.first() {
        first.as_str()
    } else {
        return None;
    };
    repo.find_remote(pick)
        .ok()
        .and_then(|r| r.url().map(|u| u.to_string()))
}

// ---------------------------------------------------------------------------
// Mutating operations (run on background threads so the UI never blocks).
// ---------------------------------------------------------------------------

pub fn stage_all(path: &Path) -> Result<usize, String> {
    let repo = Repository::open(path).map_err(|e| e.to_string())?;
    let mut idx = repo.index().map_err(|e| e.to_string())?;
    idx.add_all(["*"].iter(), IndexAddOption::DEFAULT, None)
        .map_err(|e| e.to_string())?;
    idx.write().map_err(|e| e.to_string())?;

    let mut so = StatusOptions::new();
    so.include_untracked(false);
    let count = repo
        .statuses(Some(&mut so))
        .map(|s| {
            s.iter()
                .filter(|st| {
                    let f = st.status();
                    f.intersects(
                        git2::Status::INDEX_NEW
                            | git2::Status::INDEX_MODIFIED
                            | git2::Status::INDEX_DELETED
                            | git2::Status::INDEX_RENAMED,
                    )
                })
                .count()
        })
        .unwrap_or(0);
    Ok(count)
}

pub fn unstage_all(path: &Path) -> Result<(), String> {
    let repo = Repository::open(path).map_err(|e| e.to_string())?;
    let tree = repo
        .head()
        .and_then(|h| h.peel_to_tree())
        .map_err(|_| "nothing committed yet — cannot unstage".to_string())?;
    let mut idx = repo.index().map_err(|e| e.to_string())?;
    idx.read_tree(&tree).map_err(|e| e.to_string())?;
    idx.write().map_err(|e| e.to_string())?;
    Ok(())
}

pub fn commit(path: &Path, message: &str) -> Result<String, String> {
    let msg = message.trim();
    if msg.is_empty() {
        return Err("empty commit message".into());
    }
    let repo = Repository::open(path).map_err(|e| e.to_string())?;
    let mut idx = repo.index().map_err(|e| e.to_string())?;
    let tree_id = idx.write_tree().map_err(|e| e.to_string())?;
    let tree = repo.find_tree(tree_id).map_err(|e| e.to_string())?;
    let sig = repo
        .signature()
        .or_else(|_| git2::Signature::now("repo-hub", "repo-hub@localhost"))
        .map_err(|e| e.to_string())?;
    let parent_vec: Vec<git2::Commit> = match repo.head() {
        Ok(h) => h.peel_to_commit().map(|c| vec![c]).unwrap_or_default(),
        Err(_) => Vec::new(),
    };
    let parents: &[&git2::Commit] = &parent_vec.iter().collect::<Vec<_>>();
    let id = repo
        .commit(Some("HEAD"), &sig, &sig, msg, &tree, parents)
        .map_err(|e| e.to_string())?;
    Ok(format!("committed {}", short(&id)))
}

pub fn push(path: &Path, force: bool) -> Result<String, String> {
    // Shell out to the real `git` client: it transparently uses the user's
    // ssh-agent / credential helpers, which libgit2 cannot do inside a TUI.
    let branch = current_branch(path)?;
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(path).arg("push");
    if force {
        cmd.arg("--force-with-lease");
    }
    cmd.arg("HEAD");
    run_git(cmd, &format!("pushed {branch}"))
}

pub fn fetch(path: &Path) -> Result<String, String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(path).arg("fetch").arg("--all").arg("--prune");
    run_git(cmd, "fetched")
}

pub fn pull(path: &Path) -> Result<String, String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(path).args(["pull", "--ff-only"]);
    run_git(cmd, "pulled")
}

fn current_branch(path: &Path) -> Result<String, String> {
    Repository::open(path)
        .map_err(|e| e.to_string())?
        .head()
        .map_err(|e| e.to_string())?
        .shorthand()
        .map(|s| s.to_string())
        .ok_or_else(|| "detached HEAD".to_string())
}

fn run_git(mut cmd: Command, ok_msg: &str) -> Result<String, String> {
    let out = cmd.output().map_err(|e| format!("could not run git: {e}"))?;
    if out.status.success() {
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let detail = if stdout.is_empty() { stderr } else { stdout };
        Ok(format!("{ok_msg}{}", if detail.is_empty() { String::new() } else { format!(" · {}", tail_lines(&detail, 3)) }))
    } else {
        Err(tail_lines(
            &String::from_utf8_lossy(&out.stderr),
            6,
        ))
    }
}

fn tail_lines(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().filter(|l| !l.trim().is_empty()).collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join(" | ")
}

fn short(id: &git2::Oid) -> String {
    id.to_string().chars().take(7).collect()
}

/// Size of a directory subtree (used for build-artifact accounting).
pub fn dir_size(path: &Path) -> u64 {
    let mut sum = 0;
    if let Ok(rd) = std::fs::read_dir(path) {
        for entry in rd.flatten() {
            if let Ok(meta) = entry.metadata() {
                if meta.is_dir() {
                    sum += dir_size(&entry.path());
                } else {
                    sum += meta.len();
                }
            }
        }
    }
    sum
}

/// Approximate size of build artifacts (`target`, `node_modules`, ...) under a repo.
pub fn artifact_size(path: &Path) -> u64 {
    ["target", "node_modules", "dist", "build", ".venv"]
        .iter()
        .map(|d| {
            let p = path.join(d);
            if p.is_dir() {
                dir_size(&p)
            } else {
                0
            }
        })
        .sum()
}

pub fn human_bytes(b: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{b} {}", UNITS[0])
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

pub fn human_age(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else if s < 86400 {
        format!("{}h", s / 3600)
    } else if s < 86400 * 30 {
        format!("{}d", s / 86400)
    } else if s < 86400 * 365 {
        format!("{}mo", s / (86400 * 30))
    } else {
        format!("{}y", s / (86400 * 365))
    }
}

/// Spawn an interactive shell / GUI tool rooted at `path`.
pub fn spawn_in_dir(cmd: &str, args: &[&str], path: &Path) -> Result<(), String> {
    let exe = which(cmd).ok_or_else(|| format!("`{cmd}` not found in PATH"))?;
    let status = Command::new(exe)
        .current_dir(path)
        .args(args)
        .status()
        .map_err(|e| e.to_string())?;
    let _ = status;
    Ok(())
}

pub fn which(cmd: &str) -> Option<PathBuf> {
    #[cfg(unix)]
    {
        if let Ok(out) = Command::new("which").arg(cmd).output() {
            if out.status.success() {
                let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !p.is_empty() {
                    return Some(PathBuf::from(p));
                }
            }
        }
        None
    }
    #[cfg(windows)]
    {
        if Command::new(cmd).arg("--version").output().is_ok() {
            return Some(PathBuf::from(cmd));
        }
        None
    }
}

/// List all local branches (shown in the detail pane).
pub fn branches(path: &Path) -> Vec<String> {
    Repository::open(path)
        .map(|repo| {
            repo.branches(Some(BranchType::Local))
                .map(|iter| {
                    iter.filter_map(|res| res.ok())
                        .filter_map(|(br, _)| br.name().ok().flatten().map(|n| n.to_string()))
                        .collect()
                })
                .unwrap_or_default()
        })
        .unwrap_or_default()
}

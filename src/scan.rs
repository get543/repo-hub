//! Parallel, non-blocking filesystem scanner that streams discovered repos.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;

/// Directories that never contain a repo worth indexing (or would explode the walk).
pub const PRUNE_DIRS: &[&str] = &[
    "node_modules",
    "target",
    ".venv",
    "venv",
    ".cargo",
    ".hg",
    ".svn",
    "__pycache__",
    ".next",
    ".nuxt",
    ".cache",
    ".gradle",
    ".m2",
    "AppData",
    "$Recycle.Bin",
    "$RECYCLE.BIN",
    "System Volume Information",
    "WindowsApps",
    "proc",
    "sys",
    "dev",
    "run",
    "snap",
    "lost+found",
    "site-packages",
    "Library", // macOS system library is huge; user repos live elsewhere
];

/// A scan result sent over the channel as soon as a `.git` folder is found.
#[derive(Debug, Clone)]
pub struct ScanEvent {
    pub path: PathBuf,
    /// drive index the repo belongs to (matches the order of the roots given)
    pub drive: usize,
}

pub struct Scanner {
    rx: Receiver<ScanEvent>,
    running: Arc<AtomicBool>,
    scanned: Arc<AtomicUsize>,
    done: Arc<AtomicBool>,
}

impl Scanner {
    /// Spawn a background parallel scan over `roots`.
    pub fn spawn(roots: Vec<PathBuf>) -> Self {
        let (tx, rx) = channel::<ScanEvent>();
        let running = Arc::new(AtomicBool::new(true));
        let scanned = Arc::new(AtomicUsize::new(0));
        let done = Arc::new(AtomicBool::new(false));

        let run_flag = running.clone();
        let scan_count = scanned.clone();
        let done_flag = done.clone();

        std::thread::Builder::new()
            .name("repo-scan".into())
            .spawn(move || {
                // `ignore` walks each root with one worker per CPU core and lets us
                // prune unwanted directories cheaply via `filter_entry`.
                for (idx, root) in roots.iter().enumerate() {
                    if !run_flag.load(Ordering::Relaxed) {
                        break;
                    }
                    if !root.exists() {
                        continue;
                    }
                    let tx = tx.clone();
                    let cnt = scan_count.clone();
                    let flag = run_flag.clone();
                    let walker = ignore::WalkBuilder::new(root)
                        .hidden(false)
                        .git_ignore(false)
                        .git_global(false)
                        .git_exclude(false)
                        .parents(false)
                        .follow_links(false)
                        .max_depth(Some(32))
                        .filter_entry(|entry| {
                            let name = entry.file_name().to_string_lossy();
                            !PRUNE_DIRS.contains(&name.as_ref())
                        })
                        .build_parallel();

                    walker.run(|| {
                        let tx = tx.clone();
                        let cnt = cnt.clone();
                        let flag = flag.clone();
                        Box::new(move |result| {
                            if !flag.load(Ordering::Relaxed) {
                                return ignore::WalkState::Quit;
                            }
                            if let Ok(entry) = result {
                                cnt.fetch_add(1, Ordering::Relaxed);
                                let is_git_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false)
                                    && entry.file_name() == ".git";
                                let is_git_file = entry.file_type().map(|t| t.is_file()).unwrap_or(false)
                                    && entry.file_name() == ".git"; // submodules / worktrees
                                if is_git_dir {
                                    if let Some(parent) = entry.path().parent() {
                                        let _ = tx.send(ScanEvent {
                                            path: parent.to_path_buf(),
                                            drive: idx,
                                        });
                                    }
                                    // Never descend into .git internals.
                                    return ignore::WalkState::Skip;
                                }
                                if is_git_file {
                                    if let Some(parent) = entry.path().parent() {
                                        let _ = tx.send(ScanEvent {
                                            path: parent.to_path_buf(),
                                            drive: idx,
                                        });
                                    }
                                }
                            }
                            ignore::WalkState::Continue
                        })
                    });
                }
                drop(tx); // close the channel so the UI can detect completion
                done_flag.store(true, Ordering::Relaxed);
                run_flag.store(false, Ordering::Relaxed);
            })
            .expect("failed to spawn scanner thread");

        Scanner {
            rx,
            running,
            scanned,
            done,
        }
    }

    /// Non-blocking drain of everything currently buffered from the scanner.
    pub fn drain(&mut self) -> Vec<ScanEvent> {
        let mut out = Vec::new();
        while let Ok(ev) = self.rx.try_recv() {
            out.push(ev);
        }
        out
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    pub fn dirs_scanned(&self) -> usize {
        self.scanned.load(Ordering::Relaxed)
    }

    pub fn is_done(&self) -> bool {
        self.done.load(Ordering::Relaxed)
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::Relaxed);
    }
}

impl Drop for Scanner {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Enumerate every "drive" we can see on this machine.
/// * Windows: all logical drive letters (C:\, D:\, ...).
/// * Unix/macOS: real filesystem mount points parsed from /proc/mounts or `mount -p`,
///   plus `$HOME` fallback so the tool always has something useful to walk.
pub fn detect_drives() -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        let mut drives = Vec::new();
        for b in b'A'..=b'Z' {
            let letter = (b as char).to_string();
            let p = PathBuf::from(format!("{letter}:\\"));
            if p.exists() {
                drives.push(p);
            }
        }
        if drives.is_empty() {
            drives.push(std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")));
        }
        drives
    }

    #[cfg(not(windows))]
    {
        let mut drives: Vec<PathBuf> = Vec::new();

        if let Ok(content) = std::fs::read_to_string("/proc/mounts") {
            for line in content.lines() {
                let mut it = line.split_whitespace();
                let (Some(_dev), Some(mnt), Some(fstype)) = (it.next(), it.next(), it.next()) else {
                    continue;
                };
                if !is_real_fs(fstype) {
                    continue;
                }
                let path = PathBuf::from(mnt);
                if !drives.iter().any(|d| d == &path) {
                    drives.push(path);
                }
            }
        }

        // macOS / BSD fallback: parse `mount -p` output.
        if drives.is_empty() {
            if let Ok(out) = std::process::Command::new("mount").arg("-p").output() {
                let text = String::from_utf8_lossy(&out.stdout);
                for line in text.lines() {
                    let cols: Vec<&str> = line.split_whitespace().collect();
                    if cols.len() >= 6 {
                        let fstype = cols[5].trim_start_matches('(').trim_end_matches(')');
                        if is_real_fs(fstype) {
                            let path = PathBuf::from(cols[3]);
                            if !drives.iter().any(|d| d == &path) {
                                drives.push(path);
                            }
                        }
                    }
                }
            }
        }

        // Drop pseudo/volatile mounts, keep the rest.
        let filtered: Vec<PathBuf> = drives
            .into_iter()
            .filter(|p| {
                let s = p.to_string_lossy();
                !(s.starts_with("/proc")
                    || s.starts_with("/sys")
                    || s.starts_with("/dev")
                    || s.starts_with("/run")
                    || s == "/boot/efi")
            })
            .collect();

        let mut final_list = filtered;
        if final_list.is_empty() {
            final_list.push(PathBuf::from("/"));
        }
        if let Some(home) = dirs::home_dir() {
            if !final_list.iter().any(|d| home.starts_with(d)) {
                final_list.push(home);
            }
        }
        final_list
    }
}

#[cfg(not(windows))]
fn is_real_fs(fstype: &str) -> bool {
    matches!(
        fstype,
        "ext2" | "ext3" | "ext4" | "xfs" | "btrfs" | "zfs" | "f2fs" | "jfs" | "reiserfs"
            | "apfs" | "hfs" | "hfsplus" | "exfat" | "msdos" | "vfat" | "ntfs" | "fuseblk"
            | "nfs" | "nfs4" | "cifs" | "smb2" | "sshfs" | "overlay"
    )
}

/// Human label for a drive root.
pub fn drive_label(p: &Path) -> String {
    let s = p.to_string_lossy();
    if s.ends_with(":\\") || s.ends_with(":/") {
        s.trim_end_matches(['\\', '/']).to_string()
    } else {
        s.into_owned()
    }
}

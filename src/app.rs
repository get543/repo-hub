//! Application state, event plumbing and all user-facing actions.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::Instant;

use crate::cache::CacheFile;
use crate::git::{self, FileEntry, RepoInfo};
use crate::scan::{drive_label, Scanner};

/// Async messages produced by background git threads.
pub enum AppEvent {
    /// Full info for one repo path (refresh completed).
    Info {
        path: PathBuf,
        info: Box<RepoInfo>,
        files: Vec<FileEntry>,
    },
    /// Result of a user-triggered operation ("push", "commit", ...).
    OpResult { op: String, msg: String },
    /// One repo finished fetching during a global "fetch all".
    FetchProgress { done: usize, total: usize },
    /// A selected repository folder finished deleting.
    DeleteResult {
        path: PathBuf,
        result: Result<(), String>,
    },
}

#[derive(PartialEq, Eq)]
pub enum AppMode {
    Normal,
    Search,
    Prompt,
    EditorPrompt,
    ConfirmDelete,
    Deleting,
    Message,
}

#[derive(Clone)]
pub struct RepoEntry {
    pub path: PathBuf,
    pub drive: usize,
    pub info: Option<RepoInfo>,
    pub files: Vec<FileEntry>,
    pub busy: bool, // a refresh/op is in flight for this repo
}

pub struct DriveTab {
    pub label: String,
    pub count: usize,
}

#[derive(Clone)]
pub struct DirectoryEntry {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
}

pub struct DirectoryLevel {
    pub path: PathBuf,
    pub entries: Vec<DirectoryEntry>,
    pub selected: usize,
    pub offset: usize,
}

pub struct DirectoryBrowser {
    pub levels: Vec<DirectoryLevel>,
}

pub struct App {
    pub mode: AppMode,
    pub should_quit: bool,
    pub dirty: bool,

    pub drives: Vec<DriveTab>,
    pub active_drive: Option<usize>, // None = All
    pub repos: Vec<RepoEntry>,
    pub filtered: Vec<usize>, // indices into `repos`
    pub selected: usize,      // index into `filtered`
    pub list_offset: usize,   // scroll offset
    pub browser: Option<DirectoryBrowser>,
    pub detail_width: Option<u16>,
    pub dragging_divider: bool,

    pub search_query: String,
    pub dirty_only: bool,

    pub scanner: Option<Scanner>,
    pub scan_started: Option<Instant>,
    pub scan_done: bool,

    pub tx: Sender<AppEvent>,
    pub rx: Receiver<AppEvent>,

    pub status_line: String,
    pub message: String,
    pub help: bool,

    pub prompt_title: String,
    pub prompt_value: String,
    pub pending_editor: Option<PathBuf>,
    pub pending_delete: Option<PathBuf>,

    pub fetch_progress: Option<(usize, usize)>,
    terminal_needs_clear: bool,
    use_cache: bool,
    roots: Vec<PathBuf>,
}

impl App {
    pub fn new(use_cache: bool, roots_override: Option<Vec<PathBuf>>) -> Self {
        let roots = roots_override.unwrap_or_else(crate::scan::detect_drives);
        let drives: Vec<DriveTab> = roots
            .iter()
            .map(|r| DriveTab {
                label: drive_label(r),
                count: 0,
            })
            .collect();

        let (tx, rx) = channel::<AppEvent>();
        let mut app = App {
            mode: AppMode::Normal,
            should_quit: false,
            dirty: true,
            drives,
            active_drive: None,
            repos: Vec::new(),
            filtered: Vec::new(),
            selected: 0,
            list_offset: 0,
            browser: None,
            detail_width: None,
            dragging_divider: false,
            search_query: String::new(),
            dirty_only: false,
            scanner: None,
            scan_started: None,
            scan_done: false,
            tx,
            rx,
            status_line: "starting…".into(),
            message: String::new(),
            help: false,
            prompt_title: String::new(),
            prompt_value: String::new(),
            pending_editor: None,
            pending_delete: None,
            fetch_progress: None,
            terminal_needs_clear: false,
            use_cache,
            roots,
        };

        // Instant startup from cache...
        if app.use_cache {
            if let Some(cache) = CacheFile::load() {
                for cr in &cache.repos {
                    let p = PathBuf::from(&cr.path);
                    if p.exists() {
                        app.push_repo(p, cr.drive);
                    }
                }
                if !app.repos.is_empty() {
                    app.status_line = format!(
                        "loaded {} repos from cache — refreshing in background",
                        app.repos.len()
                    );
                }
            }
        }

        // ...then always kick off a real parallel scan to catch new/removed repos.
        app.start_scan();
        app.apply_filter();
        app.refresh_selected_now();
        app
    }

    fn push_repo(&mut self, path: PathBuf, drive: usize) -> usize {
        if let Some(i) = self.repos.iter().position(|r| r.path == path) {
            return i;
        }
        self.repos.push(RepoEntry {
            path,
            drive,
            info: None,
            files: Vec::new(),
            busy: false,
        });
        if drive < self.drives.len() {
            self.drives[drive].count += 1;
        }
        self.repos.len() - 1
    }

    pub fn start_scan(&mut self) {
        self.scanner = Some(Scanner::spawn(self.roots.clone()));
        self.scan_started = Some(Instant::now());
        self.scan_done = false;
        self.dirty = true;
    }

    pub fn rescan(&mut self) {
        // Stop any running scan, wipe results, start fresh.
        if let Some(s) = self.scanner.take() {
            s.stop();
        }
        self.repos.clear();
        for d in &mut self.drives {
            d.count = 0;
        }
        self.filtered.clear();
        self.selected = 0;
        self.list_offset = 0;
        CacheFile::clear();
        self.start_scan();
        self.status_line = "rescanning all drives…".into();
        self.dirty = true;
    }

    /// Called every loop tick: move everything off the hot path.
    pub fn poll_events(&mut self) {
        let mut got_something = false;

        // 1. New repos from the filesystem walker.
        let events: Vec<_> = match &mut self.scanner {
            Some(scanner) => scanner.drain(),
            None => Vec::new(),
        };
        let scan_finished = self
            .scanner
            .as_ref()
            .map(|s| s.is_done())
            .unwrap_or(true);
        let dirs_walked = self
            .scanner
            .as_ref()
            .map(|s| s.dirs_scanned())
            .unwrap_or(0);
        if !events.is_empty() {
            for ev in events {
                if ev.path.exists() {
                    let idx = self.push_repo(ev.path, ev.drive);
                    self.spawn_info(idx);
                }
            }
            self.apply_filter();
            got_something = true;
        }
        {
            let scanner_done = scan_finished;
            if scanner_done && !self.scan_done {
                self.scan_done = true;
                got_something = true;
                let secs = self
                    .scan_started
                    .map(|t| t.elapsed().as_secs_f32())
                    .unwrap_or(0.0);
                self.status_line = format!(
                    "scan complete: {} repos in {secs:.1}s ({dirs_walked} dirs walked)",
                    self.repos.len()
                );
                self.save_cache();
                self.apply_filter();
            }
        }

        // 2. Git info / operation results.
        while let Ok(ev) = self.rx.try_recv() {
            match ev {
                AppEvent::Info { path, info, files } => {
                    if let Some(r) = self.repos.iter_mut().find(|r| r.path == path) {
                        r.info = Some(*info);
                        r.files = files;
                        r.busy = false;
                    }
                    got_something = true;
                }
                AppEvent::OpResult { op, msg } => {
                    self.message = format!("{op}: {msg}");
                    self.mode = AppMode::Message;
                    got_something = true;
                }
                AppEvent::FetchProgress { done, total } => {
                    self.fetch_progress = Some((done, total));
                    self.status_line = format!("fetching all repos… {done}/{total}");
                    got_something = true;
                }
                AppEvent::DeleteResult { path, result } => {
                    match result {
                        Ok(()) => {
                            if let Some(idx) = self.repos.iter().position(|repo| repo.path == path)
                            {
                                let drive = self.repos[idx].drive;
                                self.repos.remove(idx);
                                if let Some(tab) = self.drives.get_mut(drive) {
                                    tab.count = tab.count.saturating_sub(1);
                                }
                            }
                            self.apply_filter();
                            self.list_offset = self.list_offset.min(self.selected);
                            self.save_cache();
                            self.refresh_selected_now();
                            self.message = format!("deleted repository folder {}", path.display());
                        }
                        Err(error) => {
                            if let Some(repo) = self.repos.iter_mut().find(|repo| repo.path == path)
                            {
                                repo.busy = false;
                            }
                            self.message = format!("could not delete {}: {error}", path.display());
                        }
                    }
                    self.mode = AppMode::Message;
                    got_something = true;
                }
            }
        }

        if got_something {
            self.dirty = true;
        }
    }

    fn spawn_info(&mut self, idx: usize) {
        if idx >= self.repos.len() || self.repos[idx].busy {
            return;
        }
        self.repos[idx].busy = true;
        let path = self.repos[idx].path.clone();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let (info, files) = git::collect_info(&path);
            let _ = tx.send(AppEvent::Info {
                path,
                info: Box::new(info),
                files,
            });
        });
    }

    fn selected_path(&self) -> Option<PathBuf> {
        self.filtered
            .get(self.selected)
            .and_then(|i| self.repos.get(*i))
            .map(|r| r.path.clone())
    }

    pub fn refresh_selected_now(&mut self) {
        if let Some(i) = self.filtered.get(self.selected).copied() {
            self.spawn_info(i);
        }
    }

    // ------------------------------------------------------------------
    // Navigation
    // ------------------------------------------------------------------

    pub fn move_list(&mut self, delta: i32) {
        if let Some(level) = self
            .browser
            .as_mut()
            .and_then(|browser| browser.levels.last_mut())
        {
            if level.entries.is_empty() {
                return;
            }
            let selected = (level.selected as i64 + i64::from(delta))
                .clamp(0, level.entries.len() as i64 - 1) as usize;
            level.selected = selected;
            self.dirty = true;
            return;
        }
        if self.filtered.is_empty() {
            return;
        }
        let len = self.filtered.len() as i32;
        let mut ns = self.selected as i32 + delta;
        if ns < 0 {
            ns = 0;
        }
        if ns >= len {
            ns = len - 1;
        }
        self.selected = ns as usize;
        self.clamp_scroll();
        self.refresh_selected_now();
        self.dirty = true;
    }

    pub fn select(&mut self, i: usize) {
        if let Some(level) = self
            .browser
            .as_mut()
            .and_then(|browser| browser.levels.last_mut())
        {
            if level.entries.is_empty() {
                return;
            }
            level.selected = i.min(level.entries.len() - 1);
            self.dirty = true;
            return;
        }
        if self.filtered.is_empty() {
            return;
        }
        self.selected = i.min(self.filtered.len() - 1);
        self.clamp_scroll();
        self.refresh_selected_now();
        self.dirty = true;
    }

    fn clamp_scroll(&mut self) {
        // visible rows approximated; UI passes real height via set_viewport
        if self.selected < self.list_offset {
            self.list_offset = self.selected;
        }
    }

    /// Mouse click on a visible row opens repositories and directories.
    pub fn click_list_row(&mut self, row: usize) {
        if self.browser.is_some() {
            let target = self
                .browser
                .as_ref()
                .and_then(|browser| browser.levels.last())
                .map(|level| level.offset + row);
            if let Some(target) = target {
                self.select(target);
                self.enter_browser_selection();
            }
        } else {
            let target = self.list_offset + row;
            if target < self.filtered.len() {
                self.select(target);
                self.open_browser_selected();
            }
        }
    }

    pub fn open_browser_selected(&mut self) {
        let Some(path) = self.selected_path() else {
            return;
        };
        match read_directory(&path) {
            Ok(entries) => {
                self.browser = Some(DirectoryBrowser {
                    levels: vec![DirectoryLevel {
                        path,
                        entries,
                        selected: 0,
                        offset: 0,
                    }],
                });
                self.dirty = true;
            }
            Err(error) => {
                self.message = error;
                self.mode = AppMode::Message;
                self.dirty = true;
            }
        }
    }

    pub fn enter_browser_selection(&mut self) {
        let Some(entry) = self
            .browser
            .as_ref()
            .and_then(|browser| browser.levels.last())
            .and_then(|level| level.entries.get(level.selected))
            .cloned()
        else {
            return;
        };
        if !entry.is_dir {
            self.open_with_default_app(&entry.path);
            return;
        }
        match read_directory(&entry.path) {
            Ok(entries) => {
                if let Some(browser) = &mut self.browser {
                    browser.levels.push(DirectoryLevel {
                        path: entry.path,
                        entries,
                        selected: 0,
                        offset: 0,
                    });
                }
                self.dirty = true;
            }
            Err(error) => {
                self.message = error;
                self.mode = AppMode::Message;
                self.dirty = true;
            }
        }
    }

    pub fn go_back(&mut self) {
        if let Some(browser) = &mut self.browser {
            if browser.levels.len() > 1 {
                browser.levels.pop();
            } else {
                self.browser = None;
            }
            self.dirty = true;
        }
    }

    pub fn request_editor_selected(&mut self) {
        let Some(path) = self.selected_path() else {
            return;
        };
        self.pending_editor = Some(path);
        self.mode = AppMode::EditorPrompt;
        self.dirty = true;
    }

    fn open_with_default_app(&mut self, path: &Path) {
        match launch_default_app(path) {
            Ok(()) => {
                self.status_line = format!("opened {} with the system default app", path.display());
                self.refresh_terminal_after_external_app();
            }
            Err(error) => {
                self.message = error;
                self.mode = AppMode::Message;
            }
        }
        self.dirty = true;
    }

    pub fn launch_pending_editor(&mut self, use_os_default: bool) {
        let Some(path) = self.pending_editor.take() else {
            self.mode = AppMode::Normal;
            self.dirty = true;
            return;
        };

        if use_os_default {
            self.open_with_default_app(&path);
        } else {
            match launch_vscode(&path) {
                Ok(()) => {
                    self.status_line = format!("opened {} in editor", path.display());
                    self.mode = AppMode::Normal;
                    self.refresh_terminal_after_external_app();
                }
                Err(error) => {
                    self.message = error;
                    self.mode = AppMode::Message;
                    self.dirty = true;
                }
            }
        }
    }

    pub fn set_drive_tab(&mut self, tab: usize) {
        self.active_drive = if tab == 0 { None } else { Some(tab - 1) };
        if self.drives.is_empty() && self.active_drive.is_some() {
            self.active_drive = None;
        }
        self.apply_filter();
        self.selected = 0;
        self.list_offset = 0;
        self.refresh_selected_now();
        self.dirty = true;
    }

    pub fn cycle_drive(&mut self, dir: i32) {
        let n = self.drives.len() as i32;
        if n == 0 {
            return;
        }
        self.active_drive = match self.active_drive {
            None => Some(0),
            Some(i) => {
                let ni = (i as i32 + dir + n + 1) % (n + 1);
                if ni == n {
                    None
                } else {
                    Some(ni as usize)
                }
            }
        };
        self.apply_filter();
        self.selected = 0;
        self.list_offset = 0;
        self.refresh_selected_now();
        self.dirty = true;
    }

    pub fn toggle_dirty_filter(&mut self) {
        self.dirty_only = !self.dirty_only;
        self.apply_filter();
        self.selected = 0;
        self.dirty = true;
    }

    pub fn apply_filter(&mut self) {
        let q = self.search_query.to_lowercase();
        self.filtered = self
            .repos
            .iter()
            .enumerate()
            .filter(|(_, r)| {
                if let Some(d) = self.active_drive {
                    if r.drive != d {
                        return false;
                    }
                }
                if self.dirty_only {
                    match &r.info {
                        Some(i) if i.is_dirty() || i.needs_push() => {}
                        _ => return false,
                    }
                }
                if !q.is_empty() {
                    let name = r
                        .path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_lowercase())
                        .unwrap_or_default();
                    let full = r.path.to_string_lossy().to_lowercase();
                    if !name.contains(&q) && !full.contains(&q) {
                        return false;
                    }
                }
                true
            })
            .map(|(i, _)| i)
            .collect();
        if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len().saturating_sub(1);
        }
        self.dirty = true;
    }

    // ------------------------------------------------------------------
    // Git operations (all async through spawned threads)
    // ------------------------------------------------------------------

    pub fn request_delete_selected(&mut self) {
        let Some(index) = self.filtered.get(self.selected).copied() else {
            self.message = "no repository selected".into();
            self.mode = AppMode::Message;
            self.dirty = true;
            return;
        };
        if self.repos[index].busy {
            self.message = "repository is busy; wait for its current operation to finish".into();
            self.mode = AppMode::Message;
            self.dirty = true;
            return;
        }
        self.pending_delete = Some(self.repos[index].path.clone());
        self.mode = AppMode::ConfirmDelete;
        self.dirty = true;
    }

    pub fn cancel_delete(&mut self) {
        self.pending_delete = None;
        self.mode = AppMode::Normal;
        self.dirty = true;
    }

    pub fn confirm_delete(&mut self) {
        let Some(path) = self.pending_delete.take() else {
            self.mode = AppMode::Normal;
            self.dirty = true;
            return;
        };
        let Some(index) = self.repos.iter().position(|repo| repo.path == path) else {
            self.message = "repository is no longer in the list".into();
            self.mode = AppMode::Message;
            self.dirty = true;
            return;
        };
        if self.repos[index].busy {
            self.message = "repository is busy; wait for its current operation to finish".into();
            self.mode = AppMode::Message;
            self.dirty = true;
            return;
        }

        self.repos[index].busy = true;
        let tx = self.tx.clone();
        let delete_path = path.clone();
        let status_path = delete_path.clone();
        std::thread::spawn(move || {
            let result = std::fs::remove_dir_all(&delete_path).map_err(|error| error.to_string());
            let _ = tx.send(AppEvent::DeleteResult { path, result });
        });
        self.status_line = format!("deleting {}…", status_path.display());
        self.mode = AppMode::Deleting;
        self.dirty = true;
    }

    fn run_op<F>(&mut self, name: &str, f: F)
    where
        F: FnOnce(PathBuf) -> Result<String, String> + Send + 'static,
    {
        let Some(path) = self.selected_path() else {
            self.message = format!("{name}: no repository selected");
            self.mode = AppMode::Message;
            self.dirty = true;
            return;
        };
        if let Some(i) = self.filtered.get(self.selected).copied() {
            self.repos[i].busy = true;
        }
        let tx = self.tx.clone();
        let op = name.to_string();
        std::thread::spawn(move || {
            let msg = match f(path.clone()) {
                Ok(m) => m,
                Err(e) => e,
            };
            // then refresh info so badges update
            let (info, files) = git::collect_info(&path);
            let _ = tx.send(AppEvent::Info {
                path: path.clone(),
                info: Box::new(info),
                files,
            });
            let _ = tx.send(AppEvent::OpResult { op, msg });
        });
        self.status_line = format!("{name} running…");
        self.dirty = true;
    }

    pub fn stage_all(&mut self) {
        self.run_op("stage-all", |p| {
            let n = git::stage_all(&p)?;
            Ok(format!("staged {n} file(s)"))
        });
    }

    pub fn unstage_all(&mut self) {
        self.run_op("unstage-all", |p| {
            git::unstage_all(&p)?;
            Ok("index reset to HEAD (files kept on disk)".into())
        });
    }

    pub fn commit(&mut self, message: String) {
        if message.trim().is_empty() {
            return;
        }
        self.run_op("commit", move |p| git::commit(&p, &message));
    }

    pub fn push(&mut self, force: bool) {
        self.run_op(if force { "push --force" } else { "push" }, move |p| {
            git::push(&p, force)
        });
    }

    pub fn pull(&mut self) {
        self.run_op("pull", |p| git::pull(&p));
    }

    pub fn fetch_selected(&mut self) {
        self.run_op("fetch", |p| git::fetch(&p));
    }

    pub fn clean_build(&mut self) {
        self.run_op("clean", |p| {
            let mut freed = 0u64;
            for dir in ["target", "node_modules", "dist", "build"] {
                let d = p.join(dir);
                if d.is_dir() {
                    freed += git::artifact_size(&d);
                    let _ = std::fs::remove_dir_all(&d);
                }
            }
            if freed == 0 {
                Ok("no target/node_modules/dist/build folders found".into())
            } else {
                Ok(format!("deleted build artifacts, freed {}", git::human_bytes(freed)))
            }
        });
    }

    pub fn fetch_all(&mut self) {
        let paths: Vec<PathBuf> = self
            .repos
            .iter_mut()
            .filter_map(|repo| {
                if repo.busy {
                    None
                } else {
                    repo.busy = true;
                    Some(repo.path.clone())
                }
            })
            .collect();
        let total = paths.len();
        if total == 0 {
            self.message = if self.repos.is_empty() {
                "no repositories to fetch".into()
            } else {
                "all repositories are busy".into()
            };
            self.mode = AppMode::Message;
            self.dirty = true;
            return;
        }
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let mut done = 0;
            for p in paths {
                let _res = git::fetch(&p);
                done += 1;
                let _ = tx.send(AppEvent::FetchProgress { done, total });
                // refresh so ahead/behind badges are current
                let (info, files) = git::collect_info(&p);
                let _ = tx.send(AppEvent::Info {
                    path: p,
                    info: Box::new(info),
                    files,
                });
            }
            let _ = tx.send(AppEvent::OpResult {
                op: "fetch-all".into(),
                msg: format!("fetched {total} repositories"),
            });
        });
        self.fetch_progress = Some((0, total));
        self.dirty = true;
    }

    // ------------------------------------------------------------------
    // External tools (need exclusive terminal access -> suspend TUI)
    // ------------------------------------------------------------------

    fn with_terminal_restored<T>(&mut self, f: impl FnOnce() -> T) -> T {
        use crossterm::{
            execute,
            terminal::{disable_raw_mode, enable_raw_mode, LeaveAlternateScreen},
        };
        let _ = disable_raw_mode();
        let _ = execute!(std::io::stdout(), LeaveAlternateScreen);
        let out = f();
        let _ = enable_raw_mode();
        let _ = crossterm::execute!(std::io::stdout(), crossterm::terminal::EnterAlternateScreen);
        self.dirty = true;
        self.terminal_needs_clear = true;
        out
    }

    pub fn take_terminal_needs_clear(&mut self) -> bool {
        std::mem::take(&mut self.terminal_needs_clear)
    }

    fn refresh_terminal_after_external_app(&mut self) {
        self.terminal_needs_clear = true;
        self.dirty = true;
    }

    pub fn open_terminal(&mut self) {
        let Some(path) = self.selected_path() else { return };
        let shell = if cfg!(windows) {
            if git::which("pwsh").is_some() {
                "pwsh".into()
            } else {
                "powershell".into()
            }
        } else {
            std::env::var("SHELL").unwrap_or_else(|_| "bash".into())
        };
        self.with_terminal_restored(|| {
            eprintln!("spawning {shell} in {} — exit it to return", path.display());
            let _ = git::spawn_in_dir(&shell, &[], &path);
        });
    }

    pub fn open_lazygit(&mut self) {
        let Some(path) = self.selected_path() else { return };
        if git::which("lazygit").is_none() {
            self.message = "lazygit is not installed (https://github.com/jesseduffield/lazygit)".into();
            self.mode = AppMode::Message;
            self.dirty = true;
            return;
        }
        self.with_terminal_restored(|| {
            let _ = git::spawn_in_dir("lazygit", &[], &path);
        });
    }

    pub fn open_vscode(&mut self) {
        let Some(path) = self.selected_path() else { return };
        match launch_vscode(&path) {
            Ok(()) => {
                self.status_line = format!("opened {} in editor", path.display());
                self.refresh_terminal_after_external_app();
            }
            Err(error) => {
                self.message = error;
                self.mode = AppMode::Message;
            }
        }
        self.dirty = true;
    }

    // ------------------------------------------------------------------
    // Reporting / cache
    // ------------------------------------------------------------------

    pub fn export_report(&mut self) {
        let mut lines = Vec::new();
        lines.push(format!(
            "repo-hub report — {} repositories (dirty filter: {})",
            self.repos.len(),
            self.dirty_only
        ));
        for r in &self.repos {
            let info = r.info.as_ref();
            let branch = info.map(|i| i.branch.as_str()).unwrap_or("?");
            let ahead = info.map(|i| i.ahead).unwrap_or(0);
            let behind = info.map(|i| i.behind).unwrap_or(0);
            let dirty = info.map(|i| i.is_dirty()).unwrap_or(false);
            let last = info
                .and_then(|i| i.last_commit_ago)
                .map(git::human_age)
                .unwrap_or_else(|| "-".into());
            let remote = info.and_then(|i| i.remote.clone()).unwrap_or_default();
            lines.push(format!(
                "{}\tbranch={branch}\tahead={ahead}\tbehind={behind}\tdirty={dirty}\tlast={last}\tremote={remote}",
                r.path.display()
            ));
        }
        let path = std::env::temp_dir().join("repo-hub-report.tsv");
        match std::fs::write(&path, lines.join("\n")) {
            Ok(_) => {
                self.message = format!("report written to {}", path.display());
            }
            Err(e) => self.message = format!("could not write report: {e}"),
        }
        self.mode = AppMode::Message;
        self.dirty = true;
    }

    pub fn save_cache(&mut self) {
        let cache = CacheFile {
            drives: self
                .roots
                .iter()
                .map(|d| d.to_string_lossy().into_owned())
                .collect(),
            repos: self
                .repos
                .iter()
                .map(|r| crate::cache::CachedRepo {
                    path: r.path.to_string_lossy().into_owned(),
                    drive: r.drive,
                })
                .collect(),
        };
        cache.save();
    }

    pub fn show_help(&mut self) {
        self.help = !self.help;
        self.dirty = true;
    }

    pub fn adjust_detail_width(&mut self, delta: i16, default_width: u16, area_width: u16) {
        let current = self.detail_width.unwrap_or(default_width);
        self.detail_width = Some(clamp_detail_width(
            current.saturating_add_signed(delta),
            area_width,
        ));
        self.dirty = true;
    }

    pub fn set_detail_width(&mut self, width: u16, area_width: u16) {
        self.detail_width = Some(clamp_detail_width(width, area_width));
        self.dirty = true;
    }

    pub fn selected_entry(&self) -> Option<&RepoEntry> {
        self.filtered
            .get(self.selected)
            .and_then(|i| self.repos.get(*i))
    }
}

fn clamp_detail_width(width: u16, area_width: u16) -> u16 {
    let maximum = area_width.saturating_sub(24).max(1);
    width.clamp(20.min(maximum), maximum)
}

fn read_directory(path: &Path) -> Result<Vec<DirectoryEntry>, String> {
    let entries = std::fs::read_dir(path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    let mut result = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("could not read {}: {error}", path.display()))?;
        if entry.file_name() == ".git" {
            continue;
        }
        let file_type = entry
            .file_type()
            .map_err(|error| format!("could not inspect {}: {error}", entry.path().display()))?;
        result.push(DirectoryEntry {
            path: entry.path(),
            name: entry.file_name().to_string_lossy().into_owned(),
            is_dir: file_type.is_dir(),
        });
    }
    result.sort_by(|left, right| {
        right
            .is_dir
            .cmp(&left.is_dir)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(result)
}

fn launch_vscode(path: &Path) -> Result<(), String> {
    let launcher = ["code", "codium"]
        .into_iter()
        .find_map(git::which)
        .ok_or_else(|| "VS Code (`code`) or VSCodium (`codium`) not found in PATH".to_string())?;
    #[cfg(windows)]
    let executable = windows_editor_executable(&launcher)?;
    #[cfg(not(windows))]
    let executable = launcher;

    Command::new(executable)
        .arg(path)
        .current_dir(path)
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("could not launch VS Code: {error}"))
}

#[cfg(windows)]
fn windows_editor_executable(launcher: &Path) -> Result<PathBuf, String> {
    let extension = launcher
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default();
    if !extension.eq_ignore_ascii_case("cmd") && !extension.eq_ignore_ascii_case("bat") {
        return Ok(launcher.to_path_buf());
    }

    let app_dir = launcher
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| format!("could not locate the editor installation for {}", launcher.display()))?;
    let launcher_name = launcher
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let app_names = if launcher_name.eq_ignore_ascii_case("codium") {
        ["VSCodium.exe", "Codium.exe"]
    } else {
        ["Code.exe", "Code - Insiders.exe"]
    };

    app_names
        .iter()
        .map(|name| app_dir.join(name))
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| format!("could not find the editor executable beside {}", launcher.display()))
}

fn launch_default_app(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("explorer.exe");
        command.arg(path);
        command
    };
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = Command::new("open");
        command.arg(path);
        command
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = {
        let mut command = Command::new("xdg-open");
        command.arg(path);
        command
    };

    command
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("could not open {} with the system default app: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn browser_descends_and_returns_through_directory_levels() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("repo-hub-browser-{unique}"));
        let nested = root.join("a-folder").join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir(root.join(".git")).unwrap();
        std::fs::write(root.join("z-file.txt"), "").unwrap();
        std::fs::write(nested.join("leaf.txt"), "").unwrap();

        let mut app = App::new(false, Some(Vec::new()));
        app.repos.push(RepoEntry {
            path: root.clone(),
            drive: 0,
            info: None,
            files: Vec::new(),
            busy: false,
        });
        app.filtered.push(0);

        app.click_list_row(0);
        let root_level = app.browser.as_ref().unwrap().levels.last().unwrap();
        assert_eq!(
            root_level
                .entries
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            vec!["a-folder", "z-file.txt"]
        );

        app.click_list_row(0);
        app.click_list_row(0);
        assert_eq!(
            app.browser.as_ref().unwrap().levels.last().unwrap().path,
            nested
        );

        app.go_back();
        assert_eq!(
            app.browser.as_ref().unwrap().levels.last().unwrap().path,
            root.join("a-folder")
        );
        app.go_back();
        assert_eq!(
            app.browser.as_ref().unwrap().levels.last().unwrap().path,
            root
        );
        app.go_back();
        assert!(app.browser.is_none());

        app.request_editor_selected();
        assert!(app.mode == AppMode::EditorPrompt);
        assert_eq!(app.pending_editor.as_deref(), Some(root.as_path()));

        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }
}

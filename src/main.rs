mod app;
mod cache;
mod git;
mod scan;
mod ui;

use std::io;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use app::{App, AppMode};
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind,
        KeyModifiers, MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

fn main() -> io::Result<()> {
    // CLI flags: `repo-hub --no-cache` to force a fresh scan,
    // `--roots /a,/b` to limit scanning to selected folders.
    let args: Vec<String> = std::env::args().collect();
    let mut use_cache = true;
    let mut roots_override: Option<Vec<std::path::PathBuf>> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--no-cache" => use_cache = false,
            "--cache" => use_cache = true,
            "--roots" => {
                if let Some(list) = args.get(i + 1) {
                    let roots: Vec<std::path::PathBuf> = list
                        .split(',')
                        .map(std::path::PathBuf::from)
                        .filter(|p| p.exists())
                        .collect();
                    if !roots.is_empty() {
                        roots_override = Some(roots);
                    }
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }

    let mut app = App::new(use_cache, roots_override);

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let res = run(&mut terminal, &mut app);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
    terminal.show_cursor()?;

    if let Err(e) = res {
        eprintln!("repo-hub error: {e}");
    }
    Ok(())
}

/// Widget areas captured from the most recent render (for mouse hit-testing).
static LAST_AREAS: Mutex<Option<ui::UiAreas>> = Mutex::new(None);
/// Last list-row click, for double-click detection.
static LAST_CLICK: Mutex<Option<(usize, Instant)>> = Mutex::new(None);

fn last_areas() -> ui::UiAreas {
    LAST_AREAS.lock().map(|g| *g).unwrap_or(None).unwrap_or_default()
}

fn handle_mouse(app: &mut App, mouse: crossterm::event::MouseEvent) {
    let areas = last_areas();

    // Wheel scrolling moves the selection through the repo list.
    match mouse.kind {
        MouseEventKind::ScrollUp => {
            app.move_list(-3);
            app.dirty = true;
            return;
        }
        MouseEventKind::ScrollDown => {
            app.move_list(3);
            app.dirty = true;
            return;
        }
        _ => {}
    }

    if !matches!(mouse.kind, MouseEventKind::Down(crossterm::event::MouseButton::Left)) {
        return;
    }

    // 1. Drive tabs along the top.
    if areas.tabs.contains(ratatui::layout::Position { x: mouse.column, y: mouse.row }) {
        let titles = ui::tab_titles(app);
        if let Some(tab) = ui::tab_index_at(areas.tabs, &titles, mouse.column) {
            app.set_drive_tab(tab);
            return;
        }
    }

    // 2. Repo list rows: single click selects, double click opens a shell there.
    if let Some(row) = ui::list_row_at(areas.list, mouse.column, mouse.row) {
        let now = Instant::now();
        let is_double = LAST_CLICK
            .lock()
            .map(|g| g.map(|(r, t)| r == row && now.duration_since(t) < Duration::from_millis(400)).unwrap_or(false))
            .unwrap_or(false);
        if let Ok(mut g) = LAST_CLICK.lock() {
            *g = if is_double { None } else { Some((row, now)) };
        }
        if is_double {
            app.double_click_list_row(row);
        } else {
            app.click_list_row(row);
        }
        return;
    }

    // 3. Click anywhere else dismisses overlays / result popups.
    if app.mode == AppMode::Message || app.help {
        app.mode = AppMode::Normal;
        app.help = false;
        app.dirty = true;
    } else if app.mode == AppMode::Prompt || app.mode == AppMode::Search {
        app.mode = AppMode::Normal;
        app.dirty = true;
    }
}

fn run<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
) -> io::Result<()> {
    let tick_rate = Duration::from_millis(80);
    let mut last_render = Instant::now();

    loop {
        // Drain channel events without blocking (scan results, git info, ops).
        app.poll_events();

        // Only redraw when something changed or the tick elapsed -> keeps CPU low.
        if app.dirty || last_render.elapsed() >= tick_rate {
            let size: ratatui::layout::Rect = terminal.get_frame().area();
            let areas = ui::draw_areas(size, app);
            if let Ok(mut g) = LAST_AREAS.lock() {
                *g = Some(areas);
            }
            terminal.draw(|f| ui::draw(f, app))?;
            app.dirty = false;
            last_render = Instant::now();
        }

        if event::poll(Duration::from_millis(20))? {
            match event::read()? {
                Event::Key(key) => {
                    if key.kind == KeyEventKind::Press {
                        handle_key(app, key);
                    }
                }
                Event::Mouse(m) => handle_mouse(app, m),
                Event::Resize(_, _) => app.dirty = true,
                _ => {}
            }
        }

        if app.should_quit {
            return Ok(());
        }
    }
}

fn handle_key(app: &mut App, key: event::KeyEvent) {
    use KeyCode::*;
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    if ctrl && matches!(key.code, Char('c')) {
        app.should_quit = true;
        return;
    }

    match app.mode {
        AppMode::Message => {
            app.mode = AppMode::Normal;
            app.dirty = true;
        }
        AppMode::Search => search_keys(app, key),
        AppMode::Prompt => prompt_keys(app, key),
        AppMode::Normal => {
            if matches!(key.code, Char('q') | Char('Q')) && !ctrl {
                app.should_quit = true;
                return;
            }
            normal_keys(app, key);
        }
    }
}

fn normal_keys(app: &mut App, key: event::KeyEvent) {
    use KeyCode::*;
    match key.code {
        Up | Char('k') => app.move_list(-1),
        Down | Char('j') => app.move_list(1),
        PageUp => app.move_list(-10),
        PageDown => app.move_list(10),
        Home => app.select(0),
        End => app.move_list(i32::MAX / 2),
        Tab => app.cycle_drive(1),
        BackTab => app.cycle_drive(-1),
        Char('/') => {
            app.mode = AppMode::Search;
            app.search_query.clear();
            app.apply_filter();
        }
        Char('a') => app.stage_all(),
        Char('u') => app.unstage_all(),
        Char('c') => {
            app.mode = AppMode::Prompt;
            app.prompt_title = "Commit message".into();
            app.prompt_value.clear();
            app.dirty = true;
        }
        Char('p') => app.push(false),
        Char('P') => app.push(true),
        Char('f') => app.fetch_selected(),
        Char('F') => app.fetch_all(),
        Char('l') => app.pull(),
        Char('x') => app.clean_build(),
        Char('t') => app.open_terminal(),
        Char('g') => app.open_lazygit(),
        Char('v') => app.open_vscode(),
        Char('e') => app.export_report(),
        Char('n') => app.toggle_dirty_filter(),
        Char('r') => app.rescan(),
        Char('?') | Char('H') => app.show_help(),
        Enter => app.refresh_selected_now(),
        _ => {}
    }
}

fn search_keys(app: &mut App, key: event::KeyEvent) {
    use KeyCode::*;
    match key.code {
        Esc => {
            app.mode = AppMode::Normal;
            app.search_query.clear();
            app.apply_filter();
        }
        Enter => app.mode = AppMode::Normal,
        Backspace => {
            app.search_query.pop();
            app.apply_filter();
        }
        Char(c) => {
            app.search_query.push(c);
            app.apply_filter();
        }
        _ => {}
    }
}

fn prompt_keys(app: &mut App, key: event::KeyEvent) {
    use KeyCode::*;
    match key.code {
        Esc => app.mode = AppMode::Normal,
        Enter => {
            let msg = std::mem::take(&mut app.prompt_value);
            app.mode = AppMode::Normal;
            app.commit(msg);
        }
        Backspace => {
            app.prompt_value.pop();
            app.dirty = true;
        }
        Char(c) => {
            app.prompt_value.push(c);
            app.dirty = true;
        }
        _ => {}
    }
}

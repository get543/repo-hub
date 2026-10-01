//! Ratatui rendering: drive tabs, repo list, detail pane, popups.

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Row, Table, Tabs, Wrap},
    Frame,
};

use crate::app::{App, AppMode};
use crate::git;

/// Widget regions from the last render, used for mouse hit-testing.
#[derive(Default, Clone, Copy)]
pub struct UiAreas {
    pub tabs: Rect,
    pub list: Rect,
    pub detail: Rect,
}

pub fn layout(size: Rect) -> [Rect; 5] {
    Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // tabs
            Constraint::Min(8),    // main panes
            Constraint::Length(2), // status line
            Constraint::Length(1), // glyph legend
            Constraint::Length(1), // key hints
        ])
        .split(size)
        .to_vec()
        .try_into()
        .unwrap()
}

pub fn main_panes(area: Rect) -> [Rect; 2] {
    let right_width = if area.width > 110 { 46 } else { 40 };
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Fill(1), Constraint::Length(right_width)])
        .split(area)
        .to_vec()
        .try_into()
        .unwrap()
}

/// Accurate version: needs the tab titles, so pass them in.
pub fn tab_index_at(tabs: Rect, titles: &[String], col: u16) -> Option<usize> {
    if tabs.width == 0 || col < tabs.x + 1 || col >= tabs.x + tabs.width.saturating_sub(1) {
        return None;
    }
    // row inside the border box (any row counts, we only care about column)
    let mut x = tabs.x + 1;
    for (i, t) in titles.iter().enumerate() {
        let w = t.chars().count() as u16;
        if col < x + w {
            return Some(i);
        }
        x += w + 1; // separator
        if x > tabs.x + tabs.width {
            break;
        }
    }
    None
}

/// Row index within the visible list viewport for a given screen position.
pub fn list_row_at(list: Rect, col: u16, row: u16) -> Option<usize> {
    if list.height <= 2 || list.width == 0 {
        return None;
    }
    let inner_x = list.x + 1;
    let inner_y = list.y + 1;
    let inner_w = list.width.saturating_sub(2);
    let inner_h = list.height.saturating_sub(2);
    if col < inner_x || col >= inner_x + inner_w {
        return None;
    }
    if row < inner_y || row >= inner_y + inner_h {
        return None;
    }
    // Each repo occupies two visual rows.
    Some(((row - inner_y) / 2) as usize)
}

/// Compute the widget areas for a given terminal size (used for mouse hit-testing).
pub fn draw_areas(size: Rect, _app: &App) -> UiAreas {
    let chunks = layout(size);
    let panes = main_panes(chunks[1]);
    UiAreas {
        tabs: chunks[0],
        list: panes[0],
        detail: panes[1],
    }
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let size = f.area();
    let chunks = layout(size);
    let panes = main_panes(chunks[1]);

    draw_tabs(f, app, chunks[0]);
    draw_list(f, app, panes[0]);
    draw_detail(f, app, panes[1]);
    draw_status(f, app, chunks[2]);
    draw_legend(f, chunks[3]);
    draw_hints(f, app, chunks[4]);

    if app.help {
        draw_help(f, size);
    }
    match app.mode {
        AppMode::Search => draw_search_bar(f, app, chunks[1]),
        AppMode::Prompt => draw_prompt(f, app, size),
        AppMode::Message => draw_message(f, app, size),
        _ => {}
    }
}

pub fn tab_titles(app: &App) -> Vec<String> {
    let mut titles: Vec<String> = vec![format!(" All ({}) ", app.repos.len())];
    for d in &app.drives {
        titles.push(format!(" {} ({}) ", d.label, d.count));
    }
    titles
}

fn draw_tabs(f: &mut Frame, app: &App, area: Rect) {
    let titles = tab_titles(app);
    let selected = app.active_drive.map(|i| i + 1).unwrap_or(0);

    let tabs = Tabs::new(titles)
        .block(Block::default().borders(Borders::ALL).title(" repo-hub — drives "))
        .select(selected)
        .style(Style::default().fg(Color::DarkGray))
        .highlight_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        );
    f.render_widget(tabs, area);
}

fn draw_list(f: &mut Frame, app: &mut App, area: Rect) {
    let scanning = app
        .scanner
        .as_ref()
        .map(|s| s.is_running())
        .unwrap_or(false);

    let title = format!(
        " Discovered Repositories ({}) {} ",
        app.filtered.len(),
        if scanning {
            format!(
                "[scanning… {} dirs]",
                app.scanner.as_ref().map(|s| s.dirs_scanned()).unwrap_or(0)
            )
        } else if !app.scan_done && !app.repos.is_empty() {
            String::new()
        } else {
            String::new()
        }
    );

    let height = area.height.saturating_sub(2).max(1) as usize;

    // Keep the selection inside the viewport by adjusting the offset.
    let mut offset = app.list_offset;
    if app.selected >= offset + height {
        offset = app.selected - height + 1;
    }
    if app.selected < offset {
        offset = app.selected;
    }
    app.list_offset = offset;

    let visible: Vec<usize> = app
        .filtered
        .iter()
        .skip(offset)
        .take(height)
        .copied()
        .collect();

    let items: Vec<ListItem> = visible
        .iter()
        .enumerate()
        .map(|(row, repo_idx)| {
            let r = &app.repos[*repo_idx];
            let global_row = offset + row;
            let selected = global_row == app.selected;

            let name = r
                .path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| r.path.display().to_string());

            let (status, branch, badges, sub) = match &r.info {
                None => (
                    "⋯".to_string(),
                    "(reading…)".to_string(),
                    String::new(),
                    shorten_path(&r.path),
                ),
                Some(i) => {
                    let mut badges = String::new();
                    if i.staged > 0 {
                        badges.push_str(&format!(" +{}staged", i.staged));
                    }
                    if i.unstaged > 0 {
                        badges.push_str(&format!(" ~{}mod", i.unstaged));
                    }
                    if i.untracked > 0 {
                        badges.push_str(&format!(" ?{}untracked", i.untracked));
                    }
                    if i.conflicted > 0 {
                        badges.push_str(&format!(" !{}conflict", i.conflicted));
                    }
                    if i.ahead > 0 {
                        badges.push_str(&format!(" ↑{}", i.ahead));
                    }
                    if i.behind > 0 {
                        badges.push_str(&format!(" ↓{}", i.behind));
                    }
                    let age = i
                        .last_commit_ago
                        .map(git::human_age)
                        .unwrap_or_else(|| "-".into());
                    let stale = i
                        .last_commit_ago
                        .map(|d| d.as_secs() > 86400 * 180)
                        .unwrap_or(true);
                    let branch = if i.detached {
                        format!("{} (detached)", i.branch)
                    } else {
                        i.branch.clone()
                    };
                    (
                        i.status_char().to_string(),
                        branch,
                        badges,
                        format!("last commit {age} ago{}", if stale { " [STALE]" } else { "" }),
                    )
                }
            };

            let dot_color = if r.busy {
                Color::Yellow
            } else if status == "●" {
                Color::Red
            } else if status == "▲" {
                Color::Blue
            } else if status == "▼" {
                Color::Magenta
            } else if status == "⚡" {
                Color::LightRed
            } else {
                Color::Green
            };

            let line = Line::from(vec![
                Span::styled(format!("{status} "), Style::default().fg(dot_color)),
                Span::styled(
                    name,
                    Style::default()
                        .fg(if r.busy { Color::Yellow } else { Color::White })
                        .add_modifier(if selected { Modifier::BOLD } else { Modifier::empty() }),
                ),
                Span::styled(format!("  [{branch}]"), Style::default().fg(Color::Cyan)),
                Span::styled(badges, Style::default().fg(Color::LightYellow)),
            ]);
            let second = Line::from(vec![Span::styled(
                format!("   {sub}"),
                Style::default().fg(Color::DarkGray),
            )]);

            let bg = if selected {
                Color::Rgb(35, 45, 60)
            } else {
                Color::Reset
            };
            ListItem::new(vec![line, second]).style(Style::default().bg(bg))
        })
        .collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .title_bottom(format!(
            " showing {}-{} of {} ",
            if app.filtered.is_empty() { 0 } else { offset + 1 },
            (offset + visible.len()).min(app.filtered.len()),
            app.filtered.len()
        ))
        .border_style(Style::default().fg(if scanning {
            Color::Yellow
        } else {
            Color::DarkGray
        }));

    let list = List::new(items).block(block);
    f.render_widget(list, area);
}

fn shorten_path(p: &std::path::Path) -> String {
    let s = p.display().to_string();
    if s.len() <= 60 {
        return s;
    }
    format!("…{}", &s[s.len() - 59..])
}

fn draw_detail(f: &mut Frame, app: &App, area: Rect) {
    let entry = app.selected_entry();
    let path = entry
        .map(|e| e.path.display().to_string())
        .unwrap_or_else(|| "(no repository selected)".into());

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", shorten_path(std::path::Path::new(&path))))
        .title_alignment(ratatui::layout::Alignment::Center);

    let Some(entry) = entry else {
        f.render_widget(
            Paragraph::new("Select a repository from the list to inspect it.").block(block),
            area,
        );
        return;
    };

    let mut lines: Vec<Line> = Vec::new();

    match &entry.info {
        None => lines.push(Line::from(Span::styled(
            if entry.busy {
                "reading repository…"
            } else {
                "queued for reading…"
            },
            Style::default().fg(Color::Yellow),
        ))),
        Some(i) => {
            if let Some(err) = &i.error {
                lines.push(Line::from(Span::styled(
                    format!("error: {err}"),
                    Style::default().fg(Color::Red),
                )));
            }
            let branch_line = format!(
                "Branch : {} {}",
                i.branch,
                if i.has_upstream {
                    format!("(ahead {} / behind {})", i.ahead, i.behind)
                } else {
                    "(no upstream)".into()
                }
            );
            lines.push(Line::from(branch_line));
            lines.push(Line::from(format!(
                "Last   : {} {}",
                i.last_commit
                    .clone()
                    .unwrap_or_else(|| "(no commits yet)".into()),
                i.last_commit_ago
                    .map(|d| format!("[{} ago]", git::human_age(d)))
                    .unwrap_or_default()
            )));
            lines.push(Line::from(format!(
                "Remote : {}",
                i.remote.clone().unwrap_or_else(|| "(none — local only)".into())
            )));
            lines.push(Line::from(""));
            lines.push(Line::from(format!(
                "Status : staged {} · modified {} · untracked {} · conflicted {}",
                i.staged, i.unstaged, i.untracked, i.conflicted
            )));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Changed files:",
                Style::default().add_modifier(Modifier::UNDERLINED),
            )));
            if entry.files.is_empty() {
                lines.push(Line::from(Span::styled(
                    "  working tree clean ✓",
                    Style::default().fg(Color::Green),
                )));
            } else {
                for fe in entry.files.iter().take(200) {
                    let color = match fe.code.as_str() {
                        c if c.contains('?') => Color::DarkGray,
                        c if c.starts_with('A') || c.starts_with('R') => Color::Green,
                        c if c.starts_with('D') => Color::Red,
                        _ => Color::Yellow,
                    };
                    lines.push(Line::from(vec![
                        Span::styled(format!(" {} ", fe.code), Style::default().fg(color)),
                        Span::raw(fe.path.clone()),
                    ]));
                }
                if entry.files.len() > 200 {
                    lines.push(Line::from(format!(
                        "  … and {} more",
                        entry.files.len() - 200
                    )));
                }
            }
        }
    }

    let para = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    f.render_widget(para, area);
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let scanning = app
        .scanner
        .as_ref()
        .map(|s| s.is_running())
        .unwrap_or(false);

    let spinner = if scanning {
        let frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        let idx = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() / 120)
            .unwrap_or(0)
            % frames.len() as u128) as usize;
        Span::styled(frames[idx], Style::default().fg(Color::Yellow))
    } else {
        Span::styled("●", Style::default().fg(Color::Green))
    };

    let mut spans = vec![
        Span::raw(" "),
        spinner,
        Span::styled(format!(" {}", app.status_line), Style::default().fg(Color::White)),
    ];
    if app.dirty_only {
        spans.push(Span::styled(
            "  [dirty-only filter ON] ",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ));
    }
    if !app.search_query.is_empty() {
        spans.push(Span::styled(
            format!("  filter: \"{}\" ", app.search_query),
            Style::default().fg(Color::Cyan),
        ));
    }

    f.render_widget(
        Paragraph::new(Line::from(spans)).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        ),
        area,
    );
}

/// One-line legend explaining every status glyph & badge the list can show.
fn draw_legend(f: &mut Frame, area: Rect) {
    let spans = vec![
        Span::raw(" "),
        Span::styled("●", Style::default().fg(Color::Red)),
        Span::raw(" uncommitted changes  "),
        Span::styled("▲", Style::default().fg(Color::Blue)),
        Span::raw(" ahead → push needed  "),
        Span::styled("▼", Style::default().fg(Color::Magenta)),
        Span::raw(" behind → pull needed  "),
        Span::styled("⚡", Style::default().fg(Color::LightRed)),
        Span::raw(" merge conflict  "),
        Span::styled("○", Style::default().fg(Color::Green)),
        Span::raw(" clean  "),
        Span::styled("⋯", Style::default().fg(Color::Yellow)),
        Span::raw(" reading…  "),
        Span::styled("+n", Style::default().fg(Color::LightYellow)),
        Span::raw(" staged  "),
        Span::styled("~n", Style::default().fg(Color::LightYellow)),
        Span::raw(" modified  "),
        Span::styled("?n", Style::default().fg(Color::LightYellow)),
        Span::raw(" untracked  "),
        Span::styled("[STALE]", Style::default().fg(Color::DarkGray)),
        Span::raw(" no commit in 6 months"),
    ];
    f.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().fg(Color::DarkGray)),
        area,
    );
}

fn draw_hints(f: &mut Frame, app: &App, area: Rect) {
    let txt = match app.mode {
        AppMode::Normal => " 🖱 click=select · dbl-click=shell · wheel=scroll · Tab=drive  |  ⌨ j/k move · / search · a stage · c commit · p push · f fetch · F fetch-all · n dirty-only · t/g/v tools · x clean · ? help · q quit ",
        AppMode::Search => " 🔎 type to filter repos · Enter apply · Esc clear ",
        AppMode::Prompt => " ✏️  Enter = run commit · Esc = cancel ",
        AppMode::Message => " press any key / click to dismiss ",
    };
    f.render_widget(
        Paragraph::new(txt).style(
            Style::default()
                .fg(Color::DarkGray)
                .bg(Color::Rgb(20, 22, 28)),
        ),
        area,
    );
    let _ = app;
}

fn centered_rect(percent_x: u16, _y: u16, height: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Fill(1),
            Constraint::Length(height),
            Constraint::Fill(1),
        ])
        .split(r);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Fill((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Fill((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

fn draw_search_bar(f: &mut Frame, app: &App, area: Rect) {
    let bar = Paragraph::new(format!(" 🔎 {}", app.search_query))
        .block(Block::default().borders(Borders::ALL).title(" Search "))
        .style(Style::default().fg(Color::Cyan));
    let inner = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3)])
        .split(area)[0];
    f.render_widget(Clear, inner);
    f.render_widget(bar, inner);
}

fn draw_prompt(f: &mut Frame, app: &App, area: Rect) {
    let popup = centered_rect(60, 1, 5, area);
    f.render_widget(Clear, popup);
    let widget = Paragraph::new(app.prompt_value.as_str())
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" {} (Enter to run) ", app.prompt_title)),
        )
        .style(Style::default().fg(Color::White));
    f.render_widget(widget, popup);
}

fn draw_message(f: &mut Frame, app: &App, area: Rect) {
    let h = app.message.lines().count().max(1) as u16 + 3;
    let popup = centered_rect(70, 1, h.min(14), area);
    f.render_widget(Clear, popup);
    let widget = Paragraph::new(app.message.clone())
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" result ")
                .border_style(Style::default().fg(Color::Yellow)),
        )
        .wrap(Wrap { trim: false });
    f.render_widget(widget, popup);
}

fn draw_help(f: &mut Frame, area: Rect) {
    fn section(title: &str, color: Color) -> Row<'static> {
        Row::new(vec![
            Span::styled(
                title.to_string(),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::raw(""),
        ])
    }
    fn kv(k: &str, v: &str) -> Row<'static> {
        Row::new(vec![
            Span::styled(
                k.to_string(),
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
            ),
            Span::styled(v.to_string(), Style::default().fg(Color::Gray)),
        ])
    }

    let rows = vec![
        section("🖱  Mouse", Color::Green),
        kv("click a row", "select that repository (detail pane updates instantly)"),
        kv("double-click a row", "open a shell inside that repo — type `exit` to return"),
        kv("scroll wheel", "move through the repo list (3 rows per notch)"),
        kv("click a drive tab", "filter the list to that drive, or \"All\""),
        kv("click outside popup", "dismiss help / result / search / prompt"),
        section("⌨  Keys", Color::Cyan),
        kv("j / k / ↑ / ↓", "move selection through the repository list"),
        kv("Tab / Shift-Tab", "cycle drive tabs (All → drive1 → drive2 …)"),
        kv("/", "live search over repo names & full paths (Enter keeps, Esc clears)"),
        kv("Enter", "re-read the selected repo's git status right now"),
        kv("n", "end-of-day audit: show only dirty / unpushed repos"),
        kv("a  ·  u", "stage everything (git add -A)  ·  unstage everything"),
        kv("c", "commit staged changes (prompts for a message; Enter runs it)"),
        kv("p / P", "push  ·  push --force-with-lease (use sparingly!)"),
        kv("l", "pull --ff-only"),
        kv("f / F", "fetch this repo  ·  fetch ALL repos in the background"),
        kv("t / g / v", "open shell / lazygit / VS Code at the repo"),
        kv("x", "delete target/node_modules/dist/build to reclaim disk space"),
        kv("e", "export a TSV report of every repo to the temp folder"),
        kv("r", "rescan all drives from scratch (clears the cache)"),
        kv("?  ·  q", "toggle this help  ·  quit (Ctrl-C also works)"),
        section("◉  Status glyphs & badges", Color::Yellow),
        kv("● red", "dirty — staged/modified/untracked files not committed"),
        kv("▲ blue", "ahead — local commits not yet pushed to the remote"),
        kv("▼ magenta", "behind — remote has commits you haven't pulled"),
        kv("⚡ bright-red", "conflicted files — resolve before committing"),
        kv("○ green", "clean and in sync with its remote"),
        kv("⋯ yellow", "repo info still being read in the background"),
        kv("+n / ~n / ?n", "staged / modified / untracked file counts"),
        kv("↑n / ↓n", "commits ahead of / behind the upstream branch"),
        kv("[STALE]", "no commit for 6+ months — candidate for `x` cleanup"),
    ];

    let widths = [Constraint::Length(20), Constraint::Fill(1)];
    let table = Table::new(rows, widths)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" repo-hub — mouse, keys & symbols ")
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .column_spacing(2);
    let popup = centered_rect(80, 1, 34, area);
    f.render_widget(Clear, popup);
    f.render_widget(table, popup);
}

# repo-hub

`repo-hub` is a terminal user interface (TUI) for finding and managing Git repositories on your computer. It scans your drives, shows the current status of every repository, and lets you perform common Git and project-maintenance tasks without opening each repository separately.

## What it does

- Finds Git repositories across detected drives.
- Shows the current branch, last commit age, remote, and ahead/behind status.
- Shows staged, modified, untracked, and conflicted files.
- Filters repositories by drive, name, or path.
- Shows only repositories that are dirty or have unpushed commits.
- Runs common Git operations: stage, unstage, commit, push, pull, and fetch.
- Fetches every discovered repository in the background.
- Opens a shell, lazygit, or VS Code in the selected repository.
- Removes common build artifacts (`target`, `node_modules`, `dist`, and `build`) when requested.
- Exports a tab-separated report to your operating system's temporary directory.

The first scan can take some time because the application searches the selected roots recursively. Later launches use a cache for fast startup and refresh repository information in the background.

## Requirements

- Git, available on your `PATH`.
- Rust and Cargo, if you build the application from source.
- A terminal that supports the Crossterm TUI.
- Optional: `lazygit` for the `g` shortcut and VS Code's `code` command for the `v` shortcut.

## Download

### Download the source code

Clone the repository with Git:

```bash
git clone https://github.com/udin-agent/github-detector-ratatui.git
cd github-detector-ratatui
```

Or download the source as a ZIP from the repository's GitHub page:

<https://github.com/udin-agent/github-detector-ratatui>

This project currently documents building from source. If prebuilt binaries are published later, they should be downloaded from the repository's **Releases** page rather than from the source archive.

## Build and run

Install Rust from <https://rustup.rs/> if Rust is not already installed. Then run the application in development mode:

```bash
cargo run
```

For a faster executable, build and run the release version:

```bash
cargo run --release
```

To build without starting the application, use:

```bash
cargo build --release
```

The executable is written to `target/release/`:

- Windows: `target/release/repo-hub.exe`
- Linux and macOS: `target/release/repo-hub`

You can also install it into Cargo's local binary directory:

```bash
cargo install --path .
repo-hub
```

## Command-line options

By default, repo-hub uses its cache and scans the drives detected by the operating system.

```text
repo-hub --no-cache
```

Ignore the saved cache and perform a fresh scan while starting:

```text
repo-hub --cache
```

Explicitly enable the cache. This is the default behavior.

Limit the scan to one or more existing folders. Separate multiple paths with commas:

```text
repo-hub --roots C:\Users\you\src,D:\projects
```

On Linux or macOS:

```text
repo-hub --roots ~/src,~/projects
```

## How to use the app

When repo-hub starts, it begins scanning immediately. Repositories appear in the list as they are found. Select a repository to see its details in the right-hand pane. You can continue navigating while the scan and Git status checks run in the background.

### Navigation and filtering

| Key or action | What it does |
| --- | --- |
| `j` / `k`, `Up` / `Down` | Move through the repository list |
| `PageUp` / `PageDown` | Move by a page |
| `Home` / `End` | Move to the first or last repository |
| `Tab` / `Shift-Tab` | Cycle through **All** and the drive tabs |
| `/` | Search repository names and full paths as you type |
| `Enter` | Refresh the selected repository's Git status |
| `n` | Toggle the dirty/unpushed-only filter |
| `r` | Clear the cache and rescan all roots from scratch |
| `?` or `H` | Open the built-in help screen |
| `q` or `Ctrl-C` | Quit |

Mouse input is also supported. Click a row to select it, double-click a row to open a shell in that repository, use the wheel to scroll, or click a drive tab to filter by drive.

### Git operations

These commands apply to the selected repository:

| Key | Operation |
| --- | --- |
| `a` | Stage all changes (`git add -A`) |
| `u` | Unstage all changes while keeping files on disk |
| `c` | Enter a commit message and create a commit |
| `p` | Push to the upstream remote |
| `P` | Force-push with lease; use carefully |
| `l` | Pull with fast-forward only |
| `f` | Fetch the selected repository |
| `F` | Fetch every discovered repository |

The application refreshes the selected repository after an operation and displays the result in a popup.

### External tools and reports

| Key | Operation |
| --- | --- |
| `t` | Open the system shell in the selected repository |
| `g` | Open lazygit in the selected repository |
| `v` | Open VS Code or VSCodium in the selected repository |
| `e` | Write a TSV report to the system temporary directory |
| `x` | Delete `target`, `node_modules`, `dist`, and `build` in the selected repository |

The `x` action permanently deletes those build-artifact directories. Review the selected repository before using it.

## Status indicators

| Indicator | Meaning |
| --- | --- |
| Green circle | Clean and synchronized with its remote |
| Red circle | Staged, modified, or untracked files exist |
| Blue up arrow | Local commits need to be pushed |
| Magenta down arrow | Remote commits need to be pulled |
| Red lightning bolt | Conflicted files exist |
| `[STALE]` | No commit has been made for six months or more |
| `+n` / `~n` / `?n` | Staged / modified / untracked file counts |
| `↑n` / `↓n` | Commits ahead of / behind the upstream branch |

## Cache location

The repository list cache is stored in the standard configuration directory under `repo-hub/cache.json`:

- Windows: `%APPDATA%\repo-hub\cache.json`
- Linux: `~/.config/repo-hub/cache.json` (depending on your environment)
- macOS: `~/Library/Application Support/repo-hub/cache.json`

Use `r` or start with `--no-cache` when you need a completely fresh scan.

## License

This project is licensed under the MIT License. See [LICENSE](LICENSE).
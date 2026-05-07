# yttui

> ▶ Keyboard-driven YouTube TUI for terminals that take vim seriously.

Search YouTube via [yt-dlp](https://github.com/yt-dlp/yt-dlp), play
results via [mpv](https://mpv.io/). No Invidious dependency, no
telemetry, no comments. Built in Rust on [ratatui](https://ratatui.rs/).

## Features

- Vim-keyed list (`j`/`k`, `gg`/`G`, `Ctrl-d`/`Ctrl-u`, `/`, `?`)
- Cancellable in-flight searches (Esc kills the `yt-dlp` process group)
- Filter results live (`/`)
- Re-run, new search, audio-only mode
- Refuses to launch on upcoming livestreams (no mpv on a stream that
  hasn't started)
- Single static binary, ~1 MB. Cold start under 100 ms.

## Prerequisites

| Tool | Why | Install |
|---|---|---|
| [`yt-dlp`](https://github.com/yt-dlp/yt-dlp) | search + URL resolution | `brew install yt-dlp` |
| [`mpv`](https://mpv.io/) | playback | `brew install mpv` |

`yttui` will refuse to start if either is missing, with a friendly
message pointing you at the install command.

## Install

### From source (local)

```sh
git clone <repo-url> yttui
cd yttui
cargo install --path .
```

### From a Git remote

```sh
cargo install --git <repo-url>
```

### Homebrew

A formula isn't published yet. Build from source for now.

## Usage

```sh
yttui                              # opens an empty prompt
yttui factorio mega bus            # immediate search
yttui --recent rust                # sort by upload date
yttui --count 50 vim               # 50 results (1..=100)
yttui --audio-only "lofi hip hop"  # mpv with --no-video
yttui --help                       # full flag reference
yttui --version                    # version, author, license
```

Inside the TUI:

| Key | Action |
|---|---|
| `j` / `↓` | Next result |
| `k` / `↑` | Previous result |
| `gg` / `G` | First / last result |
| `Ctrl-d` / `Ctrl-u` | Half-page down / up |
| `Enter` | Play selected video |
| `/` | Filter current results (live) |
| `n` | New search |
| `r` | Re-run current search |
| `?` | Help overlay (also shown inline on the prompt) |
| `q` / `Esc` | Quit (or cancel current modal) |

During playback, mpv takes the terminal; close mpv and you return to
the result list. Esc on the searching screen cancels and kills the
`yt-dlp` subprocess.

## Files

| Path | What |
|---|---|
| `~/Library/Caches/yttui/yttui.log` (macOS) | warning-level log |
| `~/.cache/yttui/yttui.log` (Linux/XDG) | warning-level log |

No config file in V1. V2 will land one at
`$XDG_CONFIG_HOME/yttui/config.toml`.

## macOS launch caveat

Apps launched from Finder, Spotlight, or Raycast inherit a minimal
`$PATH` that omits `/opt/homebrew/bin`. If `yttui` reports
`yt-dlp not found in PATH` despite a working Homebrew install, launch
it from a real terminal — Ghostty, iTerm, Terminal.app — so the
shell's `$PATH` applies.

## Privacy

- `yt-dlp` talks to YouTube directly from your IP. No logged-in account.
- Everything is local. No cloud sync, no telemetry, no analytics.
- Logs are warning-level only and live entirely in the platform cache
  dir. Delete them whenever.

## Status

V1.0 — search, list, play, vim keybindings, cancellation, error
recovery. See [`roadmap.md`](./roadmap.md) for V2 (thumbnails, watch
history, config) and V3 (subscription feed + personal recommendation
engine).

## License

MIT — see [LICENSE](./LICENSE).

# yttui

A keyboard-driven YouTube TUI. Search via [yt-dlp][], play via [mpv][],
no Invidious, no comments.

![demo](docs/demo.gif)

## Features

- Vim-keyed list (`j`/`k`, `gg`/`G`, `Ctrl-d`/`Ctrl-u`, `/`, `?`)
- Cancellable in-flight searches (Esc kills the `yt-dlp` process group)
- Filter results live (`/`)
- Re-run, new search, audio-only mode
- Refuses to launch on upcoming livestreams
- Single static binary, ~1 MB

## Prerequisites

| Tool | Why | Install |
|---|---|---|
| [yt-dlp][] | search + URL resolution | `brew install yt-dlp` |
| [mpv][] | playback | `brew install mpv` |

`yttui` will refuse to start if either is missing and tell you how to
install it.

## Install

From source (after cloning):

```sh
cargo install --path .
```

From a Git remote:

```sh
cargo install --git <repo-url>
```

A Homebrew formula isn't published yet. Build from source for now.

## Usage

```sh
yttui                              # opens an empty prompt
yttui factorio mega bus            # immediate search
yttui --recent rust                # sort by upload date
yttui --count 50 vim               # 50 results (1..=100)
yttui --audio-only "lofi hip hop"  # mpv with --no-video
yttui --help
yttui --version
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

During playback, mpv takes the terminal; close mpv to return to the
result list. Esc on the searching screen cancels and kills the
`yt-dlp` subprocess.

### Playback quality and subtitles

mpv handles both. To cap quality or auto-load subtitles, configure mpv
itself in `~/.config/mpv/mpv.conf`:

```conf
ytdl-format=bestvideo[height<=1080]+bestaudio/best
ytdl-raw-options=write-subs=,write-auto-subs=,sub-lang="en"
```

During playback: `j` / `J` cycle subtitle tracks, `v` toggles visibility.

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

All state is local — no telemetry, no cloud sync. `yt-dlp` talks to
YouTube directly from your IP. Logs (warning-level only) live in your
platform cache dir; safe to delete.

## Status

V1.0. See [`roadmap.md`](./roadmap.md) for V2 (thumbnails, watch
history, config) and V3 (subscription feed + personal recommendation
engine).

## Recording the demo

The demo asset above is generated from a [vhs][] tape so it stays
reproducible. To regenerate:

```sh
brew install vhs
vhs docs/demo.tape    # produces docs/demo.gif
```

The tape file is `docs/demo.tape` — edit and re-run to update the
visual.

## License

MIT — see [LICENSE](./LICENSE).

[yt-dlp]: https://github.com/yt-dlp/yt-dlp
[mpv]: https://mpv.io/
[vhs]: https://github.com/charmbracelet/vhs

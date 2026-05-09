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

From source:

```sh
git clone https://github.com/jbenge1/yttui
cd yttui
cargo install --path .
```

Or directly:

```sh
cargo install --git https://github.com/jbenge1/yttui
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

## Configuration

`yttui` reads an optional TOML config from:

- `$XDG_CONFIG_HOME/yttui/config.toml` if `$XDG_CONFIG_HOME` is set
- otherwise `~/.config/yttui/config.toml` (Linux **and** macOS)

A missing file is fine — defaults reproduce V1 behavior exactly.
Malformed TOML is reported at `error` level (filter-immune) and
`yttui` falls back to defaults so the TUI still launches.

Two knobs are wired up in 0.1.2:

- `[player] args` — extra args appended to the `mpv` invocation,
  after ytTUI's managed flags and before the URL. mpv resolves
  conflicting options last-wins, so anything here can override a
  ytTUI default of the same option — including audio-only mode (pass
  `--video=auto` to defeat `--audio-only`). The URL position is
  fixed; user args cannot displace it.
- `[log] level` — one of `off | error | warn | info | debug | trace`.
  Default `warn`. Maps directly to `log::LevelFilter`.

Example:

```toml
[player]
args = ["--save-position-on-quit", "--no-osc"]

[log]
level = "info"
```

Not yet supported in 0.1.2: a `--config` flag, env-var overrides, and
CLI/config precedence merging. Only the on-disk file loads today;
those layers land in A1.2.

## Files

| Path | What |
|---|---|
| `$XDG_CONFIG_HOME/yttui/config.toml` or `~/.config/yttui/config.toml` | optional TOML config (see above) |
| `~/Library/Caches/yttui/yttui.log` (macOS) | log file (level configurable) |
| `~/.cache/yttui/yttui.log` (Linux/XDG) | log file (level configurable) |

## macOS launch caveat

Apps launched from Finder, Spotlight, or Raycast inherit a minimal
`$PATH` that omits `/opt/homebrew/bin`. If `yttui` reports
`yt-dlp not found in PATH` despite a working Homebrew install, launch
it from a real terminal — Ghostty, iTerm, Terminal.app — so the
shell's `$PATH` applies.

## Signals and orphan prevention

`yttui` runs `mpv` (and `yt-dlp`) in their own process groups so a
`SIGINT`/`SIGTERM` to `yttui` itself, or a `Ctrl-C` at the parent
terminal during playback, will tear the children down with it instead
of orphaning them. The signal-watcher thread runs from process start
to exit; manual verification:

```sh
# Variant A: signal yttui from another terminal.
# Terminal 1
yttui rust ratatui          # search, then Enter on a result
# Terminal 2 (while mpv is playing)
kill -INT $(pgrep -x yttui) # or `kill -TERM …`
sleep 0.2                   # let the watcher kill mpv asynchronously
pgrep mpv                   # should print nothing
```

```sh
# Variant B: literal Ctrl-C at yttui's terminal during playback.
# This is the user-facing path the orphan-prevention guarantee
# exists for; Variant A is *almost* equivalent because yttui keeps
# foreground (it doesn't tcsetpgrp to mpv), but exercising the
# real Ctrl-C is the spec-cited shape.
yttui rust ratatui          # search, then Enter on a result
# While mpv is playing, focus yttui's terminal and press Ctrl-C.
sleep 0.2
pgrep mpv                   # should print nothing
```

A second `Ctrl-C` arriving while the watcher is still running its
cleanup escalates: the process exits immediately, matching POSIX
shell convention. `SIGKILL` (`kill -9`) is uninterceptable — it
bypasses the watcher and will leak `mpv`. That's a kernel
limitation, not something `yttui` can fix.

## Privacy

All state is local — no telemetry, no cloud sync. `yt-dlp` talks to
YouTube directly from your IP. Logs (level configurable, `warn` by
default) live in your platform cache dir; safe to delete.

## Status

V1.0. See [`roadmap.md`](./roadmap.md) for V2 (thumbnails, watch
history, config) and V3 (subscription feed + personal recommendation
engine).

## License

MIT — see [LICENSE](./LICENSE).

[yt-dlp]: https://github.com/yt-dlp/yt-dlp
[mpv]: https://mpv.io/

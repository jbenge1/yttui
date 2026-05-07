# YouTube TUI — Project Spec & Roadmap

A keyboard-driven terminal application for searching, browsing, and watching YouTube. Built in Rust with Ratatui. Privacy-first, vim-keybinding-native, defensive by default.

This document is a roadmap, not a prescription. Implement V1 fully before starting V2. Implement V2 fully before V3. Do not let scope from later phases bleed into earlier ones.

## Context & Motivation

The user already has a working ~15-line zsh function (`yts`) that does V1.0 acceptably:

```bash
yts() {
  local results
  results=$(yt-dlp "ytsearch20:$*" \
    --flat-playlist \
    --print "%(title)s ::: %(channel)s ::: %(duration_string)s ::: %(id)s" \
    2>/dev/null)
  [ -z "$results" ] && { echo "No results"; return 1; }
  local selected
  selected=$(echo "$results" | \
    fzf --delimiter=' ::: ' --with-nth=1,2,3 --prompt="yt> ") || return
  local id="${selected##*::: }"
  mpv "https://youtube.com/watch?v=$id"
}
```

This is the functional baseline. The Rust rewrite must do at least this, but with proper error handling, configurability, thumbnails, and (eventually) a personal recommendation layer.

The user runs:
- macOS (Apple Silicon)
- Ghostty as terminal (Kitty graphics protocol supported)
- nom for RSS-based YouTube subscription feeds (sourced from RSSHub on a homelab)
- Vim-style keybindings everywhere (neovim, yazi, aerc)

The user does **not** want:
- Invidious dependencies (public instances are unreliable; self-hosting is a separate problem)
- Comments support (correctly identified as a trap)
- Anything Electron-flavored
- Reinventing yt-dlp's job (it's the right tool for scraping/playback)

## Non-Goals

- Replicating youtube.com 1:1
- Logged-in YouTube features (likes, watch later, subscriptions-via-account)
- Comments, live chat, or community posts
- Mobile/web/GUI versions
- Cross-platform polish for Windows or Linux (macOS-first; portable where free)

## Stack

- **Language:** Rust (stable channel)
- **TUI framework:** Ratatui + Crossterm
- **Search/scraping:** Shell out to `yt-dlp` binary. Do not reimplement YouTube scraping.
- **Playback:** Shell out to `mpv` binary.
- **Image rendering (V2+):** Kitty graphics protocol (Ghostty supports it). Provide a fallback config option for sixel and chafa, but kitty is the default.
- **Persistence (V3):** SQLite via `rusqlite`.
- **HTTP (V3, for thumbnail fetching independent of yt-dlp):** `reqwest` with rustls.
- **Config:** TOML via `serde` + `toml`. Config file at `$XDG_CONFIG_HOME/yttui/config.toml`.

## Directory Layout

```
$XDG_CONFIG_HOME/yttui/
  config.toml           # user config
$XDG_DATA_HOME/yttui/
  history.db            # SQLite, V3
  thumbnails/           # cached thumbnails, V2
$XDG_CACHE_HOME/yttui/
  search-cache.db       # ephemeral search results, V2
```

Use the `directories` or `dirs` crate. Do not invent paths.

---

## V1.0 — Search → Pick → Play

**Goal:** Match the shell function's behavior. Nothing more.

### Features

- Single command: `yttui` opens a search prompt
- User types a query, hits enter
- Results list shows: title, channel, duration
- Vim navigation in the list (`j`/`k` to move, `gg`/`G` for top/bottom, `/` to filter)
- Enter plays the selected video in `mpv` via subprocess
- `q` or `esc` quits
- `?` shows keybinding help

### CLI

```
yttui [QUERY...]              # if QUERY given, search immediately; else prompt
yttui --recent [QUERY...]     # sort by upload date instead of relevance
yttui --count N               # number of results, default 20
yttui --audio-only            # pass --no-video to mpv
```

### Behavior

- Search uses `yt-dlp "ytsearch{N}:{query}" --flat-playlist --print-json`
- Parse the JSON output. Do not parse `--print` template strings; use proper JSON.
- Display results in a Ratatui `List` widget with vim keybindings
- On enter: spawn `mpv https://youtube.com/watch?v={id}` as a subprocess
- Wait for mpv to exit, then return to the results list (don't re-search)
- `r` re-runs the same search (in case of stale results)
- `n` runs a new search (prompt for query)

### Keybindings (V1.0)

| Key | Action |
|---|---|
| `j` / `↓` | Next result |
| `k` / `↑` | Previous result |
| `gg` | First result |
| `G` | Last result |
| `Ctrl-d` / `Ctrl-u` | Half-page down/up |
| `Enter` | Play selected video |
| `n` | New search |
| `r` | Re-run current search |
| `/` | Filter current results (fuzzy) |
| `?` | Show help overlay |
| `q` / `Esc` | Quit |

### Defensive Programming Requirements (V1.x)

These are NOT V2 features. They are V1 hardening, applied before any V2 work begins.

- **No panics.** All `unwrap()` and `expect()` calls must be eliminated except in code paths provably unreachable. Use `?` propagation and explicit error types via `thiserror`.
- **yt-dlp not installed:** detect at startup, print actionable message ("install yt-dlp via `brew install yt-dlp`"), exit cleanly.
- **mpv not installed:** same.
- **Network failure:** show inline error in TUI, allow retry without restarting.
- **No results:** distinct UI state, not an empty list.
- **Malformed yt-dlp JSON:** log to stderr, skip the bad row, don't crash.
- **Terminal too small:** detect minimum dimensions (e.g. 60x20), show graceful "terminal too small" message instead of broken layout.
- **Subprocess timeouts:** yt-dlp search has a 30s timeout. Mpv has none (user controls it).
- **Ctrl-C during search:** kill the yt-dlp subprocess, return to prompt.
- **Unicode in titles:** all rendering must handle wide chars, emoji, RTL text. Use `unicode-width` for column calculations.

### V1.0 Acceptance Criteria

- `cargo build --release` produces a single binary
- `yttui factorio mega bus` returns results in under 5 seconds on broadband
- All keybindings work
- Killing the process at any point leaves no orphaned yt-dlp/mpv subprocesses
- Running with no network shows an error state, not a crash
- Tested on macOS (Apple Silicon) in Ghostty
- Cross-compiles for Linux (x86_64-unknown-linux-gnu) without macOS-specific deps

---

## V2.0 — Thumbnails, Polish, Persistence

**Goal:** Make it visually pleasant and start tracking state.

### Features

- Inline thumbnails via Kitty graphics protocol (Ghostty native)
- Video description preview pane (toggleable)
- Search history (last 100 queries, navigable with up-arrow in search prompt)
- Watch history persisted in SQLite
- Resume from where you left off in a video (mpv `--save-position-on-quit`, integrate with mpv's IPC)
- Configurable layout (preview pane left/right/bottom/hidden)
- Multiple selection with `Tab`, queue all selected to mpv playlist

### Thumbnail Implementation

- yt-dlp returns a `thumbnail` URL in its JSON output. Fetch via `reqwest`.
- Cache thumbnails on disk under `$XDG_CACHE_HOME/yttui/thumbnails/{video_id}.jpg`
- Render via Kitty graphics protocol — write the escape sequence directly. There are crates (`viuer`, `image`) that abstract this; use them if they support kitty proto correctly, otherwise emit the protocol manually.
- Async thumbnail loading: results render immediately, thumbnails fill in as they arrive
- Fallback to chafa-style ANSI rendering if user opts out of kitty proto in config

### Search History

- Stored in SQLite (`history.db` table `search_history`)
- Up-arrow in search prompt cycles through prior queries
- Ctrl-r opens fuzzy search over query history (like shell reverse-i-search)

### Watch History

- Every video played is logged with timestamp, video_id, title, channel, duration, watch_position
- `yttui --history` opens the history view (same UI as search results)
- History view supports filtering by channel, date range, watch state (completed / partial / unwatched)

### Config File (V2.0)

```toml
[search]
default_count = 20
default_sort = "relevance"   # or "date"

[playback]
player = "mpv"
player_args = ["--save-position-on-quit"]
audio_only = false

[display]
thumbnails = true
thumbnail_protocol = "kitty"  # "kitty" | "sixel" | "chafa" | "none"
preview_pane = "right"         # "right" | "left" | "bottom" | "hidden"
preview_width_pct = 40

[keybindings]
# allow user to override any binding
quit = ["q", "Esc"]
play = ["Enter"]
# etc

[history]
max_search_history = 100
track_watch_history = true
```

### V2.0 Acceptance Criteria

- Thumbnails render correctly in Ghostty within 200ms of selection change
- No memory leak with thumbnail caching across long sessions
- SQLite schema migrations handled (use a migration crate or hand-rolled versioning)
- All V1.0 acceptance criteria still pass
- Config file is hot-reloadable (or at minimum, validated on load with clear error messages for malformed TOML)

---

## V3.0 — Subscriptions Integration & Personal Algorithm

**Goal:** Replace the YouTube algorithm with one the user controls. This is the greenfield phase. Treat the spec below as a starting point, not a requirement.

### V3 has two halves:

#### V3.a — Subscription Feed Aggregation

The user already runs RSSHub generating YouTube feeds, consumed by nom. V3.a brings that into yttui as a homepage:

- yttui startup view becomes a "feed" of recent uploads from subscribed channels
- Subscription source: read OPML or list of channel IDs/RSS URLs from config
- Optional: integrate with the user's existing RSSHub instance (config: `rsshub_url`)
- Feed view supports same vim keybindings as search results
- Mark-as-watched, hide, and "never recommend this channel" actions
- Pull-to-refresh (or `R` keybind)

#### V3.b — Personal Recommendation Engine

This is the interesting/hard part. Goal: surface creators the user would like, not slop.

**The user's stated principle:** "Help me discover creators that fit my high standards, and not slop."

**Design constraints (from the user's priorities):**
- All inference happens locally. No data sent to external services.
- Models must be small enough to run on a MacBook Air without thermal events.
- The user controls the signals. No black-box scoring.

**Suggested approach (open to alternatives):**

1. **Signal collection** (V3.a provides this):
   - Watch time as fraction of duration
   - Explicit thumbs-up / thumbs-down keybinds (`+` / `-`)
   - "Never recommend channel" / "always show channel"
   - Skip behavior (videos clicked then closed within N seconds)

2. **Feature extraction:**
   - Channel-level features (avg video length, upload cadence, subscriber count via yt-dlp)
   - Content features from titles/descriptions (TF-IDF or sentence embeddings via a small local model like `all-MiniLM-L6-v2` running on CPU)
   - Optional: video transcripts via yt-dlp's `--write-auto-subs`, embedded similarly

3. **Scoring:**
   - Start dumb: weighted sum of "channels you've liked + topics you've watched"
   - Don't reach for collaborative filtering; the user is N=1
   - Avoid feedback loops (don't only show what you've already shown)

4. **Discovery:**
   - "Related channels" via yt-dlp's channel page scraping
   - User-curated seed lists (config: `seed_channels = [...]`)
   - Periodic exploration: 10% of feed slots are "channels you haven't seen" matched on content features

5. **UI:**
   - Feed view shows score next to each item (transparent, debuggable)
   - User can ask "why is this here" and see the contributing signals
   - All scoring weights are tweakable in config

**This is a research project.** Ship V3.a first. Use it for a month. Then start V3.b based on what signals you actually have.

### V3.0 Acceptance Criteria

- V3.a working: subscription feed renders, refreshes, marks read state, integrates with existing RSS source
- V3.b is explicitly out of scope for first V3 release. Do V3.a, ship it, gather data, design V3.b with real usage informing it.

---

## Cross-Cutting Concerns

### Privacy

- No telemetry. None. Not crash reports, not feature usage, not "anonymous" analytics.
- yt-dlp talks to YouTube directly from the user's IP. This is acceptable for the user's threat model (avoid logged-in tracking, not avoid being seen).
- All persistence is local SQLite. No cloud sync. No optional cloud sync. No "we'll add cloud sync if you want it." None.
- If V3 fetches metadata for recommendations, document exactly what is fetched and from where.

### Performance

- Cold start to first usable UI under 200ms
- Search response under 5s on broadband for 20 results
- Thumbnail render under 200ms after selection (cached) or 1s (uncached)
- Memory under 100MB resident with 20 thumbnails loaded
- Single binary under 20MB stripped

### Testing

- Unit tests for parsing, scoring, config loading
- Integration tests that mock yt-dlp output (capture real outputs as fixtures)
- Snapshot tests for TUI rendering using `insta`
- Manual test plan in `TESTING.md` for things that can't be automated (real network, real terminal rendering)

### Distribution

- Homebrew tap (eventually)
- Cargo install from git (V1)
- Single binary releases via GitHub Actions (V2)

---

## Implementation Order

1. V1.0 minimal: search, list, play. Single binary, zero config. ~500 LOC.
2. V1.x hardening: defensive programming pass. Tests. Error handling. Ship as 1.0.
3. V2.0 thumbnails + history. Config file lands here.
4. V3.a subscriptions. RSS/OPML ingestion + feed view.
5. Use V3.a for a month. Collect data on what signals matter.
6. V3.b recommendation engine. Iterate.

Do not skip ahead. The whole point of the V1 → V3 split is that each phase teaches you what the next phase needs.

---

## Open Questions for the Implementer

1. Is `yt-dlp --print-json` flat enough, or do we need `--dump-json` (per-video, slower)?
2. Does Ratatui have a clean way to embed Kitty graphics, or do we need to bypass it for thumbnails?
3. Mpv IPC for resume-position: use `--input-ipc-server` or rely on `--save-position-on-quit` watching the same file? Latter is simpler.
4. Should V3.a use the user's existing RSSHub or speak directly to YouTube's RSS feeds (`https://www.youtube.com/feeds/videos.xml?channel_id=...`)? Direct is simpler, no extra service dependency.

The implementer should resolve these before starting each phase, not all up-front.

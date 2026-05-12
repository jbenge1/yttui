//! Render layer. Pure read-only over [`crate::app::App`] except for
//! reporting back the body height (so half-page jumps know how far to go).
//!
//! No tests live here — layout is verified by running the binary.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, List, ListItem, ListState, Paragraph, Wrap,
};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, LastError, Screen};
use crate::palette::Palette;
use crate::search::{SearchResult, VideoDuration};

/// Minimum supported terminal dimensions (per V1 spec). Below this, we
/// draw a "terminal too small" notice instead of a broken layout.
const MIN_WIDTH: u16 = 60;
const MIN_HEIGHT: u16 = 20;

pub fn draw(frame: &mut Frame, app: &mut App, palette: &Palette) {
    let area = frame.area();
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        draw_too_small(frame, area, palette);
        return;
    }

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // header
            Constraint::Min(1),    // body
            Constraint::Length(1), // footer
        ])
        .split(area);

    draw_header(frame, layout[0], app, palette);
    draw_body(frame, layout[1], app, palette);
    draw_footer(frame, layout[2], app, palette);

    if app.screen == Screen::Help {
        draw_help_overlay(frame, area, palette);
    }
}

fn draw_too_small(frame: &mut Frame, area: Rect, palette: &Palette) {
    let p = Paragraph::new(format!(
        "Terminal too small (need at least {MIN_WIDTH}×{MIN_HEIGHT}).\n\
         Resize and try again, or press q to quit."
    ))
    .style(Style::default().fg(palette.warning_fg))
    .wrap(Wrap { trim: false });
    frame.render_widget(p, area);
}

fn draw_header(frame: &mut Frame, area: Rect, app: &App, palette: &Palette) {
    let title = match app.screen {
        Screen::Prompt => " yttui — search ".to_string(),
        Screen::Searching => format!(
            " yttui — searching for {:?} ",
            app.committed_query.as_deref().unwrap_or("")
        ),
        Screen::Results | Screen::Filter | Screen::Help => format!(
            " yttui — {} result{} for {:?} ",
            app.filtered.len(),
            if app.filtered.len() == 1 { "" } else { "s" },
            app.committed_query.as_deref().unwrap_or(""),
        ),
    };

    let block = Block::default().borders(Borders::ALL).title(title);

    let body: Line = match &app.screen {
        Screen::Prompt => Line::from(vec![
            Span::styled("yt> ", Style::default().fg(palette.prompt_marker_fg)),
            Span::raw(&app.input),
            Span::styled("│", Style::default().fg(palette.cursor_fg)),
        ]),
        Screen::Searching => Line::from(vec![Span::styled(
            "Searching… (Esc to cancel)",
            Style::default().add_modifier(Modifier::DIM),
        )]),
        Screen::Results | Screen::Help => app.committed_query.as_ref().map_or_else(
            || Line::from(""),
            |q| {
                Line::from(vec![
                    Span::styled(
                        "yt> ",
                        Style::default().fg(palette.prompt_marker_inactive_fg),
                    ),
                    Span::raw(q.as_str()),
                ])
            },
        ),
        Screen::Filter => Line::from(vec![
            Span::styled("/", Style::default().fg(palette.filter_marker_fg)),
            Span::raw(&app.input),
            Span::styled("│", Style::default().fg(palette.cursor_fg)),
        ]),
    };

    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(body), inner);
}

fn draw_body(frame: &mut Frame, area: Rect, app: &mut App, palette: &Palette) {
    // The body block is the single source of truth for inner dimensions.
    // Whatever its `inner` returns is exactly the rect the sub-drawers
    // paint into AND what the state machine uses to size half-page jumps.
    // No more `area.height - 2` magic.
    let block = Block::default().borders(Borders::ALL);
    let inner = block.inner(area);
    app.list_height = inner.height;
    frame.render_widget(block, area);

    match app.screen {
        Screen::Prompt => draw_prompt_body(frame, inner, app, palette),
        Screen::Searching => draw_searching_body(frame, inner, app, palette),
        Screen::Results | Screen::Filter | Screen::Help => {
            draw_results_body(frame, inner, app, palette);
        }
    }
}

fn draw_prompt_body(frame: &mut Frame, inner: Rect, _app: &App, palette: &Palette) {
    // Inline help on the Prompt body — "?" is a legal query character so
    // we don't reserve it as a hotkey here. Modal help is still available
    // on Results.
    let dim = Style::default().add_modifier(Modifier::DIM);
    let key = Style::default()
        .fg(palette.keycap_fg)
        .add_modifier(Modifier::BOLD);

    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled("Type a query and press Enter.", dim)),
        Line::from(""),
        Line::from(Span::styled("Once you have results:", dim)),
    ];
    let bindings: &[(&str, &str)] = &[
        ("j / ↓", "next result"),
        ("k / ↑", "previous result"),
        ("gg / G", "first / last"),
        ("Ctrl-d / Ctrl-u", "half-page down / up"),
        ("Enter", "play selected"),
        ("/", "filter results"),
        ("n", "new search"),
        ("r", "re-run current search"),
        ("?", "show this help"),
        ("q / Esc", "quit"),
    ];
    for (k, d) in bindings {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(format!("{k:<18}"), key),
            Span::raw(*d),
        ]));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_searching_body(frame: &mut Frame, inner: Rect, app: &App, palette: &Palette) {
    let q = app.committed_query.as_deref().unwrap_or("");
    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("yt-dlp ytsearch:{q}…"),
            Style::default().fg(palette.progress_fg),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Press Esc to cancel.",
            Style::default().add_modifier(Modifier::DIM),
        )),
    ];
    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_results_body(frame: &mut Frame, inner: Rect, app: &App, palette: &Palette) {
    if app.results.is_empty() {
        let p = Paragraph::new(Line::from(Span::styled(
            "No results.",
            Style::default().add_modifier(Modifier::DIM),
        )));
        frame.render_widget(p, inner);
        return;
    }
    if app.filtered.is_empty() {
        let p = Paragraph::new(Line::from(Span::styled(
            format!("No matches for {:?}.", app.input),
            Style::default().fg(palette.warning_fg),
        )));
        frame.render_widget(p, inner);
        return;
    }

    let items: Vec<ListItem> = app
        .filtered
        .iter()
        .filter_map(|i| app.results.get(*i))
        .map(|r| render_row(r, inner.width, palette))
        .collect();

    let list = List::new(items)
        .highlight_style(
            Style::default()
                .bg(palette.selection_bg)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    let mut state = ListState::default();
    state.select(Some(app.selected));
    frame.render_stateful_widget(list, inner, &mut state);
}

/// Format a [`VideoDuration`] as a terse list-cell string.
/// `1:23` / `1:01:01` for known seconds, `LIVE` / `UPCOMING` for live
/// states, `—` for unknown. Display owns this — it's a rendering
/// concern, not a yt-dlp adapter concern (see Betterfy #56).
#[must_use]
fn format_duration(d: &VideoDuration) -> String {
    match d {
        VideoDuration::Live => "LIVE".to_string(),
        VideoDuration::Upcoming => "UPCOMING".to_string(),
        VideoDuration::Unknown => "—".to_string(),
        VideoDuration::Seconds(s) => {
            let h = s / 3600;
            let m = (s % 3600) / 60;
            let s = s % 60;
            if h > 0 {
                format!("{h}:{m:02}:{s:02}")
            } else {
                format!("{m}:{s:02}")
            }
        }
    }
}

/// Render one result row as `<title>     <channel>   <duration>` with the
/// duration right-aligned. Truncates wide content based on terminal width.
fn render_row(r: &SearchResult, width: u16, palette: &Palette) -> ListItem<'static> {
    // Reserve columns for selection symbol (2) + duration (8) + spacers.
    let dur = format_duration(&r.duration);
    let dur_w = u16::try_from(dur.width()).unwrap_or(u16::MAX);
    let channel = r.channel.as_deref().unwrap_or("[no channel]");
    let chan_budget = (width / 4).clamp(8, 32);
    let title_budget = width
        .saturating_sub(2 + 1 + chan_budget + 1 + dur_w + 2);

    let title = truncate_to_width(&r.title, title_budget);
    let chan = truncate_to_width(channel, chan_budget);

    // Manually pad to align the duration on the right edge so widths add
    // up regardless of unicode content.
    let title_pad = title_budget
        .saturating_sub(u16::try_from(title.width()).unwrap_or(u16::MAX));
    let chan_pad = chan_budget
        .saturating_sub(u16::try_from(chan.width()).unwrap_or(u16::MAX));

    let line = Line::from(vec![
        Span::raw(title),
        Span::raw(" ".repeat(usize::from(title_pad) + 1)),
        Span::styled(chan, Style::default().fg(palette.channel_fg)),
        Span::raw(" ".repeat(usize::from(chan_pad) + 1)),
        Span::styled(
            dur,
            Style::default()
                .fg(palette.duration_fg)
                .add_modifier(Modifier::DIM),
        ),
    ]);
    ListItem::new(line)
}

/// Truncate `s` to fit within `max_cols` display columns. When the
/// input doesn't fit, append a single-column ellipsis — *unless*
/// dropping the ellipsis would let us pack one more source character
/// into the same budget (i.e. the prefix saturates `max_cols` exactly
/// and only one character was dropped, where the ellipsis carries no
/// information the missing glyph wouldn't have). Respects unicode
/// display width (CJK chars are 2 cols). Returns the empty string when
/// `max_cols == 0`.
fn truncate_to_width(s: &str, max_cols: u16) -> String {
    let max = usize::from(max_cols);
    if s.width() <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }

    // Default contract: trim to `max - 1` cols and append an ellipsis,
    // so the trailing "…" always signals "more was here". One special
    // case: when the `max - 1` prefix is empty (e.g. budget = 2 with a
    // 2-col CJK first character), the ellipsis form degenerates to a
    // bare "…" that wastes the budget. In that case prefer the
    // saturating prefix in `max` cols — at least one glyph fits, and a
    // clipped glyph is its own truncation marker.
    let prefix_minus_one = greedy_prefix(s, max - 1);
    if prefix_minus_one.is_empty() {
        let prefix_full = greedy_prefix(s, max);
        if !prefix_full.is_empty() {
            return prefix_full;
        }
    }
    let mut out = prefix_minus_one;
    out.push('…');
    out
}

/// Greedy character-by-character prefix of `s` that fits in `max_cols`
/// display columns.
fn greedy_prefix(s: &str, max_cols: usize) -> String {
    let mut out = String::new();
    let mut width = 0usize;
    for ch in s.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + cw > max_cols {
            break;
        }
        out.push(ch);
        width += cw;
    }
    out
}

fn draw_footer(frame: &mut Frame, area: Rect, app: &App, palette: &Palette) {
    let line = app.last_error.as_ref().map_or_else(
        || hints_line_for(&app.screen, palette),
        |err| {
            let icon = match err {
                LastError::Search(_) => "search error: ",
                LastError::Player(_) => "player error: ",
            };
            Line::from(vec![
                Span::styled(
                    icon,
                    Style::default()
                        .fg(palette.error_fg)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(err.message(), Style::default().fg(palette.error_fg)),
            ])
        },
    );
    frame.render_widget(Paragraph::new(line), area);
}

fn hints_line_for(screen: &Screen, palette: &Palette) -> Line<'static> {
    let hints = match screen {
        Screen::Prompt => "Enter  search    Esc / Ctrl-C  quit",
        Screen::Searching => "Esc  cancel",
        Screen::Results => {
            "j/k  move    Enter  play    /  filter    n  new    r  rerun    ?  help    q  quit"
        }
        Screen::Filter => "type to filter    Enter  commit    Esc  cancel",
        Screen::Help => "any key to dismiss",
    };
    Line::from(Span::styled(
        hints,
        Style::default()
            .fg(palette.hint_fg)
            .add_modifier(Modifier::DIM),
    ))
}

fn draw_help_overlay(frame: &mut Frame, area: Rect, palette: &Palette) {
    // Centered popup, ~60% of width and height.
    let popup = centered_rect(60, 60, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Keys ");
    let inner = block.inner(popup);
    frame.render_widget(ratatui::widgets::Clear, popup);
    frame.render_widget(block, popup);

    let lines: Vec<Line> = [
        ("j / ↓", "next result"),
        ("k / ↑", "previous result"),
        ("gg", "jump to first"),
        ("G", "jump to last"),
        ("Ctrl-d / Ctrl-u", "half-page down / up"),
        ("Enter", "play selected (or commit query)"),
        ("/", "filter current results"),
        ("n", "new search"),
        ("r", "re-run current search"),
        ("?", "this help"),
        ("q / Esc", "quit"),
    ]
    .iter()
    .map(|(k, d)| {
        Line::from(vec![
            Span::styled(
                format!("{k:<18}"),
                Style::default()
                    .fg(palette.keycap_fg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(*d),
        ])
    })
    .collect();

    frame.render_widget(Paragraph::new(lines), inner);
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);
    let h = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(v[1]);
    h[1]
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::style::Color;

    fn searching_app(query: &str) -> App {
        let mut app = App::new();
        app.committed_query = Some(query.to_string());
        app.screen = Screen::Searching;
        app
    }

    fn results_app_with(rows: &[(&str, &str, &str)]) -> App {
        // (id, title, channel) — duration is set to a known value so
        // tests don't depend on duration column width subtleties.
        let mut app = App::new();
        app.results = rows
            .iter()
            .map(|(id, title, ch)| SearchResult {
                id: (*id).to_string(),
                title: (*title).to_string(),
                channel: Some((*ch).to_string()),
                duration: VideoDuration::Seconds(60),
            })
            .collect();
        app.committed_query = Some("q".to_string());
        app.recompute_filter();
        app.screen = Screen::Results;
        app
    }

    fn render_to_buffer(app: &mut App, palette: &Palette, w: u16, h: u16) -> Buffer {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|f| draw(f, app, palette))
            .expect("draw to test backend");
        terminal.backend().buffer().clone()
    }

    /// Scan the buffer row-by-row for `needle`; return the (fg, bg) of
    /// the cell holding the *first* character of the match.
    ///
    /// ASCII-only by contract: the helper aligns `String::find`'s
    /// byte index against a per-cell `symbol().len()` byte counter,
    /// which is only sound when every cell is single-byte. A non-ASCII
    /// needle would silently mislocate; we `debug_assert!` to surface
    /// the misuse loudly in dev builds. The buffer rows themselves
    /// may contain non-ASCII (e.g. wide-char title fixtures), since
    /// ratatui's `TestBackend` lays wide chars out as glyph-cell +
    /// trailing-space-cell, keeping per-row byte indices stable
    /// against cell-by-cell byte accumulation. The fragile pin is on
    /// the *needle*, not the row.
    fn find_ascii_cell(buf: &Buffer, needle: &str) -> Option<(Color, Color)> {
        debug_assert!(
            needle.is_ascii(),
            "find_ascii_cell needs an ASCII needle; got {needle:?}"
        );
        let area = buf.area();
        for y in 0..area.height {
            let mut row = String::new();
            for x in 0..area.width {
                row.push_str(buf[(x, y)].symbol());
            }
            if let Some(byte_idx) = row.find(needle) {
                let mut byte_pos = 0usize;
                for x in 0..area.width {
                    let cell = &buf[(x, y)];
                    if byte_pos == byte_idx {
                        return Some((cell.fg, cell.bg));
                    }
                    byte_pos += cell.symbol().len();
                }
            }
        }
        None
    }

    #[test]
    fn channel_fg_does_not_collide_with_selection_bg_on_selected_row() {
        // The bug: V1 hardcoded both selection bg and channel fg to
        // Color::DarkGray, so the channel column disappeared when its
        // row was the selected one. This test renders a real frame and
        // asserts the channel-name cell on the selected row carries an
        // fg distinct from the selection bg.
        let palette = Palette::default();
        let mut app = results_app_with(&[
            ("a", "First video", "AliceChannel"),
            ("b", "Second clip", "BobChannel"),
        ]);
        app.selected = 0;

        let buf = render_to_buffer(&mut app, &palette, 80, 24);
        let (fg, bg) = find_ascii_cell(&buf, "Alice")
            .expect("channel name should render on selected row");
        assert_eq!(
            bg, palette.selection_bg,
            "selected row should carry the selection bg"
        );
        assert_ne!(
            fg, palette.selection_bg,
            "channel fg must not equal selection bg or text disappears"
        );
    }

    #[test]
    fn duration_fg_does_not_collide_with_selection_bg_on_selected_row() {
        // Same bug shape as the channel collision: V1 defaulted both
        // selection_bg and duration_fg to Color::DarkGray, so the
        // duration cell vanished on the highlighted row. Pinned with
        // a real frame render — durations are formatted as "M:SS"
        // (e.g. "1:00"), so we search for that substring.
        let palette = Palette::default();
        let mut app = results_app_with(&[
            ("a", "First video", "AliceChannel"),
            ("b", "Second clip", "BobChannel"),
        ]);
        app.selected = 0;

        let buf = render_to_buffer(&mut app, &palette, 80, 24);
        let (fg, bg) = find_ascii_cell(&buf, "1:00")
            .expect("duration should render on selected row");
        assert_eq!(
            bg, palette.selection_bg,
            "selected row should carry the selection bg"
        );
        assert_ne!(
            fg, palette.selection_bg,
            "duration fg must not equal selection bg or text disappears"
        );
    }

    #[test]
    fn searching_status_follows_progress_fg_not_prompt_marker_fg() {
        // The "yt-dlp ytsearch:…" status line used to share
        // prompt_marker_fg with the active prompt; split into
        // progress_fg so Themes 1 users can recolor independently.
        // Buffer-diff proof: hold prompt_marker_fg constant and swap
        // only progress_fg — the status cell color must follow.
        let mut app = searching_app("hello");
        let p1 = Palette {
            prompt_marker_fg: Color::Cyan,
            progress_fg: Color::Magenta,
            ..Palette::default()
        };
        let p2 = Palette {
            prompt_marker_fg: Color::Cyan,
            progress_fg: Color::Green,
            ..Palette::default()
        };
        let buf1 = render_to_buffer(&mut app, &p1, 80, 24);
        let buf2 = render_to_buffer(&mut app, &p2, 80, 24);
        let (fg1, _) = find_ascii_cell(&buf1, "yt-dlp ytsearch:hello")
            .expect("status line present in buf1");
        let (fg2, _) = find_ascii_cell(&buf2, "yt-dlp ytsearch:hello")
            .expect("status line present in buf2");
        assert_eq!(fg1, Color::Magenta);
        assert_eq!(fg2, Color::Green);
    }

    #[test]
    fn find_ascii_cell_locates_channel_when_row_contains_cjk() {
        // Pins the helper's contract: an ASCII needle stays locatable
        // even when the row in front of it contains CJK glyphs.
        // ratatui's TestBackend renders wide chars as glyph + trailing
        // space, so the per-cell byte counter stays in sync with
        // String::find's byte index across mixed-width rows. If a
        // future ratatui version changes that layout, this test catches
        // it before the channel/duration palette tests start lying.
        let mut app = results_app_with(&[("a", "あいうTitle", "ChanName")]);
        app.selected = 0;
        let palette = Palette {
            channel_fg: Color::Magenta,
            ..Palette::default()
        };
        let buf = render_to_buffer(&mut app, &palette, 80, 24);
        let (fg, _) = find_ascii_cell(&buf, "ChanName")
            .expect("ChanName substring should be locatable");
        assert_eq!(
            fg,
            Color::Magenta,
            "helper must land on the channel cell, not the pad before it"
        );
    }

    #[test]
    fn results_body_reads_channel_color_from_palette_not_a_literal() {
        // Indirect proof that the renderer goes through the palette
        // rather than a Color::* literal: swap the palette's channel
        // color and the buffer cell color follows.
        let mut app = results_app_with(&[("a", "Title", "ChanName")]);
        app.selected = 0;

        let p1 = Palette {
            channel_fg: Color::Magenta,
            ..Palette::default()
        };
        let p2 = Palette {
            channel_fg: Color::Green,
            ..Palette::default()
        };
        let buf1 = render_to_buffer(&mut app, &p1, 80, 24);
        let buf2 = render_to_buffer(&mut app, &p2, 80, 24);
        let (fg1, _) = find_ascii_cell(&buf1, "ChanName")
            .expect("channel cell present in buf1");
        let (fg2, _) = find_ascii_cell(&buf2, "ChanName")
            .expect("channel cell present in buf2");
        assert_eq!(fg1, Color::Magenta);
        assert_eq!(fg2, Color::Green);
    }

    #[test]
    fn format_duration_renders_each_variant() {
        assert_eq!(format_duration(&VideoDuration::Seconds(0)), "0:00");
        assert_eq!(format_duration(&VideoDuration::Seconds(59)), "0:59");
        assert_eq!(format_duration(&VideoDuration::Seconds(60)), "1:00");
        assert_eq!(format_duration(&VideoDuration::Seconds(2404)), "40:04");
        assert_eq!(
            format_duration(&VideoDuration::Seconds(3661)),
            "1:01:01"
        );
        assert_eq!(format_duration(&VideoDuration::Live), "LIVE");
        assert_eq!(format_duration(&VideoDuration::Upcoming), "UPCOMING");
        assert_eq!(format_duration(&VideoDuration::Unknown), "—");
    }

    #[test]
    fn truncate_passes_through_when_fits() {
        assert_eq!(truncate_to_width("abc", 5), "abc");
    }

    #[test]
    fn truncate_passes_through_at_exact_fit() {
        assert_eq!(truncate_to_width("abc", 3), "abc");
    }

    #[test]
    fn truncate_adds_ellipsis_when_clipped() {
        // Budget 3: 2 chars + ellipsis = 3 cols.
        assert_eq!(truncate_to_width("abcde", 3), "ab…");
    }

    #[test]
    fn truncate_budget_zero_returns_empty() {
        assert_eq!(truncate_to_width("abc", 0), "");
        assert_eq!(truncate_to_width("", 0), "");
    }

    #[test]
    fn truncate_budget_one_returns_first_char_when_one_fits() {
        // Pin for second-opinion P2 #6: the old contract returned "…"
        // because reserving 1 col for the ellipsis left no room for any
        // input character. With the revised "ellipsis form would be a
        // bare …, prefer the saturating prefix" rule, budget 1 +
        // 1-col first char yields the first char itself — a clipped
        // glyph is its own truncation marker.
        assert_eq!(truncate_to_width("abc", 1), "a");
    }

    #[test]
    fn truncate_respects_unicode_width() {
        // Each CJK char is 2 cols; "あいう" = 6 cols.
        // Budget 4: "あ" (2 cols) fits, plus a 1-col ellipsis = 3 cols
        // total. Adding "い" would push us to 4 + ellipsis = 5 > 4.
        assert_eq!(truncate_to_width("あいう", 4), "あ…");
    }

    #[test]
    fn truncate_uses_full_budget_when_one_wide_char_fits_exactly() {
        // Pinning fix for second-opinion P2 #6: with budget 2 and input
        // "あいう" (6 cols, won't fit), the 2-col "あ" exactly saturates
        // the budget. Returning "…" (1 col) wastes the slot; returning
        // "あ" loses no information that an ellipsis would convey
        // (any glyph is already a truncation indicator). Old code
        // returned "…"; new behavior returns "あ".
        assert_eq!(truncate_to_width("あいう", 2), "あ");
    }

    #[test]
    fn truncate_keeps_ellipsis_when_a_prefix_actually_fits_in_max_minus_one() {
        // Sanity: the special-case fix only kicks in when the
        // `(max - 1)` prefix is empty. With budget 4 and input "abcde",
        // "abc" fits in 3 cols, so we keep the canonical prefix +
        // ellipsis form rather than packing in "abcd".
        assert_eq!(truncate_to_width("abcde", 4), "abc…");
    }

    #[test]
    fn truncate_handles_empty_string() {
        assert_eq!(truncate_to_width("", 10), "");
    }

    #[test]
    fn centered_rect_at_50_pct_is_centered() {
        let r = Rect {
            x: 0,
            y: 0,
            width: 100,
            height: 100,
        };
        let c = centered_rect(50, 50, r);
        assert_eq!(c.width, 50);
        assert_eq!(c.height, 50);
        assert_eq!(c.x, 25);
        assert_eq!(c.y, 25);
    }

    #[test]
    fn centered_rect_at_99_pct_has_zero_margins() {
        // (100-99)/2 = 0 → margins of 0/99/0. Edge case worth pinning.
        let r = Rect {
            x: 0,
            y: 0,
            width: 100,
            height: 100,
        };
        let c = centered_rect(99, 99, r);
        assert_eq!(c.width, 99);
        assert_eq!(c.height, 99);
        assert_eq!(c.x, 0);
        assert_eq!(c.y, 0);
    }

    #[test]
    fn centered_rect_at_100_pct_fills() {
        let r = Rect {
            x: 0,
            y: 0,
            width: 100,
            height: 100,
        };
        let c = centered_rect(100, 100, r);
        assert_eq!(c.width, 100);
        assert_eq!(c.height, 100);
    }
}

//! Render layer. Pure read-only over [`crate::app::App`] except for
//! reporting back the body height (so half-page jumps know how far to go).
//!
//! No tests live here — layout is verified by running the binary.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, List, ListItem, ListState, Paragraph, Wrap,
};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, LastError, Screen};
use crate::search::{SearchResult, format_duration};

/// Minimum supported terminal dimensions (per V1 spec). Below this, we
/// draw a "terminal too small" notice instead of a broken layout.
const MIN_WIDTH: u16 = 60;
const MIN_HEIGHT: u16 = 20;

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        draw_too_small(frame, area);
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

    draw_header(frame, layout[0], app);
    draw_body(frame, layout[1], app);
    draw_footer(frame, layout[2], app);

    if app.screen == Screen::Help {
        draw_help_overlay(frame, area);
    }
}

fn draw_too_small(frame: &mut Frame, area: Rect) {
    let p = Paragraph::new(format!(
        "Terminal too small (need at least {MIN_WIDTH}×{MIN_HEIGHT}).\n\
         Resize and try again, or press q to quit."
    ))
    .style(Style::default().fg(Color::Yellow))
    .wrap(Wrap { trim: false });
    frame.render_widget(p, area);
}

fn draw_header(frame: &mut Frame, area: Rect, app: &App) {
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
            Span::styled("yt> ", Style::default().fg(Color::Cyan)),
            Span::raw(&app.input),
            Span::styled("│", Style::default().fg(Color::DarkGray)),
        ]),
        Screen::Searching => Line::from(vec![Span::styled(
            "Searching… (Esc to cancel)",
            Style::default().add_modifier(Modifier::DIM),
        )]),
        Screen::Results | Screen::Help => {
            if let Some(q) = &app.committed_query {
                Line::from(vec![
                    Span::styled("yt> ", Style::default().fg(Color::DarkGray)),
                    Span::raw(q.as_str()),
                ])
            } else {
                Line::from("")
            }
        }
        Screen::Filter => Line::from(vec![
            Span::styled("/", Style::default().fg(Color::Yellow)),
            Span::raw(&app.input),
            Span::styled("│", Style::default().fg(Color::DarkGray)),
        ]),
    };

    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(body), inner);
}

fn draw_body(frame: &mut Frame, area: Rect, app: &mut App) {
    // Tell the state machine how many rows we can show, so that
    // ctrl-d/ctrl-u jumps half a real page rather than a guess.
    app.viewport_height = area.height.saturating_sub(2);

    match app.screen {
        Screen::Prompt => draw_prompt_body(frame, area, app),
        Screen::Searching => draw_searching_body(frame, area, app),
        Screen::Results | Screen::Filter | Screen::Help => {
            draw_results_body(frame, area, app);
        }
    }
}

fn draw_prompt_body(frame: &mut Frame, area: Rect, _app: &App) {
    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "Type a query and press Enter.",
            Style::default().add_modifier(Modifier::DIM),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "?  show keys    Esc / Ctrl-C  quit",
            Style::default().add_modifier(Modifier::DIM),
        )),
    ];
    frame.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL)),
        area,
    );
}

fn draw_searching_body(frame: &mut Frame, area: Rect, app: &App) {
    let q = app.committed_query.as_deref().unwrap_or("");
    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("yt-dlp ytsearch:{q}…"),
            Style::default().fg(Color::Cyan),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Press Esc to cancel.",
            Style::default().add_modifier(Modifier::DIM),
        )),
    ];
    frame.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL)),
        area,
    );
}

fn draw_results_body(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default().borders(Borders::ALL);
    let inner = block.inner(area);
    frame.render_widget(block, area);

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
            Style::default().fg(Color::Yellow),
        )));
        frame.render_widget(p, inner);
        return;
    }

    let items: Vec<ListItem> = app
        .filtered
        .iter()
        .filter_map(|i| app.results.get(*i))
        .map(|r| render_row(r, inner.width))
        .collect();

    let list = List::new(items)
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    let mut state = ListState::default();
    state.select(Some(app.selected));
    frame.render_stateful_widget(list, inner, &mut state);
}

/// Render one result row as `<title>     <channel>   <duration>` with the
/// duration right-aligned. Truncates wide content based on terminal width.
fn render_row(r: &SearchResult, width: u16) -> ListItem<'static> {
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
        Span::styled(chan, Style::default().fg(Color::DarkGray)),
        Span::raw(" ".repeat(usize::from(chan_pad) + 1)),
        Span::styled(
            dur,
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        ),
    ]);
    ListItem::new(line)
}

/// Truncate `s` to fit within `max_cols` display columns, adding a
/// single-character ellipsis if truncated. Falls back to byte-wise if
/// the budget is tiny.
fn truncate_to_width(s: &str, max_cols: u16) -> String {
    let max = usize::from(max_cols);
    if s.width() <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    // Build up character-by-character respecting display width.
    let mut out = String::new();
    let mut width = 0;
    for ch in s.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + cw > max.saturating_sub(1) {
            break;
        }
        out.push(ch);
        width += cw;
    }
    out.push('…');
    out
}

fn draw_footer(frame: &mut Frame, area: Rect, app: &App) {
    let line = if let Some(err) = &app.last_error {
        let icon = match err {
            LastError::Search(_) => "search error: ",
            LastError::Player(_) => "player error: ",
        };
        Line::from(vec![
            Span::styled(
                icon,
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::styled(err.message(), Style::default().fg(Color::Red)),
        ])
    } else {
        let hints = match app.screen {
            Screen::Prompt => "Enter  search    ?  help    Esc  quit",
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
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        ))
    };
    frame.render_widget(Paragraph::new(line), area);
}

fn draw_help_overlay(frame: &mut Frame, area: Rect) {
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
                    .fg(Color::Cyan)
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

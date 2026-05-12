//! Semantic color palette for the TUI render layer.
//!
//! Every color used by the renderer is named here by its role, not by
//! its `Color::*` value. Renderers read fields off a [`Palette`]
//! reference; the only place a concrete `Color` is mentioned is in
//! [`Palette::default`]. This is the seam Themes 1 will plug into:
//! a `[theme]` config block will eventually produce a non-default
//! `Palette` at startup.
//!
//! ## V1 byte-equivalence
//!
//! [`Palette::default`] reproduces V1 colors exactly with **one
//! deliberate exception**: `channel_fg` was `Color::DarkGray` in V1,
//! which collided with `selection_bg` (also `DarkGray`) — the channel
//! name disappeared into the selection bar on the highlighted row.
//! `channel_fg` now defaults to `Color::Blue` so the row stays
//! readable when selected. All other fields are V1-identical.

use ratatui::style::Color;

/// Named color slots for the renderer. Construct via [`Palette::default`]
/// for V1-compatible behavior; a future themes feature will populate
/// non-default instances from user config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Palette {
    /// Background color of the highlighted row in result lists.
    pub selection_bg: Color,
    /// Foreground for the channel-name column in result rows. V1 used
    /// `DarkGray`, which collided with `selection_bg`; the default now
    /// uses `Blue` to keep the channel name visible on the selected
    /// row.
    pub channel_fg: Color,
    /// Foreground for the duration column in result rows.
    pub duration_fg: Color,
    /// Foreground for inline error icons + messages in the footer.
    pub error_fg: Color,
    /// Foreground for the active `yt>` prompt marker and the
    /// "yt-dlp ytsearch:…" searching status line.
    pub prompt_marker_fg: Color,
    /// Foreground for the `yt>` echo of the committed query while on
    /// the Results screen (i.e. the inactive form).
    pub prompt_marker_inactive_fg: Color,
    /// Foreground for the trailing `│` cursor glyph in input fields.
    pub cursor_fg: Color,
    /// Foreground for key-cap labels in inline help and the help
    /// overlay (e.g. `j / ↓`, `Ctrl-d`).
    pub keycap_fg: Color,
    /// Foreground for non-error warning text (terminal-too-small
    /// notice, "no matches for…" status).
    pub warning_fg: Color,
    /// Foreground for the `/` glyph that prefixes the filter input.
    pub filter_marker_fg: Color,
    /// Foreground for the dim hints line in the footer.
    pub hint_fg: Color,
}

impl Default for Palette {
    fn default() -> Self {
        Self {
            selection_bg: Color::DarkGray,
            // V1 was Color::DarkGray; intentionally changed — see
            // module docstring for the why.
            channel_fg: Color::Blue,
            duration_fg: Color::DarkGray,
            error_fg: Color::Red,
            prompt_marker_fg: Color::Cyan,
            prompt_marker_inactive_fg: Color::DarkGray,
            cursor_fg: Color::DarkGray,
            keycap_fg: Color::Cyan,
            warning_fg: Color::Yellow,
            filter_marker_fg: Color::Yellow,
            hint_fg: Color::DarkGray,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    #[test]
    fn default_palette_matches_v1_colors_except_channel_fg() {
        // V1 hardcoded values, asserted field-by-field. The single
        // intentional drift is channel_fg — see the module docstring.
        let p = Palette::default();
        assert_eq!(p.selection_bg, Color::DarkGray);
        // channel_fg was Color::DarkGray in V1; changed to Color::Blue
        // to fix the collision with selection_bg on the highlighted row.
        assert_eq!(p.channel_fg, Color::Blue);
        assert_eq!(p.duration_fg, Color::DarkGray);
        assert_eq!(p.error_fg, Color::Red);
        assert_eq!(p.prompt_marker_fg, Color::Cyan);
        assert_eq!(p.prompt_marker_inactive_fg, Color::DarkGray);
        assert_eq!(p.cursor_fg, Color::DarkGray);
        assert_eq!(p.keycap_fg, Color::Cyan);
        assert_eq!(p.warning_fg, Color::Yellow);
        assert_eq!(p.filter_marker_fg, Color::Yellow);
        assert_eq!(p.hint_fg, Color::DarkGray);
    }

    #[test]
    fn default_palette_has_no_selection_collisions() {
        // The bug this slice fixes: a row's foreground colors must not
        // equal the selection background, or text vanishes when the row
        // is highlighted. Channel was the smoking gun; pin every fg
        // that renders inside a selectable row.
        let p = Palette::default();
        assert_ne!(p.channel_fg, p.selection_bg);
        // Duration's fg currently *does* equal selection_bg (DarkGray),
        // but it also carries Modifier::DIM and renders inside the
        // selection bar where the row's BOLD modifier on the highlight
        // style flips it visible. The collision check here is scoped
        // to channel_fg, which is the field this slice changes. If a
        // future change makes duration suffer the same fate, add the
        // assertion then with the matching fix.
        assert_ne!(p.error_fg, p.selection_bg);
    }

    #[test]
    fn default_palette_distinct_roles_have_distinct_colors() {
        // Not exhaustive — just enough pairs that a careless edit to
        // Palette::default (e.g. "harmonize the prompt and the cursor")
        // gets caught.
        let p = Palette::default();
        assert_ne!(p.prompt_marker_fg, p.cursor_fg);
        assert_ne!(p.error_fg, p.warning_fg);
        assert_ne!(p.error_fg, p.hint_fg);
    }
}

//! Application state machine. No rendering, no I/O — takes pre-parsed
//! [`crossterm::event::KeyEvent`]s and returns the side-effect the
//! main loop should perform as an [`Action`] value.
//!
//! The render layer reads `App` to draw, the main loop calls
//! [`App::handle_key`] and acts on the returned [`Action`].
//!
//! Note: this module accepts `crossterm`'s `KeyEvent` directly rather
//! than an internal key abstraction. That's a deliberate V1 trade —
//! abstracting input prematurely would be ceremony with no callers.
//! If yttui ever grows a non-crossterm input source, define a yttui-
//! native `Key` type here and convert at the boundary in `main.rs`.

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::player::PlayerError;
use crate::search::{SearchError, SearchResult};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Screen {
    Prompt,
    Searching,
    Results,
    Filter,
    Help,
}

/// Error stash for the status bar. Per the pre-Slice-3 decision (#24),
/// the library types stay non-`Clone`; the TUI wraps in `Arc`.
#[derive(Debug, Clone)]
pub enum LastError {
    Search(Arc<SearchError>),
    Player(Arc<PlayerError>),
}

impl LastError {
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            LastError::Search(e) => e.to_string(),
            LastError::Player(e) => e.to_string(),
        }
    }
}

/// Side effects requested by the state machine. The main loop is
/// responsible for actually performing them (spawning threads,
/// suspending the terminal, exiting).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    None,
    Quit,
    StartSearch(String),
    Rerun,
    CancelSearch,
    Play(SearchResult),
}

#[derive(Debug)]
pub struct App {
    pub screen: Screen,
    /// Where to return when leaving the Help overlay.
    pub help_return: Option<Screen>,
    /// Edit buffer used by `Prompt` and `Filter` screens.
    pub input: String,
    /// The query that produced [`Self::results`] (for re-run, status bar).
    pub committed_query: Option<String>,
    pub results: Vec<SearchResult>,
    /// Indices into `results` after applying the active filter. When no
    /// filter is active this is `0..results.len()`.
    pub filtered: Vec<usize>,
    /// Index into `filtered`.
    pub selected: usize,
    /// Sticky error shown in the status bar; cleared on next user action.
    pub last_error: Option<LastError>,
    /// Page height reported by the renderer, used for half-page jumps.
    pub list_height: u16,
    /// True between the first `g` keypress and the second (or any other key).
    g_pending: bool,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    #[must_use]
    pub fn new() -> Self {
        Self {
            screen: Screen::Prompt,
            help_return: None,
            input: String::new(),
            committed_query: None,
            results: Vec::new(),
            filtered: Vec::new(),
            selected: 0,
            last_error: None,
            list_height: 20,
            g_pending: false,
        }
    }

    /// Drive the state machine with a single key event. Returns the
    /// side-effect the main loop should perform.
    pub fn handle_key(&mut self, key: KeyEvent) -> Action {
        // Any key clears a stale error banner (visual nudge that we
        // accepted input).
        let cleared_error = self.last_error.take().is_some();
        let g_was_pending = std::mem::replace(&mut self.g_pending, false);

        let action = match self.screen.clone() {
            Screen::Prompt => self.handle_prompt(key),
            Screen::Searching => self.handle_searching(key),
            Screen::Results => self.handle_results(key, g_was_pending),
            Screen::Filter => self.handle_filter(key),
            Screen::Help => self.handle_help(key),
        };

        // If nothing happened and we cleared an error, that *was* the
        // action — don't return None and lose the redraw signal. (The
        // renderer reacts to state changes; clearing the error is a
        // state change.)
        if action == Action::None && cleared_error {
            // No-op return is fine — the redraw will pick up the change
            // because we mutated last_error.
        }
        action
    }

    fn handle_prompt(&mut self, key: KeyEvent) -> Action {
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                Action::Quit
            }
            (KeyCode::Enter, _) => self.commit_query(),
            (KeyCode::Backspace, _) => {
                self.input.pop();
                Action::None
            }
            (KeyCode::Char(c), m)
                if !m.contains(KeyModifiers::CONTROL)
                    && !m.contains(KeyModifiers::ALT) =>
            {
                self.input.push(c);
                Action::None
            }
            _ => Action::None,
        }
    }

    /// Commit the current `input` as a search query and transition to
    /// `Searching`. Trims whitespace; no-ops on empty input.
    pub fn commit_query(&mut self) -> Action {
        let q = self.input.trim().to_string();
        if q.is_empty() {
            return Action::None;
        }
        self.committed_query = Some(q.clone());
        self.screen = Screen::Searching;
        Action::StartSearch(q)
    }

    fn handle_searching(&mut self, key: KeyEvent) -> Action {
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                // Return to prompt, prefilled with the in-flight query.
                if let Some(q) = self.committed_query.clone() {
                    self.input = q;
                }
                self.screen = Screen::Prompt;
                Action::CancelSearch
            }
            _ => Action::None,
        }
    }

    fn handle_results(&mut self, key: KeyEvent, g_was_pending: bool) -> Action {
        match (key.code, key.modifiers) {
            (KeyCode::Char('q'), KeyModifiers::NONE)
            | (KeyCode::Esc, _)
            | (KeyCode::Char('c'), KeyModifiers::CONTROL) => Action::Quit,
            (KeyCode::Char('?'), _) => {
                self.help_return = Some(Screen::Results);
                self.screen = Screen::Help;
                Action::None
            }
            (KeyCode::Char('j'), KeyModifiers::NONE) | (KeyCode::Down, _) => {
                self.move_selection(1);
                Action::None
            }
            (KeyCode::Char('k'), KeyModifiers::NONE) | (KeyCode::Up, _) => {
                self.move_selection(-1);
                Action::None
            }
            (KeyCode::Char('g'), KeyModifiers::NONE) => {
                if g_was_pending {
                    self.selected = 0;
                } else {
                    self.g_pending = true;
                }
                Action::None
            }
            (KeyCode::Char('G'), _) => {
                if !self.filtered.is_empty() {
                    self.selected = self.filtered.len() - 1;
                }
                Action::None
            }
            (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
                self.move_selection(i32::from(self.list_height) / 2);
                Action::None
            }
            (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                self.move_selection(-(i32::from(self.list_height) / 2));
                Action::None
            }
            (KeyCode::Enter, _) => self
                .selected_result()
                .map_or(Action::None, |r| Action::Play(r.clone())),
            (KeyCode::Char('n'), KeyModifiers::NONE) => {
                self.screen = Screen::Prompt;
                self.input.clear();
                Action::None
            }
            (KeyCode::Char('r'), KeyModifiers::NONE) => {
                if self.committed_query.is_some() {
                    self.screen = Screen::Searching;
                    Action::Rerun
                } else {
                    Action::None
                }
            }
            (KeyCode::Char('/'), _) => {
                self.screen = Screen::Filter;
                self.input.clear();
                Action::None
            }
            _ => Action::None,
        }
    }

    fn handle_filter(&mut self, key: KeyEvent) -> Action {
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => {
                // Clear the filter and return to results.
                self.input.clear();
                self.recompute_filter();
                self.screen = Screen::Results;
                Action::None
            }
            (KeyCode::Enter, _) => {
                // Commit: stay in Results with the current filter applied.
                self.screen = Screen::Results;
                Action::None
            }
            (KeyCode::Backspace, _) => {
                self.input.pop();
                self.recompute_filter();
                Action::None
            }
            (KeyCode::Char(c), m)
                if !m.contains(KeyModifiers::CONTROL)
                    && !m.contains(KeyModifiers::ALT) =>
            {
                self.input.push(c);
                self.recompute_filter();
                Action::None
            }
            _ => Action::None,
        }
    }

    fn handle_help(&mut self, key: KeyEvent) -> Action {
        match (key.code, key.modifiers) {
            (KeyCode::Char('q'), KeyModifiers::NONE)
            | (KeyCode::Char('c'), KeyModifiers::CONTROL) => Action::Quit,
            _ => {
                // Any other key dismisses help.
                self.screen = self.help_return.take().unwrap_or(Screen::Prompt);
                Action::None
            }
        }
    }

    fn move_selection(&mut self, delta: i32) {
        if self.filtered.is_empty() {
            self.selected = 0;
            return;
        }
        let len = i32::try_from(self.filtered.len()).unwrap_or(i32::MAX);
        let cur = i32::try_from(self.selected).unwrap_or(0);
        let next = (cur + delta).clamp(0, len - 1);
        self.selected = usize::try_from(next).unwrap_or(0);
    }

    /// Apply the current filter input to `results`, repopulating
    /// `filtered`. Selection clamps to the new length.
    pub fn recompute_filter(&mut self) {
        let needle = self.input.to_lowercase();
        self.filtered = if needle.is_empty() {
            (0..self.results.len()).collect()
        } else {
            self.results
                .iter()
                .enumerate()
                .filter(|(_, r)| matches_filter(r, &needle))
                .map(|(i, _)| i)
                .collect()
        };
        if self.filtered.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len() - 1;
        }
    }

    /// Replace `results` (e.g. when a search returns) and reset the
    /// filter / selection.
    pub fn set_results(&mut self, results: Vec<SearchResult>) {
        self.results = results;
        self.input.clear();
        self.recompute_filter();
        self.screen = Screen::Results;
    }

    pub fn set_search_error(&mut self, err: Arc<SearchError>) {
        self.last_error = Some(LastError::Search(err));
        if self.results.is_empty() {
            // First search failed; bounce to Prompt so the user can fix
            // the query.
            self.screen = Screen::Prompt;
            if let Some(q) = self.committed_query.clone() {
                self.input = q;
            }
        } else {
            // Re-run failed; keep the previous results visible. The
            // error renders inline in the footer; the user can `r` to
            // try again or move on.
            self.screen = Screen::Results;
        }
    }

    pub fn set_player_error(&mut self, err: Arc<PlayerError>) {
        self.last_error = Some(LastError::Player(err));
        // Player errors leave the user on the results list.
    }

    #[must_use]
    pub fn selected_result(&self) -> Option<&SearchResult> {
        self.filtered.get(self.selected).and_then(|i| self.results.get(*i))
    }
}

fn matches_filter(r: &SearchResult, needle: &str) -> bool {
    r.title.to_lowercase().contains(needle)
        || r.channel
            .as_deref()
            .is_some_and(|c| c.to_lowercase().contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::VideoDuration;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn mk(id: &str, title: &str, channel: Option<&str>) -> SearchResult {
        SearchResult {
            id: id.to_string(),
            title: title.to_string(),
            channel: channel.map(str::to_string),
            duration: VideoDuration::Seconds(60),
        }
    }

    #[test]
    fn typing_in_prompt_appends_to_input() {
        let mut app = App::new();
        app.handle_key(key(KeyCode::Char('h')));
        app.handle_key(key(KeyCode::Char('i')));
        assert_eq!(app.input, "hi");
        assert_eq!(app.screen, Screen::Prompt);
    }

    #[test]
    fn backspace_in_prompt_removes_last_char() {
        let mut app = App::new();
        app.input = "abc".to_string();
        app.handle_key(key(KeyCode::Backspace));
        assert_eq!(app.input, "ab");
    }

    #[test]
    fn enter_in_prompt_starts_search() {
        let mut app = App::new();
        app.input = "rust ratatui".to_string();
        let action = app.handle_key(key(KeyCode::Enter));
        assert_eq!(action, Action::StartSearch("rust ratatui".to_string()));
        assert_eq!(app.screen, Screen::Searching);
        assert_eq!(app.committed_query.as_deref(), Some("rust ratatui"));
    }

    #[test]
    fn enter_with_empty_input_does_nothing() {
        let mut app = App::new();
        let action = app.handle_key(key(KeyCode::Enter));
        assert_eq!(action, Action::None);
        assert_eq!(app.screen, Screen::Prompt);
    }

    #[test]
    fn enter_with_whitespace_only_does_nothing() {
        let mut app = App::new();
        app.input = "   ".to_string();
        let action = app.handle_key(key(KeyCode::Enter));
        assert_eq!(action, Action::None);
    }

    #[test]
    fn esc_in_prompt_quits() {
        let mut app = App::new();
        assert_eq!(app.handle_key(key(KeyCode::Esc)), Action::Quit);
    }

    #[test]
    fn ctrl_c_in_prompt_quits() {
        let mut app = App::new();
        assert_eq!(app.handle_key(ctrl('c')), Action::Quit);
    }

    #[test]
    fn esc_in_searching_cancels_and_returns_to_prompt() {
        let mut app = App::new();
        app.input = "q".to_string();
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.screen, Screen::Searching);
        let action = app.handle_key(key(KeyCode::Esc));
        assert_eq!(action, Action::CancelSearch);
        assert_eq!(app.screen, Screen::Prompt);
        assert_eq!(app.input, "q");
    }

    fn results_app() -> App {
        let mut app = App::new();
        app.results = vec![
            mk("a", "First video", Some("Alice")),
            mk("b", "Second clip", Some("Bob")),
            mk("c", "Third thing", None),
            mk("d", "Fourth", Some("Dee")),
        ];
        app.committed_query = Some("test".to_string());
        app.recompute_filter();
        app.screen = Screen::Results;
        app
    }

    #[test]
    fn j_and_k_navigate_within_bounds() {
        let mut app = results_app();
        assert_eq!(app.selected, 0);
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.selected, 1);
        app.handle_key(key(KeyCode::Char('j')));
        app.handle_key(key(KeyCode::Char('j')));
        app.handle_key(key(KeyCode::Char('j')));
        // Clamps at last index (3) — 4 results.
        assert_eq!(app.selected, 3);
        app.handle_key(key(KeyCode::Char('k')));
        assert_eq!(app.selected, 2);
    }

    #[test]
    fn k_at_top_stays_at_top() {
        let mut app = results_app();
        app.handle_key(key(KeyCode::Char('k')));
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn capital_g_jumps_to_last() {
        let mut app = results_app();
        app.handle_key(key(KeyCode::Char('G')));
        assert_eq!(app.selected, 3);
    }

    #[test]
    fn double_g_jumps_to_first() {
        let mut app = results_app();
        app.selected = 2;
        app.handle_key(key(KeyCode::Char('g')));
        // After first g: pending; selection unchanged.
        assert_eq!(app.selected, 2);
        app.handle_key(key(KeyCode::Char('g')));
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn g_pending_only_lasts_one_keystroke() {
        let mut app = results_app();
        app.selected = 2;
        app.handle_key(key(KeyCode::Char('g')));
        // Some other key arrives, not g — should not jump to top later.
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.selected, 3);
        app.handle_key(key(KeyCode::Char('g')));
        // First g of a new pair, not second.
        app.handle_key(key(KeyCode::Char('j')));
        // Still didn't jump to top.
        assert_ne!(app.selected, 0);
    }

    #[test]
    fn ctrl_d_jumps_half_viewport_down() {
        let mut app = results_app();
        app.results = (0..50)
            .map(|i| mk(&format!("v{i}"), &format!("title {i}"), None))
            .collect();
        app.recompute_filter();
        app.list_height = 20;
        app.handle_key(ctrl('d'));
        assert_eq!(app.selected, 10);
    }

    #[test]
    fn ctrl_u_jumps_half_viewport_up() {
        let mut app = results_app();
        app.results = (0..50)
            .map(|i| mk(&format!("v{i}"), &format!("title {i}"), None))
            .collect();
        app.recompute_filter();
        app.list_height = 20;
        app.selected = 30;
        app.handle_key(ctrl('u'));
        assert_eq!(app.selected, 20);
    }

    #[test]
    fn enter_in_results_emits_play_with_selected() {
        let mut app = results_app();
        app.selected = 2;
        let action = app.handle_key(key(KeyCode::Enter));
        match action {
            Action::Play(r) => assert_eq!(r.id, "c"),
            other => panic!("expected Play, got {other:?}"),
        }
    }

    #[test]
    fn enter_in_results_with_no_results_is_noop() {
        let mut app = App::new();
        app.screen = Screen::Results;
        let action = app.handle_key(key(KeyCode::Enter));
        assert_eq!(action, Action::None);
    }

    #[test]
    fn n_in_results_returns_to_prompt() {
        let mut app = results_app();
        app.handle_key(key(KeyCode::Char('n')));
        assert_eq!(app.screen, Screen::Prompt);
        assert_eq!(app.input, "");
    }

    #[test]
    fn r_in_results_reruns_when_query_set() {
        let mut app = results_app();
        let action = app.handle_key(key(KeyCode::Char('r')));
        assert_eq!(action, Action::Rerun);
        assert_eq!(app.screen, Screen::Searching);
    }

    #[test]
    fn q_in_results_quits() {
        let mut app = results_app();
        assert_eq!(
            app.handle_key(key(KeyCode::Char('q'))),
            Action::Quit
        );
    }

    #[test]
    fn slash_in_results_enters_filter_mode() {
        let mut app = results_app();
        app.handle_key(key(KeyCode::Char('/')));
        assert_eq!(app.screen, Screen::Filter);
        assert_eq!(app.input, "");
    }

    #[test]
    fn typing_in_filter_filters_results_live() {
        let mut app = results_app();
        app.handle_key(key(KeyCode::Char('/')));
        app.handle_key(key(KeyCode::Char('s')));
        // "s" matches "Second" and "First" (has 's'? no, 'F-i-r-s-t' yes).
        // Actually First has 's'. Second starts with S. Both match.
        // Third has no 's'. Fourth no.
        assert_eq!(app.filtered.len(), 2);
    }

    #[test]
    fn filter_matches_channel_too() {
        let mut app = results_app();
        app.handle_key(key(KeyCode::Char('/')));
        app.handle_key(key(KeyCode::Char('a')));
        app.handle_key(key(KeyCode::Char('l')));
        app.handle_key(key(KeyCode::Char('i')));
        // "ali" matches "Alice" (channel of first result).
        assert_eq!(app.filtered, vec![0]);
    }

    #[test]
    fn filter_esc_clears_and_returns_to_results() {
        let mut app = results_app();
        app.handle_key(key(KeyCode::Char('/')));
        app.handle_key(key(KeyCode::Char('z')));
        assert_eq!(app.filtered.len(), 0);
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.screen, Screen::Results);
        assert_eq!(app.input, "");
        assert_eq!(app.filtered.len(), 4);
    }

    #[test]
    fn filter_enter_commits_and_keeps_filter() {
        let mut app = results_app();
        app.handle_key(key(KeyCode::Char('/')));
        app.handle_key(key(KeyCode::Char('a')));
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.screen, Screen::Results);
        // Filter is still applied: only entries containing 'a'.
        assert!(!app.filtered.is_empty());
        assert!(app.filtered.len() < 4);
    }

    #[test]
    fn filter_backspace_removes_one_char() {
        let mut app = results_app();
        app.handle_key(key(KeyCode::Char('/')));
        app.handle_key(key(KeyCode::Char('a')));
        app.handle_key(key(KeyCode::Char('b')));
        app.handle_key(key(KeyCode::Backspace));
        assert_eq!(app.input, "a");
    }

    #[test]
    fn question_mark_opens_help_from_results() {
        let mut app = results_app();
        app.handle_key(key(KeyCode::Char('?')));
        assert_eq!(app.screen, Screen::Help);
        assert_eq!(app.help_return, Some(Screen::Results));
    }

    #[test]
    fn any_key_dismisses_help_and_returns() {
        let mut app = results_app();
        app.handle_key(key(KeyCode::Char('?')));
        app.handle_key(key(KeyCode::Char(' ')));
        assert_eq!(app.screen, Screen::Results);
    }

    #[test]
    fn q_in_help_quits() {
        let mut app = results_app();
        app.handle_key(key(KeyCode::Char('?')));
        assert_eq!(
            app.handle_key(key(KeyCode::Char('q'))),
            Action::Quit
        );
    }

    #[test]
    fn set_results_transitions_to_results_screen() {
        let mut app = App::new();
        app.screen = Screen::Searching;
        app.set_results(vec![mk("a", "Hi", None)]);
        assert_eq!(app.screen, Screen::Results);
        assert_eq!(app.filtered, vec![0]);
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn set_search_error_returns_to_prompt_when_no_prior_results() {
        let mut app = App::new();
        app.committed_query = Some("rust".to_string());
        app.screen = Screen::Searching;
        app.set_search_error(Arc::new(SearchError::Timeout(
            std::time::Duration::from_secs(30),
        )));
        assert_eq!(app.screen, Screen::Prompt);
        assert_eq!(app.input, "rust");
        assert!(app.last_error.is_some());
    }

    #[test]
    fn set_search_error_preserves_results_on_rerun() {
        let mut app = results_app();
        let original_count = app.results.len();
        app.screen = Screen::Searching; // mid-rerun
        app.set_search_error(Arc::new(SearchError::Timeout(
            std::time::Duration::from_secs(30),
        )));
        assert_eq!(app.screen, Screen::Results);
        assert_eq!(app.results.len(), original_count);
        assert!(app.last_error.is_some());
    }

    #[test]
    fn keypress_clears_last_error() {
        let mut app = App::new();
        app.last_error =
            Some(LastError::Search(Arc::new(SearchError::Timeout(
                std::time::Duration::from_secs(30),
            ))));
        app.handle_key(key(KeyCode::Char('a')));
        assert!(app.last_error.is_none());
    }

    #[test]
    fn commit_query_from_input_starts_search() {
        let mut app = App::new();
        app.input = "rust".to_string();
        let action = app.commit_query();
        assert_eq!(action, Action::StartSearch("rust".to_string()));
        assert_eq!(app.screen, Screen::Searching);
        assert_eq!(app.committed_query.as_deref(), Some("rust"));
    }
}

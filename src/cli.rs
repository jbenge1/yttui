//! Command-line argument parsing.

use clap::Parser;

/// Default result count when `--count` isn't given. Matches the spec
/// and the original `yts` shell function.
pub const DEFAULT_COUNT: u32 = 20;

/// Upper bound on `--count` — yt-dlp gets slow and rate-limited beyond
/// this, and the TUI list is paginated by viewport, not by count.
pub const MAX_COUNT: u32 = 100;

/// Long-form `--version` output. Pacman-style: small ASCII mark on the
/// left, version + author + license on the right.
pub const LONG_VERSION: &str = concat!(
    "   ▶\n",
    "   ▶▶\n",
    "   ▶▶▶          yttui v",
    env!("CARGO_PKG_VERSION"),
    "\n",
    "   ▶▶▶▶         Keyboard-driven YouTube TUI\n",
    "   ▶▶▶\n",
    "   ▶▶           Copyright (C) 2026 Justin Benge\n",
    "   ▶            Released under the MIT License.\n",
    "\n",
    "                Powered by yt-dlp + mpv + ratatui.\n",
    "                No telemetry. No Invidious. No comments.\n",
);

#[derive(Parser, Debug, Clone)]
#[command(
    name = "yttui",
    // Disable clap's auto-version flag so we can print LONG_VERSION
    // directly (without clap's "yttui " prefix on the first line) and
    // accept both `-V` and `-v` as aliases.
    disable_version_flag = true,
    about = "Keyboard-driven YouTube TUI",
    long_about = "Search YouTube and play results via mpv. \
                  Vim-keyed list, no Invidious dependency, no telemetry."
)]
pub struct Cli {
    // Note on the `-v` alias: this is a one-way door. Most Unix tools
    // reserve `-v` for verbose output; we hand it to `--version` (pacman-
    // style) for parity with the user's mental model. If a future
    // verbosity flag lands, it'll need a different short (`-d` for debug,
    // `-q` for quiet, etc.) — reclaiming `-v` would break users who
    // learned `yttui -v`. Kept as a `//` comment so the rationale stays
    // with the code without leaking into `--help`.
    #[arg(
        short = 'V',
        short_alias = 'v',
        long = "version",
        help = "Print version information and exit"
    )]
    pub version: bool,

    /// Initial search query. Multi-word queries are joined with spaces.
    /// Omit to land on the empty prompt.
    pub query: Vec<String>,

    /// Sort results by upload date instead of relevance.
    #[arg(long)]
    pub recent: bool,

    /// Number of results to fetch per search.
    #[arg(
        long,
        default_value_t = DEFAULT_COUNT,
        value_parser = clap::value_parser!(u32).range(1..=i64::from(MAX_COUNT)),
    )]
    pub count: u32,

    /// Play audio only (passes `--no-video` to mpv).
    #[arg(long)]
    pub audio_only: bool,
}

impl Cli {
    /// Joined query string, or `None` if no positional args were given.
    #[must_use]
    pub fn initial_query(&self) -> Option<String> {
        if self.query.is_empty() {
            None
        } else {
            Some(self.query.join(" "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("yttui").chain(args.iter().copied()))
    }

    #[test]
    fn no_args_produces_defaults() {
        let cli = parse(&[]).unwrap();
        assert!(cli.query.is_empty());
        assert!(!cli.recent);
        assert!(!cli.audio_only);
        assert_eq!(cli.count, DEFAULT_COUNT);
        assert_eq!(cli.initial_query(), None);
    }

    #[test]
    fn single_word_query() {
        let cli = parse(&["rust"]).unwrap();
        assert_eq!(cli.initial_query().as_deref(), Some("rust"));
    }

    #[test]
    fn multi_word_query_is_joined_with_spaces() {
        let cli = parse(&["rust", "ratatui", "tutorial"]).unwrap();
        assert_eq!(
            cli.initial_query().as_deref(),
            Some("rust ratatui tutorial")
        );
    }

    #[test]
    fn recent_flag_sets_recent() {
        let cli = parse(&["--recent", "rust"]).unwrap();
        assert!(cli.recent);
        assert_eq!(cli.initial_query().as_deref(), Some("rust"));
    }

    #[test]
    fn count_flag_overrides_default() {
        let cli = parse(&["--count", "50"]).unwrap();
        assert_eq!(cli.count, 50);
    }

    #[test]
    fn count_zero_is_rejected() {
        let err = parse(&["--count", "0"]).unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::ValueValidation);
    }

    #[test]
    fn count_above_max_is_rejected() {
        let err = parse(&["--count", "101"]).unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::ValueValidation);
    }

    #[test]
    fn count_at_lower_boundary_accepted() {
        assert_eq!(parse(&["--count", "1"]).unwrap().count, 1);
    }

    #[test]
    fn count_at_upper_boundary_accepted() {
        assert_eq!(parse(&["--count", "100"]).unwrap().count, MAX_COUNT);
    }

    #[test]
    fn audio_only_flag_sets_audio_only() {
        let cli = parse(&["--audio-only", "rust"]).unwrap();
        assert!(cli.audio_only);
    }

    #[test]
    fn flags_can_appear_after_query() {
        let cli = parse(&["rust", "ratatui", "--recent"]).unwrap();
        assert!(cli.recent);
        assert_eq!(cli.initial_query().as_deref(), Some("rust ratatui"));
    }

    #[test]
    fn help_flag_returns_clap_help_error() {
        let err = parse(&["--help"]).unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::DisplayHelp);
    }
}

//! Startup preflight checks: verify required external binaries are
//! installed before we hand the terminal over to the TUI.
//!
//! ### macOS launch caveat
//!
//! `which::which` resolves binaries against `$PATH`. macOS apps launched
//! from Finder, Spotlight, or Raycast inherit a minimal `$PATH` (no
//! `/opt/homebrew/bin` or `/usr/local/bin`) and will report `yt-dlp` /
//! `mpv` as missing even when they're installed. `yttui` is a CLI —
//! launch it from a terminal so the shell's `$PATH` applies. This will
//! be documented in the README before V1 ships.

use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PreflightError {
    #[error("required binary not found in PATH: {name}\n\n{instructions}")]
    Missing {
        name: &'static str,
        instructions: &'static str,
    },
}

/// Check that all binaries the TUI relies on are present.
///
/// # Errors
/// Returns the first missing binary as a [`PreflightError::Missing`].
pub fn check() -> Result<(), PreflightError> {
    require("yt-dlp")?;
    require("mpv")?;
    Ok(())
}

fn require(name: &'static str) -> Result<(), PreflightError> {
    which::which(name).map(|_| ()).map_err(|_| PreflightError::Missing {
        name,
        instructions: install_instructions(name),
    })
}

#[must_use]
fn install_instructions(bin: &str) -> &'static str {
    match bin {
        "yt-dlp" => {
            "Install yt-dlp:\n  \
             macOS:  brew install yt-dlp\n  \
             Linux:  pipx install yt-dlp  (or your package manager)\n  \
             Docs:   https://github.com/yt-dlp/yt-dlp#installation"
        }
        "mpv" => {
            "Install mpv:\n  \
             macOS:  brew install mpv\n  \
             Linux:  your package manager\n  \
             Docs:   https://mpv.io/installation/"
        }
        _ => "See the project's README for installation instructions.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yt_dlp_instructions_mention_brew() {
        let s = install_instructions("yt-dlp");
        assert!(s.contains("brew install yt-dlp"));
        assert!(s.contains("yt-dlp/yt-dlp"));
    }

    #[test]
    fn mpv_instructions_mention_brew() {
        let s = install_instructions("mpv");
        assert!(s.contains("brew install mpv"));
        assert!(s.contains("mpv.io"));
    }

    #[test]
    fn unknown_bin_falls_back_gracefully() {
        let s = install_instructions("nonexistent");
        assert!(s.contains("README"));
    }

    #[test]
    fn require_succeeds_for_a_known_unix_binary() {
        // `ls` is on every Unix and every CI runner — testing the
        // success branch in isolation, decoupled from whether yt-dlp is
        // preinstalled on the runner.
        require("ls").unwrap();
    }

    #[test]
    fn require_fails_for_unknown_binary() {
        let err = require("absolutely-no-such-binary-zzz").unwrap_err();
        match err {
            PreflightError::Missing { name, instructions } => {
                assert_eq!(name, "absolutely-no-such-binary-zzz");
                assert!(instructions.contains("README"));
            }
        }
    }
}

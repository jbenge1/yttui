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

use std::fmt;

use thiserror::Error;

/// One missing binary, with its install hint. Bundled into
/// [`PreflightError::Missing`] so a single launch can report every
/// dependency at once.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct MissingBinary {
    pub name: &'static str,
    pub instructions: &'static str,
}

impl fmt::Display for MissingBinary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "required binary not found in PATH: {}\n\n{}",
            self.name, self.instructions
        )
    }
}

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PreflightError {
    /// One or more required binaries are missing. Reported together so
    /// the user fixes their environment in a single pass instead of
    /// `install → relaunch → install → relaunch`.
    #[error("{}", format_missing(.0))]
    Missing(Vec<MissingBinary>),
}

fn format_missing(missing: &[MissingBinary]) -> String {
    missing
        .iter()
        .map(MissingBinary::to_string)
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Check that all binaries the TUI relies on are present.
///
/// # Errors
/// Returns [`PreflightError::Missing`] containing every missing binary
/// (not just the first), so the user can install them all in one pass.
pub fn check() -> Result<(), PreflightError> {
    let missing: Vec<MissingBinary> = ["yt-dlp", "mpv"]
        .into_iter()
        .filter(|name| which::which(name).is_err())
        .map(|name| MissingBinary {
            name,
            instructions: install_instructions(name),
        })
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(PreflightError::Missing(missing))
    }
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
    fn missing_binary_display_includes_name_and_instructions() {
        let mb = MissingBinary {
            name: "yt-dlp",
            instructions: install_instructions("yt-dlp"),
        };
        let s = mb.to_string();
        assert!(s.contains("yt-dlp"));
        assert!(s.contains("brew install yt-dlp"));
    }

    #[test]
    fn preflight_error_lists_every_missing_binary() {
        // The whole point of this slice: a user with neither tool
        // installed should see both reports in one launch, not "install
        // yt-dlp" → reinstall → "install mpv".
        let err = PreflightError::Missing(vec![
            MissingBinary {
                name: "yt-dlp",
                instructions: install_instructions("yt-dlp"),
            },
            MissingBinary {
                name: "mpv",
                instructions: install_instructions("mpv"),
            },
        ]);
        let s = err.to_string();
        assert!(s.contains("yt-dlp"), "missing yt-dlp section: {s}");
        assert!(s.contains("mpv"), "missing mpv section: {s}");
        assert!(s.contains("brew install yt-dlp"));
        assert!(s.contains("brew install mpv"));
    }
}

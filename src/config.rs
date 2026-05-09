//! Config loader (slice A1.1).
//!
//! Reads `$XDG_CONFIG_HOME/yttui/config.toml` (or
//! `~/.config/yttui/config.toml` when `XDG_CONFIG_HOME` is unset) into a
//! typed [`Config`]. Same shape on Linux and macOS — ytTUI is a CLI
//! tool and follows the convention of other CLI tools (kitty, neovim,
//! helix, starship, gh) rather than scattering into
//! `~/Library/Application Support` on macOS. Missing file or
//! unspecified fields fall back to [`Config::default`], which by
//! V0.2.0 contract reproduces V1 behavior exactly.
//!
//! Real schema sections (`[history]`, `[log]`, `[playback]`, ...) are
//! introduced by the slices that own them. A1.1 ships the
//! load/parse/defaults plumbing and the first real user-tweakable
//! knobs: `[player] args` and `[log] level`.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ConfigError {
    // Display strings deliberately omit `{source}`; the underlying
    // io/toml error is reachable via `Error::source()` and a chain
    // walker (anyhow / hand-rolled) will format it once. Embedding it
    // here would double-print under any future chain-walking caller.
    #[error("reading config file {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("parsing config file {path}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
}

/// Top-level config. All fields optional in TOML; missing ones use the
/// `Default` impl, which by contract is V1 behavior.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct Config {
    pub player: PlayerConfig,
    pub log: LogConfig,
}

/// `[player]` section. User-facing knobs for the mpv invocation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct PlayerConfig {
    /// Extra args appended to mpv's command line after ytTUI's managed
    /// flags and before the URL. Empty by default — V1 behavior.
    ///
    /// **Override semantics.** mpv resolves option-vs-option conflicts
    /// last-wins, so any flag here can override a ytTUI default of
    /// the same option — including audio-only mode (e.g.
    /// `args = ["--video=auto"]` re-enables video despite
    /// `--audio-only` on the CLI). This is intentional: power-user
    /// knob, power-user responsibility. The URL is the one exception
    /// — it's a fixed positional file argument that user args cannot
    /// displace.
    ///
    /// Useful example: `args = ["--save-position-on-quit"]` to have
    /// mpv resume where you left off across launches.
    pub args: Vec<String>,
}

/// `[log]` section.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct LogConfig {
    pub level: LogLevel,
}

/// Log level, mirrored to [`log::LevelFilter`] at logger init time.
///
/// Owned here (rather than aliasing `LevelFilter`) so the TOML schema
/// stays decoupled from the `log` crate's variant set — and so an
/// invalid level yields a [`ConfigError::Parse`] with the standard
/// "unknown variant" message instead of a stringly-typed match later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum LogLevel {
    /// Disables the file logger entirely. Useful for users with
    /// disk-budget concerns or who would rather route diagnostics
    /// elsewhere; ytTUI's logger is best-effort, so dropping it loses
    /// nothing the rest of the app relies on.
    Off,
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl Default for LogLevel {
    /// Matches the level `init_logger` previously hardcoded; preserves
    /// V1 behavior when the user does not set `[log] level`.
    fn default() -> Self {
        Self::Warn
    }
}

impl From<LogLevel> for log::LevelFilter {
    fn from(level: LogLevel) -> Self {
        match level {
            LogLevel::Off => Self::Off,
            LogLevel::Error => Self::Error,
            LogLevel::Warn => Self::Warn,
            LogLevel::Info => Self::Info,
            LogLevel::Debug => Self::Debug,
            LogLevel::Trace => Self::Trace,
        }
    }
}

impl Config {
    /// Load config from an explicit path. Used by tests and (later, in
    /// A1.2) by the `--config` CLI flag.
    ///
    /// - Path doesn't exist → `Ok(Config::default())`.
    /// - Path exists but unreadable → [`ConfigError::Io`].
    /// - Path exists and is readable but TOML is malformed →
    ///   [`ConfigError::Parse`].
    /// - Path exists and parses → typed config.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::Io`] if the file exists but cannot be
    /// read, or [`ConfigError::Parse`] if the contents are not valid
    /// TOML / do not match the schema (including unknown fields).
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let bytes = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(source) => {
                return Err(ConfigError::Io {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        toml::from_str(&bytes).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })
    }

    /// Resolve the default config path: `$XDG_CONFIG_HOME/yttui/config.toml`
    /// when `XDG_CONFIG_HOME` is set, otherwise `~/.config/yttui/config.toml`.
    /// Same shape on Linux and macOS, matching the convention used by
    /// other CLI tools (kitty, neovim, helix, starship, gh).
    /// Returns `None` only when neither `XDG_CONFIG_HOME` nor a home
    /// directory can be resolved — caller should treat that as
    /// [`Config::default`].
    #[must_use]
    pub fn default_path() -> Option<PathBuf> {
        Self::default_path_from(
            std::env::var_os("XDG_CONFIG_HOME"),
            dirs::home_dir(),
        )
    }

    /// Pure resolver split out for testability: env mutation in tests
    /// would require `unsafe`, which is forbidden crate-wide.
    fn default_path_from(
        xdg: Option<std::ffi::OsString>,
        home: Option<PathBuf>,
    ) -> Option<PathBuf> {
        let base = xdg
            .map(PathBuf::from)
            .or_else(|| home.map(|h| h.join(".config")))?;
        Some(base.join("yttui").join("config.toml"))
    }

    /// Convenience: load from [`Self::default_path`], falling back to
    /// [`Self::default`] if no XDG dir is available.
    ///
    /// # Errors
    ///
    /// Same as [`Self::load`].
    pub fn load_from_default_path() -> Result<Self, ConfigError> {
        Self::default_path()
            .map_or_else(|| Ok(Self::default()), |p| Self::load(&p))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;

    use tempfile::TempDir;

    fn write(dir: &TempDir, body: &str) -> PathBuf {
        let path = dir.path().join("config.toml");
        fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn missing_file_returns_default() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("does-not-exist.toml");
        let cfg = Config::load(&path).unwrap();
        assert_eq!(cfg, Config::default());
    }

    #[test]
    fn empty_file_returns_default() {
        let dir = TempDir::new().unwrap();
        let path = write(&dir, "");
        let cfg = Config::load(&path).unwrap();
        assert_eq!(cfg, Config::default());
    }

    #[test]
    fn partial_toml_only_overrides_specified_fields() {
        // Section present, field present: overrides default.
        let dir = TempDir::new().unwrap();
        let path = write(&dir, "[player]\nargs = [\"--no-osc\"]\n");
        let cfg = Config::load(&path).unwrap();
        assert_eq!(cfg.player.args, vec!["--no-osc".to_string()]);
    }

    #[test]
    fn partial_toml_with_empty_section_keeps_section_defaults() {
        // Section header present but no fields: section defaults apply.
        let dir = TempDir::new().unwrap();
        let path = write(&dir, "[player]\n");
        let cfg = Config::load(&path).unwrap();
        assert_eq!(cfg, Config::default());
    }

    #[test]
    fn player_args_defaults_to_empty_when_section_absent() {
        let dir = TempDir::new().unwrap();
        let path = write(&dir, "");
        let cfg = Config::load(&path).unwrap();
        assert!(cfg.player.args.is_empty());
    }

    #[test]
    fn player_args_round_trips_a_non_empty_vec() {
        let dir = TempDir::new().unwrap();
        let path = write(
            &dir,
            "[player]\nargs = [\"--save-position-on-quit\", \"--no-osc\"]\n",
        );
        let cfg = Config::load(&path).unwrap();
        assert_eq!(
            cfg.player.args,
            vec![
                "--save-position-on-quit".to_string(),
                "--no-osc".to_string(),
            ]
        );
    }

    // ---- [log] level ----

    #[test]
    fn log_level_defaults_to_warn_when_section_absent() {
        // Default must match the level previously hardcoded in
        // `init_logger` — V1 behavior unchanged unless the user opts in.
        let dir = TempDir::new().unwrap();
        let path = write(&dir, "");
        let cfg = Config::load(&path).unwrap();
        assert_eq!(cfg.log.level, LogLevel::Warn);
    }

    #[test]
    fn log_level_accepts_each_known_variant() {
        for (s, expected) in [
            ("off", LogLevel::Off),
            ("error", LogLevel::Error),
            ("warn", LogLevel::Warn),
            ("info", LogLevel::Info),
            ("debug", LogLevel::Debug),
            ("trace", LogLevel::Trace),
        ] {
            let dir = TempDir::new().unwrap();
            let path = write(&dir, &format!("[log]\nlevel = \"{s}\"\n"));
            let cfg = Config::load(&path).unwrap();
            assert_eq!(cfg.log.level, expected, "input {s}");
        }
    }

    #[test]
    fn log_level_rejects_invalid_variant_with_typed_parse_error() {
        let dir = TempDir::new().unwrap();
        let path = write(&dir, "[log]\nlevel = \"wat\"\n");
        let err = Config::load(&path).unwrap_err();
        assert!(
            matches!(err, ConfigError::Parse { .. }),
            "expected Parse, got {err:?}"
        );
    }

    #[test]
    fn log_level_maps_to_log_crate_levelfilter() {
        // Sanity-check the From impl so a variant addition doesn't
        // silently drop a mapping.
        assert_eq!(
            log::LevelFilter::from(LogLevel::Off),
            log::LevelFilter::Off
        );
        assert_eq!(
            log::LevelFilter::from(LogLevel::Error),
            log::LevelFilter::Error
        );
        assert_eq!(
            log::LevelFilter::from(LogLevel::Warn),
            log::LevelFilter::Warn
        );
        assert_eq!(
            log::LevelFilter::from(LogLevel::Info),
            log::LevelFilter::Info
        );
        assert_eq!(
            log::LevelFilter::from(LogLevel::Debug),
            log::LevelFilter::Debug
        );
        assert_eq!(
            log::LevelFilter::from(LogLevel::Trace),
            log::LevelFilter::Trace
        );
    }

    #[test]
    fn directory_in_place_of_file_returns_io_error() {
        // Pointing `load` at a directory exercises the catch-all
        // `Err(source) => ConfigError::Io` arm — `read_to_string` on
        // a directory yields `IsADirectory` / `Other` (platform-
        // dependent), neither of which is `NotFound`.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        fs::create_dir(&path).unwrap();
        let err = Config::load(&path).unwrap_err();
        assert!(
            matches!(err, ConfigError::Io { .. }),
            "expected Io, got {err:?}"
        );
    }

    #[test]
    fn malformed_toml_returns_parse_error_not_panic() {
        let dir = TempDir::new().unwrap();
        let path = write(&dir, "this is = not = valid = toml\n");
        let err = Config::load(&path).unwrap_err();
        assert!(
            matches!(err, ConfigError::Parse { .. }),
            "expected Parse, got {err:?}"
        );
    }

    #[test]
    fn unknown_field_is_rejected_so_typos_do_not_silently_no_op() {
        // `deny_unknown_fields` means user typos surface as parse
        // errors instead of being silently ignored.
        let dir = TempDir::new().unwrap();
        let path = write(&dir, "[player]\nargz = []\n");
        let err = Config::load(&path).unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }));
    }

    #[test]
    fn default_path_ends_with_yttui_config_toml() {
        // Don't assert the full prefix — that varies by platform and
        // env. Just confirm the suffix shape so a future refactor that
        // mis-joins the path fails loudly.
        if let Some(p) = Config::default_path() {
            assert!(p.ends_with("yttui/config.toml"), "got {p:?}");
        }
    }

    #[test]
    fn default_path_honors_xdg_config_home_when_set() {
        // XDG spec: when set, `$XDG_CONFIG_HOME` wins. Same on macOS
        // and Linux. Tested via the pure helper to avoid mutating
        // process env (which would also require `unsafe`).
        let xdg = std::ffi::OsString::from("/tmp/some-xdg");
        let home = Some(PathBuf::from("/home/user"));
        let path = Config::default_path_from(Some(xdg), home).unwrap();
        assert_eq!(
            path,
            PathBuf::from("/tmp/some-xdg/yttui/config.toml")
        );
    }

    #[test]
    fn default_path_falls_back_to_home_dot_config_when_xdg_unset() {
        let path = Config::default_path_from(
            None,
            Some(PathBuf::from("/home/user")),
        )
        .unwrap();
        assert_eq!(
            path,
            PathBuf::from("/home/user/.config/yttui/config.toml")
        );
    }

    #[test]
    fn default_path_returns_none_when_neither_xdg_nor_home() {
        assert!(Config::default_path_from(None, None).is_none());
    }
}

//! Config loader (slice A1.1).
//!
//! Reads `$XDG_CONFIG_HOME/yttui/config.toml` into a typed [`Config`].
//! Missing file or unspecified fields fall back to [`Config::default`],
//! which by V0.2.0 contract reproduces V1 behavior exactly.
//!
//! Real schema sections (`[history]`, `[log]`, `[playback]`, ...) are
//! introduced by the slices that own them. A1.1 ships only the
//! load/parse/defaults plumbing plus a single placeholder field so the
//! round-trip is exercised.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ConfigError {
    #[error("reading config file {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("parsing config file {path}: {source}")]
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
    pub general: GeneralConfig,
}

/// Placeholder section. Exists in A1.1 only to prove the TOML
/// round-trip works end-to-end. Real fields will be added by their
/// owning slices; do not extend this section opportunistically.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct GeneralConfig {
    /// Reserved. No runtime effect in A1.1.
    pub placeholder: bool,
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

    /// Resolve the default config path under the platform's XDG config
    /// dir (`$XDG_CONFIG_HOME/yttui/config.toml` on Linux, the macOS
    /// equivalent via `dirs::config_dir`). Returns `None` if no such
    /// directory is available — caller should treat that as
    /// [`Config::default`].
    #[must_use]
    pub fn default_path() -> Option<PathBuf> {
        dirs::config_dir().map(|p| p.join("yttui").join("config.toml"))
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
        let path = write(&dir, "[general]\nplaceholder = true\n");
        let cfg = Config::load(&path).unwrap();
        assert!(cfg.general.placeholder);
    }

    #[test]
    fn partial_toml_with_empty_section_keeps_section_defaults() {
        // Section header present but no fields: section defaults apply.
        let dir = TempDir::new().unwrap();
        let path = write(&dir, "[general]\n");
        let cfg = Config::load(&path).unwrap();
        assert_eq!(cfg, Config::default());
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
        // errors instead of being silently ignored — important when
        // real schema fields land in later slices.
        let dir = TempDir::new().unwrap();
        let path = write(&dir, "[general]\nplacehlder = true\n");
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
}

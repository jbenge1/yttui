use std::fmt;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration as StdDuration;

use serde::Deserialize;
use thiserror::Error;
use wait_timeout::ChildExt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortOrder {
    Relevance,
    Date,
}

impl SortOrder {
    #[must_use]
    fn ytsearch_prefix(self) -> &'static str {
        match self {
            SortOrder::Relevance => "ytsearch",
            SortOrder::Date => "ytsearchdate",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoDuration {
    Seconds(u64),
    Live,
    Upcoming,
    Unknown,
}

impl VideoDuration {
    /// Whether this video is currently watchable. Upcoming streams
    /// (scheduled but not started) are explicitly not playable.
    #[must_use]
    pub fn is_playable(&self) -> bool {
        !matches!(self, VideoDuration::Upcoming)
    }
}

impl fmt::Display for VideoDuration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VideoDuration::Live => f.write_str("live broadcast"),
            VideoDuration::Upcoming => f.write_str("upcoming livestream"),
            VideoDuration::Unknown => f.write_str("unknown duration"),
            VideoDuration::Seconds(s) => {
                let h = s / 3600;
                let m = (s % 3600) / 60;
                let s = s % 60;
                if h > 0 {
                    write!(f, "{h}:{m:02}:{s:02}")
                } else {
                    write!(f, "{m}:{s:02}")
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResult {
    pub id: String,
    pub title: String,
    pub channel: Option<String>,
    pub duration: VideoDuration,
}

#[derive(Debug, Error)]
pub enum SearchError {
    #[error("yt-dlp returned invalid JSON: {0}")]
    InvalidJson(#[from] serde_json::Error),
    #[error("failed to launch yt-dlp: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("failed while waiting for yt-dlp: {0}")]
    Wait(#[source] std::io::Error),
    #[error("failed to read yt-dlp output: {0}")]
    Read(#[source] std::io::Error),
    #[error("yt-dlp output reader thread panicked")]
    ReaderPanicked,
    #[error("yt-dlp exited with status {status}: {stderr}")]
    NonZeroExit { status: i32, stderr: String },
    #[error("yt-dlp timed out after {0:?}")]
    Timeout(StdDuration),
}

pub trait SearchBackend {
    /// Run a search and return the raw JSON bytes from the backend.
    ///
    /// # Errors
    /// Returns [`SearchError`] if the backend cannot be invoked, exits
    /// non-zero, exceeds its timeout, or its output cannot be read.
    fn run(
        &self,
        query: &str,
        count: u32,
        sort: SortOrder,
    ) -> Result<Vec<u8>, SearchError>;
}

#[derive(Debug, Clone)]
pub struct YtDlpBackend {
    pub timeout: StdDuration,
    pub binary: PathBuf,
}

impl Default for YtDlpBackend {
    fn default() -> Self {
        Self {
            timeout: StdDuration::from_secs(30),
            binary: PathBuf::from("yt-dlp"),
        }
    }
}

impl SearchBackend for YtDlpBackend {
    fn run(
        &self,
        query: &str,
        count: u32,
        sort: SortOrder,
    ) -> Result<Vec<u8>, SearchError> {
        let target = format!("{}{}:{}", sort.ytsearch_prefix(), count, query);
        let mut child = Command::new(&self.binary)
            .arg("--flat-playlist")
            .arg("-J")
            .arg(&target)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(SearchError::Spawn)?;

        // Drain stdout/stderr in background threads so the child can't block
        // on a full pipe buffer while we wait on it.
        let mut stdout = child
            .stdout
            .take()
            .expect("stdout was configured as piped");
        let mut stderr = child
            .stderr
            .take()
            .expect("stderr was configured as piped");
        let stdout_thread = thread::spawn(move || {
            let mut buf = Vec::new();
            stdout.read_to_end(&mut buf).map(|_| buf)
        });
        let stderr_thread = thread::spawn(move || {
            let mut buf = Vec::new();
            stderr.read_to_end(&mut buf).map(|_| buf)
        });

        let Some(status) = child
            .wait_timeout(self.timeout)
            .map_err(SearchError::Wait)?
        else {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stdout_thread.join();
            let _ = stderr_thread.join();
            return Err(SearchError::Timeout(self.timeout));
        };

        let stdout_buf = stdout_thread
            .join()
            .map_err(|_| SearchError::ReaderPanicked)?
            .map_err(SearchError::Read)?;
        let stderr_buf = stderr_thread
            .join()
            .map_err(|_| SearchError::ReaderPanicked)?
            .map_err(SearchError::Read)?;

        if !status.success() {
            return Err(SearchError::NonZeroExit {
                status: status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&stderr_buf).trim().to_string(),
            });
        }

        Ok(stdout_buf)
    }
}

/// Convenience: run the backend and parse its output into [`SearchResult`]s.
///
/// # Errors
/// Returns [`SearchError`] from the backend, or [`SearchError::InvalidJson`]
/// if the backend's output cannot be parsed.
pub fn search<B: SearchBackend>(
    backend: &B,
    query: &str,
    count: u32,
    sort: SortOrder,
) -> Result<Vec<SearchResult>, SearchError> {
    let raw = backend.run(query, count, sort)?;
    parse_results(&raw)
}

#[derive(Deserialize)]
struct RawPlaylist {
    #[serde(default)]
    entries: Vec<serde_json::Value>,
}

/// Parse `yt-dlp --flat-playlist -J` output into a list of [`SearchResult`]s.
///
/// Malformed entries (missing `id` or `title`) are logged via the `log` crate
/// and skipped; the rest of the batch is returned.
///
/// # Errors
/// Returns [`SearchError::InvalidJson`] if the input is not valid JSON in the
/// expected playlist shape.
pub fn parse_results(json: &[u8]) -> Result<Vec<SearchResult>, SearchError> {
    let playlist: RawPlaylist = serde_json::from_slice(json)?;
    let mut out = Vec::with_capacity(playlist.entries.len());
    for entry in playlist.entries {
        if let Some(r) = entry_to_result(&entry) {
            out.push(r);
        } else {
            log::warn!(
                "skipping malformed yt-dlp entry id={:?}",
                entry.get("id").and_then(serde_json::Value::as_str)
            );
        }
    }
    Ok(out)
}

fn entry_to_result(entry: &serde_json::Value) -> Option<SearchResult> {
    let id = entry.get("id")?.as_str()?.to_string();
    let title = entry.get("title")?.as_str()?.to_string();
    let channel = entry
        .get("channel")
        .and_then(serde_json::Value::as_str)
        .or_else(|| entry.get("uploader").and_then(serde_json::Value::as_str))
        .map(str::to_string);
    let duration = parse_duration(entry);
    Some(SearchResult {
        id,
        title,
        channel,
        duration,
    })
}

fn parse_duration(entry: &serde_json::Value) -> VideoDuration {
    if let Some(status) = entry.get("live_status").and_then(serde_json::Value::as_str) {
        match status {
            "is_live" => return VideoDuration::Live,
            "is_upcoming" => return VideoDuration::Upcoming,
            _ => {}
        }
    }
    match entry.get("duration").and_then(serde_json::Value::as_f64) {
        Some(d) if d.is_finite() && d >= 0.0 => {
            // Cap at u64::MAX worth of seconds — anything larger is bogus,
            // but we've already validated finite + non-negative above.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let secs = d as u64;
            VideoDuration::Seconds(secs)
        }
        _ => VideoDuration::Unknown,
    }
}

#[must_use]
pub fn format_duration(d: &VideoDuration) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE_HAPPY: &[u8] =
        include_bytes!("../tests/fixtures/search_rust_ratatui.json");
    const FIXTURE_NO_RESULTS: &[u8] =
        include_bytes!("../tests/fixtures/search_no_results.json");
    const FIXTURE_UNICODE: &[u8] =
        include_bytes!("../tests/fixtures/search_unicode.json");
    const FIXTURE_LIVE: &[u8] =
        include_bytes!("../tests/fixtures/search_live_streams.json");

    #[test]
    fn parses_real_yt_dlp_fixture() {
        let results = parse_results(FIXTURE_HAPPY).expect("fixture parses");
        assert_eq!(results.len(), 3);
        let first = &results[0];
        assert!(!first.id.is_empty());
        assert!(!first.title.is_empty());
        assert!(first.channel.is_some());
    }

    #[test]
    fn parses_zero_results_fixture() {
        let results = parse_results(FIXTURE_NO_RESULTS).expect("parses");
        assert!(results.is_empty());
    }

    #[test]
    fn parses_unicode_titles() {
        let results = parse_results(FIXTURE_UNICODE).expect("parses");
        assert!(!results.is_empty());
        // At least one title should contain non-ASCII.
        assert!(results.iter().any(|r| !r.title.is_ascii()));
    }

    #[test]
    fn parses_live_streams_as_live() {
        let results = parse_results(FIXTURE_LIVE).expect("parses");
        assert!(!results.is_empty());
        assert!(results.iter().all(|r| r.duration == VideoDuration::Live));
    }

    #[test]
    fn upcoming_stream_is_distinct_from_live() {
        let json = br#"{
            "entries": [
                {"id": "a", "title": "Live now", "live_status": "is_live"},
                {"id": "b", "title": "Scheduled", "live_status": "is_upcoming"},
                {"id": "c", "title": "Past", "live_status": "was_live", "duration": 120.0}
            ]
        }"#;
        let r = parse_results(json).unwrap();
        assert_eq!(r[0].duration, VideoDuration::Live);
        assert_eq!(r[1].duration, VideoDuration::Upcoming);
        assert_eq!(r[2].duration, VideoDuration::Seconds(120));
    }

    #[test]
    fn duration_display_uses_human_phrasing() {
        assert_eq!(VideoDuration::Live.to_string(), "live broadcast");
        assert_eq!(
            VideoDuration::Upcoming.to_string(),
            "upcoming livestream"
        );
        assert_eq!(VideoDuration::Unknown.to_string(), "unknown duration");
        assert_eq!(VideoDuration::Seconds(0).to_string(), "0:00");
        assert_eq!(VideoDuration::Seconds(2404).to_string(), "40:04");
        assert_eq!(VideoDuration::Seconds(3661).to_string(), "1:01:01");
    }

    #[test]
    fn upcoming_is_not_playable() {
        assert!(VideoDuration::Live.is_playable());
        assert!(VideoDuration::Seconds(60).is_playable());
        assert!(VideoDuration::Unknown.is_playable());
        assert!(!VideoDuration::Upcoming.is_playable());
    }

    #[test]
    fn skips_entries_missing_required_fields() {
        let json = br#"{
            "entries": [
                {"id": "abc123", "title": "Good", "channel": "Alice", "duration": 60.0},
                {"title": "No id"},
                {"id": "def456"},
                {"id": "ghi789", "title": "No duration", "channel": "Bob", "duration": null}
            ]
        }"#;
        let results = parse_results(json).expect("parses");
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].id, "abc123");
        assert_eq!(results[0].duration, VideoDuration::Seconds(60));
        assert_eq!(results[1].id, "ghi789");
        assert_eq!(results[1].duration, VideoDuration::Unknown);
    }

    #[test]
    fn missing_entries_field_parses_as_empty() {
        let results = parse_results(br"{}").expect("parses");
        assert!(results.is_empty());
    }

    #[test]
    fn invalid_json_returns_error() {
        let err = parse_results(b"not json").unwrap_err();
        assert!(matches!(err, SearchError::InvalidJson(_)));
    }

    #[test]
    fn missing_channel_is_none() {
        let json = br#"{
            "entries": [
                {"id": "x", "title": "T", "uploader": "Carol"},
                {"id": "y", "title": "T2"}
            ]
        }"#;
        let r = parse_results(json).unwrap();
        assert_eq!(r[0].channel.as_deref(), Some("Carol"));
        assert_eq!(r[1].channel, None);
    }

    #[test]
    fn negative_or_nan_duration_is_unknown() {
        let json = br#"{
            "entries": [
                {"id": "a", "title": "T", "duration": -10.0},
                {"id": "b", "title": "T", "duration": null},
                {"id": "c", "title": "T"}
            ]
        }"#;
        let r = parse_results(json).unwrap();
        assert!(r.iter().all(|x| x.duration == VideoDuration::Unknown));
    }

    #[test]
    fn formats_duration() {
        assert_eq!(format_duration(&VideoDuration::Seconds(0)), "0:00");
        assert_eq!(format_duration(&VideoDuration::Seconds(59)), "0:59");
        assert_eq!(format_duration(&VideoDuration::Seconds(60)), "1:00");
        assert_eq!(format_duration(&VideoDuration::Seconds(2404)), "40:04");
        assert_eq!(format_duration(&VideoDuration::Seconds(3661)), "1:01:01");
        assert_eq!(format_duration(&VideoDuration::Live), "LIVE");
        assert_eq!(format_duration(&VideoDuration::Upcoming), "UPCOMING");
        assert_eq!(format_duration(&VideoDuration::Unknown), "—");
    }

    struct FakeBackend {
        captured: std::sync::Mutex<Option<(String, u32, SortOrder)>>,
        response: Vec<u8>,
    }

    impl FakeBackend {
        fn ok(json: &[u8]) -> Self {
            Self {
                captured: std::sync::Mutex::new(None),
                response: json.to_vec(),
            }
        }
    }

    impl SearchBackend for FakeBackend {
        fn run(
            &self,
            query: &str,
            count: u32,
            sort: SortOrder,
        ) -> Result<Vec<u8>, SearchError> {
            *self.captured.lock().expect("lock") =
                Some((query.to_string(), count, sort));
            Ok(self.response.clone())
        }
    }

    #[test]
    fn search_passes_args_to_backend() {
        let backend = FakeBackend::ok(FIXTURE_HAPPY);
        let results =
            search(&backend, "rust ratatui", 20, SortOrder::Relevance).unwrap();
        assert_eq!(results.len(), 3);
        let captured = backend.captured.lock().unwrap().clone().unwrap();
        assert_eq!(captured.0, "rust ratatui");
        assert_eq!(captured.1, 20);
        assert_eq!(captured.2, SortOrder::Relevance);
    }

    #[test]
    fn ytsearch_prefix_changes_with_sort() {
        assert_eq!(SortOrder::Relevance.ytsearch_prefix(), "ytsearch");
        assert_eq!(SortOrder::Date.ytsearch_prefix(), "ytsearchdate");
    }

    #[test]
    fn missing_yt_dlp_binary_returns_spawn_error() {
        let backend = YtDlpBackend {
            timeout: StdDuration::from_secs(5),
            binary: PathBuf::from("definitely-not-a-real-binary-asdf"),
        };
        let err = backend.run("anything", 1, SortOrder::Relevance).unwrap_err();
        assert!(matches!(err, SearchError::Spawn(_)));
    }

    #[test]
    fn timeout_kills_long_running_process() {
        // `yes` ignores its arguments, runs forever, and floods stdout —
        // exercising both the timeout path and the pipe-drain threads.
        let backend = YtDlpBackend {
            timeout: StdDuration::from_millis(100),
            binary: PathBuf::from("yes"),
        };
        let err = backend.run("anything", 1, SortOrder::Relevance).unwrap_err();
        assert!(
            matches!(err, SearchError::Timeout(_)),
            "expected Timeout, got {err:?}"
        );
    }
}

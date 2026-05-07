use std::fmt;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration as StdDuration, Instant};

use command_group::CommandGroup;
use serde::Deserialize;
use thiserror::Error;

/// How often the cancel/timeout polling loop wakes to check the child.
const CANCEL_POLL_INTERVAL: StdDuration = StdDuration::from_millis(50);

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
#[non_exhaustive]
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
#[non_exhaustive]
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
    #[error("search cancelled by user")]
    Cancelled,
}

pub trait SearchBackend {
    /// Run a search and return parsed [`SearchResult`]s. The `cancel`
    /// flag is polled by the implementation; if set, the in-flight
    /// search is aborted and [`SearchError::Cancelled`] is returned.
    ///
    /// # Errors
    /// Returns [`SearchError`] if the backend cannot be invoked, exits
    /// non-zero, exceeds its timeout, its output cannot be read, its
    /// output cannot be parsed, or `cancel` is observed mid-run.
    fn search(
        &self,
        query: &str,
        count: u32,
        sort: SortOrder,
        cancel: &AtomicBool,
    ) -> Result<Vec<SearchResult>, SearchError>;
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

impl YtDlpBackend {
    /// Internal helper used by [`SearchBackend::search`]. Spawns `yt-dlp` in its
    /// own process group, drains its pipes in background threads, and
    /// loops on `try_wait` while watching for cancel/timeout. Process
    /// group means a kill targets `yt-dlp` *and* anything it forked
    /// (e.g. helper extractors).
    fn run_raw_with_cancel(
        &self,
        query: &str,
        count: u32,
        sort: SortOrder,
        cancel: &AtomicBool,
    ) -> Result<Vec<u8>, SearchError> {
        let target = format!("{}{}:{}", sort.ytsearch_prefix(), count, query);
        let mut child = Command::new(&self.binary)
            .arg("--flat-playlist")
            .arg("-J")
            .arg(&target)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .group_spawn()
            .map_err(SearchError::Spawn)?;

        // Drain stdout/stderr in background threads so the child can't block
        // on a full pipe buffer while we poll for cancel/timeout.
        // (`GroupChild::inner` exposes the underlying `Child`, where the
        // pipe handles live.)
        let mut stdout = child
            .inner()
            .stdout
            .take()
            .expect("stdout was configured as piped");
        let mut stderr = child
            .inner()
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

        // Poll loop: every CANCEL_POLL_INTERVAL, check (a) cancel flag,
        // (b) child exit, (c) timeout. Whichever fires first wins.
        let start = Instant::now();
        let status = loop {
            if cancel.load(Ordering::Relaxed) {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_thread.join();
                let _ = stderr_thread.join();
                return Err(SearchError::Cancelled);
            }
            if let Some(s) = child.try_wait().map_err(SearchError::Wait)? {
                break s;
            }
            if start.elapsed() >= self.timeout {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_thread.join();
                let _ = stderr_thread.join();
                return Err(SearchError::Timeout(self.timeout));
            }
            thread::sleep(CANCEL_POLL_INTERVAL);
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

    /// Non-cancellable shorthand for tests that don't care about the
    /// cancel flag. Lives behind `cfg(test)` because production code
    /// has switched to `run_raw_with_cancel` everywhere.
    #[cfg(test)]
    fn run_raw(
        &self,
        query: &str,
        count: u32,
        sort: SortOrder,
    ) -> Result<Vec<u8>, SearchError> {
        let never = AtomicBool::new(false);
        self.run_raw_with_cancel(query, count, sort, &never)
    }
}

impl SearchBackend for YtDlpBackend {
    fn search(
        &self,
        query: &str,
        count: u32,
        sort: SortOrder,
        cancel: &AtomicBool,
    ) -> Result<Vec<SearchResult>, SearchError> {
        let raw = self.run_raw_with_cancel(query, count, sort, cancel)?;
        parse_results(&raw)
    }
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
    let id = entry
        .get("id")?
        .as_str()
        .filter(|s| !s.is_empty())?
        .to_string();
    let title = entry
        .get("title")?
        .as_str()
        .filter(|s| !s.is_empty())?
        .to_string();
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
            // `as u64` is saturating since Rust 1.45: finite values above
            // `u64::MAX` (≈1.8e19, ≈584 billion years) collapse to
            // `u64::MAX`. Validated finite + non-negative above; the
            // saturating top-end is the remaining theoretical case and
            // is fine for our purposes (no real video runs that long).
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let secs = d as u64;
            VideoDuration::Seconds(secs)
        }
        _ => VideoDuration::Unknown,
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

/// Test double that constructs results directly — no JSON-shape
    /// duplication. The trait now returns `Vec<SearchResult>` so this
    /// fake doesn't need to know how `yt-dlp` formats its output.
    struct FakeBackend {
        captured: std::sync::Mutex<Option<(String, u32, SortOrder)>>,
        response: Vec<SearchResult>,
    }

    impl FakeBackend {
        fn ok(results: Vec<SearchResult>) -> Self {
            Self {
                captured: std::sync::Mutex::new(None),
                response: results,
            }
        }
    }

    impl SearchBackend for FakeBackend {
        fn search(
            &self,
            query: &str,
            count: u32,
            sort: SortOrder,
            _cancel: &AtomicBool,
        ) -> Result<Vec<SearchResult>, SearchError> {
            *self.captured.lock().expect("lock") =
                Some((query.to_string(), count, sort));
            Ok(self.response.clone())
        }
    }

    fn no_cancel() -> AtomicBool {
        AtomicBool::new(false)
    }

    fn fixture_results() -> Vec<SearchResult> {
        vec![
            SearchResult {
                id: "vid1".to_string(),
                title: "First".to_string(),
                channel: Some("Chan A".to_string()),
                duration: VideoDuration::Seconds(60),
            },
            SearchResult {
                id: "vid2".to_string(),
                title: "Second".to_string(),
                channel: None,
                duration: VideoDuration::Live,
            },
        ]
    }

    #[test]
    fn fake_backend_passes_args_through() {
        let backend = FakeBackend::ok(fixture_results());
        let results = backend
            .search("rust ratatui", 20, SortOrder::Relevance, &no_cancel())
            .unwrap();
        assert_eq!(results.len(), 2);
        let captured = backend.captured.lock().unwrap().clone().unwrap();
        assert_eq!(captured.0, "rust ratatui");
        assert_eq!(captured.1, 20);
        assert_eq!(captured.2, SortOrder::Relevance);
    }

    #[test]
    fn ytdlp_backend_search_parses_through_to_results() {
        // End-to-end via the trait: spawn errors propagate as SearchError,
        // not as bytes that fail to parse later.
        let backend = YtDlpBackend {
            timeout: StdDuration::from_secs(5),
            binary: PathBuf::from("definitely-not-a-real-binary-asdf"),
        };
        let err = backend
            .search("anything", 1, SortOrder::Relevance, &no_cancel())
            .unwrap_err();
        assert!(matches!(err, SearchError::Spawn(_)));
    }

    #[test]
    fn ytsearch_prefix_changes_with_sort() {
        assert_eq!(SortOrder::Relevance.ytsearch_prefix(), "ytsearch");
        assert_eq!(SortOrder::Date.ytsearch_prefix(), "ytsearchdate");
    }

    #[test]
    fn non_zero_exit_is_classified() {
        // `false` always exits non-zero (1 on macOS/Linux). Doesn't read
        // its args, so the constructed `--flat-playlist -J ytsearch1:…`
        // is harmlessly ignored.
        let backend = YtDlpBackend {
            timeout: StdDuration::from_secs(5),
            binary: PathBuf::from("false"),
        };
        let err = backend
            .run_raw("anything", 1, SortOrder::Relevance)
            .unwrap_err();
        match err {
            SearchError::NonZeroExit { status, .. } => {
                assert_ne!(status, 0);
            }
            other => panic!("expected NonZeroExit, got {other:?}"),
        }
    }

    #[test]
    fn empty_id_or_title_strings_are_skipped() {
        let json = br#"{
            "entries": [
                {"id": "", "title": "Has empty id"},
                {"id": "good1", "title": ""},
                {"id": "good2", "title": "Has both"}
            ]
        }"#;
        let r = parse_results(json).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].id, "good2");
    }

    #[test]
    fn missing_yt_dlp_binary_returns_spawn_error() {
        let backend = YtDlpBackend {
            timeout: StdDuration::from_secs(5),
            binary: PathBuf::from("definitely-not-a-real-binary-asdf"),
        };
        let err = backend
            .run_raw("anything", 1, SortOrder::Relevance)
            .unwrap_err();
        assert!(matches!(err, SearchError::Spawn(_)));
    }

    #[test]
    fn timeout_kills_long_running_process() {
        // `yes` ignores its arguments, runs forever, and floods stdout —
        // exercising both the timeout path and the pipe-drain threads.
        //
        // Timeout chosen ≥ 4 × CANCEL_POLL_INTERVAL: the polling loop
        // wakes every 50 ms, so the worst-case detection latency is
        // `timeout + CANCEL_POLL_INTERVAL`. 200 ms gives us comfortable
        // headroom on a busy CI runner.
        let backend = YtDlpBackend {
            timeout: StdDuration::from_millis(200),
            binary: PathBuf::from("yes"),
        };
        let err = backend
            .run_raw("anything", 1, SortOrder::Relevance)
            .unwrap_err();
        assert!(
            matches!(err, SearchError::Timeout(_)),
            "expected Timeout, got {err:?}"
        );
    }

    #[test]
    fn cancel_flag_kills_running_process() {
        // Run `yes` (forever) under a long timeout; trip the cancel flag
        // from another thread and assert we get Cancelled, not Timeout.
        let backend = YtDlpBackend {
            timeout: StdDuration::from_secs(60),
            binary: PathBuf::from("yes"),
        };
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        let cancel_signal = cancel.clone();
        let trigger = thread::spawn(move || {
            thread::sleep(StdDuration::from_millis(150));
            cancel_signal.store(true, Ordering::SeqCst);
        });
        let err = backend
            .search("anything", 1, SortOrder::Relevance, &cancel)
            .unwrap_err();
        trigger.join().expect("trigger thread");
        assert!(
            matches!(err, SearchError::Cancelled),
            "expected Cancelled, got {err:?}"
        );
    }
}

use std::path::PathBuf;
use std::process::{Command, ExitStatus};

use command_group::{CommandGroup, GroupChild};
use thiserror::Error;

use crate::search::SearchResult;

#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct PlaybackOptions {
    pub audio_only: bool,
    /// Extra args appended to mpv's command line, sourced from
    /// `[player] args` in the user config. Inserted *before* the URL
    /// (which mpv treats as a positional file arg) so flags placed
    /// here behave as expected. yttui-managed flags (`--no-video` for
    /// audio-only) are emitted first; user args follow, letting mpv's
    /// last-wins semantics apply if the user wants to override an
    /// optional default.
    pub extra_args: Vec<String>,
}

impl PlaybackOptions {
    /// Builder shorthand: start from defaults and toggle `audio_only`.
    /// Needed because `#[non_exhaustive]` forbids struct-literal
    /// construction from outside the crate.
    #[must_use]
    pub const fn with_audio_only(mut self, audio_only: bool) -> Self {
        self.audio_only = audio_only;
        self
    }

    /// Builder shorthand for `extra_args`; pairs with
    /// `with_audio_only` for `#[non_exhaustive]`-safe construction.
    #[must_use]
    pub fn with_extra_args(mut self, args: Vec<String>) -> Self {
        self.extra_args = args;
        self
    }
}

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum PlayerError {
    #[error("failed to launch player: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("failed waiting for player: {0}")]
    Wait(#[source] std::io::Error),
    #[error("player exited with status {0}")]
    NonZeroExit(i32),
    #[error("player was killed by signal {0}")]
    KilledBySignal(i32),
    #[error("video is not playable: {reason}")]
    NotPlayable { reason: String },
}

/// A spawned-but-not-yet-waited-on player subprocess.
///
/// Returned by [`Player::spawn_player`] so the caller can register the
/// pgid with the signal-watcher before blocking on wait. Splitting
/// spawn from wait closes the spawn-vs-register race (#70): the
/// registration can be performed under the same critical section as
/// the spawn syscall, with no callback gymnastics.
#[derive(Debug)]
pub struct RunningPlayer {
    inner: RunningInner,
}

#[derive(Debug)]
enum RunningInner {
    Real(GroupChild),
    /// Test-only no-op: never spawned a real process. `pid()` returns
    /// a placeholder, `wait()` returns the recorded result.
    #[cfg(test)]
    Fake(Result<(), PlayerError>),
}

impl RunningPlayer {
    /// The process-group-leader pid. Stable for the lifetime of this
    /// handle; the kernel reaps the pid only after [`Self::wait`].
    #[must_use]
    pub fn pid(&self) -> u32 {
        match &self.inner {
            RunningInner::Real(child) => {
                // GroupChild::id() returns the underlying child's pid;
                // since we used group_spawn, that pid is also the pgid.
                child.id()
            }
            #[cfg(test)]
            RunningInner::Fake(_) => 0,
        }
    }

    /// Block until the player exits. Consumes the handle.
    ///
    /// # Errors
    /// Returns [`PlayerError::Wait`] if the wait syscall itself fails,
    /// [`PlayerError::NonZeroExit`] for a non-zero exit code, or
    /// [`PlayerError::KilledBySignal`] (Unix) if the player was
    /// signal-terminated.
    pub fn wait(self) -> Result<(), PlayerError> {
        match self.inner {
            RunningInner::Real(mut child) => {
                let status = child.wait().map_err(PlayerError::Wait)?;
                classify_exit(status)
            }
            #[cfg(test)]
            RunningInner::Fake(result) => result,
        }
    }
}

pub trait Player {
    /// Play the given `YouTube` video. Blocks until the player exits.
    ///
    /// **Terminal contract:** the player inherits the parent's stdio. Any
    /// caller rendering a TUI **must** leave their alternate screen
    /// (and restore the cursor) before calling `play`, then re-enter
    /// after it returns. Otherwise the player will scribble over the
    /// TUI buffer.
    ///
    /// # Errors
    /// Returns [`PlayerError`] if the binary cannot be spawned, the wait
    /// fails, the player exits non-zero, or the player is killed by a
    /// signal (Unix only).
    fn play(
        &self,
        video_id: &str,
        opts: &PlaybackOptions,
    ) -> Result<(), PlayerError> {
        self.spawn_player(video_id, opts)?.wait()
    }

    /// Spawn the player subprocess and return a [`RunningPlayer`]
    /// handle. The caller is responsible for calling [`RunningPlayer::wait`]
    /// (directly or transitively). The split lets the caller register
    /// the pgid with the signal-watcher *between* spawn and wait, under
    /// the same lock as the spawn syscall — closing the spawn-vs-register
    /// race window (#70).
    ///
    /// # Errors
    /// Returns [`PlayerError::Spawn`] if the player binary cannot be
    /// launched.
    fn spawn_player(
        &self,
        video_id: &str,
        opts: &PlaybackOptions,
    ) -> Result<RunningPlayer, PlayerError>;
}

#[derive(Debug, Clone)]
pub struct MpvPlayer {
    pub binary: PathBuf,
}

impl Default for MpvPlayer {
    fn default() -> Self {
        Self {
            binary: PathBuf::from("mpv"),
        }
    }
}

#[must_use]
pub(crate) fn youtube_url(video_id: &str) -> String {
    format!("https://www.youtube.com/watch?v={video_id}")
}

#[must_use]
pub(crate) fn mpv_args(video_id: &str, opts: &PlaybackOptions) -> Vec<String> {
    let mut args = Vec::with_capacity(1 + opts.extra_args.len() + 1);
    if opts.audio_only {
        args.push("--no-video".to_string());
    }
    // User-supplied args (from `[player] args`) appended before the
    // URL. mpv treats the URL as a positional file arg, so anything
    // after it would be parsed as additional files. Cloning is fine:
    // this runs once per playback, not per frame.
    args.extend(opts.extra_args.iter().cloned());
    args.push(youtube_url(video_id));
    args
}

/// Map an `ExitStatus` to either `Ok(())` or the appropriate error variant.
/// On Unix, signal-terminated processes are reported as
/// [`PlayerError::KilledBySignal`]; otherwise non-zero exits are
/// [`PlayerError::NonZeroExit`].
fn classify_exit(status: ExitStatus) -> Result<(), PlayerError> {
    if status.success() {
        return Ok(());
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return Err(PlayerError::KilledBySignal(sig));
        }
    }
    Err(PlayerError::NonZeroExit(status.code().unwrap_or(-1)))
}

impl Player for MpvPlayer {
    fn spawn_player(
        &self,
        video_id: &str,
        opts: &PlaybackOptions,
    ) -> Result<RunningPlayer, PlayerError> {
        let args = mpv_args(video_id, opts);
        // Spawn the player in its own process group so that:
        //   1. We can kill the entire group (including any helper processes
        //      mpv forks) by killing the leader.
        //   2. The signal-watcher thread (`yttui::signal`) can kill the
        //      group on SIGINT/SIGTERM via the registered pgid,
        //      preventing orphaned mpv processes (V1 AC #4).
        // Note: SIGKILL on the parent is not interceptable; an orphan
        // can still occur if the user `kill -9`s yttui itself.
        let child = Command::new(&self.binary)
            .args(&args)
            .group_spawn()
            .map_err(PlayerError::Spawn)?;
        Ok(RunningPlayer { inner: RunningInner::Real(child) })
    }
}

/// Play a [`SearchResult`] via the given player, refusing to launch on
/// videos that aren't yet watchable (e.g. upcoming streams).
///
/// # Errors
/// Returns [`PlayerError::NotPlayable`] without spawning the player when
/// the video isn't playable; otherwise propagates whatever the player
/// returns.
pub fn play_result<P: Player>(
    player: &P,
    result: &SearchResult,
    opts: &PlaybackOptions,
) -> Result<(), PlayerError> {
    spawn_result(player, result, opts)?.wait()
}

/// Spawn the player for a [`SearchResult`] without waiting on it.
///
/// Returns a [`RunningPlayer`] handle; used by callers that need to
/// register the pgid with the signal-watcher before blocking on wait
/// (closing #70's spawn-vs-register race).
///
/// # Errors
/// Same as [`play_result`] for the spawn path.
pub fn spawn_result<P: Player>(
    player: &P,
    result: &SearchResult,
    opts: &PlaybackOptions,
) -> Result<RunningPlayer, PlayerError> {
    if !result.duration.is_playable() {
        return Err(PlayerError::NotPlayable {
            reason: result.duration.to_string(),
        });
    }
    player.spawn_player(&result.id, opts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::VideoDuration;

    #[test]
    fn youtube_url_uses_canonical_form() {
        assert_eq!(
            youtube_url("dQw4w9WgXcQ"),
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ"
        );
    }

    #[test]
    fn default_args_are_just_the_url() {
        let args = mpv_args("abc123", &PlaybackOptions::default());
        assert_eq!(args, vec![youtube_url("abc123")]);
    }

    #[test]
    fn audio_only_adds_no_video_flag() {
        let args = mpv_args(
            "abc123",
            &PlaybackOptions::default().with_audio_only(true),
        );
        assert_eq!(args[0], "--no-video");
        assert_eq!(args.last().unwrap(), &youtube_url("abc123"));
    }

    #[test]
    fn extra_args_default_is_empty_so_default_invocation_is_unchanged() {
        let opts = PlaybackOptions::default();
        assert!(opts.extra_args.is_empty());
    }

    #[test]
    fn extra_args_are_appended_before_the_url() {
        let opts = PlaybackOptions::default()
            .with_extra_args(vec!["--no-osc".to_string()]);
        let args = mpv_args("abc123", &opts);
        // URL still last (mpv treats it as a positional file arg);
        // user flag must precede it.
        assert_eq!(args.last().unwrap(), &youtube_url("abc123"));
        let osc_pos = args.iter().position(|a| a == "--no-osc").unwrap();
        let url_pos = args
            .iter()
            .position(|a| a.starts_with("https://"))
            .unwrap();
        assert!(osc_pos < url_pos, "user arg must precede URL: {args:?}");
    }

    #[test]
    fn extra_args_compose_with_audio_only() {
        let opts = PlaybackOptions::default()
            .with_audio_only(true)
            .with_extra_args(vec!["--save-position-on-quit".to_string()]);
        let args = mpv_args("vid", &opts);
        // Order: yttui-managed flags, user args, URL.
        assert_eq!(args[0], "--no-video");
        assert_eq!(args[1], "--save-position-on-quit");
        assert_eq!(args[2], youtube_url("vid"));
    }

    #[test]
    fn url_is_always_last_arg() {
        // mpv treats the URL as a positional file argument; flags after
        // it get parsed as additional files.
        let args = mpv_args(
            "id",
            &PlaybackOptions::default().with_audio_only(true),
        );
        assert!(args.last().unwrap().starts_with("https://"));
    }

    #[test]
    fn play_succeeds_with_zero_exit_binary() {
        let player = MpvPlayer {
            binary: PathBuf::from("true"),
        };
        player.play("anything", &PlaybackOptions::default()).unwrap();
    }

    #[test]
    fn spawn_player_returns_running_player_with_live_pid() {
        // Pin the contract the signal handler depends on: when
        // MpvPlayer spawns the player, the caller must be able to
        // observe the pgid *before* blocking on wait — otherwise the
        // signal-watcher has nothing to kill and we re-orphan mpv on
        // Ctrl-C. `true` exits immediately; we just need a non-zero pid.
        let player = MpvPlayer {
            binary: PathBuf::from("true"),
        };
        let running = player
            .spawn_player("anything", &PlaybackOptions::default())
            .unwrap();
        let pid = running.pid();
        assert!(pid > 0, "pid must be set: {pid}");
        running.wait().unwrap();
    }

    #[test]
    fn play_returns_non_zero_exit_for_failing_binary() {
        let player = MpvPlayer {
            binary: PathBuf::from("false"),
        };
        let err = player
            .play("anything", &PlaybackOptions::default())
            .unwrap_err();
        assert!(
            matches!(err, PlayerError::NonZeroExit(_)),
            "expected NonZeroExit, got {err:?}"
        );
    }

    #[test]
    fn missing_binary_returns_spawn_error() {
        let player = MpvPlayer {
            binary: PathBuf::from("definitely-not-a-real-binary-xyz"),
        };
        let err = player
            .play("anything", &PlaybackOptions::default())
            .unwrap_err();
        assert!(matches!(err, PlayerError::Spawn(_)));
    }

    #[cfg(unix)]
    #[test]
    fn classify_signal_killed_status() {
        use std::os::unix::process::ExitStatusExt;
        // Raw status with low byte = signal number, no exit code set.
        // 15 = SIGTERM.
        let status = ExitStatus::from_raw(15);
        let err = classify_exit(status).unwrap_err();
        assert!(
            matches!(err, PlayerError::KilledBySignal(15)),
            "expected KilledBySignal(15), got {err:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn classify_non_zero_exit_status() {
        use std::os::unix::process::ExitStatusExt;
        // Raw status: exit code 1 in the high byte (the WEXITSTATUS layout).
        let status = ExitStatus::from_raw(1 << 8);
        let err = classify_exit(status).unwrap_err();
        assert!(
            matches!(err, PlayerError::NonZeroExit(1)),
            "expected NonZeroExit(1), got {err:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn classify_zero_exit_is_ok() {
        use std::os::unix::process::ExitStatusExt;
        let status = ExitStatus::from_raw(0);
        classify_exit(status).unwrap();
    }

    /// Test double that records what it was asked to play without spawning.
    struct FakePlayer {
        called_with: std::sync::Mutex<Option<(String, bool)>>,
    }

    impl FakePlayer {
        fn new() -> Self {
            Self {
                called_with: std::sync::Mutex::new(None),
            }
        }
    }

    impl Player for FakePlayer {
        fn spawn_player(
            &self,
            video_id: &str,
            opts: &PlaybackOptions,
        ) -> Result<RunningPlayer, PlayerError> {
            *self.called_with.lock().unwrap() =
                Some((video_id.to_string(), opts.audio_only));
            Ok(RunningPlayer {
                inner: RunningInner::Fake(Ok(())),
            })
        }
    }

    fn result_with_duration(duration: VideoDuration) -> SearchResult {
        SearchResult {
            id: "vid123".to_string(),
            title: "Test".to_string(),
            channel: None,
            duration,
        }
    }

    #[test]
    fn play_result_refuses_upcoming_streams() {
        let player = FakePlayer::new();
        let r = result_with_duration(VideoDuration::Upcoming);
        let err =
            play_result(&player, &r, &PlaybackOptions::default()).unwrap_err();
        assert!(matches!(err, PlayerError::NotPlayable { .. }));
        assert!(
            player.called_with.lock().unwrap().is_none(),
            "player must not be invoked for unplayable videos"
        );
    }

    #[test]
    fn not_playable_reason_uses_human_phrasing() {
        let player = FakePlayer::new();
        let r = result_with_duration(VideoDuration::Upcoming);
        let err =
            play_result(&player, &r, &PlaybackOptions::default()).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("upcoming livestream"),
            "expected human phrasing, got: {msg}"
        );
        assert!(
            !msg.contains("Upcoming"),
            "Debug enum syntax leaked into user-facing message: {msg}"
        );
    }

    #[test]
    fn play_result_allows_live_streams() {
        let player = FakePlayer::new();
        let r = result_with_duration(VideoDuration::Live);
        play_result(&player, &r, &PlaybackOptions::default()).unwrap();
        let called = player.called_with.lock().unwrap().clone().unwrap();
        assert_eq!(called.0, "vid123");
    }

    #[test]
    fn play_result_allows_known_duration() {
        let player = FakePlayer::new();
        let r = result_with_duration(VideoDuration::Seconds(120));
        play_result(&player, &r, &PlaybackOptions::default()).unwrap();
        assert!(player.called_with.lock().unwrap().is_some());
    }

    #[test]
    fn play_result_allows_unknown_duration() {
        // Unknown duration shouldn't block playback — the video might be
        // perfectly playable; we just lack metadata.
        let player = FakePlayer::new();
        let r = result_with_duration(VideoDuration::Unknown);
        play_result(&player, &r, &PlaybackOptions::default()).unwrap();
        assert!(player.called_with.lock().unwrap().is_some());
    }

    #[test]
    fn play_result_forwards_audio_only_flag() {
        let player = FakePlayer::new();
        let r = result_with_duration(VideoDuration::Seconds(60));
        play_result(
            &player,
            &r,
            &PlaybackOptions::default().with_audio_only(true),
        )
        .unwrap();
        let called = player.called_with.lock().unwrap().clone().unwrap();
        assert!(called.1, "audio_only flag must reach the player");
    }
}

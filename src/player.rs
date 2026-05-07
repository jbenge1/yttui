use std::path::PathBuf;
use std::process::{Command, ExitStatus};

use command_group::CommandGroup;
use thiserror::Error;

use crate::search::SearchResult;

#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct PlaybackOptions {
    pub audio_only: bool,
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
    ) -> Result<(), PlayerError>;
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
    let mut args = Vec::new();
    if opts.audio_only {
        args.push("--no-video".to_string());
    }
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
    fn play(
        &self,
        video_id: &str,
        opts: &PlaybackOptions,
    ) -> Result<(), PlayerError> {
        let args = mpv_args(video_id, opts);
        // Spawn the player in its own process group so that:
        //   1. We can kill the entire group (including any helper processes
        //      mpv forks) by killing the leader.
        //   2. A future Ctrl-C handler / panic hook on the parent can clean
        //      up subprocesses without leaking orphans (V1 spec
        //      requirement).
        // Note: SIGKILL on the parent is not interceptable; an orphan can
        // still occur if the user `kill -9`s yttui itself. Catchable
        // signals will be wired up in Slice 3 / 4.
        let mut child = Command::new(&self.binary)
            .args(&args)
            .group_spawn()
            .map_err(PlayerError::Spawn)?;
        let status = child.wait().map_err(PlayerError::Wait)?;
        classify_exit(status)
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
    if !result.duration.is_playable() {
        return Err(PlayerError::NotPlayable {
            reason: result.duration.to_string(),
        });
    }
    player.play(&result.id, opts)
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
            &PlaybackOptions {
                audio_only: true,
            },
        );
        assert_eq!(args[0], "--no-video");
        assert_eq!(args.last().unwrap(), &youtube_url("abc123"));
    }

    #[test]
    fn url_is_always_last_arg() {
        // mpv treats the URL as a positional file argument; flags after
        // it get parsed as additional files.
        let args = mpv_args(
            "id",
            &PlaybackOptions {
                audio_only: true,
            },
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
        fn play(
            &self,
            video_id: &str,
            opts: &PlaybackOptions,
        ) -> Result<(), PlayerError> {
            *self.called_with.lock().unwrap() =
                Some((video_id.to_string(), opts.audio_only));
            Ok(())
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
            &PlaybackOptions {
                audio_only: true,
            },
        )
        .unwrap();
        let called = player.called_with.lock().unwrap().clone().unwrap();
        assert!(called.1, "audio_only flag must reach the player");
    }
}

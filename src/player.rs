use std::path::PathBuf;
use std::process::Command;

use thiserror::Error;

use crate::search::SearchResult;

#[derive(Debug, Clone, Default)]
pub struct PlaybackOptions {
    pub audio_only: bool,
}

#[derive(Debug, Error)]
pub enum PlayerError {
    #[error("failed to launch player: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("player exited with status {0}")]
    NonZeroExit(i32),
    #[error("video is not playable: {reason}")]
    NotPlayable { reason: String },
}

pub trait Player {
    /// Play the given `YouTube` video. Blocks until the player exits.
    ///
    /// # Errors
    /// Returns [`PlayerError`] if the player binary cannot be spawned, the
    /// wait fails, or the player exits non-zero.
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
pub fn youtube_url(video_id: &str) -> String {
    format!("https://www.youtube.com/watch?v={video_id}")
}

#[must_use]
pub fn mpv_args(video_id: &str, opts: &PlaybackOptions) -> Vec<String> {
    let mut args = Vec::new();
    if opts.audio_only {
        args.push("--no-video".to_string());
    }
    args.push(youtube_url(video_id));
    args
}

impl Player for MpvPlayer {
    fn play(
        &self,
        video_id: &str,
        opts: &PlaybackOptions,
    ) -> Result<(), PlayerError> {
        let args = mpv_args(video_id, opts);
        let status = Command::new(&self.binary)
            .args(&args)
            .status()
            .map_err(PlayerError::Spawn)?;
        if !status.success() {
            return Err(PlayerError::NonZeroExit(status.code().unwrap_or(-1)));
        }
        Ok(())
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
            reason: format!("video is {:?}", result.duration),
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
        // The URL is mpv's positional "file" argument and must come after
        // any flags, otherwise mpv parses subsequent flags as files.
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
        // `true` exits 0 regardless of args. Exercises the spawn + wait
        // path without needing real mpv installed.
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

    /// Test double that records what it was asked to play without spawning.
    struct FakePlayer {
        called_with: std::sync::Mutex<Option<(String, bool)>>,
        result: std::sync::Mutex<Result<(), PlayerError>>,
    }

    impl FakePlayer {
        fn ok() -> Self {
            Self {
                called_with: std::sync::Mutex::new(None),
                result: std::sync::Mutex::new(Ok(())),
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
            std::mem::replace(&mut *self.result.lock().unwrap(), Ok(()))
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
        let player = FakePlayer::ok();
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
    fn play_result_allows_live_streams() {
        let player = FakePlayer::ok();
        let r = result_with_duration(VideoDuration::Live);
        play_result(&player, &r, &PlaybackOptions::default()).unwrap();
        let called = player.called_with.lock().unwrap().clone().unwrap();
        assert_eq!(called.0, "vid123");
    }

    #[test]
    fn play_result_allows_known_duration() {
        let player = FakePlayer::ok();
        let r = result_with_duration(VideoDuration::Seconds(120));
        play_result(&player, &r, &PlaybackOptions::default()).unwrap();
        assert!(player.called_with.lock().unwrap().is_some());
    }

    #[test]
    fn play_result_allows_unknown_duration() {
        // Unknown duration shouldn't block playback — the video might be
        // perfectly playable; we just lack metadata.
        let player = FakePlayer::ok();
        let r = result_with_duration(VideoDuration::Unknown);
        play_result(&player, &r, &PlaybackOptions::default()).unwrap();
        assert!(player.called_with.lock().unwrap().is_some());
    }

    #[test]
    fn play_result_forwards_audio_only_flag() {
        let player = FakePlayer::ok();
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

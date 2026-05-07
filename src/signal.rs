//! Signal handling: ensure SIGINT/SIGTERM at the parent terminal does
//! not leave child subprocess groups (mpv, yt-dlp) orphaned.
//!
//! ## Why this exists
//!
//! `mpv` runs in its own process group via `command_group::group_spawn`,
//! and during playback `yttui` is *not* in raw mode (we leave the alt
//! screen so mpv can take the terminal). A `Ctrl-C` at the parent
//! terminal therefore generates `SIGINT` and the kernel delivers it
//! only to the foreground process group. `yttui`'s pgid ≠ `mpv`'s pgid,
//! so `yttui` dies, `child.wait()` is abandoned, and `mpv` keeps
//! running with PPID re-parented to launchd/init. The user notices
//! nothing until they `q` out of mpv and find no shell prompt.
//!
//! V1 acceptance criterion #4 — "killing the process at any point
//! leaves no orphaned yt-dlp/mpv subprocesses" — failed for the
//! signal path before this module existed.
//!
//! ## How this fixes it
//!
//! 1. [`ChildRegistry`] is a process-wide handle `play_with_swap`
//!    registers the live mpv group leader against, and unregisters
//!    when the player returns.
//! 2. [`install_handler`] spawns a watcher thread that blocks on
//!    `SIGINT`/`SIGTERM` via `signal-hook`. On signal it kills the
//!    registered process group, restores the terminal, and exits.
//!
//! Manual verification steps live in the README: starting playback,
//! sending `kill -INT $(pgrep yttui)` from another shell, and
//! confirming `pgrep mpv` returns nothing.

use std::io;
use std::sync::Mutex;

/// Process-wide registry. The signal-watcher thread captures a static
/// reference; `play_with_swap` registers the live mpv pid here so the
/// watcher can target the right process group on signal.
pub static REGISTRY: ChildRegistry = ChildRegistry::new();

/// Tracks the live child-process-group leader pid.
///
/// Terminated when the watcher receives a fatal signal. Currently we
/// only ever have one such child (mpv during playback) so an
/// `Option<u32>` is enough; promote to a `Vec` if a future feature
/// keeps multiple long-lived subprocesses alive concurrently.
#[derive(Debug, Default)]
pub struct ChildRegistry {
    inner: Mutex<Option<u32>>,
}

impl ChildRegistry {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            inner: Mutex::new(None),
        }
    }

    /// Register a process-group-leader pid. Returns the previous value
    /// (almost always `None`; a non-`None` return means the previous
    /// playback didn't unregister, which would be a bug).
    ///
    /// # Panics
    /// Panics only if the inner mutex was poisoned by a thread that
    /// panicked while holding the lock — recovery is impossible at
    /// that point and we'd rather see the panic than silently corrupt
    /// the registry.
    pub fn register(&self, pid: u32) -> Option<u32> {
        let mut g = self.inner.lock().expect("ChildRegistry mutex poisoned");
        g.replace(pid)
    }

    /// Unregister whatever pid is currently tracked. Idempotent.
    ///
    /// # Panics
    /// Panics if the inner mutex is poisoned. See [`Self::register`].
    pub fn clear(&self) -> Option<u32> {
        let mut g = self.inner.lock().expect("ChildRegistry mutex poisoned");
        g.take()
    }

    /// Snapshot the currently-tracked pid without clearing it.
    ///
    /// # Panics
    /// Panics if the inner mutex is poisoned. See [`Self::register`].
    #[must_use]
    pub fn current(&self) -> Option<u32> {
        *self.inner.lock().expect("ChildRegistry mutex poisoned")
    }

    /// Send `SIGTERM` to the registered process group, if any. Used by
    /// the signal watcher; safe to call from any thread. Errors are
    /// swallowed — we're already on the way out and there's nothing
    /// useful to do with them.
    #[cfg(unix)]
    pub fn kill_group(&self) {
        let Some(pid) = self.current() else {
            return;
        };
        // `killpg` would be more idiomatic but isn't in libstd; sending
        // to `-pid` does the same job (POSIX kill(2): negative pid
        // targets the process group whose leader has |pid|).
        let pid_i = i32::try_from(pid).unwrap_or(i32::MAX);
        // Use std's libc-free path: spawn `kill -TERM -<pid>`. Avoids
        // pulling in `nix` directly and stays inside the project's
        // `unsafe_code = forbid` boundary.
        let _ = std::process::Command::new("kill")
            .arg("-TERM")
            .arg(format!("-{pid_i}"))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }

    #[cfg(not(unix))]
    pub fn kill_group(&self) {
        // Non-Unix: process groups don't exist the same way; the bin
        // itself isn't supported on non-Unix targets at the moment.
    }
}

/// Install a SIGINT/SIGTERM watcher thread. On signal:
///   1. Kill the registered child process group (if any).
///   2. Run `cleanup` (typically: `disable_raw_mode` + leave alt screen).
///   3. Exit the process with status 130 (128 + SIGINT) so shells see
///      "interrupted by user", consistent with most CLI tools.
///
/// # Errors
/// Returns the underlying `io::Error` if `signal-hook` cannot register
/// the signals (extremely rare; usually only happens when another
/// crate has already taken them).
pub fn install_handler<F>(
    registry: &'static ChildRegistry,
    cleanup: F,
) -> io::Result<()>
where
    F: Fn() + Send + 'static,
{
    use signal_hook::consts::{SIGINT, SIGTERM};
    use signal_hook::iterator::Signals;

    let mut signals = Signals::new([SIGINT, SIGTERM])?;
    std::thread::spawn(move || {
        // Single iteration is enough — on the first fatal signal, kill
        // children, run cleanup, and exit. If the cleanup hangs and the
        // user hits Ctrl-C again, the process still gets the default
        // signal handler (we exited the thread already).
        if let Some(_sig) = signals.forever().next() {
            registry.kill_group();
            cleanup();
            // 130 = "terminated by SIGINT", the conventional shell exit
            // code for Ctrl-C. SIGTERM users will see the same; close
            // enough.
            std::process::exit(130);
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_starts_empty() {
        let r = ChildRegistry::new();
        assert_eq!(r.current(), None);
    }

    #[test]
    fn register_returns_previous_value() {
        let r = ChildRegistry::new();
        assert_eq!(r.register(1234), None);
        assert_eq!(r.current(), Some(1234));
        // A second register without a clear is a programming error,
        // surfaced as Some(_) so callers can assert/log.
        assert_eq!(r.register(5678), Some(1234));
        assert_eq!(r.current(), Some(5678));
    }

    #[test]
    fn clear_returns_previous_and_empties() {
        let r = ChildRegistry::new();
        r.register(42);
        assert_eq!(r.clear(), Some(42));
        assert_eq!(r.current(), None);
        // Idempotent: clearing again is a no-op.
        assert_eq!(r.clear(), None);
    }

    #[test]
    fn kill_group_with_no_registered_pid_is_a_noop() {
        // Empty registry: kill_group must not spawn anything or panic.
        // Hard to assert "didn't spawn" directly, so just exercise the
        // path and confirm it returns cleanly.
        let r = ChildRegistry::new();
        r.kill_group();
        assert_eq!(r.current(), None);
    }
}
